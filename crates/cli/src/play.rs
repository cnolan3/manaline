//! `manaline play`: the whole user experience in one command (§2.1). Spawns
//! the daemon, decides who sits where, publishes the table so agents can find
//! it, opens the TUI (or follows the game in the terminal when no seat is
//! yours), and tears everything down. Also `join` and `tui`, the pieces `play`
//! is made of.

use crate::bot::{self, BotSettings};
use anyhow::{anyhow, bail, Context, Result};
use daemon::StartupInfo;
use engine::Format;
use protocol::endpoint::{GameMarker, Runtime, SeatKind, SeatSlot};
use protocol::{Endpoint, Token};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(clap::Args)]
pub struct PlayArgs {
    /// Your deck: a file path or a built-in deck name (see `list decks`). Required
    /// when a seat is yours.
    #[arg(long)]
    pub deck: Option<String>,
    /// Who sits where, one entry per seat in seat order: `me` (this terminal),
    /// `human` (another terminal), `random` (the built-in bot), or an agent seat:
    /// `claude`, `codex`, or `mcp`. Defaults to `me,<--vs>`.
    #[arg(long, conflicts_with = "vs")]
    pub seats: Option<String>,
    /// Shorthand for `--seats me,<vs>`: `random`, `human`, `claude`, `codex`, or `mcp`.
    #[arg(long)]
    pub vs: Option<String>,
    /// Seat 1's deck (defaults to a built-in deck that differs from yours). `agent`
    /// leaves an agent's seat empty so it chooses one of the existing decks itself.
    #[arg(long)]
    pub opp_deck: Option<String>,
    /// A deck for any seat: `--seat-deck 2=green`. Repeatable. `agent` leaves an
    /// agent's seat empty so it chooses a deck itself.
    #[arg(long = "seat-deck", value_name = "N=DECK")]
    pub seat_decks: Vec<String>,
    #[arg(long, default_value = "cube")]
    pub format: String,
    #[arg(long)]
    pub seed: Option<u64>,
    /// Your display name at the table.
    #[arg(long)]
    pub name: Option<String>,
    /// Colour theme for this run: default, mono, or high-contrast (saved choice otherwise).
    #[arg(long)]
    pub theme: Option<String>,
    /// Also listen on TCP so a remote terminal can join (a `human` seat).
    #[arg(long)]
    pub tcp: Option<String>,
    /// With no `me` seat, watch the table in the terminal client instead of
    /// printing one line per event.
    #[arg(long)]
    pub watch: bool,
}

/// Who sits in one seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatRole {
    /// This terminal's human: the TUI runs on this seat.
    Me,
    /// Another terminal, joining with `manaline join`.
    Human,
    /// The built-in random bot, run in this process.
    Random,
    /// An MCP session that claims the seat itself.
    Agent(AgentKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    Claude,
    Codex,
    Generic,
}

impl AgentKind {
    fn name(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::Generic => "Agent",
        }
    }
}

impl SeatRole {
    fn kind(self) -> SeatKind {
        match self {
            SeatRole::Me | SeatRole::Human => SeatKind::Human,
            SeatRole::Random => SeatKind::Bot,
            SeatRole::Agent(_) => SeatKind::Agent,
        }
    }

    fn is_agent(self) -> bool {
        matches!(self, SeatRole::Agent(_))
    }

    fn word(self) -> &'static str {
        match self {
            SeatRole::Me => "me",
            SeatRole::Human => "human",
            SeatRole::Random => "random",
            SeatRole::Agent(AgentKind::Claude) => "claude",
            SeatRole::Agent(AgentKind::Codex) => "codex",
            SeatRole::Agent(AgentKind::Generic) => "mcp",
        }
    }
}

/// One seat, fully decided: who sits there, what they are called, and the deck
/// `play` assigned them (`None` for an agent that picks its own).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatPlan {
    pub seat: u8,
    pub role: SeatRole,
    pub name: String,
    pub deck: Option<String>,
}

/// Parse `--seats`: one word per seat, in seat order.
pub fn parse_seats(spec: &str) -> Result<Vec<SeatRole>> {
    let mut roles = Vec::new();
    for word in spec.split(',') {
        let w = word.trim();
        if w.is_empty() {
            bail!("empty seat in {spec:?}; seats are a comma list like me,random");
        }
        roles.push(match w.to_ascii_lowercase().as_str() {
            "me" => SeatRole::Me,
            "human" => SeatRole::Human,
            "random" | "bot" => SeatRole::Random,
            "claude" => SeatRole::Agent(AgentKind::Claude),
            "codex" => SeatRole::Agent(AgentKind::Codex),
            "mcp" | "agent" => SeatRole::Agent(AgentKind::Generic),
            other => bail!("unknown seat {other:?}; use me, human, random, claude, codex, or mcp"),
        });
    }
    if roles.len() < 2 {
        bail!("a game needs at least two seats; got {}", roles.len());
    }
    if roles.len() > u8::MAX as usize {
        bail!("at most {} seats", u8::MAX);
    }
    if roles.iter().filter(|r| **r == SeatRole::Me).count() > 1 {
        bail!("only one seat can be `me`");
    }
    Ok(roles)
}

