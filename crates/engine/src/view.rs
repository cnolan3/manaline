//! What one seat is allowed to know (§3.7). Produced only by `Game::view` and
//! `Game::view_spectator`; structurally unable to carry another seat's hand or
//! any library's order.

use crate::action::{AttackTarget, Target};
use crate::card::CardId;
use crate::game::{ActReason, Outcome};
use crate::types::{CardType, Color, Keyword, ManaCost, ManaPool, ObjectId, Phase, Seat, Zone};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandView {
    Yours(Vec<ObjectId>),
    Hidden { count: u8 },
}

impl HandView {
    pub fn count(&self) -> usize {
        match self {
            HandView::Yours(v) => v.len(),
            HandView::Hidden { count } => *count as usize,
        }
    }
}

/// Libraries are always hidden, even your own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryView {
    pub count: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerView {
    pub seat: Seat,
    pub name: String,
    pub life: i32,
    pub eliminated: bool,
    pub hand: HandView,
    pub library: LibraryView,
    pub graveyard: Vec<ObjectId>,
    pub exile: Vec<ObjectId>,
    pub battlefield: Vec<ObjectId>,
    pub command: Vec<ObjectId>,
    pub commander_damage: BTreeMap<ObjectId, i32>,
    pub poison: u8,
    pub lands_played_this_turn: u8,
    /// `Some` only for `you`.
    pub mana_pool: Option<ManaPool>,
    /// Limited pool, `Some` only for `you` and only in limited formats.
    pub pool: Option<Vec<CardId>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectView {
    pub id: ObjectId,
    pub card: CardId,
    pub name: String,
    pub cost: ManaCost,
    pub types: Vec<CardType>,
    #[serde(default)]
    pub subtypes: Vec<String>,
    /// Oracle text.
    #[serde(default)]
    pub text: String,
    /// Colours this permanent's intrinsic mana ability makes (basic lands).
    #[serde(default)]
    pub produces: Vec<Color>,
    pub owner: Seat,
    pub controller: Seat,
    pub zone: Zone,
    pub tapped: bool,
    pub summoning_sick: bool,
    pub damage: i32,
    /// Effective power/toughness, present iff the object is a creature.
    pub pt: Option<(i32, i32)>,
    pub attacking: Option<AttackTarget>,
    pub blocking: Vec<ObjectId>,
    /// Only meaningful for cards in `you`'s hand: whether a legal cast/play exists right now.
    pub castable: bool,
    /// Keywords the object has right now (printed, granted, or until end of turn).
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    #[serde(default)]
    pub attached_to: Option<ObjectId>,
    #[serde(default)]
    pub token: bool,
    /// +1/+1 counters minus -1/-1 counters.
    #[serde(default)]
    pub counters: i32,
    /// Activated abilities as text, in ability order (equip last).
    #[serde(default)]
    pub abilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackObjectView {
    /// The spell card, or the source of an ability or trigger.
    pub object: ObjectId,
    pub name: String,
    pub controller: Seat,
    pub targets: Vec<Target>,
    /// "spell", "ability", "trigger", "equip"
    #[serde(default)]
    pub kind: String,
    /// What it will do, as text.
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameView {
    /// `None` for spectators.
    pub you: Option<Seat>,
    pub turn: u32,
    pub active_player: Seat,
    pub phase: Phase,
    pub priority: Option<Seat>,
    pub must_act: BTreeMap<Seat, ActReason>,
    pub state_version: u64,
    pub outcome: Option<Outcome>,
    /// Bottom to top.
    pub stack: Vec<StackObjectView>,
    pub players: Vec<PlayerView>,
    /// Objects in public zones, plus `you`'s hand.
    pub objects: BTreeMap<ObjectId, ObjectView>,
}

impl GameView {
    pub fn player(&self, seat: Seat) -> &PlayerView {
        &self.players[seat.index()]
    }

    pub fn object(&self, id: ObjectId) -> Option<&ObjectView> {
        self.objects.get(&id)
    }
}
