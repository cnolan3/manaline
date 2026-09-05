//! `manaline daemon`, `manaline bot`, and `manaline replay`: the scripting
//! and debugging entry points. `play` wraps these for humans.

use crate::bot::{self, BotSettings};
use anyhow::{anyhow, bail, Context, Result};
use daemon::{CreateGame, Daemon, DaemonConfig};
use engine::text::{describe_event, render_view};
use protocol::{Endpoint, Token};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(clap::Args)]
pub struct DaemonArgs {
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
    let create = match (args.format, args.seats) {
        (Some(format), Some(seats)) => Some(CreateGame { format, seats, seed: args.seed }),
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
pub struct ReplayArgs {
    /// A replay log written by the daemon.
    pub file: PathBuf,
    /// Stop after this many actions.
    #[arg(long)]
    pub up_to: Option<usize>,
    /// Print the event log.
    #[arg(long)]
    pub log: bool,
}

pub fn replay(args: ReplayArgs) -> Result<()> {
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
