//! The card database the engine plays from. Every card is an IR card
//! (`cardir::Card`) plus the set it came from; tokens are synthesised from
//! their `TokenSpec` at runtime.

use crate::types::{CardType, Color, Keyword, ManaCost, Supertype};
use cardir::{Amount, Effect, TokenSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Index into a [`CardDb`]. Stable for the life of a database instance.
/// Ids at or above the database length are tokens local to one game.
pub type CardId = u32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardDef {
    pub name: String,
    pub cost: ManaCost,
    pub types: Vec<CardType>,
    #[serde(default)]
    pub supertypes: Vec<Supertype>,
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
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    /// Tokens have no card and are never in a library.
    #[serde(default)]
    pub token: bool,
    /// The card's colours (from its cost, or the token spec).
    #[serde(default)]
    pub colors: Vec<Color>,
    /// The behaviour the engine interprets.
    pub ir: Arc<cardir::Card>,
}

impl CardDef {
    pub fn from_ir(card: cardir::Card, set: &str) -> CardDef {
        CardDef {
            name: card.name.clone(),
            cost: card.cost.clone(),
            types: card.types.clone(),
            supertypes: card.supertypes.clone(),
            subtypes: card.subtypes.clone(),
            pt: card.pt,
            text: card.text.clone(),
            set: set.to_string(),
            keywords: card.keywords.clone(),
            token: false,
            colors: card.colors(),
            ir: Arc::new(card),
        }
    }

    /// A token as described by a `CreateToken` effect.
    pub fn token(spec: &TokenSpec) -> CardDef {
        let ir = cardir::Card {
            name: spec.name.clone(),
            cost: ManaCost::default(),
            types: spec.types.clone(),
            supertypes: Vec::new(),
            subtypes: spec.subtypes.clone(),
            pt: spec.pt,
            text: String::new(),
            keywords: spec.keywords.clone(),
            spell: None,
            enchant: None,
            equip: None,
            statics: Vec::new(),
            triggers: Vec::new(),
            activated: Vec::new(),
        };
        CardDef {
            name: spec.name.clone(),
            cost: ManaCost::default(),
            types: spec.types.clone(),
            supertypes: Vec::new(),
            subtypes: spec.subtypes.clone(),
            pt: spec.pt,
            text: String::new(),
            set: "token".into(),
            keywords: spec.keywords.clone(),
            token: true,
            colors: spec.colors.clone(),
            ir: Arc::new(ir),
        }
    }

    pub fn is_creature(&self) -> bool {
        self.types.contains(&CardType::Creature)
    }

    pub fn is_land(&self) -> bool {
        self.types.contains(&CardType::Land)
    }

    pub fn is_permanent(&self) -> bool {
        self.types.iter().any(|t| t.is_permanent())
    }

    pub fn is_basic(&self) -> bool {
        self.supertypes.contains(&Supertype::Basic)
    }

    pub fn is_aura(&self) -> bool {
        self.ir.is_aura()
    }

    pub fn is_equipment(&self) -> bool {
        self.ir.is_equipment()
    }

    /// A modal spell ("Choose one —").
    pub fn is_modal(&self) -> bool {
        self.ir.is_modal()
    }

    pub fn has_keyword(&self, k: Keyword) -> bool {
        self.keywords.contains(&k)
    }

    /// Colours this permanent's mana abilities can make, each once.
    pub fn produces(&self) -> Vec<Color> {
        let mut out: Vec<Color> = self
            .ir
            .mana_abilities()
            .flat_map(|a| a.effects.iter())
            .filter_map(|e| match e {
                Effect::AddMana { color: Some(c), .. } => Some(*c),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The mana abilities as `(colour, amount)`; `amount` is `None` when it
    /// depends on the board (evaluated by the engine at payment time).
    pub fn mana_abilities(&self) -> Vec<(Option<Color>, Option<i32>, usize)> {
        self.ir
            .activated
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_mana_ability())
            .flat_map(|(i, a)| {
                a.effects.iter().filter_map(move |e| match e {
                    Effect::AddMana { color, amount } => Some((
                        *color,
                        match amount {
                            Amount::Const(n) => Some(*n),
                            _ => None,
                        },
                        i,
                    )),
                    _ => None,
                })
            })
            .collect()
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

    /// Build from IR cards, validating each. One file per Oracle name, ever.
    pub fn from_ir(set: &str, cards: Vec<cardir::Card>) -> Result<CardDb, String> {
        let mut db = CardDb::default();
        for card in cards {
            cardir::validate(&card).map_err(|e| e.to_string())?;
            db.insert(CardDef::from_ir(card, set))?;
        }
        Ok(db)
    }

    /// Add a definition. Names are unique, case-insensitively.
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

    pub fn try_get(&self, id: CardId) -> Option<&CardDef> {
        self.cards.get(id as usize)
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
