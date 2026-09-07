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
    ("wu-fliers", include_str!("../../../decks/wu-fliers.txt")),
    ("ub-control", include_str!("../../../decks/ub-control.txt")),
    ("br-goblins", include_str!("../../../decks/br-goblins.txt")),
    ("rg-stompy", include_str!("../../../decks/rg-stompy.txt")),
    ("gw-elves", include_str!("../../../decks/gw-elves.txt")),
    ("wb-lifegain", include_str!("../../../decks/wb-lifegain.txt")),
];

pub fn deck_text(name: &str) -> Option<&'static str> {
    DECKS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
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
