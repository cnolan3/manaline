//! The card set the engine plays from, plus the decks directory and the text
//! deck-file parser.
//!
//! Cards are embedded at build time; decks are read from disk at run time, so
//! a deck file dropped into a decks directory is available immediately and
//! on the same footing as the ones the project ships with (see `deck_dirs`).
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

// ----- decks -----
//
// Nothing distinguishes one deck from another: a deck is a `.txt` file in
// one of a few directories, found by name. The decks the project ships are
// simply the ones a package installs (or a fresh clone has), and a deck you
// write is a deck like any other, usable anywhere a deck name is accepted.
// An installed binary must find its decks with no configuration and no
// rebuild, so the directories are known here rather than passed in.

/// Where a person's own decks live: `~/.local/share/manaline/decks` (or the
/// platform equivalent). This is the writable directory, where saving a deck
/// puts it.
pub fn user_decks_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("manaline")
        .join("decks")
}

/// Where a package installs the shipped decks. Fixed when the *package* is
/// built — `MANALINE_DATA_DIR=/usr/share/manaline cargo build` — and the FHS
/// location otherwise, so an installed binary needs nothing set at run time.
fn system_decks_dir() -> PathBuf {
    Path::new(option_env!("MANALINE_DATA_DIR").unwrap_or("/usr/share/manaline")).join("decks")
}

/// The `decks/` directory of the checkout this binary was built from, for
/// running from source. On an installed machine it simply does not exist.
fn repo_decks_dir() -> PathBuf {
    // <repo>/crates/cards -> <repo>/decks, without a `../..` in the middle:
    // this path is shown to people whenever a deck is named.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate manifest dir is <repo>/crates/cards")
        .join("decks")
}

/// The directories a deck name is looked up in, first match wins:
///
/// 1. `$MANALINE_DECKS_DIR` on its own, when set: that directory and no other.
/// 2. Your own decks (`user_decks_dir`), so your edited copy of a shipped
///    deck shadows the installed one.
/// 3. The decks a package installed (`MANALINE_DATA_DIR`, else `/usr/share/manaline`).
/// 4. The source checkout this binary was built from, if it still exists.
pub fn deck_dirs() -> Vec<PathBuf> {
    if let Some(d) = std::env::var_os("MANALINE_DECKS_DIR") {
        return vec![PathBuf::from(d)];
    }
    vec![user_decks_dir(), system_decks_dir(), repo_decks_dir()]
}

/// `deck_dirs` as one line, for messages.
pub fn deck_dirs_text() -> String {
    deck_dirs().iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")
}

/// Deck names (the stems of `.txt` files) in one directory, sorted. Empty if
/// the directory is missing or unreadable.
pub fn decks_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
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

/// Every deck reachable by name, each name once, sorted.
pub fn deck_names() -> Vec<String> {
    let mut names: Vec<String> = deck_dirs().iter().flat_map(|d| decks_in(d)).collect();
    names.sort();
    names.dedup();
    names
}

/// The file name a deck name maps to, or `None` if it is not a plain name.
/// A trailing `.txt` is tolerated, so `green` and `green.txt` are the same
/// deck. Anything with a path in it is not a name, so callers fall through
/// to treating it as a path of their own.
fn deck_file_name(name: &str) -> Option<String> {
    let stem = name.trim().trim_end_matches(".txt");
    if stem.is_empty() || stem.contains('/') || stem.contains('\\') || stem.starts_with('.') {
        return None;
    }
    Some(format!("{stem}.txt"))
}

/// The file a deck name refers to: the first directory in `deck_dirs` that
/// has it.
pub fn deck_path(name: &str) -> Option<PathBuf> {
    let file = deck_file_name(name)?;
    deck_dirs().into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Where a deck of this name is *written*: your own copy in `user_decks_dir`,
/// whether or not it exists yet. Saving a shipped deck here makes it yours;
/// the installed one is left alone and yours shadows it from then on.
pub fn user_deck_path(name: &str) -> Option<PathBuf> {
    Some(user_decks_dir().join(deck_file_name(name)?))
}

/// The text of a deck by name, or `None` if no directory has such a deck.
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
        // The repository's own decks, deliberately not `deck_names()`: that
        // merges in whatever is in the developer's home directory, and a deck
        // they are writing there is not this repository's business.
        let dir = repo_decks_dir();
        let names = decks_in(&dir);
        // Guards against a directory that resolved to nothing, which would
        // otherwise make the loop below pass vacuously.
        assert!(
            names.contains(&"green".to_string()),
            "no starter decks in {}: found {names:?}",
            dir.display()
        );
        // Every deck in the directory must parse and name only real cards.
        // Whether one is *legal* in a given format is `deck check`'s business,
        // and CI runs it over the decks a fresh clone ships with; a deck you
        // are still writing should not fail the test suite for being unfinished.
        for name in &names {
            let text = std::fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap();
            parse_decklist(&text, &db).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn deck_lookup_is_layered() {
        // Something in the search path resolves the starter decks.
        assert!(deck_path("green").is_some());
        if std::env::var_os("MANALINE_DECKS_DIR").is_some() {
            return; // the override replaces the search path wholesale; nothing below applies
        }
        // The search path has the shape the docs promise: yours, the
        // package's, the checkout's, in that order.
        let dirs = deck_dirs();
        assert_eq!(dirs, [user_decks_dir(), system_decks_dir(), repo_decks_dir()]);
        assert!(system_decks_dir().ends_with("decks"), "{:?}", system_decks_dir());
    }

    #[test]
    fn deck_names_are_not_paths() {
        assert!(deck_text("green").is_some());
        assert_eq!(deck_path("green.txt"), deck_path("green"), "a trailing .txt names the same deck");
        for bad in ["", ".", "..", "../Cargo", "a/b", "a\\b", ".hidden", ".txt"] {
            assert!(deck_path(bad).is_none(), "{bad:?} should not resolve to a deck");
            assert!(user_deck_path(bad).is_none(), "{bad:?} should not be a place to write");
        }
        // Writing by name always lands in your own directory, existing or not.
        assert_eq!(user_deck_path("brand-new"), Some(user_decks_dir().join("brand-new.txt")));
        assert_eq!(user_deck_path("green.txt"), Some(user_decks_dir().join("green.txt")));
    }

    #[test]
    fn lenient_parsing() {
        let db = core();
        let deck = parse_decklist("Deck\n4 Grizzly Bears (M19) 183 // bears\n2x forest\nSideboard\n3 Craw Wurm\n", &db).unwrap();
        assert_eq!(deck.len(), 6);
        assert!(parse_decklist("1 Black Lotus", &db).is_err());
    }
}
