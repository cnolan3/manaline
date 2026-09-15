//! The built-in random opponent as a protocol client: it joins a seat over
//! the socket like any other client, so it exercises exactly what a TUI or
//! an agent does. Its link is expected to drop (§2) and the daemon keeps the
//! seat, so every step waits a reconnect out instead of ending the game.

use anyhow::{anyhow, Context, Result};
use engine::{Action, Outcome, Seat};
use protocol::{
    async_client, AsyncClient, ClientError, ConnState, Endpoint, FrameError, Joined, ReconnectConfig, ReconnectPolicy, ServerMessage, Token,
};
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::future::Future;
use tokio::sync::mpsc::Receiver;

pub struct BotSettings {
    pub endpoint: Endpoint,
    pub token: Token,
    pub name: String,
    pub decklist: String,
    pub seed: u64,
}

/// Join, submit the deck, ready up, then play random legal actions (never
/// conceding while any other action exists) until the game ends.
pub async fn run(settings: BotSettings) -> Result<(Seat, Option<Outcome>)> {
    let Joined {
        client,
        mut pushes,
        welcome,
    } = async_client::join(ReconnectConfig {
        endpoint: settings.endpoint.clone(),
        token: settings.token.clone(),
        name: Some(settings.name.clone()),
        policy: ReconnectPolicy::default(),
    })
    .await
    .with_context(|| format!("connecting to {}", settings.endpoint))?;
    let seat = welcome
        .role
        .seat()
        .ok_or_else(|| anyhow!("the bot needs a seat token, not a spectator token"))?;
    // Subscribe before anything that changes the lobby, so no update is missed.
    retry(&client, || client.subscribe()).await.map_err(lost)?;
    if !welcome.lobby.started {
        let decklist = settings.decklist.as_str();
        match retry(&client, || client.set_deck(decklist)).await.map_err(lost)? {
            Ok(()) => {}
            Err(violations) => {
                let list: Vec<String> = violations.iter().map(ToString::to_string).collect();
                return Err(anyhow!("deck rejected: {}", list.join("; ")));
            }
        }
        retry(&client, || client.ready()).await.map_err(lost)?;
    }
    let outcome = play(&client, &mut pushes, settings.seed).await?;
    Ok((seat, outcome))
}

/// The bot's decision loop, usable by any caller holding a joined client and
/// the push channel that came with it.
pub async fn play(client: &AsyncClient, pushes: &mut Receiver<ServerMessage>, seed: u64) -> Result<Option<Outcome>> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    loop {
        // The push channel is bounded and the task that fills it also delivers
        // our replies, so a holder that stops draining stalls its own requests:
        // a bot taking many actions in a row would wedge itself. Dropping a
        // `GameOver` here costs nothing, because the daemon records the outcome
        // before it pushes the event and then offers no more actions, so the
        // `get_state` below sees it and ends the loop.
        while pushes.try_recv().is_ok() {}
        let (acts, version, _) = match retry(client, || client.get_legal_actions()).await {
            Ok(x) => x,
            // Not started yet: wait for the lobby to move.
            Err(ClientError::Protocol(e)) if e.code == protocol::ErrorCode::BadRequest => {
                wake(client, pushes).await?;
                continue;
            }
            Err(e) => return Err(lost(e)),
        };
        let playable: Vec<_> = acts.iter().filter(|a| !matches!(a.action, Action::Concede)).collect();
        if let Some(pick) = playable.choose(&mut rng) {
            let action = pick.action.clone();
            match retry(client, || client.act(action.clone(), version)).await {
                Ok((_, state, _)) => {
                    if state.outcome.is_some() {
                        return Ok(state.outcome);
                    }
                    continue;
                }
                // Also how a re-sent `act` comes back once the daemon has
                // applied the first copy: re-read the actions and carry on.
                Err(ClientError::Protocol(e)) if e.retryable => continue,
                Err(e) => return Err(lost(e)),
            }
        }
        let state = retry(client, || client.get_state()).await.map_err(lost)?;
        if state.outcome.is_some() {
            return Ok(state.outcome);
        }
        match wake(client, pushes).await? {
            Some(ServerMessage::Event {
                event: engine::EventBase::GameOver { outcome },
                ..
            }) => return Ok(Some(outcome)),
            _ => continue,
        }
    }
}

/// Run one request, waiting a dropped link out rather than failing on it: the
/// client is reconnecting underneath us, so a framing error means "ask again
/// once the seat is back", not "the game is over".
async fn retry<T, F, Fut>(client: &AsyncClient, mut step: F) -> Result<T, ClientError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ClientError>>,
{
    loop {
        match step().await {
            Err(ClientError::Frame(_)) => connected(client).await?,
            done => return done,
        }
    }
}

