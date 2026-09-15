//! The process: listeners, connection routing, and the registry of games.
//!
//! Two modes, one binary (§2.2). **Single-game mode** is what `play`, `host`
//! and `manaline daemon` spawn: one game, created at startup or by the one
//! `create_game` a connection is allowed, a socket named after it, and the
//! process exits when that game is abandoned. **Server mode** (`serve: true`,
//! `manaline server`) is the same daemon pluralised: `create_game` and
//! `join_game` make and find as many games as clients ask for, each an
//! independent `GameTask`, and nothing but a shutdown ends the process.
//!
//! Everything that is about one game lives in `game_task`; everything that is
//! shared between games lives in `registry`. A connection starts bound to
//! neither and reaches a game only through `hello`.

use crate::game_task::{self, Broadcast, Conn, Context, Swept};
use crate::registry::{self, Registry};
use protocol::messages::{ClientEnvelope, ServerEnvelope};
use protocol::{ClientMessage, ErrorCode, GameId, ProtocolError, ServerMessage, Token, PROTOCOL_VERSION};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::{broadcast, watch, Mutex};
use tracing::{debug, info, warn};

pub use crate::game_task::{IdlePolicy, Status};

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
    /// A `host:port` to serve the WebSocket transport on (§2.2). With `tls`
    /// set it is `wss://`, without it plain `ws://`.
    pub ws: Option<String>,
    /// The certificate and key the WebSocket listener presents. Getting them
    /// is a deployment concern — a reverse proxy, or Let's Encrypt tooling.
    pub tls: Option<TlsConfig>,
    pub parent_pid: Option<u32>,
    /// Where replay logs go; defaults to the platform data directory.
    pub replay_dir: Option<PathBuf>,
    pub create: Option<CreateGame>,
    /// Tier 1: host many games. `create_game` and `join_game` are accepted for
    /// as long as the process runs, unfinished games in `replay_dir` are
    /// recovered at startup, a log write that fails fails the action, and no
    /// game ending or being abandoned ever stops the process.
    pub serve: bool,
    pub cards: Arc<engine::CardDb>,
    /// Per-format legality for Scryfall-pool formats (the carddb cache), if available.
    pub legality: Option<Arc<dyn engine::LegalitySource + Send + Sync>>,
    /// What to do about a seat that disappears mid-game. `None` (the default
    /// for a daemon `play` spawns locally) waits forever.
    pub idle: Option<IdlePolicy>,
    /// Give a game up once every seat has been gone this long, in the lobby or
    /// in a game. In single-game mode that shuts the daemon down; on a server
    /// it drops that one game. `None` never gives up.
    pub abandon_after: Option<Duration>,
}

/// A PEM certificate chain and its private key, for the `wss://` listener.
#[derive(Clone, Debug)]
pub struct TlsConfig {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Setup(String),
    #[error("no listener configured: pass --socket, --tcp or --ws")]
    NoListener,
}

/// What a freshly bound daemon can tell its parent: where it listens and,
/// if it created a game at startup, the tokens.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StartupInfo {
    pub socket: Option<PathBuf>,
    pub tcp: Option<std::net::SocketAddr>,
    /// The URL clients should use for the WebSocket transport, if it is on.
    #[serde(default)]
    pub ws: Option<String>,
    pub game_id: Option<GameId>,
    pub seat_tokens: Vec<Token>,
    pub spectator_token: Option<Token>,
    pub replay_path: Option<PathBuf>,
    /// Games recovered from the data directory at startup (server mode).
    #[serde(default)]
    pub recovered: Vec<GameId>,
}

pub(crate) struct Shared {
    ctx: Arc<Context>,
    registry: Mutex<Registry>,
    /// The status watch of the one game in single-game mode, so `DaemonHandle`
    /// can hand out a receiver without knowing whether a game exists yet.
    status: watch::Sender<Status>,
    shutdown: watch::Sender<bool>,
    serve: bool,
}

/// A handle that can stop a running daemon and observe its status. Its
/// game-shaped questions are about *the* game, which is single-game mode's
/// whole point; on a server they answer for whichever game the registry hands
/// back first and `games` is the useful one.
#[derive(Clone)]
pub struct DaemonHandle {
    shared: Arc<Shared>,
}

impl DaemonHandle {
    pub fn shutdown(&self) {
        self.shared.shutdown.send_replace(true);
    }

