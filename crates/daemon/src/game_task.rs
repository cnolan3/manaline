//! One game, all of it. Everything that is about a single table — the mutex
//! around its `Lobby` and `Game`, its replay log, its status watch, its
//! broadcast channel and its idle bookkeeping — lives in a `GameShared` behind
//! an `Arc`. Concurrency is still the boring §5 design; there is simply one of
//! it per game, so a server can hold many (§2.2, "the daemon, pluralised").
//!
//! Games know nothing about one another. Nothing in this file touches the
//! registry, and the only per-process state it reads is the immutable
//! `Context` (cards, legality, where logs go, the away-from-the-table policy).

use crate::lobby::Lobby;
use crate::replay::{ReplayHeader, ReplayPlayer, ReplayWriter};
use engine::text::describe_action;
use engine::{ActReason, Action, CardDb, Event, Game, GameConfig, PlayerSetup, Seat, Violation};
use protocol::{ClientMessage, ErrorCode, GameId, LegalAction, LobbyView, ProtocolError, Role, ServerMessage, PROTOCOL_VERSION};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch, Mutex};
use tracing::{debug, info, warn};

/// What the daemon does about a seat that has gone away mid-game (§2.2, M8).
/// A seat the game is waiting on, with no connection at all, first gets one
/// warning in the table chat and then, if it still has not come back, has
/// `Action::Concede` applied for it so the other seats can finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdlePolicy {
    /// How long a disconnected seat the game is waiting on may hold everyone
    /// up before the table is told about it (once).
    pub warn_after: Duration,
    /// How long before the daemon concedes for it. Must be at least `warn_after`.
    pub concede_after: Duration,
}

/// What the watch channel carries: enough for a client to block on "my seat must act".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub state_version: u64,
    pub must_act: BTreeMap<Seat, ActReason>,
    pub game_over: bool,
}

/// Everything a game needs from the process it runs in, and nothing it can
/// change. One of these is shared by every game on a server.
pub(crate) struct Context {
    pub cards: Arc<CardDb>,
    pub legality: Option<Arc<dyn engine::LegalitySource + Send + Sync>>,
    /// `<data dir>/games`, one `<game id>.jsonl` per game.
    pub replay_dir: PathBuf,
    pub idle: Option<IdlePolicy>,
    pub abandon_after: Option<Duration>,
    /// Server mode: an action that cannot be written to the log is not acked,
    /// because a game the server cannot rebuild is not a game it should serve.
    pub durable: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Broadcast {
    /// Unfiltered engine events; every connection filters for its own role before sending.
    Events {
        events: Arc<Vec<Event>>,
        state_version: u64,
    },
    Lobby(LobbyView),
}

pub(crate) struct GameState {
    pub lobby: Lobby,
    pub game: Option<Game>,
    pub replay: Option<ReplayWriter>,
}

pub(crate) struct GameShared {
    pub(crate) ctx: Arc<Context>,
    pub(crate) game_id: GameId,
    /// Every token that reaches this game, with the role it grants. Fixed for
    /// the life of the game, so the registry can index them without locking.
    pub(crate) tokens: Vec<(protocol::Token, Role)>,
    pub(crate) state: Mutex<GameState>,
    pub(crate) status: watch::Sender<Status>,
    pub(crate) broadcast: broadcast::Sender<Broadcast>,
    /// Connections bound to this game right now, seats and spectators alike.
    /// A finished game with none left can be forgotten.
    pub(crate) clients: AtomicUsize,
}

impl GameShared {
    pub(crate) fn new(
        ctx: Arc<Context>,
        lobby: Lobby,
        status: watch::Sender<Status>,
        game: Option<Game>,
        replay: Option<ReplayWriter>,
    ) -> Arc<GameShared> {
        let (broadcast, _) = broadcast::channel(1024);
        if let Some(g) = &game {
            status.send_replace(status_of(g));
        }
        let mut tokens: Vec<(protocol::Token, Role)> = lobby
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| (s.token.clone(), Role::Seat(Seat(i as u8))))
            .collect();
        tokens.push((lobby.spectator_token.clone(), Role::Spectator));
        Arc::new(GameShared {
            ctx,
            game_id: lobby.game_id.clone(),
            tokens,
            state: Mutex::new(GameState { lobby, game, replay }),
            status,
            broadcast,
            clients: AtomicUsize::new(0),
        })
    }