/// Display names, unique per seat: a bare name when only one seat wants it,
/// otherwise suffixed with the seat number.
pub fn seat_names(roles: &[SeatRole], me: &str) -> Vec<String> {
    let base = |r: SeatRole| -> String {
        match r {
            SeatRole::Me => me.to_string(),
            SeatRole::Human => "Player".into(),
            SeatRole::Random => "Bot".into(),
            SeatRole::Agent(k) => k.name().into(),
        }
    };
    roles
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let name = base(*r);
            let shared = roles.iter().enumerate().any(|(j, o)| j != i && base(*o) == name);
            if shared {
                format!("{name} {}", i + 1)
            } else {
                name
            }
        })
        .collect()
}

/// What the deck flags said, in one place so the assignment rules are testable.
pub struct DeckFlags<'a> {
    /// `--deck`: the `me` seat's deck.
    pub mine: Option<&'a str>,
    /// `--opp-deck`: seat 1's deck.
    pub opp: Option<&'a str>,
    /// `--seat-deck N=DECK`, already parsed.
    pub per_seat: &'a [(u8, String)],
    /// The deck bots and other terminals get when nothing named one.
    pub fallback: &'a str,
}

/// Which deck each seat plays. `None` means "nothing assigned": an agent seat
/// picks one itself. Returns, per seat, the deck and whether a flag named it.
pub fn assign_decks(roles: &[SeatRole], flags: &DeckFlags<'_>) -> Result<Vec<(Option<String>, bool)>> {
    for (n, _) in flags.per_seat {
        if *n as usize >= roles.len() {
            bail!("--seat-deck {n}=… but the table has {} seats (0..{})", roles.len(), roles.len() - 1);
        }
    }
    let mut out = Vec::with_capacity(roles.len());
    for (i, role) in roles.iter().enumerate() {
        let seat = i as u8;
        let explicit = flags.per_seat.iter().rev().find(|(n, _)| *n == seat).map(|(_, d)| d.as_str());
        if explicit.is_some() {
            if *role == SeatRole::Me && flags.mine.is_some() {
                bail!("--deck and --seat-deck {seat}=… both set the deck for your own seat");
            }
            if seat == 1 && flags.opp.is_some() {
                bail!("--opp-deck and --seat-deck 1=… both set seat 1's deck");
            }
        }
        let named = match (role, explicit) {
            (_, Some(d)) => Some(d),
            (SeatRole::Me, None) => flags.mine,
            (_, None) if seat == 1 => flags.opp,
            _ => None,
        };
        let chosen = match named {
            // `agent` (or `-`) leaves an agent's seat empty: it picks a deck itself.
            Some("agent") | Some("-") if role.is_agent() => None,
            Some("agent") | Some("-") => bail!("seat {seat} is {:?}, not an agent; it needs a real deck", role.word()),
            Some(d) => Some(d.to_string()),
            None if role.is_agent() => None,
            None if *role == SeatRole::Me => bail!("pass --deck: seat {seat} is yours"),
            None => Some(flags.fallback.to_string()),
        };
        out.push((chosen, named.is_some()));
    }
    Ok(out)
}

/// Parse one `--seat-deck N=DECK`.
fn parse_seat_deck(arg: &str) -> Result<(u8, String)> {
    let (n, deck) = arg
        .split_once('=')
        .ok_or_else(|| anyhow!("--seat-deck wants N=DECK, as in `--seat-deck 2=green`; got {arg:?}"))?;
    let n: u8 = n.trim().parse().with_context(|| format!("--seat-deck seat number in {arg:?}"))?;
    if deck.trim().is_empty() {
        bail!("--seat-deck {n}= has no deck");
    }
    Ok((n, deck.trim().to_string()))
}

/// Who sits where: `--seats`, or `me,<--vs>` (and `--vs` defaults to `random`).
pub fn seat_roles(args: &PlayArgs) -> Result<Vec<SeatRole>> {
    let spec = match (&args.seats, &args.vs) {
        (Some(s), _) => s.clone(),
        (None, vs) => format!("me,{}", vs.as_deref().unwrap_or("random")),
    };
    parse_seats(&spec)
}

/// The table `play` will set up, from the flags alone (no daemon involved).
pub fn plan_table(args: &PlayArgs, me: &str) -> Result<Vec<SeatPlan>> {
    let roles = seat_roles(args)?;
    let per_seat: Vec<(u8, String)> = args.seat_decks.iter().map(|s| parse_seat_deck(s)).collect::<Result<_>>()?;
    let fallback = default_opponent_deck(args.deck.as_deref().unwrap_or(""));
    let decks = assign_decks(
        &roles,
        &DeckFlags {
            mine: args.deck.as_deref(),
            opp: args.opp_deck.as_deref(),
            per_seat: &per_seat,
            fallback: &fallback,
        },
    )?;
    let names = seat_names(&roles, me);
    Ok(roles
        .iter()
        .enumerate()
        .map(|(i, role)| SeatPlan {
            seat: i as u8,
            role: *role,
            name: names[i].clone(),
            deck: decks[i].0.clone(),
        })
        .collect())
}

