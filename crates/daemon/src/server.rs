//! One daemon, one game, any number of connections. Concurrency is a single
//! mutex around the game and a watch channel carrying who must act (§5).
//! Every connection filters what it forwards through `Event::view` and
//! `Game::view`; nothing else ever touches the wire.

use crate::lobby::Lobby;
use crate::replay::{ReplayHeader, ReplayPlayer, ReplayWriter};
use engine::text::describe_action;
use engine::{ActReason, Action, CardDb, Event, Format, Game, GameConfig, PlayerSetup, Seat, Violation};
use protocol::messages::{ClientEnvelope, ServerEnvelope};
use protocol::{
    ClientMessage, ErrorCode, FramedReader, FramedWriter, GameId, LegalAction, LobbyView, ProtocolError, Role, ServerMessage, Token,
    PROTOCOL_VERSION,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{broadcast, watch, Mutex};
use tracing::{debug, info, warn};

/// Ask the daemon to create its game at startup instead of waiting for `create_game`.
#[derive(Clone, Debug)]
pub struct CreateGame {
    pub format: String,
    pub seats: u8,
    pub seed: Option<u64>,
}

#[derive(Clone)]
pub struct DaemonConfig {
    /// Unix socket path. `None` with `no_socket == false` means
    /// `<runtime dir>/<game id>.sock` once the game is created.
    pub socket: Option<PathBuf>,
    pub no_socket: bool,
    /// A `host:port` to listen on; `127.0.0.1:0` picks a free port.
    pub tcp: Option<String>,
    pub parent_pid: Option<u32>,
    /// Where replay logs go; defaults to the platform data directory.
    pub replay_dir: Option<PathBuf>,
    pub create: Option<CreateGame>,
    pub cards: Arc<CardDb>,
    /// Per-format legality for Scryfall-pool formats (the carddb cache), if available.
    pub legality: Option<Arc<dyn engine::LegalitySource + Send + Sync>>,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Setup(String),
    #[error("no listener configured: pass --socket or --tcp")]
    NoListener,
}

/// What a freshly bound daemon can tell its parent: where it listens and,
/// if it created a game at startup, the tokens.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StartupInfo {
    pub socket: Option<PathBuf>,
    pub tcp: Option<std::net::SocketAddr>,
    pub game_id: Option<GameId>,
    pub seat_tokens: Vec<Token>,
    pub spectator_token: Option<Token>,
    pub replay_path: Option<PathBuf>,
}

/// What the watch channel carries: enough for a client to block on "my seat must act".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub state_version: u64,
    pub must_act: BTreeMap<Seat, ActReason>,
    pub game_over: bool,
}

#[derive(Clone, Debug)]
enum Broadcast {
    /// Unfiltered engine events; every connection filters for its own role before sending.
    Events {
        events: Arc<Vec<Event>>,
        state_version: u64,
    },
    Lobby(LobbyView),
}

struct State {
    lobby: Option<Lobby>,
    game: Option<Game>,
    replay: Option<ReplayWriter>,
}

pub(crate) struct Shared {
    cards: Arc<CardDb>,
    legality: Option<Arc<dyn engine::LegalitySource + Send + Sync>>,
    state: Mutex<State>,
    status: watch::Sender<Status>,
    broadcast: broadcast::Sender<Broadcast>,
    shutdown: watch::Sender<bool>,
    replay_dir: PathBuf,
}

/// A handle that can stop a running daemon and observe its status.
#[derive(Clone)]
pub struct DaemonHandle {
    shared: Arc<Shared>,
}

impl DaemonHandle {
    pub fn shutdown(&self) {
        self.shared.shutdown.send_replace(true);
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.shared.status.subscribe()
    }

    /// The path of the replay log once the game has started.
    pub async fn replay_path(&self) -> Option<PathBuf> {
        self.shared.state.lock().await.replay.as_ref().map(|r| r.path().to_path_buf())
    }

    pub async fn is_over(&self) -> bool {
        self.shared.state.lock().await.game.as_ref().and_then(|g| g.is_over()).is_some()
    }
}

pub struct Daemon {
    shared: Arc<Shared>,
    unix: Option<UnixListener>,
    tcp: Option<TcpListener>,
    info: StartupInfo,
    parent_pid: Option<u32>,
}

