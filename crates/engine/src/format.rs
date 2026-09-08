//! Formats are data (§4.4): how the game is set up, which extra rules apply,
//! and which cards are allowed. Loaded from RON; the two initial formats and
//! Commander ship embedded in the binary.

use crate::card::{CardDb, CardId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerRange {
    pub min: u8,
    pub max: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MulliganRule {
    London { free_first: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeckSize {
    Exact(u16),
    Min(u16),
    Range(u16, u16),
}

/// Named `Deck` so RON files read `deck: Deck(size: ..., singleton: ...)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deck {
    pub size: DeckSize,
    pub singleton: bool,
    pub includes_commander: bool,
    /// Most copies of one card (basic lands excepted); `None` for no limit.
    #[serde(default)]
    pub max_copies: Option<u8>,
}

/// Each variant is a hook the engine grows a format-specific rule behind.
/// Only the empty list is honoured in M0; the rest are recognised so the
/// data files can exist now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatRule {
    Commander,
    CommanderDamage(i32),
    ColorIdentity,
    Poison(u8),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardPool {
    /// A named card set compiled into the binary; `"core"` is the starter cube.
    Builtin(String),
    /// A directory of IR files relative to the manaline data dir (custom/community sets).
    Dir(PathBuf),
    /// Scryfall `legalities[format] == "legal"`, joined at load.
    Scryfall {
        format: String,
    },
    /// Set codes.
    Sets(Vec<String>),
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Legality {
    pub pool: CardPool,
    #[serde(default)]
    pub banned: Vec<String>,
    #[serde(default)]
    pub allowed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Format {
    pub name: String,
    pub players: PlayerRange,
    pub starting_life: i32,
    pub starting_hand: u8,
    #[serde(default = "default_max_hand_size")]
    pub max_hand_size: u8,
    pub mulligan: MulliganRule,
    pub deck: Deck,
    #[serde(default)]
    pub rules: Vec<FormatRule>,
    pub legality: Legality,
}

fn default_max_hand_size() -> u8 {
    7
}

/// Deck rules that accept anything, for sample hands and scenarios.
pub fn format_deck_any() -> Deck {
    Deck {
        size: DeckSize::Min(0),
        singleton: false,
        includes_commander: false,
        max_copies: None,
    }
}

/// Where per-format legality comes from for a `CardPool::Scryfall` pool:
/// the Scryfall cache in `carddb`, or a table in a test. `None` means the
/// card is unknown to the source.
pub trait LegalitySource {
    fn legal_in(&self, card_name: &str, format: &str) -> Option<bool>;
}

impl<F: Fn(&str, &str) -> Option<bool>> LegalitySource for F {
    fn legal_in(&self, card_name: &str, format: &str) -> Option<bool> {
        self(card_name, format)
    }
}

/// One reason a deck is not legal in a format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Violation {
    TooFewCards {
        have: usize,
        need: usize,
    },
    TooManyCards {
        have: usize,
        max: usize,
    },
    NotSingleton {
        name: String,
        count: usize,
    },
    TooManyCopies {
        name: String,
        count: usize,
        max: usize,
    },
    Banned {
        name: String,
    },
    NotInPool {
        name: String,
    },
    UnsupportedPool {
        pool: String,
    },
    UnsupportedRule {
        rule: String,
    },
    /// The decklist could not be parsed (unknown card name, bad line).
    Unparsable {
        reason: String,
    },
    /// The pool needs the Scryfall card data and none is cached.
    NeedsCardData {
        format: String,
    },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::TooFewCards { have, need } => {
                write!(f, "deck has {have} cards, needs at least {need}")
            }
            Violation::TooManyCards { have, max } => {
                write!(f, "deck has {have} cards, at most {max} allowed")
            }
            Violation::NotSingleton { name, count } => {
                write!(f, "{name}: {count} copies in a singleton format")
            }
            Violation::TooManyCopies { name, count, max } => write!(f, "{name}: {count} copies, at most {max} allowed"),
            Violation::Banned { name } => write!(f, "{name} is banned"),
            Violation::NotInPool { name } => write!(f, "{name} is not in this format's card pool"),
            Violation::UnsupportedPool { pool } => {
                write!(f, "card pool {pool} is not supported yet")
            }
            Violation::UnsupportedRule { rule } => {
                write!(f, "format rule {rule} is not implemented yet")
            }
            Violation::Unparsable { reason } => write!(f, "could not read the decklist: {reason}"),
            Violation::NeedsCardData { format } => {
                write!(f, "legality in {format} needs the Scryfall card data: run `manaline cards update`")
            }
        }
    }
}

const CUBE_RON: &str = include_str!("../../../formats/cube.ron");
const TWO_PLAYER_RON: &str = include_str!("../../../formats/two-player.ron");
const FREE_FOR_ALL_RON: &str = include_str!("../../../formats/free-for-all.ron");
const COMMANDER_RON: &str = include_str!("../../../formats/commander.ron");

impl Format {
    pub fn from_ron(text: &str) -> Result<Format, String> {
        ron::from_str(text).map_err(|e| e.to_string())
    }

    /// Formats compiled into the binary, by file stem.
    pub fn builtin_names() -> &'static [&'static str] {
        &["cube", "two-player", "free-for-all", "commander"]
    }

    pub fn builtin(name: &str) -> Option<Format> {
        let text = match name {
            "cube" => CUBE_RON,
            "two-player" => TWO_PLAYER_RON,
            "free-for-all" => FREE_FOR_ALL_RON,
            "commander" => COMMANDER_RON,
            _ => return None,
        };
        Some(Format::from_ron(text).expect("embedded format file is valid"))
    }

    pub fn cube() -> Format {
        Format::builtin("cube").unwrap()
    }

    pub fn allows_player_count(&self, n: usize) -> bool {
        n >= self.players.min as usize && n <= self.players.max as usize
    }

    /// Rules the engine cannot honour yet. A format with any of these cannot start a game.
    pub fn unsupported_rules(&self) -> Vec<Violation> {
        self.rules
            .iter()
            .map(|r| Violation::UnsupportedRule { rule: format!("{r:?}") })
            .collect()
    }

    /// Check a deck against this format's construction rules and card pool,
    /// without card data (a Scryfall pool then reports `NeedsCardData`).
    pub fn check_deck(&self, deck: &[CardId], db: &CardDb) -> Vec<Violation> {
        self.check_deck_with(deck, db, None)
    }

    /// Check a deck, consulting `legality` for Scryfall-pool formats.
    pub fn check_deck_with(&self, deck: &[CardId], db: &CardDb, legality: Option<&dyn LegalitySource>) -> Vec<Violation> {
        let mut out = Vec::new();
        let n = deck.len();
        match self.deck.size {
            DeckSize::Exact(k) => {
                if n < k as usize {
                    out.push(Violation::TooFewCards { have: n, need: k as usize });
                } else if n > k as usize {
                    out.push(Violation::TooManyCards { have: n, max: k as usize });
                }
            }
            DeckSize::Min(k) => {
                if n < k as usize {
                    out.push(Violation::TooFewCards { have: n, need: k as usize });
                }
            }
            DeckSize::Range(lo, hi) => {
                if n < lo as usize {
                    out.push(Violation::TooFewCards {
                        have: n,
                        need: lo as usize,
                    });
                } else if n > hi as usize {
                    out.push(Violation::TooManyCards { have: n, max: hi as usize });
                }
            }
        }

        let mut counts: HashMap<CardId, usize> = HashMap::new();
        for &c in deck {
            *counts.entry(c).or_default() += 1;
        }
        let mut ids: Vec<CardId> = counts.keys().copied().collect();
        ids.sort();
        for id in ids {
            let card = db.get(id);
            let count = counts[&id];
            if self.deck.singleton && count > 1 && !card.is_basic() {
                out.push(Violation::NotSingleton {
                    name: card.name.clone(),
                    count,
                });
            } else if let Some(max) = self.deck.max_copies {
                if count > max as usize && !card.is_basic() {
                    out.push(Violation::TooManyCopies {
                        name: card.name.clone(),
                        count,
                        max: max as usize,
                    });
                }
            }
            let banned = self.legality.banned.iter().any(|b| b.eq_ignore_ascii_case(&card.name));
            if banned {
                out.push(Violation::Banned { name: card.name.clone() });
                continue;
            }
            let allowed = self.legality.allowed.iter().any(|a| a.eq_ignore_ascii_case(&card.name));
            if allowed {
                continue;
            }
            match &self.legality.pool {
                CardPool::All => {}
                CardPool::Builtin(set) => {
                    if &card.set != set {
                        out.push(Violation::NotInPool { name: card.name.clone() });
                    }
                }
                CardPool::Scryfall { format } => match legality {
                    None => {
                        if !out.iter().any(|v| matches!(v, Violation::NeedsCardData { .. })) {
                            out.push(Violation::NeedsCardData { format: format.clone() });
                        }
                    }
                    Some(src) => {
                        if src.legal_in(&card.name, format) != Some(true) {
                            out.push(Violation::NotInPool { name: card.name.clone() });
                        }
                    }
                },
                other => {
                    let pool = format!("{other:?}");
                    if !out
                        .iter()
                        .any(|v| matches!(v, Violation::UnsupportedPool { pool: p } if *p == pool))
                    {
                        out.push(Violation::UnsupportedPool { pool });
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_formats_parse() {
        for name in Format::builtin_names() {
            let f = Format::builtin(name).unwrap();
            assert!(f.starting_hand > 0, "{name}");
        }
        let cube = Format::cube();
        assert_eq!(cube.players, PlayerRange { min: 2, max: 2 });
        assert_eq!(cube.starting_life, 20);
        assert_eq!(cube.max_hand_size, 7);
        assert_eq!(cube.legality.pool, CardPool::Builtin("core".into()));
        let cmd = Format::builtin("commander").unwrap();
        assert_eq!(cmd.starting_life, 40);
        assert!(cmd.rules.contains(&FormatRule::CommanderDamage(21)));
    }
}