/// The marker `play` publishes so agents can find this table and claim a seat.
pub fn build_marker(info: &StartupInfo, format: &str, plans: &[SeatPlan]) -> Result<GameMarker> {
    let game_id = info.game_id.clone().ok_or_else(|| anyhow!("the daemon created no game"))?;
    if info.seat_tokens.len() < plans.len() {
        bail!("daemon reported {} seat tokens for {} seats", info.seat_tokens.len(), plans.len());
    }
    let seats = plans
        .iter()
        .map(|p| SeatSlot {
            seat: p.seat,
            kind: p.role.kind(),
            name: p.name.clone(),
            deck: p.deck.clone(),
            // Only an agent seat publishes its token; humans and bots are seated from here.
            token: p.role.is_agent().then(|| info.seat_tokens[p.seat as usize].clone()),
        })
        .collect();
    Ok(GameMarker {
        game_id: game_id.0,
        pid: std::process::id(),
        socket: info.socket.clone(),
        tcp: info.tcp.map(|a| a.to_string()),
        format: format.to_string(),
        spectator_token: info.spectator_token.clone(),
        seats,
        dir: std::path::PathBuf::new(),
    })
}

/// A published game, withdrawn however `play` exits.
struct Published(GameMarker);

impl Published {
    fn new(runtime: &Runtime, marker: &GameMarker) -> Result<Published> {
        let published = runtime
            .publish_game(marker)
            .with_context(|| format!("publishing the game in {}", runtime.games_dir().display()))?;
        Ok(Published(published))
    }
}

impl Drop for Published {
    fn drop(&mut self) {
        self.0.withdraw();
    }
}

pub async fn play(args: PlayArgs) -> Result<()> {
    play_in(args, Runtime::default()).await
}

/// `play` against an explicit runtime directory, so tests can publish into one
/// of their own.
pub async fn play_in(args: PlayArgs, runtime: Runtime) -> Result<()> {
    let db = cards::core();
    let format = Format::builtin(&args.format).ok_or_else(|| anyhow!("unknown format {:?}", args.format))?;
    let name = args.name.clone().unwrap_or_else(whoami);
    if args.watch && seat_roles(&args)?.contains(&SeatRole::Me) {
        bail!("--watch watches a table you are not sitting at; drop `me` from --seats");
    }
    let plans = plan_table(&args, &name)?;
    let me = plans.iter().find(|p| p.role == SeatRole::Me).map(|p| p.seat);

    // Every deck `play` assigned is checked before anything starts, except a
    // fallback handed to another terminal (which brings its own).
    let mut decklists: Vec<Option<String>> = Vec::with_capacity(plans.len());
    for plan in &plans {
        let Some(deck) = &plan.deck else {
            decklists.push(None);
            continue;
        };
        let text = crate::deck_text(deck)?;
        if plan.role != SeatRole::Human {
            check_deck(&text, &format, &db, deck)?;
        }
        decklists.push(Some(text));
    }

    let mut daemon = spawn_daemon(&args.format, plans.len() as u8, args.seed, args.tcp.as_deref()).await?;
    let info = daemon.info.clone();
    let socket = info.socket.clone().ok_or_else(|| anyhow!("daemon reported no socket"))?;
    let endpoint = Endpoint::Unix(socket.clone());
    let tokens = info.seat_tokens.clone();

    // Published before anyone is seated, so an agent that is already waiting
    // finds the table as soon as the daemon is up.
    let marker = build_marker(&info, &args.format, &plans)?;
    let published = Published::new(&runtime, &marker)?;

    // Bots play in this process, one task per `random` seat.
    let mut bot_tasks = Vec::new();
    for plan in plans.iter().filter(|p| p.role == SeatRole::Random) {
        let settings = BotSettings {
            endpoint: endpoint.clone(),
            token: tokens[plan.seat as usize].clone(),
            name: plan.name.clone(),
            decklist: decklists[plan.seat as usize].clone().expect("a bot seat always has a deck"),
            seed: rand::random(),
        };
        bot_tasks.push(tokio::spawn(bot::run(settings)));
    }

    let mut hints = human_hints(&plans, &socket, &tokens, info.tcp.as_ref());
    hints.extend(agent_hints(&published.0, &plans));

    let result = match me {
        Some(seat) => {
            let config = tui::TuiConfig {
                endpoint,
                token: tokens[seat as usize].clone(),
                name,
                decklist: decklists[seat as usize].clone(),
                hints,
                deck_path: args.deck.as_deref().and_then(deck_path_of),
                theme: tui::theme_flag(args.theme.as_deref())?,
            };
            tui::run(config).await
        }
        None => {
            let token = info
                .spectator_token
                .clone()
                .ok_or_else(|| anyhow!("the daemon issued no spectator token"))?;
            if args.watch {
                let config = tui::TuiConfig {
                    endpoint,
                    token,
                    name,
                    decklist: None,
                    hints,
                    deck_path: None,
                    theme: tui::theme_flag(args.theme.as_deref())?,
                };
                tui::run(config).await
            } else {
                for h in &hints {
                    println!("{h}");
                }
                follow(&endpoint, &token, &plans).await
            }
        }
    };

    for t in bot_tasks {
        t.abort();
    }
    drop(published);
    daemon.stop().await;

    let outcome = result?;
    if let Some(o) = outcome {
        println!("{}", describe_outcome(o, me));
    }
    if let Some(p) = &info.replay_path {
        if p.exists() {
            println!("Replay saved to {}", p.display());
        }
    }
    Ok(())
}

