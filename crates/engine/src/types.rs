//! Core value types shared by every part of the engine.
//!
//! Nothing here assumes two players, a fixed life total, or a single machine.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A seat at the table. Seats are indices; no type anywhere names "player 1/2".
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seat(pub u8);

impl Seat {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for Seat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seat {}", self.0)
    }
}

/// Identity of a game object (card, token, ability) for the life of the game.
/// Shown to humans and agents as `#12`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub u32);

impl ObjectId {
    pub fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.index())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Zone {
    Library,
    Hand,
    Battlefield,
    Graveyard,
    Exile,
    Stack,
    Command,
    /// Objects owned by a player who has left the game (rule 800.4a).
    OutOfGame,
}

impl Zone {
    /// Public zones are visible to every seat and to spectators.
    pub fn is_public(self) -> bool {
        matches!(
            self,
            Zone::Battlefield | Zone::Graveyard | Zone::Exile | Zone::Stack | Zone::Command
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Untap,
    Upkeep,
    Draw,
    Main1,
    BeginCombat,
    DeclareAttackers,
    DeclareBlockers,
    CombatDamage,
    EndCombat,
    Main2,
    End,
    Cleanup,
}

impl Phase {
    pub const ALL: [Phase; 12] = [
        Phase::Untap,
        Phase::Upkeep,
        Phase::Draw,
        Phase::Main1,
        Phase::BeginCombat,
        Phase::DeclareAttackers,
        Phase::DeclareBlockers,
        Phase::CombatDamage,
        Phase::EndCombat,
        Phase::Main2,
        Phase::End,
        Phase::Cleanup,
    ];

    /// The step that follows this one in the same turn, or `None` after cleanup.
    pub fn next(self) -> Option<Phase> {
        let i = Phase::ALL.iter().position(|&p| p == self)?;
        Phase::ALL.get(i + 1).copied()
    }

    pub fn is_main(self) -> bool {
        matches!(self, Phase::Main1 | Phase::Main2)
    }

    pub fn is_combat(self) -> bool {
        matches!(
            self,
            Phase::BeginCombat
                | Phase::DeclareAttackers
                | Phase::DeclareBlockers
                | Phase::CombatDamage
                | Phase::EndCombat
        )
    }

    /// Players receive priority in every step except untap and cleanup (rule 500.3, 514.3).
    pub fn has_priority(self) -> bool {
        !matches!(self, Phase::Untap | Phase::Cleanup)
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Untap => "Untap",
            Phase::Upkeep => "Upkeep",
            Phase::Draw => "Draw",
            Phase::Main1 => "Main 1",
            Phase::BeginCombat => "Begin Combat",
            Phase::DeclareAttackers => "Declare Attackers",
            Phase::DeclareBlockers => "Declare Blockers",
            Phase::CombatDamage => "Combat Damage",
            Phase::EndCombat => "End Combat",
            Phase::Main2 => "Main 2",
            Phase::End => "End",
            Phase::Cleanup => "Cleanup",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    White,
    Blue,
    Black,
    Red,
    Green,
}

impl Color {
    pub const ALL: [Color; 5] = [Color::White, Color::Blue, Color::Black, Color::Red, Color::Green];

    pub fn symbol(self) -> char {
        match self {
            Color::White => 'W',
            Color::Blue => 'U',
            Color::Black => 'B',
            Color::Red => 'R',
            Color::Green => 'G',
        }
    }

    pub fn from_symbol(c: char) -> Option<Color> {
        match c.to_ascii_uppercase() {
            'W' => Some(Color::White),
            'U' => Some(Color::Blue),
            'B' => Some(Color::Black),
            'R' => Some(Color::Red),
            'G' => Some(Color::Green),
            _ => None,
        }
    }
}

/// One unit of mana in a pool or a payment.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mana {
    Colored(Color),
    Colorless,
}

impl Mana {
    pub const ALL: [Mana; 6] = [
        Mana::Colored(Color::White),
        Mana::Colored(Color::Blue),
        Mana::Colored(Color::Black),
        Mana::Colored(Color::Red),
        Mana::Colored(Color::Green),
        Mana::Colorless,
    ];

    fn slot(self) -> usize {
        match self {
            Mana::Colored(c) => c as usize,
            Mana::Colorless => 5,
        }
    }

    pub fn symbol(self) -> char {
        match self {
            Mana::Colored(c) => c.symbol(),
            Mana::Colorless => 'C',
        }
    }
}

impl fmt::Display for Mana {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.symbol())
    }
}

/// A player's mana pool: a count per mana kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManaPool {
    amounts: [u8; 6],
}

