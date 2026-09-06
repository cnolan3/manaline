//! Integration tests over real sockets: bots play through the protocol,
//! version and seat checks, reconnects, the replay log, and the §10 guard
//! that inspects the raw bytes a seat receives.

use daemon::{CreateGame, Daemon, DaemonConfig, DaemonHandle};
use engine::{Action, Outcome, Seat};
use protocol::messages::{ClientEnvelope, ServerEnvelope};
use protocol::{Client, ClientError, ClientMessage, Endpoint, ErrorCode, FramedReader, FramedWriter, ServerMessage, Token};
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("manaline-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Running {
    handle: DaemonHandle,
    endpoint: Endpoint,
    tokens: Vec<Token>,
    spectator: Token,
    replay_path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

async fn start(seats: u8, seed: u64, tcp: bool) -> Running {
    let dir = scratch();
    let config = DaemonConfig {
        socket: (!tcp).then(|| dir.join("game.sock")),
        no_socket: tcp,
        tcp: tcp.then(|| "127.0.0.1:0".to_string()),
        parent_pid: None,
        replay_dir: Some(dir.join("games")),
        create: Some(CreateGame {
            format: if seats == 2 { "cube".into() } else { "free-for-all".into() },
            seats,
            seed: Some(seed),
        }),
        cards: Arc::new(cards::core()),
    };
    let daemon = Daemon::bind(config).await.unwrap();
    let info = daemon.info().clone();
    let handle = daemon.handle();
    let endpoint = match (info.socket, info.tcp) {
        (Some(p), _) => Endpoint::Unix(p),
        (None, Some(a)) => Endpoint::Tcp(a.to_string()),
        _ => unreachable!(),
    };
    let task = tokio::spawn(async move { daemon.run().await.unwrap() });
    Running {
        handle,
        endpoint,
        tokens: info.seat_tokens,
        spectator: info.spectator_token.unwrap(),
        replay_path: info.replay_path.unwrap(),
        task,
    }
}

async fn seat_client(r: &Running, seat: usize, name: &str, deck: &str) -> Client {
    let mut c = Client::connect(&r.endpoint).await.unwrap();
    let w = c.hello(&r.tokens[seat], Some(name)).await.unwrap();
    assert_eq!(w.role, protocol::Role::Seat(Seat(seat as u8)));
    c.set_deck(cards::deck_text(deck).unwrap()).await.unwrap().unwrap();
    c.subscribe().await.unwrap();
    c
}

/// Play as `seat` with random choices until the game ends.
async fn bot_loop(mut c: Client, seed: u64) -> Client {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    loop {
        let (acts, version) = c.get_legal_actions().await.unwrap();
        let playable: Vec<_> = acts.iter().filter(|a| !matches!(a.action, Action::Concede)).collect();
        if let Some(pick) = playable.choose(&mut rng) {
            match c.act_by_id(pick.id, version).await {
                Ok((_, state, _)) => {
                    if state.outcome.is_some() {
                        return c;
                    }
                }
                Err(ClientError::Protocol(e)) if e.retryable => continue,
                Err(e) => panic!("{e}"),
            }
            continue;
        }
        // Not our turn: wait for something to happen.
        match c.next_push().await.unwrap() {
            ServerMessage::Event { event: engine::EventBase::GameOver { .. }, .. } => return c,
            _ => continue,
        }
    }
}

#[tokio::test]
async fn bots_play_a_whole_game_over_a_unix_socket_and_the_log_replays_it() {
    let r = start(2, 11, false).await;
    let mut a = seat_client(&r, 0, "Ann", "green").await;
    let b = seat_client(&r, 1, "Bob", "red").await;
    let mut spec = Client::connect(&r.endpoint).await.unwrap();
    let w = spec.hello(&r.spectator, None).await.unwrap();
    assert_eq!(w.role, protocol::Role::Spectator);
    assert!(w.state.is_none(), "not started yet");
    assert_eq!(w.lobby.seats.len(), 2);
    assert!(w.lobby.seats.iter().all(|s| s.deck_ok && s.connected && !s.ready));

    a.ready().await.unwrap();
    let mut status = r.handle.status();
    b_ready_and_start(b, &mut a, &mut status).await;

    let mut status = r.handle.status();
    status.wait_for(|s| s.game_over).await.unwrap();
    assert!(r.handle.is_over().await);

    let final_state = spec.get_state().await.unwrap();
    assert!(matches!(final_state.outcome, Some(Outcome::Winner(_))));
    assert_eq!(final_state.players[0].name, "Ann");

    // The replay log reconstructs the same game.
    let (header, rebuilt) = daemon::replay::rebuild(&r.replay_path, Arc::new(cards::core()), None).unwrap();
    assert_eq!(header.seed, 11);
    assert_eq!(header.players[0].deck.len(), 40);
    assert_eq!(rebuilt.view_spectator(), final_state);

    r.handle.shutdown();
    r.task.await.unwrap();
}

async fn b_ready_and_start(
    b: Client,
    a: &mut Client,
    status: &mut tokio::sync::watch::Receiver<daemon::Status>,
) {
    let mut b = b;
    b.ready().await.unwrap();
    status.wait_for(|s| s.state_version > 0 || !s.must_act.is_empty()).await.unwrap();
    // Both seats now see a started lobby and a state.
    let st = a.get_state().await.unwrap();
    assert_eq!(st.turn, 0, "mulligans first");
    let a_task = tokio::spawn(bot_loop(std::mem::replace(a, dummy_client()), 1));
    let b_task = tokio::spawn(bot_loop(b, 2));
    let a_back = a_task.await.unwrap();
    b_task.await.unwrap();
    *a = a_back;
}

fn dummy_client() -> Client {
    let (x, _y) = tokio::io::duplex(16);
    let (xr, xw) = tokio::io::split(x);
    Client::from_parts(Box::new(xr), Box::new(xw))
}

#[tokio::test]
async fn bots_play_over_tcp_at_four_seats() {
    let r = start(4, 5, true).await;
    let decks = ["white", "blue", "black", "red"];
    let mut clients = Vec::new();
    for (i, d) in decks.iter().enumerate() {
        let mut c = seat_client(&r, i, &format!("P{i}"), d).await;
        c.ready().await.unwrap();
        clients.push(c);
    }
    let tasks: Vec<_> = clients.into_iter().enumerate().map(|(i, c)| tokio::spawn(bot_loop(c, i as u64))).collect();
    for t in tasks {
        t.await.unwrap();
    }
    assert!(r.handle.is_over().await);
    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn versions_tokens_and_turn_order_are_enforced() {
    let r = start(2, 3, false).await;

    // Wrong token, wrong version.
    let mut bad = Client::connect(&r.endpoint).await.unwrap();
    let err = bad.hello(&Token("nope".into()), None).await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(e) if e.code == ErrorCode::BadToken));
    let err = bad
        .request(ClientMessage::Hello { token: r.tokens[0].clone(), protocol_version: 99, name: None })
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Protocol(e) if e.code == ErrorCode::UnsupportedVersion));
    // Acting before hello.
    let err = bad.get_state().await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(e) if e.code == ErrorCode::BadRequest));

    let mut a = seat_client(&r, 0, "A", "green").await;
    let mut b = seat_client(&r, 1, "B", "red").await;
    // Ready without a deck is refused; a short deck is rejected with reasons.
    let mut c = Client::connect(&r.endpoint).await.unwrap();
    c.hello(&r.spectator, None).await.unwrap();
    let err = c.ready().await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(e) if e.code == ErrorCode::BadRequest));
    let rejected = a.set_deck("10 Forest\n").await.unwrap().unwrap_err();
    assert!(matches!(rejected[0], engine::Violation::TooFewCards { .. }));
    let rejected = a.set_deck("40 Black Lotus\n").await.unwrap().unwrap_err();
    assert!(matches!(rejected[0], engine::Violation::Unparsable { .. }));
    a.set_deck(cards::deck_text("green").unwrap()).await.unwrap().unwrap();

    a.ready().await.unwrap();
    b.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();

    // Whoever must act first: the other seat is told it is not their turn.
    let must = status.borrow().must_act.clone();
    let (&acting, _) = must.iter().next().unwrap();
    let (actor, other) = if acting == Seat(0) { (&mut a, &mut b) } else { (&mut b, &mut a) };
    let (acts, version) = actor.get_legal_actions().await.unwrap();
    assert!(!acts.is_empty());
    let (other_acts, _) = other.get_legal_actions().await.unwrap();
    assert!(other_acts.is_empty());
    let err = other.act(Action::Mulligan { keep: true }, version).await.unwrap_err();
    assert!(matches!(&err, ClientError::Protocol(e) if e.code == ErrorCode::NotYourTurnToAct && e.retryable));

    // Stale version.
    let err = actor.act_by_id(0, version + 5).await.unwrap_err();
    match err {
        ClientError::Protocol(e) => {
            assert_eq!(e.code, ErrorCode::StaleStateVersion);
            assert!(e.retryable);
            assert_eq!(e.state_version, Some(version));
        }
        other => panic!("{other}"),
    }
    // Both forms of act, and an unknown id.
    let err = actor.act_by_id(999, version).await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(e) if e.code == ErrorCode::BadRequest));
    let (_, state, _) = actor.act(Action::Mulligan { keep: true }, version).await.unwrap();
    assert_eq!(state.state_version, version + 1);
    let err = actor.act_by_id(0, version).await.unwrap_err();
    assert!(matches!(err, ClientError::Protocol(e) if e.code == ErrorCode::StaleStateVersion));

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn a_seat_that_disconnects_keeps_its_seat_and_resumes() {
    let r = start(2, 8, false).await;
    let mut a = seat_client(&r, 0, "A", "green").await;
    let mut b = seat_client(&r, 1, "B", "red").await;
    a.ready().await.unwrap();
    b.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();
    drop(a);

    // The lobby shows seat 0 gone.
    let lobby = loop {
        match b.next_push().await.unwrap() {
            ServerMessage::Lobby { lobby } if !lobby.seats[0].connected => break lobby,
            _ => continue,
        }
    };
    assert!(lobby.started);

    // Reconnect with the same token: welcomed straight into the running game.
    let mut a2 = Client::connect(&r.endpoint).await.unwrap();
    let w = a2.hello(&r.tokens[0], None).await.unwrap();
    assert_eq!(w.role, protocol::Role::Seat(Seat(0)));
    let state = w.state.expect("game in progress");
    assert_eq!(state.you, Some(Seat(0)));
    assert!(w.lobby.seats[0].connected);
    let (acts, _) = a2.get_legal_actions().await.unwrap();
    let must = state.must_act.contains_key(&Seat(0));
    assert_eq!(!acts.is_empty(), must);

    r.handle.shutdown();
    r.task.await.unwrap();
}

