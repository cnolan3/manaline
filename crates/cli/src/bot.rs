//! The built-in random opponent as a protocol client: it joins a seat over
//! the socket like any other client, so it exercises exactly what a TUI or
//! an agent does.

use anyhow::{anyhow, Context, Result};
use engine::{Action, Outcome, Seat};
use protocol::{Client, ClientError, Endpoint, ServerMessage, Token};
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

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
    let mut client = Client::connect(&settings.endpoint)
        .await
        .with_context(|| format!("connecting to {}", settings.endpoint))?;
    let welcome = client.hello(&settings.token, Some(&settings.name)).await?;
    let seat = welcome.role.seat().ok_or_else(|| anyhow!("the bot needs a seat token, not a spectator token"))?;
    client.subscribe().await?;
    if !welcome.lobby.started {
        match client.set_deck(&settings.decklist).await? {
            Ok(()) => {}
            Err(violations) => {
                let list: Vec<String> = violations.iter().map(ToString::to_string).collect();
                return Err(anyhow!("deck rejected: {}", list.join("; ")));
            }
        }
        client.ready().await?;
    }
    let outcome = play(&mut client, settings.seed).await?;
    Ok((seat, outcome))
}

/// The bot's decision loop, usable by any caller holding a joined client.
pub async fn play(client: &mut Client, seed: u64) -> Result<Option<Outcome>> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    loop {
        let (acts, version) = match client.get_legal_actions().await {
            Ok(x) => x,
            // Not started yet: wait for the lobby to move.
            Err(ClientError::Protocol(e)) if e.code == protocol::ErrorCode::BadRequest => {
                client.next_push().await?;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let playable: Vec<_> = acts.iter().filter(|a| !matches!(a.action, Action::Concede)).collect();
        if let Some(pick) = playable.choose(&mut rng) {
            match client.act_by_id(pick.id, version).await {
                Ok((_, state, _)) => {
                    if state.outcome.is_some() {
                        return Ok(state.outcome);
                    }
                    continue;
                }
                Err(ClientError::Protocol(e)) if e.retryable => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let state = client.get_state().await?;
        if state.outcome.is_some() {
            return Ok(state.outcome);
        }
        match client.next_push().await {
            Ok(ServerMessage::Event { event: engine::EventBase::GameOver { outcome }, .. }) => return Ok(Some(outcome)),
            Ok(_) => continue,
            Err(ClientError::Frame(protocol::FrameError::Closed)) => return Ok(None),
            Err(e) => return Err(e.into()),
        }
    }
}
