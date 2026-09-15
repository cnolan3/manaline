//! A client whose reader runs in its own task: requests are awaited by any
//! number of holders of a cloneable handle, and pushed messages arrive on a
//! channel. This is what an interactive client (the TUI) needs, since it
//! must react to pushes while a request is in flight.
//!
//! `join` adds the other half of what a networked seat needs: the connection
//! comes back by itself. A remote seat's link is expected to drop (§5) and the
//! daemon keeps the seat, so the client's job is to reconnect, say hello with
//! the same token, and carry on as if nothing had happened.

use crate::client::{BoxedRead, BoxedWrite, Client, ClientError, Welcome};
use crate::endpoint::Endpoint;
use crate::framing::{FrameError, FramedReader, FramedWriter};
use crate::messages::{ClientEnvelope, ClientMessage, LegalAction, ServerEnvelope, ServerMessage, Token, PROTOCOL_VERSION};
use engine::{Action, EventView, GameView};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<ServerMessage>>>>;

#[derive(Clone)]
pub struct AsyncClient {
    out: mpsc::Sender<ClientEnvelope>,
    pending: Pending,
    next_req: Arc<AtomicU64>,
    state: watch::Receiver<ConnState>,
}

/// How a client comes back after the connection drops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub initial: Duration,
    pub max: Duration,
    /// Total wall time from the first failure, not per attempt.
    pub give_up_after: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        ReconnectPolicy {
            initial: Duration::from_millis(500),
            max: Duration::from_secs(10),
            give_up_after: Duration::from_secs(300),
        }
    }
}

/// Where a client's connection stands, for an app that wants to say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Connected,
    /// The connection dropped; trying again. `attempt` counts tries so far.
    Connecting {
        attempt: u32,
    },
    /// Connected again after a drop; the app should resync its state.
    /// Stays here until the next drop.
    Reconnected,
    /// Gave up after `ReconnectPolicy::give_up_after`. Every request now fails.
    GaveUp,
}

impl ConnState {
    pub fn is_connected(self) -> bool {
        matches!(self, ConnState::Connected | ConnState::Reconnected)
    }
}

/// What a reconnecting client needs to re-announce itself.
#[derive(Clone, Debug)]
pub struct ReconnectConfig {
    pub endpoint: Endpoint,
    pub token: Token,
    pub name: Option<String>,
    pub policy: ReconnectPolicy,
}

/// A joined, reconnecting client.
pub struct Joined {
    pub client: AsyncClient,
    pub pushes: mpsc::Receiver<ServerMessage>,
    pub welcome: Welcome,
}

/// Connect, say hello, and keep the connection up: a drop is retried with
/// backoff, `hello` is re-sent with the same token, a `subscribe` is renewed,
/// and each request in flight is retried once. The first `hello` is not
/// retried — a token the daemon will not welcome is the caller's problem.
pub async fn join(config: ReconnectConfig) -> Result<Joined, ClientError> {
    let mut client = Client::connect(&config.endpoint).await?;
    let welcome = client.hello(&config.token, config.name.as_deref()).await?;
    let (reader, writer, queued) = client.into_parts();
    let (client, pushes) = start(reader, writer, queued, Some(config));
    Ok(Joined { client, pushes, welcome })
}

/// Connect and spawn the reader and writer tasks. Pushed messages arrive on
/// the returned receiver; it closes when the connection does.
pub async fn connect(endpoint: &Endpoint) -> Result<(AsyncClient, mpsc::Receiver<ServerMessage>), ClientError> {
    let (reader, writer, queued) = Client::connect(endpoint).await?.into_parts();
    Ok(start(reader, writer, queued, None))
}

pub fn spawn(reader: BoxedRead, writer: BoxedWrite) -> (AsyncClient, mpsc::Receiver<ServerMessage>) {
    start(FramedReader::new(reader), FramedWriter::new(writer), Vec::new(), None)
}