/// The §10 guard: everything seat 1 receives over the wire, inspected as
/// raw JSON, contains no object outside the public zones and its own hand,
/// no other seat's hand contents, and no library contents.
#[tokio::test]
async fn the_wire_never_carries_another_seats_hidden_information() {
    let r = start(2, 21, false).await;
    let a = seat_client(&r, 0, "A", "green").await;
    let a_ready = async move {
        let mut a = a;
        a.ready().await.unwrap();
        a
    };

    // Seat 1 speaks the protocol by hand so every raw line can be checked.
    let stream = tokio::net::UnixStream::connect(match &r.endpoint {
        Endpoint::Unix(p) => p.clone(),
        _ => unreachable!(),
    })
    .await
    .unwrap();
    let (rd, wr) = stream.into_split();
    let mut raw = RawSeat {
        reader: FramedReader::new(rd),
        writer: FramedWriter::new(wr),
        next: 1,
        me: Seat(1),
        lines: 0,
        pushed: Default::default(),
    };
    raw.request(ClientMessage::Hello { token: r.tokens[1].clone(), protocol_version: 1, name: Some("B".into()) })
        .await;
    raw.request(ClientMessage::SetDeck { decklist: cards::deck_text("red").unwrap().into(), commander: None })
        .await;
    raw.request(ClientMessage::Subscribe).await;
    let a = a_ready.await;
    raw.request(ClientMessage::Ready).await;

    let a_task = tokio::spawn(bot_loop(a, 4));
    let mut rng = ChaCha8Rng::seed_from_u64(5);
    loop {
        let reply = raw.request(ClientMessage::GetLegalActions).await;
        let (acts, version) = match reply {
            ServerMessage::LegalActions { actions, state_version, .. } => (actions, state_version),
            other => panic!("{other:?}"),
        };
        let playable: Vec<_> = acts.iter().filter(|x| !matches!(x.action, Action::Concede)).collect();
        if let Some(pick) = playable.choose(&mut rng) {
            match raw.request(ClientMessage::Act { action_id: Some(pick.id), action: None, state_version: version }).await {
                ServerMessage::Ack { state, .. } if state.outcome.is_some() => break,
                ServerMessage::Ack { .. } | ServerMessage::Error(_) => continue,
                other => panic!("{other:?}"),
            }
        }
        if let ServerMessage::State { state } = raw.request(ClientMessage::GetState).await {
            if state.outcome.is_some() {
                break;
            }
        }
        raw.next_push().await;
    }
    a_task.await.unwrap();
    assert!(raw.lines > 200, "checked {} lines", raw.lines);
    r.handle.shutdown();
    r.task.await.unwrap();
}