    pub(crate) async fn replay_path(&self) -> Option<PathBuf> {
        self.state.lock().await.replay.as_ref().map(|r| r.path().to_path_buf())
    }

    pub(crate) async fn is_over(&self) -> bool {
        self.state.lock().await.game.as_ref().and_then(|g| g.is_over()).is_some()
    }
}

/// Per-connection state. A connection starts unbound; `hello` (or, on a
/// server, `join_game` followed by `hello`) attaches it to one game, and from
/// then on every message is that game's business.
pub(crate) struct Conn {
    pub game: Option<Arc<GameShared>>,
    pub role: Option<Role>,
    pub subscribed: Option<broadcast::Receiver<Broadcast>>,
    pub peer: String,
}

impl Conn {
    pub(crate) fn new(peer: String) -> Conn {
        Conn {
            game: None,
            role: None,
            subscribed: None,
            peer,
        }
    }
}

/// Bind `conn` to `game` as `role` and build the welcome. Called by the
/// `hello` handler once the token has been resolved, which is the only way a
/// connection ever reaches a game.
pub(crate) async fn welcome(game: &Arc<GameShared>, conn: &mut Conn, role: Role, name: Option<String>) -> ServerMessage {
    let mut state = game.state.lock().await;
    let GameState { lobby, game: running, .. } = &mut *state;
    // Re-`hello` on a live connection: leave the seat it held first.
    if let Some(Role::Seat(old)) = conn.role {
        lobby.disconnect(old);
    }
    if conn.game.is_none() {
        game.clients.fetch_add(1, Ordering::SeqCst);
    }
    if let Role::Seat(seat) = role {
        lobby.seats[seat.index()].claimed = true;
        lobby.connect(seat);
        if let Some(n) = name {
            lobby.seats[seat.index()].name = Some(n);
        }
    }
    conn.game = Some(game.clone());
    conn.role = Some(role);
    let _ = game.broadcast.send(Broadcast::Lobby(lobby.view()));
    ServerMessage::Welcome {
        role,
        game_id: lobby.game_id.clone(),
        format: lobby.format.clone(),
        protocol_version: PROTOCOL_VERSION,
        lobby: lobby.view(),
        state: running.as_ref().map(|g| view_for(g, role)),
    }
}

/// The other end of `welcome`: a connection is going away.
pub(crate) async fn detach(conn: &Conn) {
    let Some(game) = &conn.game else { return };
    game.clients.fetch_sub(1, Ordering::SeqCst);
    if let Some(Role::Seat(seat)) = conn.role {
        let mut state = game.state.lock().await;
        state.lobby.disconnect(seat);
        let view = state.lobby.view();
        let _ = game.broadcast.send(Broadcast::Lobby(view));
    }
}

/// The seat-filter boundary: everything pushed to a connection goes through here.
pub(crate) fn filter_broadcast(b: Broadcast, role: Option<Role>) -> Vec<ServerMessage> {
    let viewer = role.and_then(Role::seat);
    match b {
        Broadcast::Events { events, state_version } => events
            .iter()
            .filter_map(|e| e.view(viewer))
            .map(|event| ServerMessage::Event { event, state_version })
            .collect(),
        Broadcast::Lobby(lobby) => vec![ServerMessage::Lobby { lobby }],
    }
}

fn legal_actions_for(game: &Game, seat: Seat) -> Vec<LegalAction> {
    game.legal_actions(seat)
        .into_iter()
        .enumerate()
        .map(|(i, action)| LegalAction {
            id: i as u32,
            description: describe_action(game, &action),
            action,
        })
        .collect()
}

pub(crate) fn status_of(game: &Game) -> Status {
    Status {
        state_version: game.state_version(),
        must_act: game.must_act(),
        game_over: game.is_over().is_some(),
    }
}

fn view_for(game: &Game, role: Role) -> engine::GameView {
    match role {
        Role::Seat(s) => game.view(s),
        Role::Spectator => game.view_spectator(),
    }
}

fn require_role(conn: &Conn) -> Result<Role, ProtocolError> {
    conn.role.ok_or_else(|| ProtocolError::bad_request("send hello first"))
}

