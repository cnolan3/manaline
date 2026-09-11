//! `manaline daemon`, `manaline bot`, and `manaline replay`: the scripting
//! and debugging entry points. `play` wraps these for humans.

use crate::bot::{self, BotSettings};
use anyhow::{anyhow, bail, Context, Result};
use daemon::{CreateGame, Daemon, DaemonConfig};
use engine::text::{describe_event, render_view};
use protocol::{Endpoint, Token};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(clap::Subcommand)]
pub enum DaemonCmd {
    /// Stop running daemons gracefully (they remove their sockets).
    Stop {
        /// Only the daemon serving this game id.
        #[arg(long)]
        game: Option<String>,
    },
}

#[derive(clap::Subcommand)]
pub enum McpCmd {
    /// Stop running MCP servers.
    Stop {
        /// Only the server listening at this address (host:port).
        #[arg(long)]
        http: Option<String>,
    },
}

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub cmd: Option<DaemonCmd>,
    /// Create the game at startup with this format (built-in name).
    #[arg(long)]
    pub format: Option<String>,
    #[arg(long)]
    pub seats: Option<u8>,
    #[arg(long)]
    pub seed: Option<u64>,
    /// Unix socket path. Defaults to a fresh path in the runtime directory.
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Also listen on TCP, e.g. `127.0.0.1:7454` or `0.0.0.0:0`.
    #[arg(long)]
    pub tcp: Option<String>,
    /// Listen on TCP only (no Unix socket).
    #[arg(long)]
    pub no_socket: bool,
    /// Exit when this process is gone.
    #[arg(long)]
    pub parent_pid: Option<u32>,
    /// Where to write replay logs.
    #[arg(long)]
    pub replay_dir: Option<PathBuf>,
}

/// Print one JSON line describing the listening daemon, then serve until
/// shut down. `play` reads that line to learn the socket and tokens.
pub async fn daemon(args: DaemonArgs) -> Result<()> {
    if let Some(DaemonCmd::Stop { game }) = args.cmd {
        return stop_processes("daemon", game.as_deref(), None).await;
    }
    let create = match (args.format, args.seats) {
        (Some(format), Some(seats)) => Some(CreateGame {
            format,
            seats,
            seed: args.seed,
        }),
        (None, None) => None,
        _ => bail!("--format and --seats go together"),
    };
    let config = DaemonConfig {
        socket: args.socket,
        no_socket: args.no_socket,
        tcp: args.tcp,
        parent_pid: args.parent_pid,
        replay_dir: args.replay_dir,
        create,
        cards: Arc::new(cards::core()),
        legality: crate::deck::legality_source(),
    };
    let daemon = Daemon::bind(config).await?;
    println!("{}", serde_json::to_string(daemon.info())?);
    let handle = daemon.handle();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending::<()>().await } } => {}
        }
        handle.shutdown();
    });
    daemon.run().await?;
    Ok(())
}

#[derive(clap::Args)]
pub struct BotArgs {
    /// Socket path or host:port of the daemon.
    #[arg(long)]
    pub connect: String,
    #[arg(long)]
    pub token: String,
    /// Deck file path or built-in deck name.
    #[arg(long)]
    pub deck: String,
    #[arg(long, default_value = "Bot")]
    pub name: String,
    #[arg(long, default_value_t = 7)]
    pub seed: u64,
}