/// The one way in: a single supervisor task owns the connection, so handing it
/// a new one is all a reconnect has to be.
fn start(
    reader: FramedReader<BoxedRead, ServerEnvelope>,
    writer: FramedWriter<BoxedWrite, ClientEnvelope>,
    queued: Vec<ServerMessage>,
    config: Option<ReconnectConfig>,
) -> (AsyncClient, mpsc::Receiver<ServerMessage>) {
    let (out_tx, out_rx) = mpsc::channel::<ClientEnvelope>(64);
    let (push_tx, push_rx) = mpsc::channel::<ServerMessage>(256);
    let (state_tx, state_rx) = watch::channel(ConnState::Connected);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let next_req = Arc::new(AtomicU64::new(1));

    let supervisor = Supervisor {
        out_rx,
        push_tx,
        pending: pending.clone(),
        state: state_tx,
        next_req: next_req.clone(),
        config,
        inflight: HashMap::new(),
        subscribed: false,
    };
    tokio::spawn(supervisor.run(link(reader, writer), queued));

    (
        AsyncClient {
            out: out_tx,
            pending,
            next_req,
            state: state_rx,
        },
        push_rx,
    )
}

/// One connection: its reader pumped into a channel — `FramedReader::recv` is
/// not cancel-safe, so the supervisor must not `select!` on it directly.
struct Link {
    reads: mpsc::Receiver<ServerEnvelope>,
    writer: FramedWriter<BoxedWrite, ClientEnvelope>,
}

fn link(mut reader: FramedReader<BoxedRead, ServerEnvelope>, writer: FramedWriter<BoxedWrite, ClientEnvelope>) -> Link {
    let (tx, reads) = mpsc::channel::<ServerEnvelope>(64);
    tokio::spawn(async move {
        loop {
            match reader.recv().await {
                Ok(env) => {
                    if tx.send(env).await.is_err() {
                        break;
                    }
                }
                // One malformed line is not a dead connection.
                Err(FrameError::Json(_)) => continue,
                Err(_) => break,
            }
        }
    });
    Link { reads, writer }
}

/// Why the steady-state loop stopped.
enum Exit {
    /// Every `AsyncClient` handle is gone; nothing left to do.
    HandlesGone,
    /// The connection failed.
    Dead,
}

enum Handshake {
    Ok,
    /// The new connection died before the welcome; try another.
    Dropped,
    /// The daemon answered, but not with a welcome: trying again won't help.
    Refused,
}

struct Supervisor {
    out_rx: mpsc::Receiver<ClientEnvelope>,
    push_tx: mpsc::Sender<ServerMessage>,
    pending: Pending,
    state: watch::Sender<ConnState>,
    next_req: Arc<AtomicU64>,
    /// `None` for the plain `spawn` path, which does not reconnect.
    config: Option<ReconnectConfig>,
    /// Requests written but not yet answered, and whether each has already
    /// been retried once.
    inflight: HashMap<u64, (ClientEnvelope, bool)>,
    subscribed: bool,
}

impl Supervisor {
    async fn run(mut self, mut conn: Link, queued: Vec<ServerMessage>) {
        for msg in queued {
            let _ = self.push_tx.send(msg).await;
        }
        loop {
            match self.steady(&mut conn).await {
                // Nobody can send another request, but a push receiver may still
                // be listening, so keep reading until the connection itself ends.
                Exit::HandlesGone => {
                    while let Some(env) = conn.reads.recv().await {
                        self.dispatch(env).await;
                    }
                    return;
                }
                // Replies that arrived before the link died still count.
                Exit::Dead => {
                    while let Ok(env) = conn.reads.try_recv() {
                        self.dispatch(env).await;
                    }
                }
            }
            let Some(config) = self.config.clone() else {
                // Without a reconnect config a dead reader is simply the end.
                self.give_up();
                return;
            };
            match self.reconnect(&config).await {
                Some(next) => conn = next,
                None => {
                    self.give_up();
                    return;
                }
            }
        }
    }

    /// Connected: write what the handles send, dispatch what arrives.
    async fn steady(&mut self, conn: &mut Link) -> Exit {
        loop {
            tokio::select! {
                out = self.out_rx.recv() => match out {
                    None => return Exit::HandlesGone,
                    Some(env) => {
                        let sent = conn.writer.send(&env).await.is_ok();
                        self.record(env, sent);
                        if !sent {
                            return Exit::Dead;
                        }
                    }
                },
                read = conn.reads.recv() => match read {
                    Some(env) => self.dispatch(env).await,
                    None => return Exit::Dead,
                },
            }
        }
    }

    fn record(&mut self, env: ClientEnvelope, sent: bool) {
        if sent && matches!(env.msg, ClientMessage::Subscribe) {
            self.subscribed = true;
        }
        if let Some(req) = env.req {
            self.inflight.insert(req, (env, false));
        }
    }

