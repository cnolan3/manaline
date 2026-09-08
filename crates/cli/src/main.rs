//! `manaline` — the front door.

mod bot;
mod commands;
mod deck;
mod play;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use engine::bot::RandomBot;
use engine::text::{describe_action, describe_event, render_view};
use engine::{Format, Game, GameConfig, Outcome, PlayerSetup};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "manaline", version, about = "Terminal Magic: The Gathering")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Play a game in this terminal: `manaline play --deck green --vs random`.
    Play(play::PlayArgs),
    /// Join a game someone else is hosting, with the token they gave you.
    Join(play::JoinArgs),
    /// Open the terminal client on an existing game (advanced).
    Tui(play::TuiArgs),
    /// Play random bots against each other in-process and print the result.
    Sim(SimArgs),
    /// Run a game daemon (advanced; `play` does this for you).
    Daemon(commands::DaemonArgs),
    /// Join a daemon as the built-in random bot.
    Bot(commands::BotArgs),
    /// Run the MCP server on a seat so an agent can play it (advanced; `play --vs claude` does this).
    Mcp(commands::McpArgs),
    /// Reconstruct a game from its replay log.
    Replay(commands::ReplayArgs),
    /// List the formats, decks, and cards built into this binary.
    List {
        #[arg(value_enum)]
        what: ListWhat,
    },
    /// Check, create, or analyse a deck file (offline).
    Deck {
        #[command(subcommand)]
        cmd: deck::DeckCommand,
    },
    /// Card data: refresh the Scryfall cache or look a card up.
    Cards {
        #[command(subcommand)]
        cmd: deck::CardsCommand,
    },
    /// Card IR tooling (dev): round-trip checks.
    Ingest {
        #[command(subcommand)]
        cmd: deck::IngestCommand,
    },
    /// Which game daemons and MCP servers are running on this machine.
    Status {
        /// Remove socket files whose daemon is gone.
        #[arg(long)]
        clean: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum ListWhat {
    Formats,
    Decks,
    Cards,
}

#[derive(clap::Args)]
struct SimArgs {
    /// Number of seats. 2 uses the cube format; more uses free-for-all.
    #[arg(long, default_value_t = 2)]
    seats: usize,
    /// Game seed. Each additional game uses seed + 1, + 2, ...
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Seed for the bots' choices (independent of the game seed).
    #[arg(long, default_value_t = 7)]
    bot_seed: u64,
    /// How many games to play.
    #[arg(long, default_value_t = 1)]
    games: u32,
    /// Deck for each seat, as a file path or a built-in name (see `list decks`).
    /// Cycles if fewer than `--seats` are given.
    #[arg(long = "deck")]
    decks: Vec<String>,
    /// Format name (built-in) or path to a format .ron file.
    #[arg(long)]
    format: Option<String>,
    /// Stop a game after this many actions and report it as unfinished.
    #[arg(long, default_value_t = 20_000)]
    max_actions: usize,
    /// Print every event as it happens.
    #[arg(long)]
    log: bool,
    /// Print every action as the bots choose it.
    #[arg(long)]
    actions: bool,
    /// Print the final board from the spectator's view.
    #[arg(long)]
    board: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("warn".parse()?))
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Play(args) => runtime()?.block_on(play::play(args)),
        Command::Join(args) => runtime()?.block_on(play::join(args)),
        Command::Tui(args) => runtime()?.block_on(play::tui(args)),
        Command::Sim(args) => sim(args),
        Command::List { what } => list(what),
        Command::Daemon(args) => runtime()?.block_on(commands::daemon(args)),
        Command::Bot(args) => runtime()?.block_on(commands::bot(args)),
        Command::Mcp(args) => runtime()?.block_on(commands::mcp(args)),
        Command::Replay(args) => commands::replay(args),
        Command::Deck { cmd } => deck::deck(cmd),
        Command::Cards { cmd } => deck::cards_cmd(cmd),
        Command::Ingest { cmd } => deck::ingest(cmd),
        Command::Status { clean } => runtime()?.block_on(commands::status(clean)),
    }
}

pub fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

/// A deck as text: a built-in name or a file path.
pub fn deck_text(spec: &str) -> Result<String> {
    match cards::deck_text(spec) {
        Some(t) => Ok(t),
        None => std::fs::read_to_string(spec).with_context(|| format!("reading deck {spec}")),
    }
}

