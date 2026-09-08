//! `manaline play`: the whole user experience in one command (§2.1). Spawns
//! the daemon, seats the opponent, opens the TUI, and tears everything down.
//! Also `join` and `tui`, the pieces `play` is made of.

use crate::bot::{self, BotSettings};
use anyhow::{anyhow, bail, Context, Result};
use daemon::StartupInfo;
use engine::Format;
use protocol::Endpoint;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

#[derive(clap::Args)]
pub struct PlayArgs {
    /// Your deck: a file path or a built-in deck name (see `list decks`).
    #[arg(long)]
    pub deck: String,
    /// Opponent: `random` (built-in bot), `human` (a second terminal), or an agent:
    /// `claude`, `codex`, or `mcp` (starts the MCP server on seat 1 and tells you how to connect).
    #[arg(long, default_value = "random")]
    pub vs: String,
    /// The bot's deck (defaults to a built-in deck that differs from yours).
    #[arg(long)]
    pub opp_deck: Option<String>,
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
    /// Also listen on TCP so a remote terminal can join (`--vs human`).
    #[arg(long)]
    pub tcp: Option<String>,
}

pub async fn play(args: PlayArgs) -> Result<()> {
    let db = cards::core();
    let format = Format::builtin(&args.format).ok_or_else(|| anyhow!("unknown format {:?}", args.format))?;
    let decklist = crate::deck_text(&args.deck)?;
    check_deck(&decklist, &format, &db, &args.deck)?;
    let name = args.name.clone().unwrap_or_else(whoami);

    let opponent = match args.vs.as_str() {
        "random" => Opponent::Random,
        "human" => Opponent::Human,
        "claude" => Opponent::Agent(AgentKind::Claude),
        "codex" => Opponent::Agent(AgentKind::Codex),
        "mcp" => Opponent::Agent(AgentKind::Generic),
        other => bail!("unknown opponent {other:?}; use random, human, claude, codex, or mcp"),
    };
    let opp_deck_name = match &args.opp_deck {
        Some(d) => d.clone(),
        None => default_opponent_deck(&args.deck),
    };
    let opp_decklist = crate::deck_text(&opp_deck_name)?;
    if !matches!(opponent, Opponent::Human) {
        check_deck(&opp_decklist, &format, &db, &opp_deck_name)?;
    }

    let mut daemon = spawn_daemon(&args.format, 2, args.seed, args.tcp.as_deref()).await?;
    let info = daemon.info.clone();
    let socket = info.socket.clone().ok_or_else(|| anyhow!("daemon reported no socket"))?;
    let endpoint = Endpoint::Unix(socket.clone());
    let tokens = info.seat_tokens.clone();
    if tokens.len() < 2 {
        bail!("daemon reported {} seat tokens", tokens.len());
    }

    let mut hints = Vec::new();
    let mut bot_task = None;
    let mut mcp_child: Option<tokio::process::Child> = None;
    match opponent {
        Opponent::Random => {
            let settings = BotSettings {
                endpoint: endpoint.clone(),
                token: tokens[1].clone(),
                name: "Bot".into(),
                decklist: opp_decklist,
                seed: rand::random(),
            };
            bot_task = Some(tokio::spawn(bot::run(settings)));
        }
        Opponent::Agent(kind) => {
            let (child, url) = spawn_mcp(&socket, &tokens[1], &opp_deck_name, kind.name()).await?;
            mcp_child = Some(child);
            hints.push(agent_hints(kind, &url, &socket, &tokens[1]));
        }
        Opponent::Human => {
            let mut lines = vec!["To seat the other player, run this in another terminal:".to_string()];
            lines.push(format!(
                "  manaline join {} --token {} --deck <their deck>",
                socket.display(),
                tokens[1]
            ));
            if let Some(addr) = info.tcp {
                lines.push("or from another machine on the same network:".to_string());
                lines.push(format!("  manaline join {addr} --token {} --deck <their deck>", tokens[1]));
            }
            hints.push(lines.join("\n"));
        }
    }

    let config = tui::TuiConfig {
        endpoint,
        token: tokens[0].clone(),
        name,
        decklist: Some(decklist),
        hints,
        deck_path: deck_path_of(&args.deck),
        theme: tui::theme_flag(args.theme.as_deref())?,
    };
    let result = tui::run(config).await;

    if let Some(t) = bot_task {
        t.abort();
    }
    if let Some(mut child) = mcp_child {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    daemon.stop().await;

    match result {
        Ok(outcome) => {
            if let Some(o) = outcome {
                println!("{}", describe_outcome(o, &info));
            }
            if let Some(p) = &info.replay_path {
                if p.exists() {
                    println!("Replay saved to {}", p.display());
                }
            }
            Ok(())
        }
        Err(e) => Err(e),
    }
}

enum Opponent {
    Random,
    Human,
    Agent(AgentKind),
}

#[derive(Clone, Copy)]
enum AgentKind {
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

/// The one thing the human needs to know: how to point their agent at the game.
fn agent_hints(kind: AgentKind, url: &str, socket: &std::path::Path, token: &protocol::Token) -> String {
    let stdio = format!("manaline mcp --stdio --connect {} --token {}", socket.display(), token);
    let mut lines = vec![
        format!("{} plays seat 1. Point it at the game:", kind.name()),
        format!("  MCP server (streamable HTTP):  {url}"),
    ];
    match kind {
        AgentKind::Claude => {
            lines.push(format!("  Claude Code:  claude mcp add --transport http manaline {url}"));
        }
        AgentKind::Codex => {
            lines.push(format!("  Codex:  codex mcp add manaline --url {url}"));
        }
        AgentKind::Generic => {}
    }
    lines.push(format!(
        "  Config snippet:  {{\"mcpServers\":{{\"manaline\":{{\"type\":\"http\",\"url\":\"{url}\"}}}}}}"
    ));
    lines.push(format!("  stdio alternative:  {stdio}"));
    lines.push("Then tell it: \"You're playing Magic against me. Pull the play-a-game prompt from the manaline server and go.\"".into());
    lines.join("\n")
}

/// Spawn `manaline mcp` on seat 1 and read the URL it prints.
async fn spawn_mcp(socket: &std::path::Path, token: &protocol::Token, deck: &str, name: &str) -> Result<(tokio::process::Child, String)> {
    let exe = std::env::current_exe().context("locating the manaline binary")?;
    let log_dir = protocol::endpoint::data_dir().join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let log_path = log_dir.join(format!("mcp-{}.log", std::process::id()));
    let log_file = std::fs::File::create(&log_path).with_context(|| format!("creating {}", log_path.display()))?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("mcp")
        .arg("--connect")
        .arg(socket)
        .arg("--token")
        .arg(&token.0)
        .arg("--deck")
        .arg(deck)
        .arg("--name")
        .arg(name)
        .arg("--http")
        .arg("127.0.0.1:7454")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_file))
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("starting the MCP server")?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .context("the MCP server did not start in time")?
        .context("reading from the MCP server")?
        .ok_or_else(|| anyhow!("the MCP server exited before it was ready (see {})", log_path.display()))?;
    let info: serde_json::Value =
        serde_json::from_str(&first).with_context(|| format!("the MCP server said something unexpected: {first}"))?;
    let url = info["url"].as_str().ok_or_else(|| anyhow!("no url in {first}"))?.to_string();
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    Ok((child, url))
}

fn describe_outcome(o: engine::Outcome, _info: &StartupInfo) -> String {
    match o {
        engine::Outcome::Winner(engine::Seat(0)) => "You won.".into(),
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
    for (name, _) in cards::DECKS.iter().rev() {
        if *name != mine {
            return name.to_string();
        }
    }
    "red".into()
}

/// A deck given as a file path (not a built-in name) can be edited in place.
pub fn deck_path_of(spec: &str) -> Option<std::path::PathBuf> {
    if cards::deck_text(spec).is_some() {
        return None;
    }
    let p = std::path::PathBuf::from(spec);
    p.exists().then_some(p)
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
    /// Socket path or host:port printed by `play --vs human` or `host`.
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