/// Wait until the client has a link again, failing only once its policy has
/// run out — from then on every request fails anyway.
async fn connected(client: &AsyncClient) -> Result<(), ClientError> {
    let mut states = client.watch_state();
    loop {
        // The clone starts out unread, so mark before waiting: otherwise
        // `changed` returns at once on a transition already acted on.
        let state = *states.borrow_and_update();
        if state.is_connected() {
            return Ok(());
        }
        if state == ConnState::GaveUp {
            return Err(ClientError::Frame(FrameError::Closed));
        }
        states.changed().await.map_err(|_| ClientError::Frame(FrameError::Closed))?;
    }
}

/// Wait for the game to move: a pushed message, or `None` for a reconnect —
/// nothing that happened while the link was down reached us, so the only safe
/// move then is to look at the game again.
async fn wake(client: &AsyncClient, pushes: &mut Receiver<ServerMessage>) -> Result<Option<ServerMessage>> {
    let mut states = client.watch_state();
    let mut state = *states.borrow_and_update();
    loop {
        if state == ConnState::GaveUp {
            return Err(gone());
        }
        tokio::select! {
            push = pushes.recv() => return push.map(Some).ok_or_else(gone),
            changed = states.changed() => {
                if changed.is_err() {
                    return Err(gone());
                }
                state = *states.borrow_and_update();
                if state.is_connected() {
                    return Ok(None);
                }
            }
        }
    }
}

/// The one connection failure left to report: everything short of the client
/// giving up for good is waited out.
fn gone() -> anyhow::Error {
    anyhow!("lost the connection to the game and could not get it back")
}

fn lost(e: ClientError) -> anyhow::Error {
    match e {
        ClientError::Frame(_) => gone(),
        e => e.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{EventBase, GameView, Phase};
    use protocol::{ClientEnvelope, ClientMessage, FramedReader, FramedWriter, LegalAction, ServerEnvelope};
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tokio::io::DuplexStream;

    /// A handful of pushes per action, as a real game emits, so the stub never
    /// blocks on the socket: the pile-up that matters builds up across actions.
    const PER_ACT: usize = 4;
    const ACTIONS: usize = 300;

    fn view(outcome: Option<Outcome>, version: u64) -> GameView {
        GameView {
            you: Some(Seat(0)),
            turn: 1,
            active_player: Seat(0),
            phase: Phase::Main1,
            priority: Some(Seat(0)),
            must_act: BTreeMap::new(),
            prompt: None,
            state_version: version,
            outcome,
            stack: Vec::new(),
            players: Vec::new(),
            objects: BTreeMap::new(),
        }
    }

    /// A stub daemon with one action always on offer, pushing an event batch
    /// for every action taken and ending the game on the last one.
    async fn stub(sock: DuplexStream) {
        let (r, w) = tokio::io::split(sock);
        let mut reader: FramedReader<_, ClientEnvelope> = FramedReader::new(r);
        let mut writer: FramedWriter<_, ServerEnvelope> = FramedWriter::new(w);
        let mut taken = 0u64;
        while let Ok(env) = reader.recv().await {
            let reply = match env.msg {
                ClientMessage::GetLegalActions => ServerMessage::LegalActions {
                    actions: vec![LegalAction {
                        id: 0,
                        action: Action::PassPriority,
                        description: "pass priority".into(),
                    }],
                    state_version: taken,
                    reason: None,
                },
                ClientMessage::Act { .. } => {
                    taken += 1;
                    for _ in 0..PER_ACT {
                        let push = ServerEnvelope {
                            req: None,
                            msg: ServerMessage::Event {
                                event: EventBase::PriorityPassed { seat: Seat(0) },
                                state_version: taken,
                            },
                        };
                        if writer.send(&push).await.is_err() {
                            return;
                        }
                    }
                    let over = (taken as usize >= ACTIONS).then_some(Outcome::Draw);
                    ServerMessage::Ack {
                        applied: Action::PassPriority,
                        events: Vec::new(),
                        state: view(over, taken),
                        legal_actions: Vec::new(),
                    }
                }
                ClientMessage::GetState => ServerMessage::State { state: view(None, taken) },
                _ => ServerMessage::Ok,
            };
            if writer.send(&ServerEnvelope { req: env.req, msg: reply }).await.is_err() {
                return;
            }
        }
    }

    /// A bot that acts without pause still finishes: far more pushes arrive
    /// than the channel holds, and the loop drains them instead of wedging the
    /// task that also carries its replies.
    #[tokio::test]
    async fn a_flood_of_pushes_does_not_stall_the_bot() {
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        tokio::spawn(stub(theirs));
        let (reader, writer) = tokio::io::split(ours);
        let (client, mut pushes) = async_client::spawn(Box::new(reader), Box::new(writer));

        let outcome = tokio::time::timeout(Duration::from_secs(20), play(&client, &mut pushes, 7))
            .await
            .expect("the bot kept playing")
            .unwrap();
        assert_eq!(outcome, Some(Outcome::Draw));
    }
}
