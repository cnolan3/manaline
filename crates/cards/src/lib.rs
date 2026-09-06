//! The card set the engine plays from, plus starter decklists and the text
//! deck-file parser.
//!
//! In M0 the "core" set is a hand-written table of basic lands and vanilla
//! creatures, enough for random bots and a human to play a whole game. From
//! M3 this crate loads card IR files instead; the loader's interface
//! (`core()` → `CardDb`) does not change.

use engine::{CardDb, CardId};
use include_dir::{include_dir, Dir};

/// The card files, embedded at build time. One `.ron` per Oracle name.
static CORE: Dir = include_dir!("$CARGO_MANIFEST_DIR/data/core");

/// Every card in the built-in "core" set as IR, in file-name order.
pub fn core_ir() -> Vec<cardir::Card> {
    let mut files: Vec<_> = CORE.files().filter(|f| f.path().extension().is_some_and(|e| e == "ron")).collect();
    files.sort_by_key(|f| f.path().to_path_buf());
    files
        .into_iter()
        .map(|f| {
            let text = f.contents_utf8().expect("card file is UTF-8");
            cardir::load(text).unwrap_or_else(|e| panic!("{}: {e}", f.path().display()))
        })
        .collect()
}

/// The built-in "core" card database.
pub fn core() -> CardDb {
    CardDb::from_ir("core", core_ir()).expect("core card set is consistent")
}

/// Starter decklists shipped in `decks/`, as `(name, text)`.
pub const DECKS: &[(&str, &str)] = &[
    ("white", include_str!("../../../decks/white.txt")),
    ("blue", include_str!("../../../decks/blue.txt")),
    ("black", include_str!("../../../decks/black.txt")),
    ("red", include_str!("../../../decks/red.txt")),
    ("green", include_str!("../../../decks/green.txt")),
];

pub fn deck_text(name: &str) -> Option<&'static str> {
    DECKS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// Parse the standard text deck format (§4.5): `N Name [(SET) number]` per
/// line, optional `Deck` / `Sideboard` / `Commander` headers, `//` comments,
/// `SB:` prefixes. Sideboard and commander sections are ignored for now.
/// Names match case-insensitively; unknown names are an error.
pub fn parse_decklist(text: &str, db: &CardDb) -> Result<Vec<CardId>, String> {
    let mut deck = Vec::new();
    let mut in_main = true;
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        match line.to_ascii_lowercase().as_str() {
            "deck" | "main" | "maindeck" => {
                in_main = true;
                continue;
            }
            "sideboard" | "commander" | "companion" => {
                in_main = false;
                continue;
            }
            _ => {}
        }
        if line.starts_with("SB:") {
            continue;
        }
        if !in_main {
            continue;
        }
        let (count, name) = match line.split_once(' ') {
            Some((n, rest)) if n.trim_end_matches('x').parse::<usize>().is_ok() => {
                (n.trim_end_matches('x').parse::<usize>().unwrap(), rest.trim())
            }
            _ => (1, line),
        };
        let name = strip_printing(name);
        let id = db
            .lookup(name)
            .ok_or_else(|| format!("line {}: unknown card {name:?}", lineno + 1))?;
        deck.extend(std::iter::repeat_n(id, count));
    }
    Ok(deck)
}

/// Drop a trailing `(SET) 123` printing reference.
fn strip_printing(name: &str) -> &str {
    if let Some(open) = name.rfind(" (") {
        if name[open..].contains(')') {
            return name[..open].trim();
        }
    }
    name.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_card_round_trips_to_its_oracle_text() {
        let mut failures = Vec::new();
        for card in core_ir() {
            if let Err((want, got)) = cardir::round_trips(&card) {
                failures.push(format!("{}\n  oracle:   {want}\n  rendered: {got}", card.name));
            }
        }
        assert!(failures.is_empty(), "{} card(s) do not round-trip:\n{}", failures.len(), failures.join("\n"));
    }

    #[test]
    fn core_loads_and_decks_parse() {
        let db = core();
        assert!(db.len() > 70, "{}", db.len());
        for (name, text) in DECKS {
            let deck = parse_decklist(text, &db).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(deck.len(), 40, "{name}");
        }
    }

    #[test]
    fn lenient_parsing() {
        let db = core();
        let deck = parse_decklist("Deck\n4 Grizzly Bears (M19) 183 // bears\n2x forest\nSideboard\n3 Craw Wurm\n", &db).unwrap();
        assert_eq!(deck.len(), 6);
        assert!(parse_decklist("1 Black Lotus", &db).is_err());
    }
}