struct RawSeat {
    reader: FramedReader<tokio::net::unix::OwnedReadHalf, ServerEnvelope>,
    writer: FramedWriter<tokio::net::unix::OwnedWriteHalf, ClientEnvelope>,
    next: u64,
    me: Seat,
    lines: usize,
    pushed: std::collections::VecDeque<ServerMessage>,
}

impl RawSeat {
    async fn request(&mut self, msg: ClientMessage) -> ServerMessage {
        let req = self.next;
        self.next += 1;
        self.writer.send(&ClientEnvelope { req: Some(req), msg }).await.unwrap();
        loop {
            let env = self.recv_checked().await;
            if env.req == Some(req) {
                return env.msg;
            }
            if env.req.is_none() {
                self.pushed.push_back(env.msg);
            }
        }
    }

    async fn next_push(&mut self) -> ServerMessage {
        if let Some(m) = self.pushed.pop_front() {
            return m;
        }
        loop {
            let env = self.recv_checked().await;
            if env.req.is_none() {
                return env.msg;
            }
        }
    }

    async fn recv_checked(&mut self) -> ServerEnvelope {
        let bytes = self.reader.recv_raw().await.unwrap();
        self.lines += 1;
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        check_value(&value, self.me, &String::from_utf8_lossy(&bytes));
        serde_json::from_value(value).unwrap()
    }
}

