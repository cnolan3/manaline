//! The card set the engine plays from, plus the decks directory and the text
//! deck-file parser.
//!
//! Cards are embedded at build time; decks are read from disk at run time, so
//! a deck file dropped into the decks directory is available immediately and
//! on the same footing as the ones the repository ships with (see `decks_dir`).
//!
//! In M0 the "core" set is a hand-written table of basic lands and vanilla
//! creatures, enough for random bots and a human to play a whole game. From
//! M3 this crate loads card IR files instead; the loader's interface
//! (`core()` → `CardDb`) does not change.

use engine::{CardDb, CardId};
use include_dir::{include_dir, Dir};
use std::path::{Path, PathBuf};

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

/// Where deck files live: `$MANALINE_DECKS_DIR`, else the repository's
/// `decks/` directory, located relative to this crate at compile time.
///
/// Nothing distinguishes one deck from another. The decks the repository
/// ships with are simply the files that are already in this directory on a
/// fresh clone; a deck you write into it is a deck like any other, usable
/// anywhere a deck name is accepted.
pub fn decks_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("MANALINE_DECKS_DIR") {
        return PathBuf::from(d);
    }
    // <repo>/crates/cards -> <repo>/decks, without a `../..` in the middle:
    // this path is shown to people whenever a deck is named.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate manifest dir is <repo>/crates/cards")
        .join("decks")
}

/// Every deck in the decks directory, by name (the file stem), sorted.
/// Empty if the directory is missing or unreadable.
pub fn deck_names() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(decks_dir()) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "txt"))
        .filter_map(|p| p.file_stem()?.to_str().map(str::to_string))
        .collect();
    names.sort();
    names
}

/// The file a deck name refers to, if that deck exists.
pub fn deck_path(name: &str) -> Option<PathBuf> {
    // A deck name is a plain file stem. Anything with a path in it is not a
    // name, so callers fall through to treating it as a path of their own.
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return None;
    }
    let path = decks_dir().join(format!("{name}.txt"));
    path.is_file().then_some(path)
}

/// The text of a deck by name, or `None` if the decks directory has no such deck.
pub fn deck_text(name: &str) -> Option<String> {
    std::fs::read_to_string(deck_path(name)?).ok()
}

/// Parse a deck file (§4.5, see `deckstats::parse`) and resolve its main
/// deck against `db`. Unknown names are an error naming each one, with a
/// suggestion when a card is close.
pub fn parse_decklist(text: &str, db: &CardDb) -> Result<Vec<CardId>, String> {
    let list = deckstats::parse(text).map_err(|e| e.to_string())?;
    let res = list.resolve(db);
    if !res.unresolved.is_empty() {
        let names: Vec<String> = res
            .unresolved
            .iter()
            .map(|u| match &u.suggestion {
                Some(s) => format!("line {}: unknown card {:?} (did you mean {s}?)", u.entry.line, u.entry.name),
                None => format!("line {}: unknown card {:?}", u.entry.line, u.entry.name),
            })
            .collect();
        return Err(names.join("; "));
    }
    Ok(res.deck)
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
        assert!(
            failures.is_empty(),
            "{} card(s) do not round-trip:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn core_loads_and_decks_parse() {
        let db = core();
        assert!(db.len() > 70, "{}", db.len());
        let names = deck_names();
        // Guards against a decks directory that resolved to nothing, which
        // would otherwise make the loop below pass vacuously.
        assert!(
            names.contains(&"green".to_string()),
            "no starter decks in {}: found {names:?}",
            decks_dir().display()
        );
        // Every deck in the directory must parse and name only real cards.
        // Whether one is *legal* in a given format is `deck check`'s business,
        // and CI runs it over the decks a fresh clone ships with; a deck you
        // are still writing should not fail the test suite for being unfinished.
        for name in &names {
            let text = deck_text(name).unwrap_or_else(|| panic!("{name} is listed but could not be read"));
            parse_decklist(&text, &db).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn deck_names_are_not_paths() {
        assert!(deck_text("green").is_some());
        assert!(deck_path("green").is_some());
        for bad in ["", ".", "..", "../Cargo", "a/b", "a\\b", ".hidden"] {
            assert!(deck_path(bad).is_none(), "{bad:?} should not resolve to a deck");
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
