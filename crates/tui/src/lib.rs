//! The manaline terminal client (docs/SPEC.md §6): a thin client that renders
//! the seat-filtered view and turns keys into protocol messages.

pub mod app;
pub mod cardbox;
pub mod editor;
pub mod editor_ui;
pub mod settings;
pub mod theme;
pub mod ui;

use crate::app::{App, Command, LogKind, Mode};
use anyhow::{anyhow, bail, Context, Result};
use engine::Outcome;
use futures::StreamExt;
use protocol::{async_client, AsyncClient, ClientError, Endpoint, Role, Token};
use ratatui::crossterm::event::{Event as TermEvent, EventStream, KeyEventKind};
use ratatui::crossterm::{execute, terminal};
use std::time::Duration;

pub struct TuiConfig {
    pub endpoint: Endpoint,
    pub token: Token,
    pub name: String,
    /// Deck text to submit if the game has not started. `None` for spectators or rejoins.
    pub decklist: Option<String>,
    /// Lines to show in the lobby and log (a join command, agent instructions).
    pub hints: Vec<String>,
    /// Where the deck came from, so the lobby can open the deckbuilder on it.
    pub deck_path: Option<std::path::PathBuf>,
    /// A theme name overriding the saved setting for this run.
    pub theme: Option<String>,
}

/// A joined session: the client handle, its push channel, and the app state.
pub struct Session {
    pub client: AsyncClient,
    pub pushes: tokio::sync::mpsc::Receiver<protocol::ServerMessage>,
    pub app: App,
}

/// Connect, join, and run the interactive client until the user quits.
/// Returns the outcome if the game ended while we were watching.
pub async fn run(config: TuiConfig) -> Result<Option<Outcome>> {
    let Session { client, pushes, mut app } = join(config).await?;
    let guard = TerminalGuard::enter()?;
    let mut terminal = ratatui::init();
    let result = event_loop(&client, pushes, &mut app, &mut terminal).await;
    ratatui::restore();
    drop(guard);
    result.map(|_| app.outcome())
}

/// Everything `run` does before touching the terminal: connect, hello,
/// submit the deck, ready up, subscribe, and load the initial state.
pub async fn join(config: TuiConfig) -> Result<Session> {
    let (client, pushes) = async_client::connect(&config.endpoint)
        .await
        .with_context(|| format!("connecting to {}", config.endpoint))?;
    let welcome = client.hello(&config.token, Some(&config.name)).await?;
    let me = match welcome.role {
        Role::Seat(s) => Some(s),
        Role::Spectator => None,
    };
    // Subscribe before anything that changes the lobby, so no update is missed.
    client.subscribe().await?;
    if let (Some(_), Some(deck), false) = (me, &config.decklist, welcome.lobby.started) {
        match client.set_deck(deck).await? {
            Ok(()) => {}
            Err(violations) => {
                let list: Vec<String> = violations.iter().map(ToString::to_string).collect();
                bail!("deck rejected: {}", list.join("; "));
            }
        }
        client.ready().await?;
    }

    let mut settings = crate::settings::Settings::load();
    if let Some(t) = &config.theme {
        settings.theme = t.clone();
    }
    let mut app = App::new(me, welcome.game_id.0.clone(), welcome.format.name.clone(), welcome.lobby.clone()).with_settings(settings);
    app.hints = config.hints.clone();
    if let (Some(_), Some(text)) = (me, &config.decklist) {
        app.deck_source = Some(app::DeckSource {
            path: config.deck_path.clone(),
            text: text.clone(),
            format: welcome.format.clone(),
        });
    }
    for h in &config.hints {
        for line in h.lines() {
            app.push_log(LogKind::System, line.to_string());
        }
    }
    if let Some(state) = welcome.state {
        app.set_view(state);
        refresh_legal(&client, &mut app).await;
    }
    Ok(Session { client, pushes, app })
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<TerminalGuard> {
        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(std::io::stdout(), terminal::LeaveAlternateScreen);
    }
}

async fn event_loop(
    client: &AsyncClient,
    mut pushes: tokio::sync::mpsc::Receiver<protocol::ServerMessage>,
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(TermEvent::Key(key))) if key.kind != KeyEventKind::Release => {
                    let commands = app.handle_key(key);
                    for c in commands {
                        execute(client, app, c).await;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
            push = pushes.recv() => match push {
                Some(msg) => app.handle_push(msg),
                None => {
                    app.set_status("Connection to the game closed");
                    app.push_log(LogKind::System, "Connection to the game closed. Press q to quit.".into());
                    // Keep rendering so the user can read the final state.
                    while let Some(Ok(TermEvent::Key(key))) = events.next().await {
                        if key.kind != KeyEventKind::Release {
                            app.handle_key(key);
                            if app.quit {
                                return Ok(());
                            }
                            terminal.draw(|f| ui::draw(f, app))?;
                        }
                    }
                    return Ok(());
                }
            },
            _ = tick.tick() => {
                if let Some((_, at)) = &app.status {
                    if at.elapsed() > Duration::from_secs(6) {
                        app.status = None;
                    }
                }
            }
        }
        if app.needs_refresh {
            app.needs_refresh = false;
            refresh(client, app).await;
        }
        if app.auto_pass_due() {
            app.auto_pass_at = None;
            execute(client, app, Command::Act(engine::Action::PassPriority)).await;
        }
        if app.quit {
            return Ok(());
        }
    }
}