fn require_seat(conn: &Conn) -> Result<Seat, ProtocolError> {
    match require_role(conn)? {
        Role::Seat(s) => Ok(s),
        Role::Spectator => Err(ProtocolError::bad_request("spectators can't do that")),
    }
}

/// Everything a bound connection can ask of its game. Unchanged in behaviour
/// from the one-game daemon; it now reads its state out of a `GameShared`
/// instead of the process.
pub(crate) async fn handle(shared: &Arc<GameShared>, conn: &mut Conn, msg: ClientMessage) -> Result<ServerMessage, ProtocolError> {
    match msg {
        ClientMessage::GetPool => {
            require_seat(conn)?;
            Err(ProtocolError::bad_request("this format has no limited pool"))
        }

        ClientMessage::SetDeck { decklist, commander } => {
            let seat = require_seat(conn)?;
            if commander.is_some() {
                return Err(ProtocolError::bad_request("commanders are not supported yet"));
            }
            let mut state = shared.state.lock().await;
            let lobby = &mut state.lobby;
            if lobby.started {
                return Err(ProtocolError::bad_request("the game has already started"));
            }
            let deck = match cards::parse_decklist(&decklist, &shared.ctx.cards) {
                Ok(d) => d,
                Err(reason) => {
                    return Ok(ServerMessage::DeckRejected {
                        violations: vec![Violation::Unparsable { reason }],
                    })
                }
            };
            let legality = shared.ctx.legality.as_deref().map(|l| l as &dyn engine::LegalitySource);
            let violations = lobby.format.check_deck_with(&deck, &shared.ctx.cards, legality);
            if !violations.is_empty() {
                return Ok(ServerMessage::DeckRejected { violations });
            }
            let cards = shared.ctx.cards.clone();
            let slot = &mut lobby.seats[seat.index()];
            slot.deck_names = deck.iter().map(|&c| cards.get(c).name.clone()).collect();
            slot.deck = Some(deck);
            slot.ready = false;
            let _ = shared.broadcast.send(Broadcast::Lobby(lobby.view()));
            Ok(ServerMessage::DeckOk)
        }

        ClientMessage::Ready => {
            let seat = require_seat(conn)?;
            let mut state = shared.state.lock().await;
            if state.lobby.started {
                return Ok(ServerMessage::Ok);
            }
            if state.lobby.seats[seat.index()].deck.is_none() {
                return Err(ProtocolError::new(ErrorCode::DeckRejected, "submit a deck before readying"));
            }
            state.lobby.seats[seat.index()].ready = true;
            let _ = shared.broadcast.send(Broadcast::Lobby(state.lobby.view()));
            if state.lobby.all_ready() {
                start_game(shared, &mut state)?;
            }
            Ok(ServerMessage::Ok)
        }

        ClientMessage::GetState => {
            let role = require_role(conn)?;
            let state = shared.state.lock().await;
            let game = state
                .game
                .as_ref()
                .ok_or_else(|| ProtocolError::bad_request("the game has not started"))?;
            Ok(ServerMessage::State {
                state: view_for(game, role),
            })
        }

        ClientMessage::GetLegalActions => {
            let role = require_role(conn)?;
            let state = shared.state.lock().await;
            let game = state
                .game
                .as_ref()
                .ok_or_else(|| ProtocolError::bad_request("the game has not started"))?;
            let (actions, reason) = match role {
                Role::Seat(s) => (legal_actions_for(game, s), game.must_act().get(&s).copied()),
                Role::Spectator => (Vec::new(), None),
            };
            Ok(ServerMessage::LegalActions {
                actions,
                state_version: game.state_version(),
                reason,
            })
        }

        ClientMessage::Act {
            action_id,
            action,
            state_version,
        } => {
            let seat = require_seat(conn)?;
            let mut state = shared.state.lock().await;
            let GameState { game, replay, .. } = &mut *state;
            let game = game
                .as_mut()
                .ok_or_else(|| ProtocolError::bad_request("the game has not started"))?;
            let current = game.state_version();
            if state_version != current {
                return Err(ProtocolError::new(
                    ErrorCode::StaleStateVersion,
                    format!("state is at version {current}, you sent {state_version}; fetch state and try again"),
                )
                .with_version(current));
            }
            let action: Action = match (action_id, action) {
                (Some(_), Some(_)) | (None, None) => return Err(ProtocolError::bad_request("send exactly one of action_id or action")),
                (None, Some(a)) => a,
                (Some(id), None) => game
                    .legal_actions(seat)
                    .into_iter()
                    .nth(id as usize)
                    .ok_or_else(|| ProtocolError::bad_request(format!("no legal action with id {id}")))?,
            };
            let events = game
                .apply(seat, &action)
                .map_err(|e| ProtocolError::from(e).with_version(current))?;
            // Durability first: the log line is on its way to the OS before
            // the ack is built, let alone sent (§2.2).
            let logged = log_action(replay, seat, &action, shared.ctx.durable);
            let version = game.state_version();
            shared.status.send_replace(status_of(game));
            let _ = shared.broadcast.send(Broadcast::Events {
                events: Arc::new(events.clone()),
                state_version: version,
            });
            logged?;
            let legal_actions = if game.must_act().contains_key(&seat) {
                legal_actions_for(game, seat)
            } else {
                Vec::new()
            };
            Ok(ServerMessage::Ack {
                applied: action,
                events: events.iter().filter_map(|e| e.view(Some(seat))).collect(),
                state: game.view(seat),
                legal_actions,
            })
        }

        ClientMessage::Subscribe => {
            require_role(conn)?;
            conn.subscribed = Some(shared.broadcast.subscribe());
            Ok(ServerMessage::Ok)
        }

        ClientMessage::Chat { text, to } => {
            let seat = require_seat(conn)?;
            let state = shared.state.lock().await;
            let version = state.game.as_ref().map(|g| g.state_version()).unwrap_or(0);
            let event = Event::Chat { from: seat, to, text };
            let _ = shared.broadcast.send(Broadcast::Events {
                events: Arc::new(vec![event]),
                state_version: version,
            });
            Ok(ServerMessage::Ok)
        }

        // Routed before this point; a bound connection never gets here.
        ClientMessage::Ping | ClientMessage::CreateGame { .. } | ClientMessage::JoinGame { .. } | ClientMessage::Hello { .. } => {
            Err(ProtocolError::internal("message routed to a game that does not handle it"))
        }
    }
}

