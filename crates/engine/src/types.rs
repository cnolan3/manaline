//! Core value types shared by every part of the engine.
//!
//! Nothing here assumes two players, a fixed life total, or a single machine.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A seat at the table. Seats are indices; no type anywhere names "player 1/2".
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Seat(pub u8);

impl<'de> Deserialize<'de> for Seat {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Seat, D::Error> {
        Ok(Seat(deserialize_int_or_string(d)? as u8))
    }
}

/// Integers used as JSON map keys arrive as strings, and serde's `flatten`
/// routes them through a buffer that keeps them that way. Accept both.
fn deserialize_int_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = u64;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an integer or a numeric string")
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u64, E> {
            u64::try_from(v).map_err(|_| E::custom("negative id"))
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u64, E> {
            v.parse().map_err(|_| E::custom(format!("{v:?} is not a number")))
        }
    }
    d.deserialize_any(V)
}

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
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ObjectId(pub u32);

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ObjectId, D::Error> {
        Ok(ObjectId(deserialize_int_or_string(d)? as u32))
    }
}

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

pub use cardir::{CardType, Color, Keyword, ManaCost, Supertype};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_order() {
        assert_eq!(Phase::Untap.next(), Some(Phase::Upkeep));
        assert_eq!(Phase::Cleanup.next(), None);
        assert!(!Phase::Untap.has_priority());
        assert!(Phase::Main2.is_main());
    }

    #[test]
    fn ids_deserialize_from_integers_and_strings() {
        let s: Seat = serde_json::from_str("3").unwrap();
        assert_eq!(s, Seat(3));
        let s: Seat = serde_json::from_str("\"3\"").unwrap();
        assert_eq!(s, Seat(3));
        let m: std::collections::BTreeMap<ObjectId, i32> = serde_json::from_str(r#"{"12":4}"#).unwrap();
        assert_eq!(m[&ObjectId(12)], 4);
        assert!(serde_json::from_str::<Seat>("\"x\"").is_err());
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