/// How another terminal joins each `human` seat.
fn human_hints(plans: &[SeatPlan], socket: &std::path::Path, tokens: &[Token], tcp: Option<&std::net::SocketAddr>) -> Vec<String> {
    let seats: Vec<&SeatPlan> = plans.iter().filter(|p| p.role == SeatRole::Human).collect();
    if seats.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![match seats.len() {
        1 => "To seat the other player, run this in another terminal:".to_string(),
        n => format!("To seat the other {n} players, run one of these in another terminal each:"),
    }];
    for p in &seats {
        lines.push(format!(
            "  manaline join {} --token {} --deck <their deck>",
            socket.display(),
            tokens[p.seat as usize]
        ));
    }
    if let Some(addr) = tcp {
        lines.push("or from another machine on the same network:".to_string());
        for p in &seats {
            lines.push(format!(
                "  manaline join {addr} --token {} --deck <their deck>",
                tokens[p.seat as usize]
            ));
        }
    }
    vec![lines.join("\n")]
}

/// The one thing the human needs to know: the table is published, and an agent
/// session finds it by itself. Nobody starts or stops an MCP server by hand.
fn agent_hints(marker: &GameMarker, plans: &[SeatPlan]) -> Vec<String> {
    let agents: Vec<&SeatPlan> = plans.iter().filter(|p| p.role.is_agent()).collect();
    if agents.is_empty() {
        return Vec::new();
    }
    let who: Vec<String> = agents.iter().map(|p| format!("seat {} ({})", p.seat, p.name)).collect();
    let mut lines = vec![
        format!("Table {} is published for agents: {}.", marker.game_id, who.join(", ")),
        "Point your agent at the manaline MCP server; it finds this table and takes the next free".into(),
        "agent seat the first time it calls a game tool.".into(),
    ];
    if agents.iter().any(|p| p.role == SeatRole::Agent(AgentKind::Claude)) {
        lines.push("  Claude Code:  claude mcp add manaline -- manaline mcp --stdio".into());
    }
    if agents.iter().any(|p| p.role == SeatRole::Agent(AgentKind::Codex)) {
        lines.push("  Codex:  codex mcp add manaline -- manaline mcp --stdio".into());
    }
    lines.push("  Config snippet:  {\"mcpServers\":{\"manaline\":{\"command\":\"manaline\",\"args\":[\"mcp\",\"--stdio\"]}}}".into());
    if agents.len() > 1 {
        lines.push(format!(
            "There are {n} agent seats, so {n} separate agent sessions are needed — one each.",
            n = agents.len()
        ));
    }
    let undecided: Vec<String> = agents.iter().filter(|p| p.deck.is_none()).map(|p| p.seat.to_string()).collect();
    if !undecided.is_empty() {
        lines.push(format!(
            "Seat{} {} ha{} no deck: the agent must pick one with list_decks and submit_deck before the game starts.",
            if undecided.len() == 1 { "" } else { "s" },
            undecided.join(", "),
            if undecided.len() == 1 { "s" } else { "ve" }
        ));
    }
    lines.push("Then tell it: \"You're playing Magic against me. Pull the play-a-game prompt from the manaline server and go.\"".into());
    vec![lines.join("\n")]
}

