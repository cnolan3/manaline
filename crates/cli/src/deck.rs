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
    /// Start a new deck as a commented skeleton in your decks directory (or at `--out`).
    New {
        /// A deck name; it becomes `<name>.txt` in your decks directory.
        name: String,
        #[arg(long, default_value = "cube")]
        format: String,
        /// Write to this file instead of your decks directory.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Open the deckbuilder on the new deck straight away.
        #[arg(long)]
        edit: bool,
    },
    /// Open the deckbuilder (three panes: search, deck, card/stats) on a deck.
    Edit {
        /// A deck name or a file path. By name you always edit your own copy in
        /// your decks directory: a shipped deck is copied there when you save,
        /// and a name nobody has yet starts from a skeleton.
        deck: String,
        #[arg(long, default_value = "cube")]
        format: String,
        /// Colour theme: default, mono, or high-contrast.
        #[arg(long)]
        theme: Option<String>,
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
    /// Search cards with Scryfall-style syntax: `t:creature c:g mv<=2 o:"draw a card"`.
    Search {
        query: Vec<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Include cards the engine cannot play yet (needs the Scryfall cache).
        #[arg(long)]
        all: bool,
    },
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

/// A deck as `(text, label)`, the label being the file it came from.
fn read_deck(spec: &str) -> Result<(String, String)> {
    let (text, path) = super::locate_deck(spec)?;
    Ok((text, path.display().to_string()))
}

/// What `deck edit` opens for `spec`: the file it saves to, the text it
/// starts from, and a note for the person if the two are not the same file.
///
/// An existing file path is edited in place. A name always saves to your own
/// copy (`cards::user_deck_path`), starting from the deck of that name
/// wherever it currently is, or from a skeleton if there is none. Nothing is
/// written until you save, so opening a shipped deck and quitting leaves no
/// copy behind.
fn edit_target(spec: &str, format_name: &str, f: &Format) -> Result<(PathBuf, String, Option<String>)> {
    let as_path = PathBuf::from(spec);
    if as_path.is_file() {
        let text = std::fs::read_to_string(&as_path).with_context(|| format!("reading {}", as_path.display()))?;
        return Ok((as_path, text, None));
    }
    let stem = spec.trim().trim_end_matches(".txt");
    let Some(mine) = cards::user_deck_path(spec) else {
        // A path that does not exist yet: it is created on save.
        let text = skeleton(as_path.file_stem().and_then(|s| s.to_str()).unwrap_or("deck"), format_name, f);
        return Ok((as_path, text, None));
    };
    match cards::deck_path(spec) {
        Some(p) if p == mine => {
            let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
            Ok((mine, text, None))
        }
        Some(p) => {
            let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
            let note = format!("editing your own copy of {}: it is saved to {}", p.display(), mine.display());
            Ok((mine, text, Some(note)))
        }
        None => {
            let note = format!("new deck {stem:?}: it is saved to {}", mine.display());
            Ok((mine, skeleton(stem, format_name, f), Some(note)))
        }
    }
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
        DeckCommand::New { name, format, out, edit } => {
            let f = super::load_format(Some(&format), 2)?;
            let path = match out {
                Some(p) => p,
                None => {
                    let Some(p) = cards::user_deck_path(&name) else {
                        bail!("{name:?} is not a deck name; give a plain name, or --out for a file path");
                    };
                    if let Some(existing) = cards::deck_path(&name) {
                        bail!(
                            "a deck named {name:?} already exists at {}; `manaline deck edit {name}` opens it",
                            existing.display()
                        );
                    }
                    p
                }
            };
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            }
            let text = skeleton(name.trim().trim_end_matches(".txt"), &format, &f);
            std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
            println!("wrote {}", path.display());
            if edit {
                return edit_deck(path, text, f, None);
            }
            Ok(())
        }
        DeckCommand::Edit { deck, format, theme } => {
            let f = super::load_format(Some(&format), 2)?;
            let (path, text, note) = edit_target(&deck, &format, &f)?;
            if let Some(note) = note {
                println!("{note}");
            }
            edit_deck(path, text, f, tui::theme_flag(theme.as_deref())?)
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

/// Open the deckbuilder on `text`, saving to `path` (which need not exist yet).
fn edit_deck(path: PathBuf, text: String, format: Format, theme: Option<String>) -> Result<()> {
    let mut settings = tui::settings::Settings::load();
    if let Some(t) = theme {
        settings.theme = t;
    }
    let theme = tui::theme::Theme::named(&settings.theme).unwrap_or_default();
    let db = Arc::new(cards::core());
    let known = carddb::Cache::load().ok().flatten().map(Arc::new);
    let index = Arc::new(match &known {
        Some(c) => cardsearch::Index::from_cache(c, &db),
        None => cardsearch::Index::from_db(&db),
    });
    let banner = match super::runtime()?.block_on(super::commands::ensure_mcp_server()) {
        Ok((m, started)) => Some(format!(
            "agent server {} at {} (`manaline mcp stop` ends it)",
            if started { "started" } else { "already running" },
            m.url
        )),
        Err(e) => Some(format!("no agent server: {e:#}")),
    };
    let setup = tui::editor::EditorSetup {
        banner,
        path: Some(path),
        text,
        format,
        db,
        index,
        known,
        theme,
    };
    super::runtime()?.block_on(tui::run_editor(setup))
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
    let copies = match f.deck.max_copies {
        Some(n) if !f.deck.singleton => format!(", up to {n} of a card"),
        _ => String::new(),
    };
    format!(
        "// {name} — a {} deck ({size}{}{copies}).\n\
         // One card per line as `N Card Name`; `//` starts a comment.\n\
         // Check it with `manaline deck check {name} --format {format_name}`.\n\
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
        CardsCommand::Search { query, limit, all } => {
            let db = cards::core();
            let index = cardsearch::Index::load(&db);
            let q = query.join(" ");
            let q = if all || !index.from_cache {
                q
            } else {
                format!("({q}) is:implemented")
            };
            let hits = index.query(&q, limit).map_err(|e| anyhow!("{e}"))?;
            for e in &hits {
                println!("{}", e.line());
            }
            if hits.is_empty() {
                println!("no cards match");
            } else if hits.len() == limit {
                println!("(first {limit}; raise --limit for more)");
            }
            if !index.from_cache {
                println!("searching the core set only: `manaline cards update` fetches every card");
            }
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
            let index = match &cache {
                Some(c) => cardsearch::Index::from_cache(c, &db),
                None => cardsearch::Index::from_db(&db),
            };
            if let Some(e) = index.get(&name) {
                for line in tui::cardbox::render(&tui::cardbox::CardFace::from_entry(e), 44) {
                    println!("{line}");
                }
                println!(
                    "implemented: {}",
                    if e.implemented {
                        "yes (core set)"
                    } else {
                        "no (the engine cannot play this card yet)"
                    }
                );
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

#[cfg(test)]
mod edit_target_tests {
    use super::*;

    fn cube() -> Format {
        Format::cube()
    }

    #[test]
    fn a_name_always_saves_to_your_own_copy() {
        // A shipped deck: starts from its text, saves to your copy. Whether a
        // note is shown depends only on whether those are already the same file.
        let (path, text, note) = edit_target("green", "cube", &cube()).unwrap();
        assert_eq!(path, cards::user_deck_path("green").unwrap());
        assert_eq!(text, cards::deck_text("green").unwrap());
        assert_eq!(note.is_some(), cards::deck_path("green") != Some(path.clone()));
        // `.txt` names the same deck and the same destination.
        let (p2, t2, _) = edit_target("green.txt", "cube", &cube()).unwrap();
        assert_eq!((p2, t2), (path, text));
    }

    #[test]
    fn a_new_name_starts_from_a_skeleton() {
        let name = "zz-no-such-deck-for-this-test";
        assert!(cards::deck_path(name).is_none(), "test needs a name nobody has");
        let (path, text, note) = edit_target(name, "cube", &cube()).unwrap();
        assert_eq!(path, cards::user_deck_path(name).unwrap());
        assert!(text.contains("Deck\n"), "{text}");
        assert!(text.contains(name), "the skeleton names the deck: {text}");
        assert!(note.unwrap().contains(&path.display().to_string()));
        assert!(!path.exists(), "nothing is written until you save");
    }

    #[test]
    fn an_existing_file_is_edited_in_place() {
        let dir = std::env::temp_dir().join(format!("manaline-edit-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mine.txt");
        std::fs::write(&file, "Deck\n4 Grizzly Bears\n").unwrap();
        let (path, text, note) = edit_target(file.to_str().unwrap(), "cube", &cube()).unwrap();
        assert_eq!(path, file);
        assert_eq!(text, "Deck\n4 Grizzly Bears\n");
        assert!(note.is_none(), "an explicit file needs no explanation");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