/// Append one accepted action. A local daemon warns and plays on; a server
/// refuses the action, because a game it cannot replay is a game it cannot
/// survive a restart with.
fn log_action(replay: &mut Option<ReplayWriter>, seat: Seat, action: &Action, durable: bool) -> Result<(), ProtocolError> {
    let Some(w) = replay.as_mut() else { return Ok(()) };
    match w.append(seat, action) {
        Ok(()) => Ok(()),
        Err(e) if durable => Err(ProtocolError::internal(format!(
            "the action was applied but could not be written to the durable log ({e}); fetch state and stop"
        ))),
        Err(e) => {
            warn!("replay log write failed: {e}");
            Ok(())
        }
    }
}

/// All seats are ready: construct the game, open the replay log, and tell everyone.
fn start_game(shared: &Arc<GameShared>, state: &mut GameState) -> Result<(), ProtocolError> {
    let lobby = &mut state.lobby;
    let players: Vec<PlayerSetup> = lobby
        .seats
        .iter()
        .enumerate()
        .map(|(i, s)| PlayerSetup {
            name: lobby.seat_name(Seat(i as u8)),
            deck: s.deck.clone().unwrap_or_default(),
        })
        .collect();
    let config = GameConfig {
        format: lobby.format.clone(),
        players,
        cards: shared.ctx.cards.clone(),
        starting_player: None,
    };
    let game = Game::new(config, lobby.seed).map_err(|e| ProtocolError::internal(e.to_string()))?;

    let header = ReplayHeader {
        game_id: lobby.game_id.0.clone(),
        format: lobby.format_name.clone(),
        seed: lobby.seed,
        players: lobby
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| ReplayPlayer {
                name: lobby.seat_name(Seat(i as u8)),
                deck: s.deck_names.clone(),
            })
            .collect(),
        seat_tokens: lobby.seats.iter().map(|s| s.token.0.clone()).collect(),
        spectator_token: Some(lobby.spectator_token.0.clone()),
    };
    let path = shared.ctx.replay_dir.join(format!("{}.jsonl", lobby.game_id));
    match ReplayWriter::create(&path, &header) {
        Ok(w) => state.replay = Some(w),
        Err(e) if shared.ctx.durable => {
            return Err(ProtocolError::internal(format!(
                "could not open the durable log {}: {e}",
                path.display()
            )))
        }
        Err(e) => warn!("could not open replay log {}: {e}", path.display()),
    }

    let lobby = &mut state.lobby;
    lobby.started = true;
    info!(game = %lobby.game_id, seed = lobby.seed, "game started");
    let _ = shared.broadcast.send(Broadcast::Lobby(lobby.view()));
    let _ = shared.broadcast.send(Broadcast::Events {
        events: Arc::new(game.log.clone()),
        state_version: game.state_version(),
    });
    shared.status.send_replace(status_of(&game));
    state.game = Some(game);
    Ok(())
}