/// Watch a table nobody in this terminal is sitting at: connect as a spectator
/// and print one line per thing worth knowing. Returns the outcome if the game
/// ended while we watched.
async fn follow(endpoint: &Endpoint, token: &Token, plans: &[SeatPlan]) -> Result<Option<engine::Outcome>> {
    use protocol::ServerMessage;
    let (client, mut pushes) = protocol::async_client::connect(endpoint)
        .await
        .with_context(|| format!("connecting to {endpoint}"))?;
    let welcome = client.hello(token, None).await?;
    client.subscribe().await?;
    println!("Watching {} ({}). Ctrl-C stops the table.", welcome.game_id, welcome.format.name);

    let mut lobby = protocol::LobbyView::default();
    report_lobby(&mut lobby, &welcome.lobby, plans);
    let mut seen: Option<Turn> = None;
    if let Some(state) = &welcome.state {
        report_state(&mut seen, state, plans);
        if let Some(o) = state.outcome {
            return Ok(Some(o));
        }
    }
    loop {
        let msg = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("Stopping the table.");
                return Ok(None);
            }
            push = pushes.recv() => match push {
                Some(m) => m,
                None => {
                    println!("The game connection closed.");
                    return Ok(None);
                }
            },
        };
        // Collapse a burst of events into one state report.
        let mut batch = vec![msg];
        while let Ok(m) = pushes.try_recv() {
            batch.push(m);
        }
        let mut refresh = false;
        for m in batch {
            match m {
                ServerMessage::Lobby { lobby: next } => report_lobby(&mut lobby, &next, plans),
                ServerMessage::Event {
                    event: engine::EventBase::GameOver { outcome },
                    ..
                } => return Ok(Some(outcome)),
                ServerMessage::Event { .. } | ServerMessage::State { .. } => refresh = true,
                _ => {}
            }
        }
        if refresh {
            match client.get_state().await {
                Ok(state) => {
                    report_state(&mut seen, &state, plans);
                    if let Some(o) = state.outcome {
                        return Ok(Some(o));
                    }
                }
                // Not started yet; the lobby pushes say when it is.
                Err(protocol::ClientError::Protocol(e)) if e.code == protocol::ErrorCode::BadRequest => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// What the follower last said about the game, so it only speaks on a change.
/// Priority passing is left out: it changes constantly and says nothing.
#[derive(PartialEq, Eq)]
struct Turn {
    turn: u32,
    active: engine::Seat,
    waiting: Vec<(engine::Seat, engine::ActReason)>,
}

fn seat_label(plans: &[SeatPlan], seat: engine::Seat) -> String {
    match plans.get(seat.index()) {
        Some(p) => format!("{} (seat {})", p.name, p.seat),
        None => format!("seat {}", seat.0),
    }
}

fn report_lobby(last: &mut protocol::LobbyView, next: &protocol::LobbyView, plans: &[SeatPlan]) {
    for seat in &next.seats {
        let before = last.seats.iter().find(|s| s.seat == seat.seat);
        let who = seat.name.clone().unwrap_or_else(|| seat_label(plans, seat.seat));
        match before {
            None if seat.connected => println!("{who} joined."),
            Some(b) if !b.connected && seat.connected => println!("{who} joined."),
            Some(b) if b.connected && !seat.connected => println!("{who} left."),
            _ => {}
        }
        if seat.ready && !before.is_some_and(|b| b.ready) {
            println!("{who} is ready.");
        }
    }
    if next.started && !last.started {
        println!("The game has started.");
    }
    *last = next.clone();
}

fn report_state(last: &mut Option<Turn>, state: &engine::GameView, plans: &[SeatPlan]) {
    let now = Turn {
        turn: state.turn,
        active: state.active_player,
        waiting: state
            .must_act
            .iter()
            .filter(|(_, r)| **r != engine::ActReason::Priority)
            .map(|(s, r)| (*s, *r))
            .collect(),
    };
    if last.as_ref() == Some(&now) {
        return;
    }
    if last.as_ref().is_none_or(|l| l.turn != now.turn || l.active != now.active) {
        println!("Turn {}: {}.", now.turn, seat_label(plans, now.active));
    }
    if last.as_ref().is_none_or(|l| l.waiting != now.waiting) {
        for (seat, reason) in &now.waiting {
            let what = match reason {
                engine::ActReason::Priority => continue,
                engine::ActReason::DeclareAttackers => "is declaring attackers".to_string(),
                engine::ActReason::DeclareBlockers => "is declaring blockers".into(),
                engine::ActReason::AssignDamage => "is assigning combat damage".into(),
                engine::ActReason::Mulligan => "is deciding on a mulligan".into(),
                engine::ActReason::BottomCards => "is putting cards on the bottom".into(),
                engine::ActReason::Discard => "is discarding to hand size".into(),
                engine::ActReason::Choice => match &state.prompt {
                    Some(p) => format!("must decide: {p}"),
                    None => "must decide".into(),
                },
            };
            println!("  {} {what}.", seat_label(plans, *seat));
        }
    }
    *last = Some(now);
}

fn describe_outcome(o: engine::Outcome, me: Option<u8>) -> String {
    match o {
        engine::Outcome::Winner(s) if Some(s.0) == me => "You won.".into(),
        engine::Outcome::Winner(s) => format!("Seat {} won.", s.0),
        engine::Outcome::Draw => "The game was a draw.".into(),
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "You".into())
}

fn default_opponent_deck(mine: &str) -> String {
    cards::deck_names()
        .into_iter()
        .rev()
        .find(|name| name != mine)
        .unwrap_or_else(|| "red".into())
}

/// Where the lobby's deckbuilder saves this seat's deck: a file path as
/// given is edited in place; a deck named by name saves to your own copy in
/// the user decks directory (created on save), so a shipped deck is never
/// edited where it was installed.
pub fn deck_path_of(spec: &str) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(spec);
    if p.is_file() {
        return Some(p);
    }
    cards::user_deck_path(spec)
}

fn check_deck(decklist: &str, format: &Format, db: &engine::CardDb, label: &str) -> Result<()> {
    let report = crate::deck::check_text(decklist, format, db, None)?;
    if !report.is_legal() {
        bail!("{}", crate::deck::render_check(&report, label, format).trim_end());
    }
    Ok(())
}

/// A daemon child process, killed on drop.
pub struct DaemonChild {
    child: tokio::process::Child,
    pub info: StartupInfo,
}

impl DaemonChild {
    /// Ask the daemon to stop (it removes its socket on the way out), then
    /// kill it if it lingers.
    pub async fn stop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            if tokio::time::timeout(std::time::Duration::from_secs(2), self.child.wait())
                .await
                .is_ok()
            {
                return;
            }
        }
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// Spawn `manaline daemon` as a child on a fresh socket and read its startup line.
pub async fn spawn_daemon(format: &str, seats: u8, seed: Option<u64>, tcp: Option<&str>) -> Result<DaemonChild> {
    let exe = std::env::current_exe().context("locating the manaline binary")?;
    let log_dir = protocol::endpoint::data_dir().join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let log_path = log_dir.join(format!("daemon-{}.log", std::process::id()));
    let log_file = std::fs::File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;

    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("daemon")
        .arg("--format")
        .arg(format)
        .arg("--seats")
        .arg(seats.to_string())
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true);
    if let Some(s) = seed {
        cmd.arg("--seed").arg(s.to_string());
    }
    if let Some(addr) = tcp {
        cmd.arg("--tcp").arg(addr);
    }
    let mut child = cmd.spawn().context("starting the game daemon")?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .context("the game daemon did not start in time")?
        .context("reading from the game daemon")?
        .ok_or_else(|| anyhow!("the game daemon exited before it was ready (see {})", log_path.display()))?;
    let info: StartupInfo = serde_json::from_str(&first).with_context(|| format!("the game daemon said something unexpected: {first}"))?;
    // Keep draining stdout so the child never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    Ok(DaemonChild { child, info })
}

