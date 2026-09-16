//! One seat's connection to the daemon, kept current by a background task
//! that consumes pushed events. The MCP tools read from here; the daemon is
//! still where the state lives.

use anyhow::{anyhow, bail, Context, Result};
use engine::text::describe_event_view;
use engine::{ActReason, Action, EventBase, EventView, Format, GameView, ObjectId, Outcome, Seat};
use protocol::{
    async_client, AsyncClient, ClientError, ConnState, Endpoint, ErrorCode, Joined, LegalAction, LobbyView, ReconnectConfig,
    ReconnectPolicy, Role, ServerMessage, Token,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::watch;

/// How often `wait_for_turn` asks the daemon for the state directly.
const POLL_EVERY: Duration = Duration::from_secs(3);

/// How long an ordinary request (say, take_action, get_game_state) may sit on
/// a link that is reconnecting before it comes back and says so. The
/// reconnect policy runs for five minutes; a tool call that blocked for that
/// long would look to the agent like a hang. `wait_for_turn` is the
/// exception — waiting is what it is for — and keeps its own timeout.
pub const REQUEST_GRACE: Duration = Duration::from_secs(15);

/// What every game tool says once the game has ended and the table has gone.
pub const TABLE_CLOSED: &str = "the game is over and the table has closed; call leave";

/// What an ordinary request says while the link is down but the game is live.
/// Careful not to promise nothing happened: a request already written before
/// the drop is re-sent on reconnect, and the daemon's `state_version` check is
/// what keeps that from applying twice.
pub const RECONNECTING: &str =
    "the connection to the table has dropped and is being put back up; this call went unanswered. Try it again in a moment — fetch the state first, in case it did land. (wait_for_turn waits through a reconnect on its own.)";

pub struct SessionConfig {
    pub endpoint: Endpoint,
    pub token: Token,
    pub name: String,
    /// Deck to submit if the game has not started.
    pub decklist: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LogLine {
    pub turn: u32,
    pub text: String,
    /// Somebody else talking to the table. The tools push these into their own
    /// replies, because an agent that never calls `get_log` would otherwise
    /// never hear a word anyone said to it. Your own chat is not marked: you
    /// know what you said.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat: bool,
}

pub struct Session {
    pub client: AsyncClient,
    pub me: Seat,
    pub game_id: String,
    pub format: Format,
    view: watch::Sender<Option<GameView>>,
    lobby: Mutex<LobbyView>,
    log: Mutex<Vec<LogLine>>,
    names: Mutex<HashMap<ObjectId, String>>,
    /// The last legal actions handed out, so `take_action` can accept an id.
    last_legal: Mutex<Option<(Vec<LegalAction>, u64)>>,
    /// Bumped by every tool call; a `wait_for_turn` loop whose generation is
    /// behind has been abandoned by the client and must stop acting.
    wait_gen: std::sync::atomic::AtomicU64,
    /// How far into `log` this session has already been shown the chat.
    chat_cursor: Mutex<usize>,
    pub cards: engine::CardDb,
}

pub enum Wait {
    Ready(GameView),
    TimedOut,
    /// The connection dropped and did not come back: the client gave up
    /// reconnecting. A drop on its own is not this — the wait sits through one.
    Disconnected,
    /// A newer tool call arrived; this wait must not act any further.
    Superseded,
}

impl Session {
    pub async fn connect(config: SessionConfig) -> Result<Arc<Session>> {
        // `join` says hello for us and, per §5, puts the link back up by itself:
        // a seat that drops is still this seat's, so an agent mid-game is not
        // thrown out of it by a broken pipe.
        let Joined { client, pushes, welcome } = async_client::join(ReconnectConfig {
            endpoint: config.endpoint.clone(),
            token: config.token.clone(),
            name: Some(config.name.clone()),
            policy: ReconnectPolicy::default(),
        })
        .await
        .with_context(|| format!("connecting to {}", config.endpoint))?;
        let me = match welcome.role {
            Role::Seat(s) => s,
            Role::Spectator => bail!("the MCP server needs a seat token, not a spectator token"),
        };
        client.subscribe().await?;
        if let (Some(deck), false) = (&config.decklist, welcome.lobby.started) {
            match client.set_deck(deck).await? {
                Ok(()) => {}
                Err(violations) => {
                    let list: Vec<String> = violations.iter().map(ToString::to_string).collect();
                    bail!("deck rejected: {}", list.join("; "));
                }
            }
            client.ready().await?;
        }

        let (view_tx, _) = watch::channel(None);
        let session = Arc::new(Session {
            client,
            me,
            game_id: welcome.game_id.0.clone(),
            format: welcome.format.clone(),
            view: view_tx,
            lobby: Mutex::new(welcome.lobby.clone()),
            log: Mutex::new(Vec::new()),
            names: Mutex::new(HashMap::new()),
            last_legal: Mutex::new(None),
            wait_gen: std::sync::atomic::AtomicU64::new(0),
            chat_cursor: Mutex::new(0),
            cards: cards::core(),
        });
        if let Some(state) = welcome.state {
            session.store_view(state);
        }

        pump_pushes(&session, pushes);
        watch_connection(&session);
        Ok(session)
    }

    fn store_view(&self, view: GameView) {
        {
            let mut names = self.names.lock().unwrap();
            for (id, o) in &view.objects {
                names.insert(*id, o.name.clone());
            }
        }
        self.view.send_replace(Some(view));
    }

    pub fn view(&self) -> Option<GameView> {
        self.view.borrow().clone()
    }

    pub fn lobby(&self) -> LobbyView {
        self.lobby.lock().unwrap().clone()
    }

    pub fn started(&self) -> bool {
        self.view().is_some()
    }

    pub fn name_of(&self, id: ObjectId) -> String {
        self.names.lock().unwrap().get(&id).cloned().unwrap_or_else(|| id.to_string())
    }

    pub fn seat_name(&self, seat: Seat) -> String {
        if let Some(v) = self.view() {
            if let Some(p) = v.players.get(seat.index()) {
                return p.name.clone();
            }
        }
        self.lobby()
            .seats
            .get(seat.index())
            .and_then(|s| s.name.clone())
            .unwrap_or_else(|| format!("seat {}", seat.0))
    }

    /// Why this seat must act right now, if it must.
    pub fn my_reason(&self) -> Option<ActReason> {
        self.view()?.must_act.get(&self.me).copied()
    }

    pub fn outcome(&self) -> Option<Outcome> {
        self.view()?.outcome
    }

    /// The game has ended *and* the link is gone: the headless `play` exits
    /// when the game is over and takes its daemon with it, so there is nothing
    /// left to reconnect to. Says so once, and tells the client to stop trying
    /// — otherwise every request blocks behind a five-minute retry.
    pub fn table_closed(&self) -> bool {
        if self.outcome().is_none() || self.client.conn_state().is_connected() {
            return false;
        }
        self.client.stop_reconnecting();
        true
    }

    /// Run one request without letting a broken link swallow it for the whole
    /// reconnect policy: a finished table fails at once, and a live one that is
    /// reconnecting gets `REQUEST_GRACE` before the tool comes back and says
    /// so. The outer error means the request was never sent; the inner one is
    /// the daemon's own answer, kept whole so callers can still read its code.
    pub async fn bounded<T>(&self, request: impl std::future::Future<Output = Result<T, ClientError>>) -> Result<Result<T, ClientError>> {
        if self.table_closed() {
            bail!("{TABLE_CLOSED}");
        }
        let mut conn = self.client.watch_state();
        let deadline = tokio::time::Instant::now() + REQUEST_GRACE;
        tokio::pin!(request);
        loop {
            tokio::select! {
                done = &mut request => return Ok(done),
                // A drop that turns out to be the end of a finished game, or a
                // client that has given up, is not worth the full grace period.
                changed = conn.changed() => {
                    if self.table_closed() {
                        bail!("{TABLE_CLOSED}");
                    }
                    if changed.is_err() || *conn.borrow() == ConnState::GaveUp {
                        bail!("lost the connection to the game and could not get it back; the game may still be running — try again or rejoin.");
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    if self.table_closed() {
                        bail!("{TABLE_CLOSED}");
                    }
                    bail!("{RECONNECTING}");
                }
            }
        }
    }

    async fn handle_push(&self, msg: ServerMessage) {
        match msg {
            ServerMessage::Event { event, state_version } => {
                self.log_event(&event);
                let current = self.view().map(|v| v.state_version).unwrap_or(0);
                if state_version > current {
                    self.refresh().await;
                }
            }
            ServerMessage::Lobby { lobby } => {
                let started = lobby.started;
                *self.lobby.lock().unwrap() = lobby;
                if started && !self.started() {
                    self.refresh().await;
                }
            }
            ServerMessage::MustAct { .. } => self.refresh().await,
            _ => {}
        }
    }

    fn log_event(&self, event: &EventView) {
        let noisy = matches!(
            event,
            EventBase::PriorityPassed { .. }
                | EventBase::Tapped { .. }
                | EventBase::Untapped { .. }
                | EventBase::Shuffled { .. }
                | EventBase::ManaAdded { .. }
        );
        if noisy {
            return;
        }
        let turn = match event {
            EventBase::TurnStarted { turn, .. } => *turn,
            _ => self.view().map(|v| v.turn).unwrap_or(0),
        };
        let mut chat = false;
        let text = match event {
            EventBase::Chat { from, to, text } => {
                chat = *from != self.me;
                let who = if *from == self.me {
                    "You".to_string()
                } else {
                    self.seat_name(*from)
                };
                match to {
                    Some(t) => format!("{who} → {}: {text}", self.seat_name(*t)),
                    None => format!("{who}: {text}"),
                }
            }
            e => {
                let names = |id: ObjectId| format!("{} {id}", self.name_of(id));
                let seats = |s: Seat| self.seat_name(s);
                describe_event_view(e, &names, &seats)
            }
        };
        self.log.lock().unwrap().push(LogLine { turn, text, chat });
    }

    /// What the table has said to this seat since the last tool reply, and
    /// marks it delivered. The tools append it to their own text: chat is
    /// pushed live and never replayed, so an agent that only reads tool
    /// replies would otherwise be talked at and never hear it.
    pub fn undelivered_chat(&self) -> Vec<String> {
        let log = self.log.lock().unwrap();
        let mut cursor = self.chat_cursor.lock().unwrap();
        let from = (*cursor).min(log.len());
        let lines = log[from..].iter().filter(|l| l.chat).map(|l| l.text.clone()).collect();
        *cursor = log.len();
        lines
    }

    /// Fetch the latest state from the daemon. Never blocks longer than
    /// `REQUEST_GRACE`: the cached view is better than a tool that hangs.
    pub async fn refresh(&self) {
        if self.table_closed() {
            // Nothing to ask: the cached final state is the whole truth now.
            return;
        }
        match tokio::time::timeout(REQUEST_GRACE, self.client.get_state()).await {
            Ok(Ok(state)) => self.store_view(state),
            Ok(Err(ClientError::Protocol(e))) if e.code == ErrorCode::BadRequest => {} // not started yet
            Ok(Err(e)) => tracing::warn!("get_state failed: {e}"),
            Err(_) => tracing::warn!("get_state gave up after {REQUEST_GRACE:?}: the link is still down"),
        }
    }

    pub fn log_since(&self, since_turn: Option<u32>) -> Vec<LogLine> {
        let log = self.log.lock().unwrap();
        match since_turn {
            Some(t) => log.iter().filter(|l| l.turn >= t).cloned().collect(),
            None => log.clone(),
        }
    }

    /// Start a new tool call: any wait loop still running from an earlier
    /// call (one the client gave up on) is told to stop. Returns this call's generation.
    pub fn begin_call(&self) -> u64 {
        self.wait_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }

    pub fn superseded(&self, generation: u64) -> bool {
        self.wait_gen.load(std::sync::atomic::Ordering::SeqCst) != generation
    }

    /// Block until this seat must act or the game ends, the timeout passes,
    /// or a newer tool call supersedes this one.
    pub async fn wait_for_turn(&self, timeout: Duration, generation: u64) -> Wait {
        let me = self.me;
        let ready = |v: &Option<GameView>| match v {
            Some(v) => v.must_act.contains_key(&me) || v.outcome.is_some(),
            None => false,
        };
        let mut rx = self.view.subscribe();
        let mut conn = self.client.watch_state();
        let deadline = tokio::time::Instant::now() + timeout;
        // Pushes are the fast path; a periodic poll of the daemon covers a
        // push that was lost or that carried nothing visible to this seat.
        loop {
            if self.superseded(generation) {
                return Wait::Superseded;
            }
            if ready(&rx.borrow_and_update()) {
                return Wait::Ready(self.view().expect("ready implies a view"));
            }
            let state = *conn.borrow_and_update();
            if state == ConnState::GaveUp {
                return Wait::Disconnected;
            }
            // A link that is down is not the agent taking too long: the seat is
            // still ours and the turn may be waiting on the other side of the
            // reconnect, so the timeout only runs while we are connected.
            let slice = if state.is_connected() {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Wait::TimedOut;
                }
                (deadline - now).min(POLL_EVERY).min(Duration::from_millis(500))
            } else {
                POLL_EVERY
            };
            tokio::select! {
                changed = rx.changed() => {
                    if changed.is_err() {
                        return Wait::TimedOut; // session gone
                    }
                }
                changed = conn.changed() => {
                    if changed.is_err() {
                        return Wait::Disconnected; // the client itself is gone
                    }
                }
                _ = tokio::time::sleep(slice) => self.refresh().await,
            }
        }
    }

    /// Legal actions for this seat, remembered for `take_action` by id.
    pub async fn legal_actions(&self) -> Result<(Vec<LegalAction>, u64, Option<ActReason>)> {
        let (actions, version, reason) = self
            .bounded(self.client.get_legal_actions())
            .await?
            .map_err(describe_client_error)?;
        *self.last_legal.lock().unwrap() = Some((actions.clone(), version));
        Ok((actions, version, reason))
    }

    /// Resolve an action id from the last `legal_actions` call. The id is
    /// bound to the state version that list came from: if the game has moved
    /// on since, or the caller names a different version, it is refused
    /// rather than remapped onto whatever id 0 means now.
    pub fn action_by_id(&self, id: u32, expected_version: Option<u64>) -> Result<(Action, u64)> {
        let guard = self.last_legal.lock().unwrap();
        let (actions, version) = guard
            .as_ref()
            .ok_or_else(|| anyhow!("call get_legal_actions first to get action ids"))?;
        if let Some(want) = expected_version {
            if want != *version {
                bail!("your action ids are from state version {version}, but you named version {want}; call get_legal_actions again");
            }
        }
        let current = self.current_version();
        if current > *version {
            bail!("the game has moved on (your action list is from version {version}, the game is at {current}); call get_legal_actions or wait_for_turn again before choosing");
        }
        let a = actions
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| anyhow!("no action with id {id} in the last legal_actions list (ids: 0..{})", actions.len()))?;
        Ok((a.action.clone(), *version))
    }

    pub async fn act(&self, action: Action, version: u64) -> Result<(Vec<EventView>, GameView, Vec<LegalAction>), ClientError> {
        let (events, view, legal) = self.client.act(action, version).await?;
        self.store_view(view.clone());
        *self.last_legal.lock().unwrap() = Some((legal.clone(), view.state_version));
        Ok((events, view, legal))
    }

    pub fn current_version(&self) -> u64 {
        self.view().map(|v| v.state_version).unwrap_or(0)
    }
}

/// Feed pushed messages into the session. Holds the session weakly and
/// upgrades per message: a reconnecting client's push channel outlives any one
/// connection, so a strong reference would pin a session nobody wants — one the
/// agent left — to its seat until the reconnector finally gave up.
fn pump_pushes(session: &Arc<Session>, mut pushes: tokio::sync::mpsc::Receiver<ServerMessage>) {
    let weak: Weak<Session> = Arc::downgrade(session);
    tokio::spawn(async move {
        while let Some(msg) = pushes.recv().await {
            // The upgrade must not be held across the `recv` above.
            match weak.upgrade() {
                Some(session) => session.handle_push(msg).await,
                None => return,
            }
        }
    });
}

/// Resync after the link comes back: the daemon welcomed us into a game that
/// moved on while we were away, so the cached view is stale. Holds the session
/// weakly, so the task ends with the session rather than keeping it alive.
fn watch_connection(session: &Arc<Session>) {
    let weak: Weak<Session> = Arc::downgrade(session);
    let mut states = session.client.watch_state();
    tokio::spawn(async move {
        while states.changed().await.is_ok() {
            if *states.borrow_and_update() != ConnState::Reconnected {
                continue;
            }
            match weak.upgrade() {
                Some(session) => session.refresh().await,
                None => return,
            }
        }
    });
}

pub fn describe_client_error(e: ClientError) -> anyhow::Error {
    match e {
        ClientError::Protocol(p) => anyhow!("{}", p.message),
        other => anyhow!("{other}"),
    }
}