pub async fn refresh(client: &AsyncClient, app: &mut App) {
    match client.get_state().await {
        Ok(state) => app.set_view(state),
        Err(ClientError::Protocol(e)) if e.code == protocol::ErrorCode::BadRequest => return, // not started
        Err(e) => {
            app.set_status(format!("{e}"));
            return;
        }
    }
    refresh_legal(client, app).await;
}

async fn refresh_legal(client: &AsyncClient, app: &mut App) {
    if app.is_spectator() {
        return;
    }
    match client.get_legal_actions().await {
        Ok((legal, version, reason)) => app.set_legal(legal, version, reason),
        Err(e) => app.set_status(format!("{e}")),
    }
}

pub async fn execute(client: &AsyncClient, app: &mut App, command: Command) {
    match command {
        Command::Quit => app.quit = true,
        Command::SetDeck(text) => match client.set_deck(&text).await {
            Ok(Ok(())) => {
                if let Some(src) = app.deck_source.as_mut() {
                    src.text = text;
                }
                match client.ready().await {
                    Ok(()) => app.set_status("Deck resubmitted; you are ready"),
                    Err(e) => app.set_status(format!("deck accepted but ready failed: {e}")),
                }
            }
            Ok(Err(violations)) => {
                let list: Vec<String> = violations.iter().map(ToString::to_string).collect();
                app.set_status(format!("deck rejected: {}", list.join("; ")));
            }
            Err(e) => app.set_status(format!("set_deck failed: {e}")),
        },
        Command::Refresh => app.needs_refresh = true,
        Command::Chat(text) => {
            if let Err(e) = client.chat(&text, None).await {
                app.set_status(format!("chat failed: {e}"));
            }
        }
        Command::Act(action) => {
            let version = app.view.as_ref().map(|v| v.state_version).unwrap_or(app.legal_version);
            match client.act(action, version).await {
                Ok((_, state, legal)) => {
                    app.mode = Mode::Normal;
                    app.set_view(state);
                    let reason = app.my_reason();
                    let version = app.view.as_ref().map(|v| v.state_version).unwrap_or(0);
                    app.set_legal(legal, version, reason);
                }
                Err(ClientError::Protocol(e)) => {
                    app.set_status(e.message.clone());
                    if e.retryable {
                        app.needs_refresh = true;
                    }
                }
                Err(e) => app.set_status(format!("{e}")),
            }
        }
    }
}

/// Build a `TuiConfig` from the usual CLI pieces.
pub fn config(endpoint: &str, token: &str, name: &str, decklist: Option<String>) -> Result<TuiConfig> {
    Ok(TuiConfig {
        endpoint: Endpoint::parse(endpoint).map_err(|e| anyhow!(e))?,
        token: Token(token.to_string()),
        name: name.to_string(),
        decklist,
        hints: Vec::new(),
        deck_path: None,
        theme: None,
    })
}

/// Validate a `--theme` flag.
pub fn theme_flag(name: Option<&str>) -> Result<Option<String>> {
    match name {
        None => Ok(None),
        Some(n) if theme::Theme::named(n).is_some() => Ok(Some(n.to_string())),
        Some(n) => bail!("unknown theme {n:?}; themes are {}", theme::NAMES.join(", ")),
    }
}

/// Step through a replay in the client (`manaline replay --step`).
pub async fn run_replay(replay: app::ReplayState, theme: Option<String>) -> Result<()> {
    let mut settings = settings::Settings::load();
    if let Some(t) = theme {
        settings.theme = t;
    }
    let lobby = protocol::LobbyView {
        seats: Vec::new(),
        started: true,
    };
    let mut app = App::new(None, replay.title.clone(), String::new(), lobby).with_settings(settings);
    app.load_replay(replay);
    let guard = TerminalGuard::enter()?;
    let mut terminal = ratatui::init();
    let result = replay_loop(&mut app, &mut terminal).await;
    ratatui::restore();
    drop(guard);
    result
}

async fn replay_loop(app: &mut App, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(700));
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(TermEvent::Key(key))) if key.kind != KeyEventKind::Release => {
                    let _ = app.handle_key(key);
                    if app.quit {
                        return Ok(());
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
            _ = tick.tick() => app.replay_tick(),
        }
    }
}

/// Run the deckbuilder on its own (`manaline deck edit`).
pub async fn run_editor(setup: editor::EditorSetup) -> Result<()> {
    let mut ed = editor::Editor::new(setup).map_err(|e| anyhow!(e))?;
    // Announce the open file so an MCP server can find what the human is editing.
    let session = ed
        .path
        .clone()
        .and_then(|p| protocol::endpoint::EditorSession::announce(&p, &ed.format.name).ok());
    let guard = TerminalGuard::enter()?;
    let mut terminal = ratatui::init();
    let result = editor_loop(&mut ed, &mut terminal).await;
    ratatui::restore();
    drop(guard);
    if let Some(session) = session {
        session.withdraw();
    }
    result
}

async fn editor_loop(ed: &mut editor::Editor, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        terminal.draw(|f| editor_ui::draw(f, ed))?;
        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(TermEvent::Key(key))) if key.kind != KeyEventKind::Release => {
                    for c in ed.handle_key(key) {
                        if c == editor::EditorCommand::Quit {
                            return Ok(());
                        }
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
                None => return Ok(()),
            },
            _ = tick.tick() => ed.check_disk(),
        }
    }
}