pub async fn bot(args: BotArgs) -> Result<()> {
    let endpoint = Endpoint::parse(&args.connect).map_err(|e| anyhow!(e))?;
    let decklist = crate::deck_text(&args.deck)?;
    let (seat, outcome) = bot::run(BotSettings {
        endpoint,
        token: Token(args.token),
        name: args.name,
        decklist,
        seed: args.seed,
    })
    .await?;
    match outcome {
        Some(engine::Outcome::Winner(w)) if w == seat => println!("{seat}: won"),
        Some(engine::Outcome::Winner(w)) => println!("{seat}: lost to {w}"),
        Some(engine::Outcome::Draw) => println!("{seat}: draw"),
        None => println!("{seat}: connection closed before the game ended"),
    }
    Ok(())
}

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct McpArgs {
    #[command(subcommand)]
    pub cmd: Option<McpCmd>,
    /// Socket path or host:port of the daemon.
    #[arg(long, conflicts_with = "game")]
    pub connect: Option<String>,
    /// A game id; connects to that game's socket in the runtime directory.
    #[arg(long)]
    pub game: Option<String>,
    /// The seat token the agent plays with (not needed without a game).
    #[arg(long, default_value = "")]
    pub token: String,
    /// Format for deck analysis when no game is connected.
    #[arg(long, default_value = "cube")]
    pub format: String,
    /// Deck to submit for the agent if the game has not started (file or built-in name).
    #[arg(long)]
    pub deck: Option<String>,
    /// The agent's display name at the table.
    #[arg(long, default_value = "Agent")]
    pub name: String,
    /// Serve streamable HTTP at this address (default 127.0.0.1:7454; port 0 picks a free one).
    #[arg(long, conflicts_with = "stdio")]
    pub http: Option<String>,
    /// Serve over stdin/stdout for clients that launch processes.
    #[arg(long)]
    pub stdio: bool,
    /// Exit when this process is gone.
    #[arg(long)]
    pub parent_pid: Option<u32>,
}

/// Run the MCP server on a seat. In HTTP mode, prints one JSON line with the
/// URL first (`play` reads it), then serves until stopped.
pub async fn mcp(args: McpArgs) -> Result<()> {
    if let Some(McpCmd::Stop { http }) = args.cmd {
        return stop_processes("mcp", None, http.as_deref()).await;
    }
    let server = match (&args.connect, &args.game) {
        (None, None) => {
            // No game: serve card data for deckbuilding.
            let format = crate::load_format(Some(&args.format), 2)?;
            eprintln!(
                "no game given: serving card data only ({} format); start a game with `manaline play` to play",
                format.name
            );
            mcp::standalone(format)
        }
        (connect, game) => {
            let endpoint = match (connect, game) {
                (Some(c), _) => c.clone(),
                (None, Some(g)) => protocol::endpoint::socket_path(g).display().to_string(),
                (None, None) => unreachable!(),
            };
            let decklist = match &args.deck {
                Some(d) => Some(crate::deck_text(d)?),
                None => None,
            };
            let config = mcp::SessionConfig {
                endpoint: Endpoint::parse(&endpoint).map_err(|e| anyhow!(e))?,
                token: Token(args.token.clone()),
                name: args.name.clone(),
                decklist,
            };
            mcp::connect(config).await?
        }
    };
    if let Some(pid) = args.parent_pid {
        mcp::watch_parent(pid, || std::process::exit(0));
    }
    if args.stdio {
        return mcp::serve_stdio(server).await;
    }
    let addr = args.http.clone().unwrap_or_else(|| "127.0.0.1:7454".into());
    let http = match mcp::serve_http(server.clone(), &addr).await {
        Ok(h) => h,
        Err(e) if addr.ends_with(":7454") => {
            tracing::warn!("{e:#}; falling back to a free port");
            mcp::serve_http(server.clone(), "127.0.0.1:0").await?
        }
        Err(e) => return Err(e),
    };
    println!("{}", serde_json::json!({ "url": http.url(), "addr": http.addr.to_string() }));
    // Advertise for `status`, `play`, and `deck edit`, and take control requests.
    let control_socket = mcp::control::control_socket_path();
    let _ = std::fs::remove_file(&control_socket);
    let advertised = match tokio::net::UnixListener::bind(&control_socket) {
        Ok(listener) => {
            let marker = mcp::control::Marker {
                pid: std::process::id(),
                url: http.url(),
                control_socket: control_socket.clone(),
                mode: server.mode(),
            };
            match mcp::control::Advertised::write(&mcp::control::marker_path(), &marker) {
                Ok(a) => {
                    let a = std::sync::Arc::new(a);
                    tokio::spawn(mcp::control::serve(listener, server.clone(), http.url(), a.clone()));
                    Some(a)
                }
                Err(e) => {
                    tracing::warn!("could not advertise the MCP server: {e}");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!("could not open the control socket: {e}");
            None
        }
    };
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    let result = tokio::select! {
        r = http.wait() => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending::<()>().await } } => Ok(()),
    };
    drop(advertised);
    result
}