    async fn dispatch(&mut self, env: ServerEnvelope) {
        match env.req {
            Some(req) => {
                self.inflight.remove(&req);
                let waiter = self.pending.lock().unwrap().remove(&req);
                if let Some(w) = waiter {
                    let _ = w.send(env.msg);
                }
            }
            // A closed push channel only means nobody is listening.
            None => {
                let _ = self.push_tx.send(env.msg).await;
            }
        }
    }

    /// Back off, reconnect, re-announce. `None` once the policy runs out.
    async fn reconnect(&mut self, config: &ReconnectConfig) -> Option<Link> {
        let started = Instant::now();
        let mut delay = config.policy.initial;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let _ = self.state.send(ConnState::Connecting { attempt });
            let left = config.policy.give_up_after.checked_sub(started.elapsed())?;
            tokio::time::sleep(delay.min(left)).await;
            delay = (delay * 2).min(config.policy.max);

            let Ok(client) = Client::connect(&config.endpoint).await else {
                continue;
            };
            let (reader, writer, queued) = client.into_parts();
            let mut conn = link(reader, writer);
            for msg in queued {
                let _ = self.push_tx.send(msg).await;
            }
            match self.hello(config, &mut conn).await {
                Handshake::Ok => {}
                Handshake::Dropped => continue,
                Handshake::Refused => return None,
            }
            if self.subscribed {
                let req = self.next_req.fetch_add(1, Ordering::SeqCst);
                let env = ClientEnvelope {
                    req: Some(req),
                    msg: ClientMessage::Subscribe,
                };
                // Nobody waits on this one; its reply falls through `dispatch`.
                if conn.writer.send(&env).await.is_err() {
                    continue;
                }
            }
            if self.resend(&mut conn).await {
                let _ = self.state.send(ConnState::Reconnected);
                return Some(conn);
            }
        }
    }

    /// Re-announce this seat. The daemon welcomes a rejoin straight into the
    /// running game, so the reply is the same `welcome` as the first time.
    async fn hello(&mut self, config: &ReconnectConfig, conn: &mut Link) -> Handshake {
        let req = self.next_req.fetch_add(1, Ordering::SeqCst);
        let env = ClientEnvelope {
            req: Some(req),
            msg: ClientMessage::Hello {
                token: config.token.clone(),
                protocol_version: PROTOCOL_VERSION,
                name: config.name.clone(),
            },
        };
        if conn.writer.send(&env).await.is_err() {
            return Handshake::Dropped;
        }
        loop {
            let Some(env) = conn.reads.recv().await else {
                return Handshake::Dropped;
            };
            if env.req == Some(req) {
                return match env.msg {
                    ServerMessage::Welcome { .. } => Handshake::Ok,
                    _ => Handshake::Refused,
                };
            }
            // Anything else that arrived meanwhile is ordinary traffic.
            self.dispatch(env).await;
        }
    }

    /// Re-send every request still in flight, once. `false` if the new
    /// connection died while we were doing it.
    ///
    /// Re-sending an `act` is safe because the daemon checks `state_version`:
    /// a duplicate comes back `stale_state_version` rather than applying twice.
    async fn resend(&mut self, conn: &mut Link) -> bool {
        let mut reqs: Vec<u64> = self.inflight.keys().copied().collect();
        reqs.sort_unstable();
        for req in reqs {
            let retried = match self.inflight.get(&req) {
                Some(&(_, retried)) => retried,
                None => continue,
            };
            if retried {
                // A second drop with the same request still unanswered: fail it
                // rather than send a third copy. The client itself stays up —
                // dropping the pending sender wakes its caller with `Closed`.
                self.inflight.remove(&req);
                self.pending.lock().unwrap().remove(&req);
                continue;
            }
            let env = {
                let slot = self.inflight.get_mut(&req).expect("present, checked just above");
                slot.1 = true;
                slot.0.clone()
            };
            if conn.writer.send(&env).await.is_err() {
                return false;
            }
        }
        true
    }

    /// Out of retries: fail everything waiting and stop. Dropping the push
    /// sender along with `self` ends the caller's `pushes.recv()`.
    fn give_up(&mut self) {
        let _ = self.state.send(ConnState::GaveUp);
        self.pending.lock().unwrap().clear();
    }
}

impl AsyncClient {
    pub fn conn_state(&self) -> ConnState {
        *self.state.borrow()
    }

    pub fn watch_state(&self) -> watch::Receiver<ConnState> {
        self.state.clone()
    }