impl Daemon {
    /// Bind the listeners and, if asked, create the game. Nothing is served until `run`.
    pub async fn bind(config: DaemonConfig) -> Result<Daemon, DaemonError> {
        if config.no_socket && config.tcp.is_none() {
            return Err(DaemonError::NoListener);
        }
        let lobby = match config.create {
            Some(create) => Some(create_lobby(&create.format, create.seats, create.seed)?),
            None => None,
        };
        let socket = if config.no_socket {
            None
        } else {
            match config.socket {
                Some(p) => Some(p),
                None => {
                    let dir = protocol::endpoint::ensure_runtime_dir()?;
                    let name = lobby.as_ref().map(|l| l.game_id.0.clone()).unwrap_or_else(random_name);
                    Some(dir.join(format!("{name}.sock")))
                }
            }
        };
        let (status, _) = watch::channel(Status::default());
        let (broadcast, _) = broadcast::channel(1024);
        let (shutdown, _) = watch::channel(false);
        let replay_dir = config.replay_dir.unwrap_or_else(|| protocol::endpoint::data_dir().join("games"));
        let shared = Arc::new(Shared {
            cards: config.cards,
            legality: config.legality,
            state: Mutex::new(State {
                lobby: None,
                game: None,
                replay: None,
            }),
            status,
            broadcast,
            shutdown,
            replay_dir,
        });

        let mut info = StartupInfo {
            socket: None,
            tcp: None,
            game_id: None,
            seat_tokens: Vec::new(),
            spectator_token: None,
            replay_path: None,
        };

        let unix = match &socket {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
                let listener = UnixListener::bind(path)?;
                info.socket = Some(path.clone());
                Some(listener)
            }
            None => None,
        };
        let tcp = match &config.tcp {
            Some(addr) => {
                let listener = TcpListener::bind(addr).await?;
                info.tcp = Some(listener.local_addr()?);
                Some(listener)
            }
            None => None,
        };

        if let Some(lobby) = lobby {
            info.game_id = Some(lobby.game_id.clone());
            info.seat_tokens = lobby.seat_tokens();
            info.spectator_token = Some(lobby.spectator_token.clone());
            info.replay_path = Some(shared.replay_dir.join(format!("{}.jsonl", lobby.game_id)));
            shared.state.lock().await.lobby = Some(lobby);
        }

        Ok(Daemon {
            shared,
            unix,
            tcp,
            info,
            parent_pid: config.parent_pid,
        })
    }

    pub fn info(&self) -> &StartupInfo {
        &self.info
    }

    pub fn handle(&self) -> DaemonHandle {
        DaemonHandle {
            shared: self.shared.clone(),
        }
    }

    /// Serve until shut down. Removes the socket file on exit.
    pub async fn run(self) -> Result<(), DaemonError> {
        let Daemon {
            shared,
            unix,
            tcp,
            info,
            parent_pid,
        } = self;
        let mut shutdown = shared.shutdown.subscribe();
        if let Some(pid) = parent_pid {
            tokio::spawn(watch_parent(pid, shared.clone()));
        }
        info!(socket = ?info.socket, tcp = ?info.tcp, "daemon listening");
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                accepted = accept_unix(&unix), if unix.is_some() => match accepted {
                    Ok(stream) => {
                        let (r, w) = stream.into_split();
                        tokio::spawn(serve_connection(shared.clone(), r, w, "unix".to_string()));
                    }
                    Err(e) => warn!("unix accept failed: {e}"),
                },
                accepted = accept_tcp(&tcp), if tcp.is_some() => match accepted {
                    Ok((stream, peer)) => {
                        stream.set_nodelay(true).ok();
                        let (r, w) = stream.into_split();
                        tokio::spawn(serve_connection(shared.clone(), r, w, peer.to_string()));
                    }
                    Err(e) => warn!("tcp accept failed: {e}"),
                },
            }
        }
        if let Some(path) = &info.socket {
            let _ = std::fs::remove_file(path);
        }
        info!("daemon stopped");
        Ok(())
    }
}

async fn accept_unix(l: &Option<UnixListener>) -> std::io::Result<tokio::net::UnixStream> {
    l.as_ref().expect("guarded by select precondition").accept().await.map(|(s, _)| s)
}

async fn accept_tcp(l: &Option<TcpListener>) -> std::io::Result<(tokio::net::TcpStream, std::net::SocketAddr)> {
    l.as_ref().expect("guarded by select precondition").accept().await
}

