//! One seat's connection to the daemon, kept current by a background task
//! that consumes pushed events. The MCP tools read from here; the daemon is
//! still where the state lives.

use anyhow::{anyhow, bail, Context, Result};
use engine::text::describe_event_view;
use engine::{ActReason, Action, EventBase, EventView, Format, GameView, ObjectId, Outcome, Seat};
use protocol::{async_client, AsyncClient, ClientError, Endpoint, ErrorCode, LegalAction, LobbyView, Role, ServerMessage, Token};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

/// How often `wait_for_turn` asks the daemon for the state directly.
const POLL_EVERY: Duration = Duration::from_secs(3);

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
    pub cards: engine::CardDb,
}

pub enum Wait {
    Ready(GameView),
    TimedOut,
}

impl Session {
    pub async fn connect(config: SessionConfig) -> Result<Arc<Session>> {
        let (client, mut pushes) = async_client::connect(&config.endpoint)
            .await
            .with_context(|| format!("connecting to {}", config.endpoint))?;
        let welcome = client.hello(&config.token, Some(&config.name)).await?;
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
            cards: cards::core(),
        });
        if let Some(state) = welcome.state {
            session.store_view(state);
        }

        let bg = session.clone();
        tokio::spawn(async move {
            while let Some(msg) = pushes.recv().await {
                bg.handle_push(msg).await;
            }
        });
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
        let text = match event {
            EventBase::Chat { from, to, text } => {
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
        self.log.lock().unwrap().push(LogLine { turn, text });
    }

    /// Fetch the latest state from the daemon.
    pub async fn refresh(&self) {
        match self.client.get_state().await {
            Ok(state) => self.store_view(state),
            Err(ClientError::Protocol(e)) if e.code == ErrorCode::BadRequest => {} // not started yet
            Err(e) => tracing::warn!("get_state failed: {e}"),
        }
    }

    pub fn log_since(&self, since_turn: Option<u32>) -> Vec<LogLine> {
        let log = self.log.lock().unwrap();
        match since_turn {
            Some(t) => log.iter().filter(|l| l.turn >= t).cloned().collect(),
            None => log.clone(),
        }
    }

    /// Block until this seat must act or the game ends, or the timeout passes.
    pub async fn wait_for_turn(&self, timeout: Duration) -> Wait {
        let me = self.me;
        let ready = |v: &Option<GameView>| match v {
            Some(v) => v.must_act.contains_key(&me) || v.outcome.is_some(),
            None => false,
        };
        let mut rx = self.view.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        // Pushes are the fast path; a periodic poll of the daemon covers a
        // push that was lost or that carried nothing visible to this seat.
        loop {
            if ready(&rx.borrow_and_update()) {
                return Wait::Ready(self.view().expect("ready implies a view"));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Wait::TimedOut;
            }
            let slice = (deadline - now).min(POLL_EVERY);
            match tokio::time::timeout(slice, rx.changed()).await {
                Ok(Err(_)) => return Wait::TimedOut, // session gone
                Ok(Ok(())) => {}
                Err(_) => self.refresh().await,
            }
        }
    }

    /// Legal actions for this seat, remembered for `take_action` by id.
    pub async fn legal_actions(&self) -> Result<(Vec<LegalAction>, u64, Option<ActReason>)> {
        let (actions, version, reason) = self.client.get_legal_actions().await.map_err(describe_client_error)?;
        *self.last_legal.lock().unwrap() = Some((actions.clone(), version));
        Ok((actions, version, reason))
    }

    /// Resolve an action id from the last `legal_actions` call.
    pub fn action_by_id(&self, id: u32) -> Result<(Action, u64)> {
        let guard = self.last_legal.lock().unwrap();
        let (actions, version) = guard
            .as_ref()
            .ok_or_else(|| anyhow!("call get_legal_actions first to get action ids"))?;
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

pub fn describe_client_error(e: ClientError) -> anyhow::Error {
    match e {
        ClientError::Protocol(p) => anyhow!("{}", p.message),
        other => anyhow!("{other}"),
    }
}
