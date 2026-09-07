//! Deck files and deck analysis (§4.5). The text deck format every tool
//! exports is the interchange format; parsing is lenient and resolution
//! against the card database suggests rather than guesses. Analysis is pure
//! functions over a resolved deck plus the card database, so the TUI, the
//! CLI, and the MCP server share one implementation.

pub mod check;
pub mod parse;
pub mod stats;

pub use check::{CardStatus, CheckReport, KnownCards, LineReport};
pub use parse::{parse, Decklist, Entry, ParseError, Resolution, Section, Unresolved};
pub use stats::{hypergeometric_at_least, sample_hands, Stats};