#[derive(clap::Args)]
pub struct ReplayArgs {
    /// A replay log written by the daemon.
    pub file: PathBuf,
    /// Stop after this many actions.
    #[arg(long)]
    pub up_to: Option<usize>,
    /// Print the event log.
    #[arg(long)]
    pub log: bool,
    /// Step through the game in the terminal client instead of printing it.
    #[arg(long)]
    pub step: bool,
    /// Colour theme for `--step`: default, mono, or high-contrast.
    #[arg(long)]
    pub theme: Option<String>,
}

pub fn replay(args: ReplayArgs) -> Result<()> {
    if args.step {
        let db = Arc::new(cards::core());
        let (header, actions) = daemon::replay::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
        let config = daemon::replay::config_from_header(&header, db)?;
        let mut game = engine::Game::new(config, header.seed)?;
        let mut views = vec![game.view_spectator()];
        let mut events = vec![game.log.iter().filter_map(|e| e.view(None)).collect::<Vec<_>>()];
        let limit = args.up_to.unwrap_or(usize::MAX);
        for (seat, action) in actions.into_iter().take(limit) {
            let produced = game.apply(seat, &action).map_err(|e| anyhow!("replay diverged: {e}"))?;
            views.push(game.view_spectator());
            events.push(produced.iter().filter_map(|e| e.view(None)).collect());
        }
        let title = format!("{} · {} · seed {}", header.game_id, header.format, header.seed);
        let replay = tui::app::ReplayState {
            title,
            views,
            events,
            index: 0,
            playing: false,
        };
        let theme = tui::theme_flag(args.theme.as_deref())?;
        return crate::runtime()?.block_on(tui::run_replay(replay, theme));
    }
    let (header, game) = daemon::replay::rebuild(&args.file, Arc::new(cards::core()), args.up_to)
        .with_context(|| format!("replaying {}", args.file.display()))?;
    println!(
        "game {} · format {} · seed {} · {} players · {} actions",
        header.game_id,
        header.format,
        header.seed,
        header.players.len(),
        game.history().len()
    );
    if args.log {
        for e in &game.log {
            println!("{}", describe_event(&game, e));
        }
        println!();
    }
    print!("{}", render_view(&game.view_spectator()));
    Ok(())
}

/// `manaline status`: which daemons and MCP servers are running on this machine.
pub async fn status(clean: bool) -> Result<()> {
    // Daemons: one socket per game in the runtime directory; ping each.
    let dir = protocol::endpoint::runtime_dir();
    let mut sockets: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "sock"))
                .collect()
        })
        .unwrap_or_default();
    sockets.sort();
    println!("game daemons ({}):", dir.display());
    if sockets.is_empty() {
        println!("  none");
    }
    let mut stale = Vec::new();
    for sock in &sockets {
        let game = sock.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        let alive = match Endpoint::parse(&sock.display().to_string()) {
            Ok(ep) => match tokio::time::timeout(std::time::Duration::from_secs(2), protocol::Client::connect(&ep)).await {
                Ok(Ok(mut c)) => c.ping().await.is_ok(),
                _ => false,
            },
            Err(_) => false,
        };
        if alive {
            println!("  {game:<14} running   {}", sock.display());
        } else {
            println!("  {game:<14} stale     {} (no daemon answering)", sock.display());
            stale.push(sock.clone());
        }
    }

    // Processes: daemons and MCP servers by command line (macOS and Linux `ps`).
    let procs = our_processes();
    let mut mcp_addrs: Vec<String> = Vec::new();
    println!("\nprocesses:");
    if procs.is_empty() {
        println!("  none");
    }
    for p in &procs {
        println!("  {:>7}  {:<7} {}", p.pid, p.kind, p.describe());
        if p.kind == "mcp" {
            mcp_addrs.push(p.http_addr());
        }
    }

    if let Some(m) = mcp::control::running() {
        println!("\nmcp server: {} ({}), pid {}", m.url, m.mode, m.pid);
    }
    // MCP servers: the default port plus whatever the processes named.
    if !mcp_addrs.iter().any(|a| a == "127.0.0.1:7454") {
        mcp_addrs.push("127.0.0.1:7454".into());
    }
    println!("\nmcp servers:");
    for addr in mcp_addrs {
        let probe = addr.replace("0.0.0.0", "127.0.0.1");
        let up = tokio::time::timeout(std::time::Duration::from_secs(1), tokio::net::TcpStream::connect(&probe)).await;
        match up {
            Ok(Ok(_)) => println!("  http://{probe}/mcp   listening"),
            _ => println!("  http://{probe}/mcp   not listening"),
        }
    }
    if !stale.is_empty() {
        if clean {
            for p in &stale {
                let _ = std::fs::remove_file(p);
            }
            println!("\nremoved {} stale socket file(s)", stale.len());
        } else {
            println!("\n{} stale socket file(s); `manaline status --clean` removes them", stale.len());
        }
    }
    Ok(())
}