impl ManaPool {
    pub fn get(&self, m: Mana) -> u8 {
        self.amounts[m.slot()]
    }

    pub fn add(&mut self, m: Mana, n: u8) {
        self.amounts[m.slot()] = self.amounts[m.slot()].saturating_add(n);
    }

    /// Remove `n` of `m`; returns false (and changes nothing) if the pool holds fewer.
    pub fn remove(&mut self, m: Mana, n: u8) -> bool {
        if self.amounts[m.slot()] < n {
            return false;
        }
        self.amounts[m.slot()] -= n;
        true
    }

    pub fn total(&self) -> u32 {
        self.amounts.iter().map(|&a| a as u32).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn clear(&mut self) {
        self.amounts = [0; 6];
    }

    /// Every unit in the pool, one entry per unit, in W U B R G C order.
    pub fn units(&self) -> Vec<Mana> {
        let mut out = Vec::new();
        for m in Mana::ALL {
            for _ in 0..self.get(m) {
                out.push(m);
            }
        }
        out
    }
}

impl fmt::Display for ManaPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("-");
        }
        for m in self.units() {
            write!(f, "{m}")?;
        }
        Ok(())
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardType {
    Creature,
    Land,
    Instant,
    Sorcery,
    Artifact,
    Enchantment,
    Planeswalker,
}

impl CardType {
    pub fn is_permanent(self) -> bool {
        !matches!(self, CardType::Instant | CardType::Sorcery)
    }
}

/// A mana cost. v1 has no hybrid, phyrexian, or X symbols.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManaCost {
    pub generic: u8,
    pub pips: Vec<Color>,
}

impl ManaCost {
    /// Parse Oracle-style cost text such as `{1}{G}{G}`. The empty string is a free cost.
    pub fn parse(text: &str) -> Result<ManaCost, String> {
        let mut cost = ManaCost::default();
        let mut rest = text.trim();
        while !rest.is_empty() {
            let close = rest
                .find('}')
                .ok_or_else(|| format!("unterminated mana symbol in {text:?}"))?;
            if !rest.starts_with('{') {
                return Err(format!("expected '{{' in mana cost {text:?}"));
            }
            let sym = &rest[1..close];
            if let Ok(n) = sym.parse::<u8>() {
                cost.generic = cost.generic.saturating_add(n);
            } else if sym.len() == 1 {
                let c = Color::from_symbol(sym.chars().next().unwrap())
                    .ok_or_else(|| format!("unknown mana symbol {{{sym}}} in {text:?}"))?;
                cost.pips.push(c);
            } else {
                return Err(format!("unsupported mana symbol {{{sym}}} in {text:?}"));
            }
            rest = &rest[close + 1..];
        }
        cost.pips.sort();
        Ok(cost)
    }

    pub fn mana_value(&self) -> u32 {
        self.generic as u32 + self.pips.len() as u32
    }

    pub fn pips_of(&self, color: Color) -> u8 {
        self.pips.iter().filter(|&&c| c == color).count() as u8
    }

    pub fn is_free(&self) -> bool {
        self.generic == 0 && self.pips.is_empty()
    }
}

impl fmt::Display for ManaCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.generic > 0 || self.pips.is_empty() {
            write!(f, "{{{}}}", self.generic)?;
        }
        for c in &self.pips {
            write!(f, "{{{}}}", c.symbol())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_costs() {
        let c = ManaCost::parse("{1}{G}{G}").unwrap();
        assert_eq!(c.generic, 1);
        assert_eq!(c.pips, vec![Color::Green, Color::Green]);
        assert_eq!(c.mana_value(), 3);
        assert_eq!(c.to_string(), "{1}{G}{G}");
        assert!(ManaCost::parse("").unwrap().is_free());
        assert_eq!(ManaCost::parse("{3}{R}{R}").unwrap().mana_value(), 5);
        assert!(ManaCost::parse("{X}{R}").is_err());
    }

    #[test]
    fn phase_order() {
        assert_eq!(Phase::Untap.next(), Some(Phase::Upkeep));
        assert_eq!(Phase::Cleanup.next(), None);
        assert!(!Phase::Untap.has_priority());
        assert!(Phase::Main2.is_main());
    }

    #[test]
    fn pool_arithmetic() {
        let mut p = ManaPool::default();
        p.add(Mana::Colored(Color::Red), 2);
        assert_eq!(p.total(), 2);
        assert!(!p.remove(Mana::Colored(Color::Red), 3));
        assert!(p.remove(Mana::Colored(Color::Red), 2));
        assert!(p.is_empty());
        assert_eq!(p.to_string(), "-");
    }
}