/// What one sweep of one game decided about it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Swept {
    /// Still live: keep it.
    Keep,
    /// Every seat has been gone longer than `abandon_after`.
    Abandoned,
    /// Over, and the last client has left.
    Finished,
}

/// One pass over one game's seats: the idle warning, the idle concession, and
/// the two reasons a game stops being worth holding on to. The lock is held
/// for the whole pass and across no await.
pub(crate) async fn sweep(shared: &Arc<GameShared>) -> Swept {
    let now = Instant::now();
    let mut state = shared.state.lock().await;
    let GameState { lobby, game, replay } = &mut *state;
    let away = |slot: &crate::lobby::SeatSlot| slot.disconnected_since.map(|t| now.duration_since(t));

    // Nobody is left at the table.
    if let Some(limit) = shared.ctx.abandon_after {
        if lobby.seats.iter().all(|s| away(s).is_some_and(|d| d >= limit)) {
            info!(game = %lobby.game_id, "every seat has been gone for {limit:?}; giving up");
            return Swept::Abandoned;
        }
    }

    let over = game.as_ref().and_then(|g| g.is_over()).is_some();
    if over && shared.clients.load(Ordering::SeqCst) == 0 {
        return Swept::Finished;
    }

    let Some(policy) = shared.ctx.idle else {
        return Swept::Keep;
    };
    let Some(game) = game.as_mut() else {
        return Swept::Keep;
    };
    if over {
        return Swept::Keep;
    }
    // Decide from one snapshot: a concession changes who must act.
    let must_act = game.must_act();
    let mut warn = Vec::new();
    let mut concede = Vec::new();
    for (i, slot) in lobby.seats.iter().enumerate() {
        let seat = Seat(i as u8);
        let Some(gone) = away(slot) else { continue };
        if !must_act.contains_key(&seat) {
            continue;
        }
        if gone >= policy.concede_after {
            concede.push(seat);
        } else if gone >= policy.warn_after && !slot.idle_warned {
            warn.push(seat);
        }
    }

    for seat in warn {
        lobby.seats[seat.index()].idle_warned = true;
        let text = format!(
            "{} ({seat}) has been away for {} s",
            lobby.seat_name(seat),
            policy.warn_after.as_secs()
        );
        info!(game = %lobby.game_id, "{text}");
        let _ = shared.broadcast.send(Broadcast::Events {
            events: Arc::new(vec![Event::Chat {
                from: seat,
                to: None,
                text,
            }]),
            state_version: game.state_version(),
        });
    }

    for seat in concede {
        // The game may have ended on an earlier concession in this same pass.
        let events = match game.apply(seat, &Action::Concede) {
            Ok(events) => events,
            Err(e) => {
                debug!("could not concede for {seat}: {e}");
                continue;
            }
        };
        // Nobody asked for this action, so there is no request to fail: the
        // most a server can do about an unwritable log here is say so.
        if let Err(e) = log_action(replay, seat, &Action::Concede, shared.ctx.durable) {
            warn!("{e}");
        }
        info!(game = %lobby.game_id, "{} has been away for {:?}; conceding for {seat}", lobby.seat_name(seat), policy.concede_after);
        shared.status.send_replace(status_of(game));
        let _ = shared.broadcast.send(Broadcast::Events {
            events: Arc::new(events),
            state_version: game.state_version(),
        });
    }
    Swept::Keep
}