#[derive(clap::Args)]
pub struct JoinArgs {
    /// Socket path or host:port printed by a `human` seat in `play`, or by `host`.
    pub endpoint: String,
    #[arg(long)]
    pub token: String,
    /// Your deck: a file path or a built-in deck name.
    #[arg(long)]
    pub deck: String,
    #[arg(long)]
    pub name: Option<String>,
    /// Colour theme for this run: default, mono, or high-contrast (saved choice otherwise).
    #[arg(long)]
    pub theme: Option<String>,
}

pub async fn join(args: JoinArgs) -> Result<()> {
    let decklist = crate::deck_text(&args.deck)?;
    let name = args.name.unwrap_or_else(whoami);
    let mut config = tui::config(&args.endpoint, &args.token, &name, Some(decklist))?;
    config.deck_path = deck_path_of(&args.deck);
    config.theme = tui::theme_flag(args.theme.as_deref())?;
    let outcome = tui::run(config).await?;
    if let Some(o) = outcome {
        println!(
            "{}",
            match o {
                engine::Outcome::Winner(s) => format!("Seat {} won.", s.0),
                engine::Outcome::Draw => "The game was a draw.".into(),
            }
        );
    }
    Ok(())
}

#[derive(clap::Args)]
pub struct TuiArgs {
    /// Socket path or host:port of the daemon.
    #[arg(long, conflicts_with = "game")]
    pub connect: Option<String>,
    /// A game id; connects to that game's socket in the runtime directory.
    #[arg(long)]
    pub game: Option<String>,
    /// A seat or spectator token.
    #[arg(long)]
    pub token: String,
    /// Deck to submit if the game has not started (seats only).
    #[arg(long)]
    pub deck: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    /// Colour theme for this run: default, mono, or high-contrast (saved choice otherwise).
    #[arg(long)]
    pub theme: Option<String>,
}