    pub async fn request(&self, msg: ClientMessage) -> Result<ServerMessage, ClientError> {
        let req = self.next_req.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(req, tx);
        self.out
            .send(ClientEnvelope { req: Some(req), msg })
            .await
            .map_err(|_| ClientError::Frame(FrameError::Closed))?;
        match rx.await {
            Ok(ServerMessage::Error(e)) => Err(ClientError::Protocol(e)),
            Ok(m) => Ok(m),
            Err(_) => Err(ClientError::Frame(FrameError::Closed)),
        }
    }

    pub async fn hello(&self, token: &Token, name: Option<&str>) -> Result<Welcome, ClientError> {
        let msg = ClientMessage::Hello {
            token: token.clone(),
            protocol_version: PROTOCOL_VERSION,
            name: name.map(String::from),
        };
        match self.request(msg).await? {
            ServerMessage::Welcome {
                role,
                game_id,
                format,
                lobby,
                state,
                ..
            } => Ok(Welcome {
                role,
                game_id,
                format,
                lobby,
                state,
            }),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn set_deck(&self, decklist: &str) -> Result<Result<(), Vec<engine::Violation>>, ClientError> {
        match self
            .request(ClientMessage::SetDeck {
                decklist: decklist.into(),
                commander: None,
            })
            .await?
        {
            ServerMessage::DeckOk => Ok(Ok(())),
            ServerMessage::DeckRejected { violations } => Ok(Err(violations)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    async fn expect_ok(&self, msg: ClientMessage) -> Result<(), ClientError> {
        match self.request(msg).await? {
            ServerMessage::Ok => Ok(()),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn ready(&self) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Ready).await
    }

    pub async fn subscribe(&self) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Subscribe).await
    }

    pub async fn chat(&self, text: &str, to: Option<engine::Seat>) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Chat { text: text.into(), to }).await
    }

    pub async fn get_state(&self) -> Result<GameView, ClientError> {
        match self.request(ClientMessage::GetState).await? {
            ServerMessage::State { state } => Ok(state),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn get_legal_actions(&self) -> Result<(Vec<LegalAction>, u64, Option<engine::ActReason>), ClientError> {
        match self.request(ClientMessage::GetLegalActions).await? {
            ServerMessage::LegalActions {
                actions,
                state_version,
                reason,
            } => Ok((actions, state_version, reason)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn act(&self, action: Action, state_version: u64) -> Result<(Vec<EventView>, GameView, Vec<LegalAction>), ClientError> {
        let msg = ClientMessage::Act {
            action_id: None,
            action: Some(action),
            state_version,
        };
        match self.request(msg).await? {
            ServerMessage::Ack {
                events,
                state,
                legal_actions,
                ..
            } => Ok((events, state, legal_actions)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::LobbyView;
    use tokio::net::{TcpListener, TcpStream};

    /// Every `hello` the stub daemon saw, in order.
    type Hellos = Arc<Mutex<Vec<(Token, Option<String>)>>>;

    /// A stub daemon: welcomes every `hello`, answers `ping`, and — on the
    /// connection numbered `hang_up_on` — drops the socket mid-request instead.
    async fn stub(listener: TcpListener, hang_up_on: u32, hellos: Hellos) {
        let mut n = 0;
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            n += 1;
            serve(sock, n == hang_up_on, &hellos).await;
        }
    }

    async fn serve(sock: TcpStream, hang_up: bool, hellos: &Hellos) {
        let (r, w) = sock.into_split();
        let mut reader: FramedReader<_, ClientEnvelope> = FramedReader::new(r);
        let mut writer: FramedWriter<_, ServerEnvelope> = FramedWriter::new(w);
        while let Ok(env) = reader.recv().await {
            let reply = match env.msg {
                ClientMessage::Hello { token, name, .. } => {
                    hellos.lock().unwrap().push((token, name));
                    ServerMessage::Welcome {
                        role: crate::messages::Role::Spectator,
                        game_id: crate::messages::GameId("g".into()),
                        format: engine::Format::builtin("two-player").unwrap(),
                        protocol_version: PROTOCOL_VERSION,
                        lobby: LobbyView {
                            seats: Vec::new(),
                            started: false,
                        },
                        state: None,
                    }
                }
                ClientMessage::Ping if hang_up => return,
                ClientMessage::Ping => ServerMessage::Pong,
                _ => ServerMessage::Ok,
            };
            if writer.send(&ServerEnvelope { req: env.req, msg: reply }).await.is_err() {
                return;
            }
        }
    }

    fn brisk() -> ReconnectPolicy {
        ReconnectPolicy {
            initial: Duration::from_millis(30),
            max: Duration::from_millis(50),
            give_up_after: Duration::from_secs(2),
        }
    }

    async fn listen() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        (l, addr)
    }

    fn config(addr: String, policy: ReconnectPolicy) -> ReconnectConfig {
        ReconnectConfig {
            endpoint: Endpoint::Tcp(addr),
            token: Token("seat-token".into()),
            name: Some("Ann".into()),
            policy,
        }
    }

    #[tokio::test]
    async fn a_dropped_connection_comes_back_with_the_same_token() {
        let (listener, addr) = listen().await;
        let hellos = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(stub(listener, 1, hellos.clone()));

        let joined = join(config(addr, brisk())).await.unwrap();
        assert_eq!(joined.client.conn_state(), ConnState::Connected);

        // Watch from a task so no transition is missed while the request runs.
        let mut states = joined.client.watch_state();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            while states.changed().await.is_ok() {
                let s = *states.borrow();
                record.lock().unwrap().push(s);
                if s == ConnState::Reconnected {
                    break;
                }
            }
        });

        // This request kills the first connection; it is answered after the rejoin.
        let reply = joined.client.request(ClientMessage::Ping).await.unwrap();
        assert_eq!(reply, ServerMessage::Pong);
        assert_eq!(joined.client.conn_state(), ConnState::Reconnected);
        assert!(joined.client.conn_state().is_connected());

        let seen = seen.lock().unwrap().clone();
        assert!(
            seen.iter().any(|s| matches!(s, ConnState::Connecting { attempt: 1 })),
            "expected a Connecting state, saw {seen:?}"
        );
        assert_eq!(seen.last(), Some(&ConnState::Reconnected));

        let hellos = hellos.lock().unwrap().clone();
        assert_eq!(hellos.len(), 2, "hello re-sent once");
        assert!(hellos.iter().all(|(t, n)| t.0 == "seat-token" && n.as_deref() == Some("Ann")));
    }

    #[tokio::test]
    async fn a_subscribe_is_renewed_and_later_requests_still_work() {
        let (listener, addr) = listen().await;
        let hellos = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(stub(listener, 1, hellos.clone()));

        let joined = join(config(addr, brisk())).await.unwrap();
        joined.client.subscribe().await.unwrap();
        assert!(joined.client.request(ClientMessage::Ping).await.is_ok());
        // Still usable once the dust settles.
        assert_eq!(joined.client.request(ClientMessage::Ping).await.unwrap(), ServerMessage::Pong);
        assert_eq!(joined.client.conn_state(), ConnState::Reconnected);
    }

    #[tokio::test]
    async fn giving_up_fails_every_request_and_ends_the_pushes() {
        let (listener, addr) = listen().await;
        let hellos = Arc::new(Mutex::new(Vec::new()));
        // Serve the first connection, then stop listening entirely.
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            serve(sock, true, &hellos).await;
        });

        let policy = ReconnectPolicy {
            initial: Duration::from_millis(10),
            max: Duration::from_millis(20),
            give_up_after: Duration::from_millis(150),
        };
        let joined = join(config(addr, policy)).await.unwrap();
        let mut pushes = joined.pushes;

        // The ping kills the connection and is never answered.
        assert!(matches!(
            joined.client.request(ClientMessage::Ping).await,
            Err(ClientError::Frame(FrameError::Closed))
        ));
        assert_eq!(joined.client.conn_state(), ConnState::GaveUp);
        assert!(!joined.client.conn_state().is_connected());
        assert!(pushes.recv().await.is_none());
        assert!(matches!(
            joined.client.request(ClientMessage::Ping).await,
            Err(ClientError::Frame(FrameError::Closed))
        ));
    }

    #[tokio::test]
    async fn the_plain_spawn_path_does_not_reconnect() {
        let (a, b) = tokio::io::duplex(1024);
        let (ar, aw) = tokio::io::split(a);
        let (client, mut pushes) = spawn(Box::new(ar), Box::new(aw));
        assert_eq!(client.conn_state(), ConnState::Connected);
        drop(b);
        assert!(pushes.recv().await.is_none());
        assert_eq!(client.conn_state(), ConnState::GaveUp);
    }
}