    /// The status watch of the daemon's one game. In server mode there is no
    /// such thing — each game has its own — and this one never changes.
    pub fn status(&self) -> watch::Receiver<Status> {
        self.shared.status.subscribe()
    }

    /// The path of the replay log once the game has started.
    pub async fn replay_path(&self) -> Option<PathBuf> {
        let game = self.shared.registry.lock().await.only()?;
        game.replay_path().await
    }

    pub async fn is_over(&self) -> bool {
        let Some(game) = self.shared.registry.lock().await.only() else {
            return false;
        };
        game.is_over().await
    }

    /// How many games this process is holding.
    pub async fn games(&self) -> usize {
        self.shared.registry.lock().await.len()
    }

    pub async fn game_ids(&self) -> Vec<GameId> {
        self.shared.registry.lock().await.ids()
    }

    /// Whether the registry still knows this game.
    pub async fn has_game(&self, id: &GameId) -> bool {
        self.shared.registry.lock().await.get(id).is_some()
    }
}

pub struct Daemon {
    shared: Arc<Shared>,
    unix: Option<UnixListener>,
    tcp: Option<TcpListener>,
    ws: Option<TcpListener>,
    tls: Option<protocol::ws::TlsAcceptor>,
    info: StartupInfo,
    parent_pid: Option<u32>,
}