fn list(what: ListWhat) -> Result<()> {
    match what {
        ListWhat::Formats => {
            for name in Format::builtin_names() {
                let f = Format::builtin(name).unwrap();
                println!(
                    "{name:14} {:<16} {}–{} players, {} life",
                    f.name, f.players.min, f.players.max, f.starting_life
                );
            }
        }
        ListWhat::Decks => {
            let names = cards::deck_names();
            if names.is_empty() {
                println!("no decks in {}", cards::decks_dir().display());
            }
            for name in names {
                println!("{name}");
            }
        }
        ListWhat::Cards => {
            let db = cards::core();
            for (_, c) in db.iter() {
                let pt = c.pt.map(|(p, t)| format!(" {p}/{t}")).unwrap_or_default();
                println!("{:<32} {}{}", c.name, c.cost, pt);
            }
        }
    }
    Ok(())
}

pub fn load_format(name: Option<&str>, seats: usize) -> Result<Format> {
    match name {
        None => {
            let f = if seats == 2 { "cube" } else { "free-for-all" };
            Ok(Format::builtin(f).unwrap())
        }
        Some(n) => {
            if let Some(f) = Format::builtin(n) {
                return Ok(f);
            }
            let path = PathBuf::from(n);
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading format {}", path.display()))?;
            Format::from_ron(&text).map_err(|e| anyhow!("{}: {e}", path.display()))
        }
    }
}

fn load_deck(spec: &str, db: &engine::CardDb) -> Result<Vec<engine::CardId>> {
    let text = deck_text(spec)?;
    cards::parse_decklist(&text, db).map_err(|e| anyhow!("{spec}: {e}"))
}

fn sim(args: SimArgs) -> Result<()> {
    if args.seats < 2 {
        bail!("--seats must be at least 2");
    }
    let db = Arc::new(cards::core());
    let format = load_format(args.format.as_deref(), args.seats)?;
    let deck_specs: Vec<String> = if args.decks.is_empty() {
        let names = cards::deck_names();
        if names.is_empty() {
            bail!("no decks in {}; pass --deck", cards::decks_dir().display());
        }
        names
    } else {
        args.decks.clone()
    };
    let decks: Vec<Vec<engine::CardId>> = deck_specs.iter().map(|s| load_deck(s, &db)).collect::<Result<_>>()?;

    let mut wins = vec![0u32; args.seats];
    let mut draws = 0u32;
    let mut unfinished = 0u32;
    let mut total_turns = 0u64;
    let mut total_actions = 0u64;

    for g in 0..args.games {
        let seed = args.seed + g as u64;
        let players = (0..args.seats)
            .map(|i| PlayerSetup {
                name: format!("Bot{i}"),
                deck: decks[i % decks.len()].clone(),
            })
            .collect();
        let config = GameConfig {
            format: format.clone(),
            players,
            cards: db.clone(),
            starting_player: None,
        };
        let mut game = Game::new(config, seed)?;
        let mut bot = RandomBot::new(args.bot_seed + g as u64);
        let mut applied = 0usize;
        let mut printed = 0usize;

        if args.log {
            printed = print_log(&game, printed);
        }
        while game.is_over().is_none() && applied < args.max_actions {
            let must = game.must_act();
            let (&seat, &reason) = must.iter().next().expect("must_act non-empty while game not over");
            let action = bot.choose(&game, seat).expect("seat in must_act has a legal action");
            if args.actions {
                println!("  > {} ({reason:?}): {}", game.player_name(seat), describe_action(&game, &action));
            }
            game.apply(seat, &action)?;
            applied += 1;
            if args.log {
                printed = print_log(&game, printed);
            }
        }
        total_actions += applied as u64;
        total_turns += game.turn as u64;
        let result = match game.is_over() {
            Some(Outcome::Winner(s)) => {
                wins[s.index()] += 1;
                format!("{} wins", game.player_name(s))
            }
            Some(Outcome::Draw) => {
                draws += 1;
                "draw".to_string()
            }
            None => {
                unfinished += 1;
                format!("unfinished after {applied} actions")
            }
        };
        println!(
            "game {} seed={seed} seats={} turns={} actions={applied}: {result}",
            g + 1,
            args.seats,
            game.turn
        );
        if args.board {
            println!("{}", render_view(&game.view_spectator()));
        }
    }

    if args.games > 1 {
        let n = args.games as f64;
        let win_list: Vec<String> = wins.iter().enumerate().map(|(i, w)| format!("Bot{i}={w}")).collect();
        println!(
            "\n{} games: {}, draws={draws}, unfinished={unfinished}; mean turns {:.1}, mean actions {:.1}",
            args.games,
            win_list.join(" "),
            total_turns as f64 / n,
            total_actions as f64 / n
        );
    }
    Ok(())
}

fn print_log(game: &Game, from: usize) -> usize {
    for e in &game.log[from..] {
        println!("{}", describe_event(game, e));
    }
    game.log.len()
}