pub async fn tui(args: TuiArgs) -> Result<()> {
    let endpoint = match (&args.connect, &args.game) {
        (Some(c), _) => c.clone(),
        (None, Some(g)) => protocol::endpoint::socket_path(g).display().to_string(),
        (None, None) => bail!("pass --connect <endpoint> or --game <id>"),
    };
    let decklist = match &args.deck {
        Some(d) => Some(crate::deck_text(d)?),
        None => None,
    };
    let name = args.name.unwrap_or_else(whoami);
    let mut config = tui::config(&endpoint, &args.token, &name, decklist)?;
    config.deck_path = args.deck.as_deref().and_then(deck_path_of);
    config.theme = tui::theme_flag(args.theme.as_deref())?;
    tui::run(config).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::GameId;

    fn flags<'a>(mine: Option<&'a str>, opp: Option<&'a str>, per_seat: &'a [(u8, String)]) -> DeckFlags<'a> {
        DeckFlags {
            mine,
            opp,
            per_seat,
            fallback: "fallback",
        }
    }

    fn args() -> PlayArgs {
        PlayArgs {
            deck: Some("mine".into()),
            seats: None,
            vs: None,
            opp_deck: None,
            seat_decks: Vec::new(),
            format: "cube".into(),
            seed: None,
            name: None,
            theme: None,
            tcp: None,
            watch: false,
        }
    }

    #[test]
    fn parses_seat_specs() {
        use AgentKind::*;
        use SeatRole::*;
        assert_eq!(parse_seats("me,random").unwrap(), vec![Me, Random]);
        assert_eq!(parse_seats(" ME , Human ").unwrap(), vec![Me, Human]);
        assert_eq!(
            parse_seats("me,claude,codex,mcp").unwrap(),
            vec![Me, Agent(Claude), Agent(Codex), Agent(Generic)]
        );
        assert_eq!(parse_seats("random,random,random").unwrap(), vec![Random, Random, Random]);
        assert!(parse_seats("me").unwrap_err().to_string().contains("at least two"));
        assert!(parse_seats("me,me").unwrap_err().to_string().contains("only one seat"));
        assert!(parse_seats("me,").unwrap_err().to_string().contains("empty seat"));
        assert!(parse_seats("me,wizard").unwrap_err().to_string().contains("unknown seat"));
    }

    #[test]
    fn vs_is_shorthand_for_a_two_seat_spec() {
        let plans = plan_table(&args(), "Connor").unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].role, SeatRole::Me);
        assert_eq!(plans[1].role, SeatRole::Random);
        assert_eq!(plans[0].deck.as_deref(), Some("mine"));
        assert_eq!(plans[0].name, "Connor");
        assert_eq!(plans[1].seat, 1);

        let claude = PlayArgs {
            vs: Some("claude".into()),
            ..args()
        };
        let plans = plan_table(&claude, "Connor").unwrap();
        assert_eq!(plans[1].role, SeatRole::Agent(AgentKind::Claude));
        assert_eq!(plans[1].name, "Claude");
        // Nothing named the agent's deck, so it picks one itself.
        assert_eq!(plans[1].deck, None);

        // A headless table: no `me`, so nothing opens the TUI.
        let headless = PlayArgs {
            deck: None,
            seats: Some("random,claude,mcp".into()),
            ..args()
        };
        let plans = plan_table(&headless, "Connor").unwrap();
        assert_eq!(plans.len(), 3);
        assert!(plans.iter().all(|p| p.role != SeatRole::Me));
        assert!(plans[0].deck.is_some(), "the bot gets the fallback deck");
    }

    #[test]
    fn names_stay_unique() {
        use SeatRole::*;
        assert_eq!(seat_names(&[Me, Random], "Connor"), vec!["Connor", "Bot"]);
        assert_eq!(seat_names(&[Me, Random, Random], "Connor"), vec!["Connor", "Bot 2", "Bot 3"]);
        assert_eq!(
            seat_names(&[Agent(AgentKind::Claude), Agent(AgentKind::Claude), Human], "Connor"),
            vec!["Claude 1", "Claude 2", "Player"]
        );
    }

    #[test]
    fn decks_follow_the_flags() {
        use SeatRole::*;
        let claude = Agent(AgentKind::Claude);

        // `--deck` is mine; seat 1 falls back; an agent with no deck picks its own.
        let decks = assign_decks(&[Me, Random], &flags(Some("mine"), None, &[])).unwrap();
        assert_eq!(decks[0].0.as_deref(), Some("mine"));
        assert_eq!(decks[1].0.as_deref(), Some("fallback"));
        let decks = assign_decks(&[Me, claude], &flags(Some("mine"), None, &[])).unwrap();
        assert_eq!(decks[1].0, None);

        // `--opp-deck` is seat 1's, whoever sits there; `agent` empties an agent's seat.
        let decks = assign_decks(&[Me, claude], &flags(Some("mine"), Some("red"), &[])).unwrap();
        assert_eq!(decks[1], (Some("red".into()), true));
        let decks = assign_decks(&[Me, claude], &flags(Some("mine"), Some("agent"), &[])).unwrap();
        assert_eq!(decks[1], (None, true));
        assert!(assign_decks(&[Me, Random], &flags(Some("mine"), Some("agent"), &[]))
            .unwrap_err()
            .to_string()
            .contains("not an agent"));

        // `--seat-deck` reaches any seat.
        let per = [(2u8, "green".to_string()), (3u8, "agent".to_string())];
        let decks = assign_decks(&[Me, Random, claude, claude], &flags(Some("mine"), None, &per)).unwrap();
        assert_eq!(decks[1].0.as_deref(), Some("fallback"));
        assert_eq!(decks[2].0.as_deref(), Some("green"));
        assert_eq!(decks[3].0, None);

        // Two flags for one seat, and a seat that does not exist, are mistakes.
        let per = [(1u8, "green".to_string())];
        assert!(assign_decks(&[Me, Random], &flags(Some("mine"), Some("red"), &per))
            .unwrap_err()
            .to_string()
            .contains("--opp-deck and --seat-deck 1="));
        let per = [(0u8, "green".to_string())];
        assert!(assign_decks(&[Me, Random], &flags(Some("mine"), None, &per))
            .unwrap_err()
            .to_string()
            .contains("--deck and --seat-deck 0="));
        let per = [(7u8, "green".to_string())];
        assert!(assign_decks(&[Me, Random], &flags(Some("mine"), None, &per))
            .unwrap_err()
            .to_string()
            .contains("the table has 2 seats"));

        // A seat of mine with no `--deck` cannot be seated.
        assert!(assign_decks(&[Me, Random], &flags(None, None, &[]))
            .unwrap_err()
            .to_string()
            .contains("pass --deck"));
        // A headless table needs no `--deck` at all.
        let decks = assign_decks(&[Random, claude], &flags(None, None, &[])).unwrap();
        assert_eq!(decks[0].0.as_deref(), Some("fallback"));
        assert_eq!(decks[1].0, None);
    }

    #[test]
    fn parses_seat_deck_flags() {
        assert_eq!(parse_seat_deck("2=green").unwrap(), (2, "green".to_string()));
        assert_eq!(parse_seat_deck(" 0 = my-deck.txt ").unwrap(), (0, "my-deck.txt".to_string()));
        assert!(parse_seat_deck("green").is_err());
        assert!(parse_seat_deck("x=green").is_err());
        assert!(parse_seat_deck("1=").is_err());
    }

    fn startup(seats: usize) -> StartupInfo {
        StartupInfo {
            socket: Some("/run/manaline/quiet-owl.sock".into()),
            tcp: Some("127.0.0.1:7455".parse().unwrap()),
            game_id: Some(GameId("quiet-owl".into())),
            seat_tokens: (0..seats).map(|i| Token(format!("tok{i}"))).collect(),
            spectator_token: Some(Token("spec".into())),
            replay_path: None,
        }
    }

    fn table(roles: &[SeatRole], decks: &[Option<&str>]) -> Vec<SeatPlan> {
        let names = seat_names(roles, "Connor");
        roles
            .iter()
            .enumerate()
            .map(|(i, r)| SeatPlan {
                seat: i as u8,
                role: *r,
                name: names[i].clone(),
                deck: decks[i].map(String::from),
            })
            .collect()
    }

    #[test]
    fn the_marker_describes_the_table() {
        use SeatRole::*;
        let plans = table(
            &[Me, Random, Agent(AgentKind::Claude), Agent(AgentKind::Codex)],
            &[Some("mine"), Some("red"), Some("green"), None],
        );
        let marker = build_marker(&startup(4), "cube", &plans).unwrap();
        assert_eq!(marker.game_id, "quiet-owl");
        assert_eq!(marker.pid, std::process::id());
        assert_eq!(marker.format, "cube");
        assert_eq!(marker.spectator_token, Some(Token("spec".into())));
        assert_eq!(marker.tcp.as_deref(), Some("127.0.0.1:7455"));
        assert_eq!(marker.endpoint(), Some(Endpoint::Unix("/run/manaline/quiet-owl.sock".into())));

        let kinds: Vec<SeatKind> = marker.seats.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SeatKind::Human, SeatKind::Bot, SeatKind::Agent, SeatKind::Agent]);
        // Only agent seats publish a token; that is how an agent joins.
        let tokens: Vec<Option<&Token>> = marker.seats.iter().map(|s| s.token.as_ref()).collect();
        assert_eq!(tokens, vec![None, None, Some(&Token("tok2".into())), Some(&Token("tok3".into()))]);
        assert_eq!(marker.seats[2].deck.as_deref(), Some("green"));
        assert_eq!(marker.seats[3].deck, None);
        assert_eq!(marker.seats[3].name, "Codex");
        assert_eq!(marker.seats.iter().map(|s| s.seat).collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_marker_needs_a_game_and_enough_tokens() {
        let plans = table(&[SeatRole::Me, SeatRole::Random], &[Some("mine"), Some("red")]);
        let mut info = startup(2);
        info.game_id = None;
        assert!(build_marker(&info, "cube", &plans).unwrap_err().to_string().contains("no game"));
        let mut info = startup(2);
        info.seat_tokens.pop();
        assert!(build_marker(&info, "cube", &plans)
            .unwrap_err()
            .to_string()
            .contains("1 seat tokens for 2 seats"));
    }

    #[test]
    fn agent_hints_point_at_the_published_table() {
        use SeatRole::*;
        let plans = table(&[Me, Agent(AgentKind::Claude)], &[Some("mine"), None]);
        let marker = build_marker(&startup(2), "cube", &plans).unwrap();
        let hints = agent_hints(&marker, &plans).join("\n");
        assert!(hints.contains("Table quiet-owl is published"));
        assert!(hints.contains("claude mcp add manaline -- manaline mcp --stdio"));
        assert!(hints.contains("submit_deck"), "the empty seat is called out");
        assert!(!hints.contains("separate agent sessions"), "one agent needs no warning");
        assert!(!hints.contains("--connect"), "agents no longer take an endpoint or a token");
        assert!(!hints.contains("--token"));

        let plans = table(
            &[Me, Agent(AgentKind::Claude), Agent(AgentKind::Generic)],
            &[Some("m"), Some("a"), Some("b")],
        );
        let marker = build_marker(&startup(3), "cube", &plans).unwrap();
        let hints = agent_hints(&marker, &plans).join("\n");
        assert!(hints.contains("2 separate agent sessions"));
        assert!(!hints.contains("submit_deck"), "both agent seats have decks");

        // No agent seat, nothing to say.
        let plans = table(&[Me, Random], &[Some("m"), Some("r")]);
        let marker = build_marker(&startup(2), "cube", &plans).unwrap();
        assert!(agent_hints(&marker, &plans).is_empty());
    }

    #[test]
    fn human_seats_get_a_join_command_each() {
        use SeatRole::*;
        let plans = table(&[Me, Human, Human], &[Some("m"), Some("a"), Some("b")]);
        let tokens: Vec<Token> = (0..3).map(|i| Token(format!("tok{i}"))).collect();
        let hints = human_hints(&plans, std::path::Path::new("/run/g.sock"), &tokens, None).join("\n");
        assert!(hints.contains("other 2 players"));
        assert!(hints.contains("manaline join /run/g.sock --token tok1 --deck <their deck>"));
        assert!(hints.contains("--token tok2"));
        assert!(!hints.contains("tok0"), "your own seat needs no join command");

        let plans = table(&[Me, Random], &[Some("m"), Some("r")]);
        assert!(human_hints(&plans, std::path::Path::new("/run/g.sock"), &tokens, None).is_empty());
    }

    #[tokio::test]
    async fn publishing_a_table_withdraws_it_on_the_way_out() {
        use SeatRole::*;
        let rt = Runtime::at(std::env::temp_dir().join(format!("manaline-play-test-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&rt.dir);
        let plans = table(
            &[Me, Agent(AgentKind::Claude), Agent(AgentKind::Codex)],
            &[Some("mine"), None, Some("red")],
        );
        let marker = build_marker(&startup(3), "cube", &plans).unwrap();

        let published = Published::new(&rt, &marker).unwrap();
        let live = rt.live_games();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].game_id, "quiet-owl");
        assert_eq!(live[0].seats.len(), 3);

        // An agent session finds the table and takes the first free agent seat.
        let first = rt.claim_seat(&live[0], None).unwrap().unwrap();
        assert_eq!(first.seat(), 1);
        assert_eq!(first.slot.token, Some(Token("tok1".into())));
        assert_eq!(first.slot.deck, None, "this seat's agent picks its own deck");
        let second = rt.claim_seat(&live[0], None).unwrap().unwrap();
        assert_eq!(second.seat(), 2);
        assert!(rt.claim_seat(&live[0], None).unwrap().is_none(), "only two agent seats");
        assert_eq!(published.0.claims(), vec![(1, std::process::id()), (2, std::process::id())]);
        drop(first);
        drop(second);

        drop(published);
        assert!(rt.live_games().is_empty(), "`play` exiting unpublishes the table");
        let _ = std::fs::remove_dir_all(&rt.dir);
    }
}
