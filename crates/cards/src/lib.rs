//! The card set the engine plays from, plus starter decklists and the text
//! deck-file parser.
//!
//! In M0 the "core" set is a hand-written table of basic lands and vanilla
//! creatures, enough for random bots and a human to play a whole game. From
//! M3 this crate loads card IR files instead; the loader's interface
//! (`core()` → `CardDb`) does not change.

use engine::{CardDb, CardDef, CardId, CardType, Color, ManaCost};

const SET: &str = "core";

fn basic(name: &str, color: Color) -> CardDef {
    CardDef {
        name: name.into(),
        cost: ManaCost::default(),
        types: vec![CardType::Land],
        subtypes: vec![name.into()],
        pt: None,
        text: format!("({{T}}: Add {{{}}}.)", color.symbol()),
        set: SET.into(),
        basic: true,
        produces: vec![color],
    }
}

fn creature(name: &str, cost: &str, subtypes: &str, power: i32, toughness: i32) -> CardDef {
    CardDef {
        name: name.into(),
        cost: ManaCost::parse(cost).unwrap_or_else(|e| panic!("{name}: {e}")),
        types: vec![CardType::Creature],
        subtypes: subtypes.split_whitespace().map(String::from).collect(),
        pt: Some((power, toughness)),
        text: String::new(),
        set: SET.into(),
        basic: false,
        produces: Vec::new(),
    }
}

/// Every card definition in the built-in "core" set.
pub fn core_cards() -> Vec<CardDef> {
    vec![
        basic("Plains", Color::White),
        basic("Island", Color::Blue),
        basic("Swamp", Color::Black),
        basic("Mountain", Color::Red),
        basic("Forest", Color::Green),
        // White
        creature("Savannah Lions", "{W}", "Cat", 2, 1),
        creature("Devoted Hero", "{W}", "Elf Soldier", 1, 2),
        creature("Eager Cadet", "{W}", "Human Soldier", 1, 1),
        creature("Glory Seeker", "{1}{W}", "Human Soldier", 2, 2),
        creature("Oreskos Swiftclaw", "{1}{W}", "Cat Warrior", 3, 1),
        creature("Pearled Unicorn", "{2}{W}", "Unicorn", 2, 2),
        creature("Regal Unicorn", "{2}{W}", "Unicorn", 2, 3),
        creature("Alaborn Trooper", "{2}{W}", "Human Soldier", 2, 3),
        // Blue
        creature("Merfolk of the Pearl Trident", "{U}", "Merfolk", 1, 1),
        creature("Fugitive Wizard", "{U}", "Human Wizard", 1, 1),
        creature("Coral Merfolk", "{1}{U}", "Merfolk", 2, 1),
        creature("Vodalian Soldiers", "{1}{U}", "Merfolk Soldier", 1, 2),
        creature("Horned Turtle", "{2}{U}", "Turtle", 1, 4),
        // Black
        creature("Muck Rats", "{B}", "Rat", 1, 1),
        creature("Walking Corpse", "{1}{B}", "Zombie", 2, 2),
        creature("Scathe Zombies", "{2}{B}", "Zombie", 2, 2),
        creature("Undead Minotaur", "{3}{B}", "Zombie Minotaur", 2, 3),
        creature("Zombie Goliath", "{4}{B}", "Zombie Giant", 4, 3),
        // Red
        creature("Goblin Piker", "{1}{R}", "Goblin Warrior", 2, 1),
        creature("Gray Ogre", "{2}{R}", "Ogre", 2, 2),
        creature("Balduvian Barbarians", "{1}{R}{R}", "Human Barbarian", 3, 2),
        creature("Hill Giant", "{3}{R}", "Giant", 3, 3),
        creature("Canyon Minotaur", "{3}{R}", "Minotaur", 3, 3),
        creature("Borderland Minotaur", "{3}{R}", "Minotaur Warrior", 4, 3),
        creature("Fire Elemental", "{3}{R}{R}", "Elemental", 5, 4),
        // Green
        creature("Grizzly Bears", "{1}{G}", "Bear", 2, 2),
        creature("Runeclaw Bear", "{1}{G}", "Bear", 2, 2),
        creature("Terrain Elemental", "{1}{G}", "Elemental", 3, 2),
        creature("Elvish Warriors", "{G}{G}", "Elf Warrior", 2, 3),
        creature("Centaur Courser", "{2}{G}", "Centaur Warrior", 3, 3),
        creature("Alpine Grizzly", "{2}{G}", "Bear", 4, 2),
        creature("Rumbling Baloth", "{2}{G}{G}", "Beast", 4, 4),
        creature("Grizzled Outrider", "{4}{G}", "Elf Warrior", 5, 5),
        creature("Craw Wurm", "{4}{G}{G}", "Wurm", 6, 4),
    ]
}

/// The built-in "core" card database.
pub fn core() -> CardDb {
    CardDb::new(core_cards()).expect("core card table is consistent")
}

/// Starter decklists shipped in `decks/`, as `(name, text)`.
pub const DECKS: &[(&str, &str)] = &[
    ("m0-white", include_str!("../../../decks/m0-white.txt")),
    ("m0-blue", include_str!("../../../decks/m0-blue.txt")),
    ("m0-black", include_str!("../../../decks/m0-black.txt")),
    ("m0-red", include_str!("../../../decks/m0-red.txt")),
    ("m0-green", include_str!("../../../decks/m0-green.txt")),
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
    fn core_loads_and_decks_parse() {
        let db = core();
        assert!(db.len() > 30);
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