/// A running manaline daemon or MCP server, from `ps`.
struct Proc {
    pid: u32,
    kind: &'static str,
    cmd: String,
}

impl Proc {
    fn flag(&self, name: &str) -> Option<String> {
        let mut words = self.cmd.split_whitespace();
        while let Some(w) = words.next() {
            if w == name {
                return words.next().map(String::from);
            }
            if let Some(v) = w.strip_prefix(&format!("{name}=")) {
                return Some(v.to_string());
            }
        }
        None
    }

    fn http_addr(&self) -> String {
        self.flag("--http").unwrap_or_else(|| "127.0.0.1:7454".into())
    }
}

fn our_processes() -> Vec<Proc> {
    let me = std::process::id();
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return Vec::new();
    };
    let mut procs = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((pid, cmd)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else { continue };
        let cmd = cmd.trim().to_string();
        if pid == me || !cmd.contains("manaline") {
            continue;
        }
        let Some(kind) = classify(&cmd) else { continue };
        procs.push(Proc { pid, kind, cmd });
    }
    procs
}

/// What kind of manaline process a command line is, if it is one we report.
pub fn classify(cmd: &str) -> Option<&'static str> {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let first = words.first()?;
    if !first.contains("manaline") {
        return None;
    }
    let rest = &words[1..];
    match rest.first().copied() {
        Some("daemon") if !rest.contains(&"stop") => Some("daemon"),
        Some("mcp") if !rest.contains(&"stop") => Some("mcp"),
        Some("play") => Some("play"),
        Some("join") | Some("tui") => Some("client"),
        Some("deck") if rest.get(1) == Some(&"edit") => Some("editor"),
        Some("replay") if rest.contains(&"--step") => Some("replay"),
        _ => None,
    }
}

impl Proc {
    /// A one-line description for `status`.
    fn describe(&self) -> String {
        let words: Vec<&str> = self.cmd.split_whitespace().collect();
        match self.kind {
            "editor" => format!("deck editor on {}", words.get(3).unwrap_or(&"?")),
            "play" => format!(
                "game client (play) deck {} vs {}",
                self.flag("--deck").unwrap_or_else(|| "?".into()),
                self.flag("--vs").unwrap_or_else(|| "random".into())
            ),
            "client" => match (self.flag("--game"), self.flag("--connect"), words.get(2)) {
                (Some(g), _, _) => format!("game client on game {g}"),
                (_, Some(c), _) => format!("game client connected to {c}"),
                (_, _, Some(ep)) if words.get(1) == Some(&"join") => format!("game client joined {ep}"),
                _ => "game client".into(),
            },
            "replay" => format!("replay viewer on {}", words.get(2).unwrap_or(&"?")),
            "mcp" => match (self.flag("--game"), self.flag("--connect")) {
                (Some(g), _) => format!("MCP server for game {g} at {}", self.http_addr()),
                (_, Some(c)) => format!("MCP server for {c} at {}", self.http_addr()),
                _ => format!("MCP server, card data only, at {}", self.http_addr()),
            },
            "daemon" => format!(
                "game daemon{}",
                self.flag("--socket").map(|s| format!(" on {s}")).unwrap_or_default()
            ),
            _ => self.cmd.clone(),
        }
    }
}

/// The pid holding a Unix socket open, via `lsof`.
fn socket_owner(path: &std::path::Path) -> Option<u32> {
    let out = std::process::Command::new("lsof").args(["-t", "-U"]).arg(path).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.trim().parse().ok())
}

