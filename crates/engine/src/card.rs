//! The card database the engine plays from.
//!
//! In M0 this is a flat table of card definitions, enough for lands and vanilla
//! creatures. The card IR (`cardir`, M3) replaces `CardDef`'s behaviour fields;
//! the identity fields (name, cost, types, P/T) stay.

use crate::types::{CardType, Color, ManaCost};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Index into a [`CardDb`]. Stable for the life of a database instance.
pub type CardId = u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDef {
    pub name: String,
    pub cost: ManaCost,
    pub types: Vec<CardType>,
    #[serde(default)]
    pub subtypes: Vec<String>,
    /// Present iff the card is a creature.
    pub pt: Option<(i32, i32)>,
    /// Oracle text, for display.
    #[serde(default)]
    pub text: String,
    /// The set this definition belongs to, used by `CardPool::Builtin`.
    #[serde(default)]
    pub set: String,
    /// Basic lands are exempt from singleton rules and may appear in any number.
    #[serde(default)]
    pub basic: bool,
    /// Intrinsic mana ability: `{T}: Add {C}` for each listed colour. Basic lands
    /// list exactly one colour. Empty for everything else in M0.
    #[serde(default)]
    pub produces: Vec<Color>,
}

impl CardDef {
    pub fn is_creature(&self) -> bool {
        self.types.contains(&CardType::Creature)
    }

    pub fn is_land(&self) -> bool {
        self.types.contains(&CardType::Land)
    }

    pub fn is_permanent(&self) -> bool {
        self.types.iter().any(|t| t.is_permanent())
    }

    /// The colours of this card's mana cost.
    pub fn colors(&self) -> Vec<Color> {
        let mut cs: Vec<Color> = self.cost.pips.clone();
        cs.sort();
        cs.dedup();
        cs
    }
}

#[derive(Clone, Debug, Default)]
pub struct CardDb {
    cards: Vec<CardDef>,
    by_name: HashMap<String, CardId>,
}

impl CardDb {
    pub fn new(cards: Vec<CardDef>) -> Result<CardDb, String> {
        let mut db = CardDb::default();
        for card in cards {
            db.insert(card)?;
        }
        Ok(db)
    }

    /// Add a definition. Names are unique, case-insensitively: one file per Oracle name, ever.
    pub fn insert(&mut self, card: CardDef) -> Result<CardId, String> {
        let key = Self::key(&card.name);
        if self.by_name.contains_key(&key) {
            return Err(format!("duplicate card name {:?}", card.name));
        }
        if card.is_creature() != card.pt.is_some() {
            return Err(format!("{:?}: P/T must be present iff the card is a creature", card.name));
        }
        let id = self.cards.len() as CardId;
        self.by_name.insert(key, id);
        self.cards.push(card);
        Ok(id)
    }

    fn key(name: &str) -> String {
        name.trim().to_lowercase()
    }

    pub fn get(&self, id: CardId) -> &CardDef {
        &self.cards[id as usize]
    }

    pub fn lookup(&self, name: &str) -> Option<CardId> {
        self.by_name.get(&Self::key(name)).copied()
    }

    pub fn len(&self) -> usize {
        self.cards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (CardId, &CardDef)> {
        self.cards.iter().enumerate().map(|(i, c)| (i as CardId, c))
    }
}