impl Daemon {
    /// Bind the listeners and, if asked, create the game. Nothing is served until `run`.
    pub async fn bind(config: DaemonConfig) -> Result<Daemon, DaemonError> {
        if config.no_socket && config.tcp.is_none() && config.ws.is_none() {
            return Err(DaemonError::NoListener);
        }
        let replay_dir = config.replay_dir.unwrap_or_else(|| protocol::endpoint::data_dir().join("games"));
        let ctx = Arc::new(Context {
            cards: config.cards,
            legality: config.legality,
            replay_dir,
            idle: config.idle,
            abandon_after: config.abandon_after,
            durable: config.serve,
        });
        let (status, _) = watch::channel(Status::default());
        let (shutdown, _) = watch::channel(false);

        let mut info = StartupInfo {
            socket: None,
            tcp: None,
            ws: None,
            game_id: None,
            seat_tokens: Vec::new(),
            spectator_token: None,
            replay_path: None,
            recovered: Vec::new(),
        };

        let mut registry = Registry::default();
        // Durability falls out of determinism (§2.2): every unfinished log in
        // the data directory is a game this server is still hosting.
        if config.serve {
            registry::recover(&mut registry, &ctx, &ctx.replay_dir);
            info.recovered = registry.ids();
        }
        let created = match &config.create {
            Some(create) => Some(registry.create(&ctx, &create.format, create.seats, create.seed, status.clone())?),
            None => None,
        };

        let socket = if config.no_socket {
            None
        } else {
            match config.socket {
                Some(p) => Some(p),
                None => {
                    let dir = protocol::endpoint::ensure_runtime_dir()?;
                    let name = created.as_ref().map(|g| g.game_id.0.clone()).unwrap_or_else(random_name);
                    Some(dir.join(format!("{name}.sock")))
                }
            }
        };

        let shared = Arc::new(Shared {
            ctx,
            registry: Mutex::new(registry),
            status,
            shutdown,
            serve: config.serve,
        });

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
        let tls = match &config.tls {
            Some(t) => Some(protocol::ws::tls_acceptor(&t.cert, &t.key)?),
            None => None,
        };
        let ws = match &config.ws {
            Some(addr) => {
                let listener = TcpListener::bind(addr).await?;
                let scheme = if tls.is_some() { "wss" } else { "ws" };
                info.ws = Some(format!("{scheme}://{}", listener.local_addr()?));
                Some(listener)
            }
            None => None,
        };

        if let Some(game) = &created {
            let state = game.state.lock().await;
            info.game_id = Some(game.game_id.clone());
            info.seat_tokens = state.lobby.seat_tokens();
            info.spectator_token = Some(state.lobby.spectator_token.clone());
            info.replay_path = Some(shared.ctx.replay_dir.join(format!("{}.jsonl", game.game_id)));
        }

        Ok(Daemon {
            shared,
            unix,
            tcp,
            ws,
            tls,
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
            ws,
            tls,
            info,
            parent_pid,
        } = self;
        let mut shutdown = shared.shutdown.subscribe();
        if let Some(pid) = parent_pid {
            tokio::spawn(watch_parent(pid, shared.clone()));
        }
        if shared.ctx.idle.is_some() || shared.ctx.abandon_after.is_some() || shared.serve {
            tokio::spawn(watch_away(shared.clone()));
        }
        info!(socket = ?info.socket, tcp = ?info.tcp, ws = ?info.ws, serve = shared.serve, "daemon listening");
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                accepted = accept_unix(&unix), if unix.is_some() => match accepted {
                    Ok(stream) => {
                        let (r, w) = stream.into_split();
                        let transport = protocol::framing::LineTransport::boxed(r, w);
                        tokio::spawn(serve_connection(shared.clone(), transport, "unix".to_string()));
                    }
                    Err(e) => warn!("unix accept failed: {e}"),
                },
                accepted = accept_tcp(&tcp), if tcp.is_some() => match accepted {
                    Ok((stream, peer)) => {
                        stream.set_nodelay(true).ok();
                        let (r, w) = stream.into_split();
                        let transport = protocol::framing::LineTransport::boxed(r, w);
                        tokio::spawn(serve_connection(shared.clone(), transport, peer.to_string()));
                    }
                    Err(e) => warn!("tcp accept failed: {e}"),
                },
                accepted = accept_tcp(&ws), if ws.is_some() => match accepted {
                    Ok((stream, peer)) => {
                        stream.set_nodelay(true).ok();
                        tokio::spawn(serve_websocket(shared.clone(), stream, tls.clone(), peer.to_string()));
                    }
                    Err(e) => warn!("websocket accept failed: {e}"),
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

/// The idle and abandonment clocks (§2.2, M8), now one pass per game. A server
/// drops a game the sweep gives up on and keeps running; a single-game daemon
/// has nothing left to do and stops.
async fn watch_away(shared: Arc<Shared>) {
    let mut shutdown = shared.shutdown.subscribe();
    let tick = sweep_interval(&shared);
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(tick) => {}
        }
        let games = shared.registry.lock().await.games();
        let mut drop_these = Vec::new();
        for game in games {
            match game_task::sweep(&game).await {
                Swept::Keep => {}
                // A server forgets the game and carries on; a single-game
                // daemon has nothing left to serve, so the process ends —
                // exactly as it did before there was more than one game.
                swept if shared.serve => drop_these.push((game.game_id.clone(), swept)),
                Swept::Abandoned => {
                    shared.shutdown.send_replace(true);
                    return;
                }
                Swept::Finished => {}
            }
        }
        if !drop_these.is_empty() {
            let mut registry = shared.registry.lock().await;
            for (id, why) in drop_these {
                info!(game = %id, "dropping the game from the lobby ({why:?})");
                registry.remove(&id);
            }
        }
    }
}

/// Fast enough that a policy measured in a few hundred milliseconds is not
/// rounded away, slow enough never to be a hot loop.
fn sweep_interval(shared: &Shared) -> Duration {
    let mut shortest = Duration::from_millis(400);
    if let Some(p) = shared.ctx.idle {
        shortest = shortest.min(p.warn_after).min(p.concede_after);
    }
    if let Some(a) = shared.ctx.abandon_after {
        shortest = shortest.min(a);
    }
    (shortest / 4).clamp(Duration::from_millis(10), Duration::from_millis(100))
}

fn random_name() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..8).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

/// The TLS handshake (when configured) and then the WebSocket one; after that
/// the connection is a message stream like any other and the handlers below
/// cannot tell which transport carried it.
async fn serve_websocket(shared: Arc<Shared>, stream: tokio::net::TcpStream, tls: Option<protocol::ws::TlsAcceptor>, peer: String) {
    let transport = match tls {
        Some(acceptor) => match acceptor.accept(stream).await {
            Ok(tls_stream) => protocol::ws::accept(tls_stream).await,
            Err(e) => {
                debug!(%peer, "tls handshake failed: {e}");
                return;
            }
        },
        None => protocol::ws::accept(stream).await,
    };
    match transport {
        Ok(transport) => serve_connection(shared, transport, peer).await,
        Err(e) => debug!(%peer, "websocket handshake failed: {e}"),
    }
}

async fn serve_connection(shared: Arc<Shared>, transport: protocol::BoxedTransport, peer: String) {
    let conn_io: protocol::MessageConnection<ClientEnvelope, ServerEnvelope> = protocol::MessageConnection::new(transport);
    let (mut reader, mut writer) = conn_io.split();
    let mut conn = Conn::new(peer);
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
                    for msg in game_task::filter_broadcast(b, conn.role) {
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
    game_task::detach(&conn).await;
    debug!(peer = %conn.peer, "connection closed");
}

async fn recv_push(rx: &mut Option<broadcast::Receiver<Broadcast>>) -> Result<Broadcast, broadcast::error::RecvError> {
    rx.as_mut().expect("guarded by select precondition").recv().await
}

async fn handle(shared: &Arc<Shared>, conn: &mut Conn, msg: ClientMessage) -> ServerMessage {
    match handle_inner(shared, conn, msg).await {
        Ok(m) => m,
        Err(e) => ServerMessage::Error(e),
    }
}

/// Routing. Three messages are the process's business — `ping`, `create_game`
/// and `join_game` need no game, and `hello` is what finds one; everything
/// else belongs to the game this connection is bound to.
async fn handle_inner(shared: &Arc<Shared>, conn: &mut Conn, msg: ClientMessage) -> Result<ServerMessage, ProtocolError> {
    match msg {
        ClientMessage::Ping => Ok(ServerMessage::Pong),

        ClientMessage::CreateGame { format, seats, seed } => {
            let mut registry = shared.registry.lock().await;
            if !shared.serve && registry.len() > 0 {
                return Err(ProtocolError::bad_request("this daemon already hosts a game"));
            }
            // In single-game mode the daemon's one status watch is this game's.
            let status = if shared.serve {
                watch::channel(Status::default()).0
            } else {
                shared.status.clone()
            };
            let game = registry
                .create(&shared.ctx, &format, seats, seed, status)
                .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
            drop(registry);
            let state = game.state.lock().await;
            info!(game = %game.game_id, seats, "game created");
            Ok(ServerMessage::GameCreated {
                game_id: game.game_id.clone(),
                seat_tokens: state.lobby.seat_tokens(),
                spectator_token: state.lobby.spectator_token.clone(),
            })
        }

        ClientMessage::JoinGame { code, name } => {
            let game = {
                let registry = shared.registry.lock().await;
                registry
                    .get(&GameId(code.to_uppercase()))
                    .ok_or_else(|| ProtocolError::bad_request(format!("no game here with the code {code:?}")))?
            };
            let mut state = game.state.lock().await;
            if state.lobby.started {
                return Err(ProtocolError::bad_request(format!(
                    "game {} has already started; ask for a spectator token instead",
                    game.game_id
                )));
            }
            let seat = state.lobby.claim_seat().ok_or_else(|| {
                ProtocolError::bad_request(format!(
                    "game {} is full: all {} seats are taken",
                    game.game_id,
                    state.lobby.seats.len()
                ))
            })?;
            if let Some(n) = name {
                state.lobby.seats[seat.index()].name = Some(n);
            }
            let token = state.lobby.seats[seat.index()].token.clone();
            let view = state.lobby.view();
            drop(state);
            let _ = game.broadcast.send(Broadcast::Lobby(view));
            info!(game = %game.game_id, "{seat} joined by code");
            Ok(ServerMessage::Joined {
                token,
                seat,
                game_id: game.game_id.clone(),
            })
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
            let found = shared.registry.lock().await.resolve(&token);
            let (game, role) = found.ok_or_else(|| {
                ProtocolError::new(
                    ErrorCode::BadToken,
                    if shared.serve {
                        "no live game here has that token"
                    } else {
                        "that token does not belong to this game"
                    },
                )
            })?;
            if let Some(bound) = &conn.game {
                if bound.game_id != game.game_id {
                    return Err(ProtocolError::new(
                        ErrorCode::BadToken,
                        format!(
                            "this connection is at game {}; that token is for game {}. Open a second connection.",
                            bound.game_id, game.game_id
                        ),
                    ));
                }
            }
            Ok(game_task::welcome(&game, conn, role, name).await)
        }

        other => {
            let game = conn.game.clone().ok_or_else(|| ProtocolError::bad_request("send hello first"))?;
            game_task::handle(&game, conn, other).await
        }
    }
}