/// `manaline daemon stop` / `manaline mcp stop`: terminate our processes gracefully.
pub async fn stop_processes(kind: &str, game: Option<&str>, http: Option<&str>) -> Result<()> {
    let mut targets: Vec<Proc> = our_processes().into_iter().filter(|p| p.kind == kind).collect();
    if let Some(game) = game {
        let sock = protocol::endpoint::socket_path(game);
        let owner = socket_owner(&sock);
        targets.retain(|p| Some(p.pid) == owner || p.flag("--socket").map(|s| s.ends_with(&format!("{game}.sock"))).unwrap_or(false));
        if targets.is_empty() {
            bail!("no daemon is serving game {game} (see `manaline status`)");
        }
    }
    if let Some(http) = http {
        targets.retain(|p| p.http_addr() == http);
        if targets.is_empty() {
            bail!("no MCP server is listening at {http} (see `manaline status`)");
        }
    }
    if targets.is_empty() {
        println!("no {kind} processes are running");
        return Ok(());
    }
    for p in &targets {
        unsafe {
            libc::kill(p.pid as i32, libc::SIGTERM);
        }
        println!("stopping {kind} {} ({})", p.pid, p.cmd);
    }
    // Give them a moment, then kill any that ignored the request.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let alive = our_processes();
        if !targets.iter().any(|t| alive.iter().any(|a| a.pid == t.pid)) {
            break;
        }
    }
    let alive = our_processes();
    for t in &targets {
        if alive.iter().any(|a| a.pid == t.pid) {
            unsafe {
                libc::kill(t.pid as i32, libc::SIGKILL);
            }
            println!("{} did not stop; killed", t.pid);
        }
    }
    // Any socket left behind is now stale.
    let dir = protocol::endpoint::runtime_dir();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "sock") && socket_owner(&p).is_none() {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    Ok(())
}

/// The machine's MCP server, starting one if none is running. Returns its
/// marker and whether this call started it.
pub async fn ensure_mcp_server() -> Result<(mcp::control::Marker, bool)> {
    if let Some(m) = mcp::control::running() {
        return Ok((m, false));
    }
    let exe = std::env::current_exe().context("locating the manaline binary")?;
    let log_dir = protocol::endpoint::data_dir().join("logs");
    std::fs::create_dir_all(&log_dir).ok();
    let log = std::fs::File::create(log_dir.join("mcp.log")).context("creating the MCP log file")?;
    std::process::Command::new(exe)
        .args(["mcp", "--http", "127.0.0.1:7454"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log))
        .spawn()
        .context("starting the MCP server")?;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Some(m) = mcp::control::running() {
            return Ok((m, true));
        }
    }
    bail!("the MCP server did not start (see {})", log_dir.join("mcp.log").display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_command_lines() {
        assert_eq!(classify("./target/debug/manaline daemon --format cube --seats 2"), Some("daemon"));
        assert_eq!(classify("manaline daemon stop"), None);
        assert_eq!(classify("/usr/local/bin/manaline mcp --http 127.0.0.1:7454"), Some("mcp"));
        assert_eq!(classify("manaline mcp stop --http 127.0.0.1:7454"), None);
        assert_eq!(classify("manaline play --deck rg-stompy --vs claude"), Some("play"));
        assert_eq!(classify("manaline join 127.0.0.1:7455 --token abc --deck green"), Some("client"));
        assert_eq!(classify("manaline tui --game ABC --token t"), Some("client"));
        assert_eq!(classify("manaline deck edit decks/rg-stompy.txt"), Some("editor"));
        assert_eq!(classify("manaline deck check decks/rg-stompy.txt"), None);
        assert_eq!(classify("manaline replay game.jsonl --step"), Some("replay"));
        assert_eq!(classify("manaline replay game.jsonl --log"), None);
        assert_eq!(classify("manaline status"), None);
        assert_eq!(classify("vim manaline.txt"), None);
        let p = Proc {
            pid: 1,
            kind: "editor",
            cmd: "manaline deck edit decks/rg-stompy.txt".into(),
        };
        assert_eq!(p.describe(), "deck editor on decks/rg-stompy.txt");
        let p = Proc {
            pid: 1,
            kind: "mcp",
            cmd: "manaline mcp --http 127.0.0.1:7461".into(),
        };
        assert_eq!(p.describe(), "MCP server, card data only, at 127.0.0.1:7461");
        let p = Proc {
            pid: 1,
            kind: "play",
            cmd: "manaline play --deck rg-stompy --vs claude".into(),
        };
        assert_eq!(p.describe(), "game client (play) deck rg-stompy vs claude");
    }
}