/// Walk every JSON object in a message and apply the hidden-information rules.
fn check_value(v: &serde_json::Value, me: Seat, line: &str) {
    let me_n = me.0 as u64;
    if let Some(obj) = v.as_object() {
        // An ObjectView: must be public, or in my hand.
        if let (Some(zone), Some(owner)) = (obj.get("zone").and_then(|z| z.as_str()), obj.get("owner").and_then(|o| o.as_u64())) {
            let public = matches!(zone, "battlefield" | "graveyard" | "exile" | "stack" | "command");
            assert!(public || (zone == "hand" && owner == me_n), "leaked object in {zone} owned by {owner}: {line}");
        }
        // A PlayerView: hands of others hidden, libraries always a count.
        if let (Some(seat), Some(hand)) = (obj.get("seat").and_then(|s| s.as_u64()), obj.get("hand")) {
            if hand.get("yours").is_some() {
                assert_eq!(seat, me_n, "another seat's hand contents on the wire: {line}");
            }
            if let Some(lib) = obj.get("library") {
                assert!(lib.get("count").is_some() && lib.as_object().map(|o| o.len()) == Some(1), "library contents on the wire: {line}");
            }
            assert!(obj.get("mana_pool").map(|m| m.is_null() || seat == me_n).unwrap_or(true));
        }
        // A draw event: cards only for my own draws.
        if obj.get("kind").and_then(|k| k.as_str()) == Some("drew") {
            let seat = obj.get("seat").and_then(|s| s.as_u64()).unwrap();
            if obj.get("cards").and_then(|c| c.get("yours")).is_some() {
                assert_eq!(seat, me_n, "another seat's draw on the wire: {line}");
            }
        }
        for child in obj.values() {
            check_value(child, me, line);
        }
    } else if let Some(arr) = v.as_array() {
        for child in arr {
            check_value(child, me, line);
        }
    }
}
