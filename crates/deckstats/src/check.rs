//! `deck check`: every line of a deck file classified, plus the format's
//! deck-level violations.

use crate::parse::{Decklist, Section};
use engine::{CardDb, Format, LegalitySource, Violation};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardStatus {
    Ok,
    /// Not in the card database and not in the Scryfall data either.
    Unknown {
        suggestion: Option<String>,
    },
    /// A real card the engine has no IR for yet.
    NotImplemented,
    Banned,
    NotInPool,
    NotSingleton {
        count: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineReport {
    pub line: usize,
    pub count: u32,
    pub name: String,
    pub status: CardStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub lines: Vec<LineReport>,
    /// Deck-level problems (size, missing card data).
    pub deck: Vec<Violation>,
}

impl CheckReport {
    pub fn is_legal(&self) -> bool {
        self.deck.is_empty() && self.lines.iter().all(|l| l.status == CardStatus::Ok)
    }

    pub fn problems(&self) -> usize {
        self.deck.len() + self.lines.iter().filter(|l| l.status != CardStatus::Ok).count()
    }
}

/// Classify a deck against a format. `known` says whether a name is a real
/// card (the Scryfall cache); without it every unresolved name is unknown.
pub fn check(list: &Decklist, format: &Format, db: &CardDb, known: Option<&dyn KnownCards>) -> CheckReport {
    let res = list.resolve(db);
    let mut lines: Vec<LineReport> = Vec::new();
    for e in list.section(Section::Main) {
        lines.push(LineReport {
            line: e.line,
            count: e.count,
            name: e.name.clone(),
            status: CardStatus::Ok,
        });
    }
    for u in &res.unresolved {
        let exists = known.map(|k| k.is_card(&u.entry.name)).unwrap_or(false);
        let status = if exists {
            CardStatus::NotImplemented
        } else {
            CardStatus::Unknown {
                suggestion: u.suggestion.clone(),
            }
        };
        if let Some(l) = lines.iter_mut().find(|l| l.line == u.entry.line) {
            l.status = status;
        }
    }
    let legality = known.map(|k| k.as_legality());
    let violations = format.check_deck_with(&res.deck, db, legality);
    let mut deck = Vec::new();
    for v in violations {
        match &v {
            Violation::Banned { name } => mark(&mut lines, name, CardStatus::Banned),
            Violation::NotInPool { name } => mark(&mut lines, name, CardStatus::NotInPool),
            Violation::NotSingleton { name, count } => mark(&mut lines, name, CardStatus::NotSingleton { count: *count }),
            _ => deck.push(v),
        }
    }
    CheckReport { lines, deck }
}

fn mark(lines: &mut [LineReport], name: &str, status: CardStatus) {
    for l in lines
        .iter_mut()
        .filter(|l| l.name.eq_ignore_ascii_case(name) && l.status == CardStatus::Ok)
    {
        l.status = status.clone();
    }
}

/// What the checker needs from the Scryfall cache, so this crate does not
/// depend on it.
pub trait KnownCards {
    fn is_card(&self, name: &str) -> bool;
    fn as_legality(&self) -> &dyn LegalitySource;
}

impl std::fmt::Display for CardStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CardStatus::Ok => write!(f, "ok"),
            CardStatus::Unknown { suggestion: Some(s) } => write!(f, "unknown card (did you mean {s}?)"),
            CardStatus::Unknown { suggestion: None } => write!(f, "unknown card"),
            CardStatus::NotImplemented => write!(f, "not implemented yet (a real card, but the engine cannot play it)"),
            CardStatus::Banned => write!(f, "banned"),
            CardStatus::NotInPool => write!(f, "not legal in this format"),
            CardStatus::NotSingleton { count } => write!(f, "{count} copies in a singleton format"),
        }
    }
}