/// Exit when the parent process goes away (`--parent-pid`).
async fn watch_parent(pid: u32, shared: Arc<Shared>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        if !alive {
            info!("parent {pid} is gone; shutting down");
            shared.shutdown.send_replace(true);
            return;
        }
    }
}

fn random_name() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..8).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

fn create_lobby(format_name: &str, seats: u8, seed: Option<u64>) -> Result<Lobby, DaemonError> {
    let format = Format::builtin(format_name).ok_or_else(|| DaemonError::Setup(format!("unknown format {format_name:?}")))?;
    if !format.allows_player_count(seats as usize) {
        return Err(DaemonError::Setup(format!(
            "{} needs {}–{} players, not {seats}",
            format.name, format.players.min, format.players.max
        )));
    }
    let unsupported = format.unsupported_rules();
    if !unsupported.is_empty() {
        let list: Vec<String> = unsupported.iter().map(ToString::to_string).collect();
        return Err(DaemonError::Setup(list.join("; ")));
    }
    let seed = seed.unwrap_or_else(rand::random);
    Ok(Lobby::new(format_name, format, seats, seed))
}

/// Per-connection state.
struct Conn {
    role: Option<Role>,
    subscribed: Option<broadcast::Receiver<Broadcast>>,
    peer: String,
}

