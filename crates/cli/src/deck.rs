//! `manaline deck …`, `manaline cards …`, and `manaline ingest …`: the
//! offline deck and card-data commands (§4.5, §8). None of these need a
//! daemon; `deck` commands need no network either.

use anyhow::{anyhow, bail, Context, Result};
use clap::Subcommand;
use deckstats::{CardStatus, CheckReport, KnownCards};
use engine::{Format, LegalitySource};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Subcommand)]
pub enum DeckCommand {
    /// Validate a deck file against a format and explain every problem.
    Check {
        /// Deck file path, or a built-in deck name.
        deck: String,
        /// Format name (built-in) or path to a format .ron file. Default: cube.
        #[arg(long, default_value = "cube")]
        format: String,
    },
    /// Write a commented deck skeleton to `<name>.txt` (or `--out`).
    New {
        name: String,
        #[arg(long, default_value = "cube")]
        format: String,
        /// Where to write; defaults to `<name>.txt` in the current directory.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Mana curve, colour sources, interaction count, and sample opening hands.
    Stats {
        deck: String,
        #[arg(long, default_value = "cube")]
        format: String,
        /// How many sample opening hands to deal.
        #[arg(long, default_value_t = 3)]
        hands: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
}

#[derive(Subcommand)]
pub enum CardsCommand {
    /// Download (or refresh) the Scryfall card data used for legality and metadata.
    Update,
    /// Show a card: its rules text, and its printing and legality if card data is cached.
    Show { name: Vec<String> },
}

#[derive(Subcommand)]
pub enum IngestCommand {
    /// Load every card IR file, validate it, and check it renders back to its Oracle text.
    Roundtrip {
        /// A directory of .ron files. Default: the built-in core set.
        dir: Option<PathBuf>,
    },
}

/// The Scryfall cache as a legality source for the daemon, if it is present.
pub fn legality_source() -> Option<Arc<dyn LegalitySource + Send + Sync>> {
    match carddb::Cache::load() {
        Ok(Some(c)) => Some(Arc::new(c)),
        _ => None,
    }
}

struct Known(carddb::Cache);

impl KnownCards for Known {
    fn is_card(&self, name: &str) -> bool {
        self.0.contains(name)
    }
    fn as_legality(&self) -> &dyn LegalitySource {
        &self.0
    }
}

fn load_cache() -> Option<Known> {
    carddb::Cache::load().ok().flatten().map(Known)
}

fn read_deck(spec: &str) -> Result<(String, String)> {
    if let Some(t) = cards::deck_text(spec) {
        return Ok((t.to_string(), format!("built-in deck {spec}")));
    }
    let text = std::fs::read_to_string(spec).with_context(|| format!("reading deck {spec}"))?;
    Ok((text, spec.to_string()))
}

pub fn deck(cmd: DeckCommand) -> Result<()> {
    match cmd {
        DeckCommand::Check { deck, format } => {
            let format = super::load_format(Some(&format), 2)?;
            let (text, label) = read_deck(&deck)?;
            let db = cards::core();
            let known = load_cache();
            let report = check_text(&text, &format, &db, known.as_ref().map(|k| k as &dyn KnownCards))?;
            print!("{}", render_check(&report, &label, &format));
            match &known {
                Some(k) => println!("{}", k.0.age_text()),
                None => {
                    println!("no Scryfall card data cached; unknown names cannot be told apart from unimplemented cards (`manaline cards update`)")
                }
            }
            if report.is_legal() {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
        DeckCommand::New { name, format, out } => {
            let f = super::load_format(Some(&format), 2)?;
            let path = out.unwrap_or_else(|| PathBuf::from(format!("{name}.txt")));
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            std::fs::write(&path, skeleton(&name, &format, &f))?;
            println!("wrote {}", path.display());
            Ok(())
        }
        DeckCommand::Stats { deck, format, hands, seed } => {
            let f = super::load_format(Some(&format), 2)?;
            let (text, label) = read_deck(&deck)?;
            let db = Arc::new(cards::core());
            let list = deckstats::parse(&text)?;
            let res = list.resolve(&db);
            if !res.unresolved.is_empty() {
                for u in &res.unresolved {
                    eprintln!("line {}: unknown card {:?} (ignored)", u.entry.line, u.entry.name);
                }
            }
            let stats = deckstats::Stats::compute(&res.deck, &db);
            println!("{label}");
            print!("{}", deckstats::stats::render(&stats, &f.name));
            if hands > 0 && res.deck.len() >= 7 {
                println!("\nsample opening hands:");
                for hand in deckstats::sample_hands(&res.deck, &db, &f, hands, seed) {
                    println!("  {}", hand.join(", "));
                }
            }
            Ok(())
        }
    }
}

pub fn check_text(text: &str, format: &Format, db: &engine::CardDb, known: Option<&dyn KnownCards>) -> Result<CheckReport> {
    let list = deckstats::parse(text)?;
    Ok(deckstats::check::check(&list, format, db, known))
}

pub fn render_check(report: &CheckReport, label: &str, format: &Format) -> String {
    let mut out = String::new();
    let total: u32 = report.lines.iter().map(|l| l.count).sum();
    if report.is_legal() {
        out.push_str(&format!("{label}: {total} cards, legal in {}\n", format.name));
        return out;
    }
    out.push_str(&format!(
        "{label}: {total} cards, {} problem(s) in {}\n",
        report.problems(),
        format.name
    ));
    for v in &report.deck {
        out.push_str(&format!("  - {v}\n"));
    }
    for l in report.lines.iter().filter(|l| l.status != CardStatus::Ok) {
        out.push_str(&format!("  line {:>3}: {} {} — {}\n", l.line, l.count, l.name, l.status));
    }
    out
}

fn skeleton(name: &str, format_name: &str, f: &Format) -> String {
    let size = match f.deck.size {
        engine::format::DeckSize::Exact(n) => format!("exactly {n} cards"),
        engine::format::DeckSize::Min(n) => format!("at least {n} cards"),
        engine::format::DeckSize::Range(lo, hi) => format!("{lo}–{hi} cards"),
    };
    format!(
        "// {name} — a {} deck ({size}{}).\n\
         // One card per line as `N Card Name`; `//` starts a comment.\n\
         // Check it with `manaline deck check {name}.txt --format {format_name}`.\n\
         Deck\n\
         // 17 Forest\n\
         // 4 Grizzly Bears\n\
         \n\
         Sideboard\n",
        f.name,
        if f.deck.singleton { ", singleton" } else { "" }
    )
}

pub fn cards_cmd(cmd: CardsCommand) -> Result<()> {
    match cmd {
        CardsCommand::Update => {
            let dir = carddb::cache_dir().ok_or_else(|| anyhow!("no cache directory on this system"))?;
            println!("downloading Scryfall Oracle Cards into {} …", dir.display());
            let cache = carddb::Cache::update(&dir)?;
            println!("{} cards; {}", cache.len(), cache.age_text());
            Ok(())
        }
        CardsCommand::Show { name } => {
            let name = name.join(" ");
            if name.trim().is_empty() {
                bail!("give a card name");
            }
            let db = cards::core();
            let cache = carddb::Cache::load().ok().flatten();
            let mut shown = false;
            if let Some(id) = db.lookup(&name) {
                let c = db.get(id);
                let pt = c.pt.map(|(p, t)| format!("  {p}/{t}")).unwrap_or_default();
                let types: Vec<String> = c.types.iter().map(|t| capitalize(t.word())).collect();
                let mut line = types.join(" ");
                if !c.subtypes.is_empty() {
                    line.push_str(" — ");
                    line.push_str(&c.subtypes.join(" "));
                }
                println!("{} {}{pt}\n{line}\n{}\n", c.name, c.cost, c.text);
                println!("implemented: yes (core set)");
                shown = true;
            }
            match &cache {
                Some(cache) => match cache.get(&name) {
                    Some(m) => {
                        if !shown {
                            let pt = match (&m.power, &m.toughness) {
                                (Some(p), Some(t)) => format!("  {p}/{t}"),
                                _ => String::new(),
                            };
                            println!("{} {}{pt}\n{}\n{}\n", m.name, m.mana_cost, m.type_line, m.oracle_text);
                            println!("implemented: no (the engine cannot play this card yet)");
                        }
                        println!(
                            "printing: {} ({}) #{}, {}, art by {}",
                            m.set_name,
                            m.set.to_uppercase(),
                            m.collector_number,
                            m.rarity,
                            m.artist
                        );
                        let legal: Vec<String> = m
                            .legalities
                            .iter()
                            .filter(|(_, v)| v.as_str() == "legal")
                            .map(|(k, _)| k.clone())
                            .collect();
                        println!("legal in: {}", if legal.is_empty() { "nothing".into() } else { legal.join(", ") });
                        println!("{}", cache.age_text());
                    }
                    None if !shown => bail!("no card named {name:?} in the card data or the core set"),
                    None => {}
                },
                None if !shown => {
                    bail!("no card named {name:?} in the core set; no Scryfall data cached (`manaline cards update`)")
                }
                None => println!("no Scryfall data cached (`manaline cards update`)"),
            }
            Ok(())
        }
    }
}

pub fn ingest(cmd: IngestCommand) -> Result<()> {
    match cmd {
        IngestCommand::Roundtrip { dir } => {
            let (mut report, loaded) = match dir {
                Some(d) => {
                    let r = ingest::roundtrip_dir(Path::new(&d)).with_context(|| format!("reading {}", d.display()))?;
                    let cards = ingest::load_dir(Path::new(&d))?;
                    (r, cards)
                }
                None => (ingest::roundtrip_core(), cards::core_ir()),
            };
            match carddb::Cache::load() {
                Ok(Some(cache)) => report.check_oracle(&loaded, &cache),
                _ => {
                    eprintln!("no Scryfall data cached: skipping the Oracle text comparison (`manaline cards update`)")
                }
            }
            print!("{}", report.render());
            if report.is_clean() {
                Ok(())
            } else {
                std::process::exit(1)
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
