//! `manaline` — the front door. In M0 only `sim` exists: random bots play a
//! whole game in-process at any seat count. `play`, `daemon`, `tui`, and
//! `mcp` arrive with M1 and M2.

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
    /// Play random bots against each other in-process and print the result.
    Sim(SimArgs),
    /// List the formats, decks, and cards built into this binary.
    List {
        #[arg(value_enum)]
        what: ListWhat,
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
    let cli = Cli::parse();
    match cli.command {
        Command::Sim(args) => sim(args),
        Command::List { what } => list(what),
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
            for (name, _) in cards::DECKS {
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

fn load_format(name: Option<&str>, seats: usize) -> Result<Format> {
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
    let text = match cards::deck_text(spec) {
        Some(t) => t.to_string(),
        None => std::fs::read_to_string(spec).with_context(|| format!("reading deck {spec}"))?,
    };
    cards::parse_decklist(&text, db).map_err(|e| anyhow!("{spec}: {e}"))
}

fn sim(args: SimArgs) -> Result<()> {
    if args.seats < 2 {
        bail!("--seats must be at least 2");
    }
    let db = Arc::new(cards::core());
    let format = load_format(args.format.as_deref(), args.seats)?;
    let deck_specs: Vec<String> = if args.decks.is_empty() {
        cards::DECKS.iter().map(|(n, _)| n.to_string()).collect()
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
        let config = GameConfig { format: format.clone(), players, cards: db.clone(), starting_player: None };
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
        println!("game {} seed={seed} seats={} turns={} actions={applied}: {result}", g + 1, args.seats, game.turn);
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