async fn serve_connection<R, W>(shared: Arc<Shared>, reader: R, writer: W, peer: String)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader: FramedReader<R, ClientEnvelope> = FramedReader::new(reader);
    let mut writer: FramedWriter<W, ServerEnvelope> = FramedWriter::new(writer);
    let mut conn = Conn {
        role: None,
        subscribed: None,
        peer,
    };
    debug!(peer = %conn.peer, "connection opened");
    loop {
        tokio::select! {
            incoming = reader.recv() => match incoming {
                Ok(env) => {
                    let reply = handle(&shared, &mut conn, env.msg).await;
                    if writer.send(&ServerEnvelope { req: env.req, msg: reply }).await.is_err() {
                        break;
                    }
                }
                Err(protocol::FrameError::Json(e)) => {
                    let err = ProtocolError::bad_request(format!("malformed message: {e}"));
                    if writer.send(&ServerMessage::Error(err).into()).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            pushed = recv_push(&mut conn.subscribed), if conn.subscribed.is_some() => match pushed {
                Ok(b) => {
                    let mut failed = false;
                    for msg in filter_broadcast(b, conn.role) {
                        if writer.send(&msg.into()).await.is_err() {
                            failed = true;
                            break;
                        }
                    }
                    if failed {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(peer = %conn.peer, "connection lagged {n} messages behind");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    if let Some(Role::Seat(seat)) = conn.role {
        let mut state = shared.state.lock().await;
        if let Some(lobby) = state.lobby.as_mut() {
            lobby.seats[seat.index()].connections = lobby.seats[seat.index()].connections.saturating_sub(1);
            let view = lobby.view();
            let _ = shared.broadcast.send(Broadcast::Lobby(view));
        }
    }
    debug!(peer = %conn.peer, "connection closed");
}

async fn recv_push(rx: &mut Option<broadcast::Receiver<Broadcast>>) -> Result<Broadcast, broadcast::error::RecvError> {
    rx.as_mut().expect("guarded by select precondition").recv().await
}

/// The seat-filter boundary: everything pushed to a connection goes through here.
fn filter_broadcast(b: Broadcast, role: Option<Role>) -> Vec<ServerMessage> {
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

fn status_of(game: &Game) -> Status {
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

async fn handle(shared: &Arc<Shared>, conn: &mut Conn, msg: ClientMessage) -> ServerMessage {
    match handle_inner(shared, conn, msg).await {
        Ok(m) => m,
        Err(e) => ServerMessage::Error(e),
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

async fn handle_inner(shared: &Arc<Shared>, conn: &mut Conn, msg: ClientMessage) -> Result<ServerMessage, ProtocolError> {
    match msg {
        ClientMessage::Ping => Ok(ServerMessage::Pong),

        ClientMessage::CreateGame { format, seats, seed } => {
            let mut state = shared.state.lock().await;
            if state.lobby.is_some() {
                return Err(ProtocolError::bad_request("this daemon already hosts a game"));
            }
            let lobby = create_lobby(&format, seats, seed).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
            let reply = ServerMessage::GameCreated {
                game_id: lobby.game_id.clone(),
                seat_tokens: lobby.seat_tokens(),
                spectator_token: lobby.spectator_token.clone(),
            };
            info!(game = %lobby.game_id, seats, "game created");
            state.lobby = Some(lobby);
            Ok(reply)
        }

        ClientMessage::Hello {
            token,
            protocol_version,
            name,
        } => {
            if protocol_version != PROTOCOL_VERSION {
                return Err(ProtocolError::new(
                    ErrorCode::UnsupportedVersion,
                    format!("this daemon speaks protocol version {PROTOCOL_VERSION}, not {protocol_version}"),
                ));
            }
            let mut state = shared.state.lock().await;
            let State { lobby, game, .. } = &mut *state;
            let lobby = lobby
                .as_mut()
                .ok_or_else(|| ProtocolError::bad_request("no game has been created yet"))?;
            let role = lobby
                .resolve(&token)
                .ok_or_else(|| ProtocolError::new(ErrorCode::BadToken, "that token does not belong to this game"))?;
            if let Some(Role::Seat(old)) = conn.role {
                lobby.seats[old.index()].connections = lobby.seats[old.index()].connections.saturating_sub(1);
            }
            if let Role::Seat(seat) = role {
                let slot = &mut lobby.seats[seat.index()];
                slot.connections += 1;
                if let Some(n) = name {
                    slot.name = Some(n);
                }
            }
            conn.role = Some(role);
            let _ = shared.broadcast.send(Broadcast::Lobby(lobby.view()));
            Ok(ServerMessage::Welcome {
                role,
                game_id: lobby.game_id.clone(),
                format: lobby.format.clone(),
                protocol_version: PROTOCOL_VERSION,
                lobby: lobby.view(),
                state: game.as_ref().map(|g| view_for(g, role)),
            })
        }

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
            let lobby = state.lobby.as_mut().ok_or_else(|| ProtocolError::bad_request("no game"))?;
            if lobby.started {
                return Err(ProtocolError::bad_request("the game has already started"));
            }
            let deck = match cards::parse_decklist(&decklist, &shared.cards) {
                Ok(d) => d,
                Err(reason) => {
                    return Ok(ServerMessage::DeckRejected {
                        violations: vec![Violation::Unparsable { reason }],
                    })
                }
            };
            let legality = shared.legality.as_deref().map(|l| l as &dyn engine::LegalitySource);
            let violations = lobby.format.check_deck_with(&deck, &shared.cards, legality);
            if !violations.is_empty() {
                return Ok(ServerMessage::DeckRejected { violations });
            }
            let slot = &mut lobby.seats[seat.index()];
            slot.deck_names = deck.iter().map(|&c| shared.cards.get(c).name.clone()).collect();
            slot.deck = Some(deck);
            slot.ready = false;
            let _ = shared.broadcast.send(Broadcast::Lobby(lobby.view()));
            Ok(ServerMessage::DeckOk)
        }

        ClientMessage::Ready => {
            let seat = require_seat(conn)?;
            let mut state = shared.state.lock().await;
            let lobby = state.lobby.as_mut().ok_or_else(|| ProtocolError::bad_request("no game"))?;
            if lobby.started {
                return Ok(ServerMessage::Ok);
            }
            if lobby.seats[seat.index()].deck.is_none() {
                return Err(ProtocolError::new(ErrorCode::DeckRejected, "submit a deck before readying"));
            }
            lobby.seats[seat.index()].ready = true;
            let _ = shared.broadcast.send(Broadcast::Lobby(lobby.view()));
            if lobby.all_ready() {
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
            let State { game, replay, .. } = &mut *state;
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
            if let Some(w) = replay.as_mut() {
                if let Err(e) = w.append(seat, &action) {
                    warn!("replay log write failed: {e}");
                }
            }
            let version = game.state_version();
            shared.status.send_replace(status_of(game));
            let _ = shared.broadcast.send(Broadcast::Events {
                events: Arc::new(events.clone()),
                state_version: version,
            });
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
    }
}

/// All seats are ready: construct the game, open the replay log, and tell everyone.
fn start_game(shared: &Arc<Shared>, state: &mut State) -> Result<(), ProtocolError> {
    let lobby = state.lobby.as_mut().expect("lobby exists");
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
        cards: shared.cards.clone(),
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
    };
    let path = shared.replay_dir.join(format!("{}.jsonl", lobby.game_id));
    match ReplayWriter::create(&path, &header) {
        Ok(w) => state.replay = Some(w),
        Err(e) => warn!("could not open replay log {}: {e}", path.display()),
    }

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
