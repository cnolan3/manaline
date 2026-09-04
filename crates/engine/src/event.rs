//! Events the engine emits, and the seat-filtered view of them (§3.7).
//!
//! `Event` and `EventView` are the same enum with one type parameter: the
//! payload of `Drew`. The only hidden information an event can carry is which
//! cards were drawn, so that is the only place the two types differ, and a
//! `Hidden { count }` payload is structurally incapable of naming a card.

use crate::action::{AttackTarget, DamageTarget, Target};
use crate::game::{Elimination, Outcome};
use crate::types::{Mana, ObjectId, Phase, Seat, Zone};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventBase<D> {
    GameStarted { starting_player: Seat, seats: u8 },
    Drew { seat: Seat, cards: D },
    Shuffled { seat: Seat },
    MulliganTaken { seat: Seat, to: u8 },
    HandKept { seat: Seat, size: u8 },
    Bottomed { seat: Seat, count: u8 },
    TurnStarted { turn: u32, active: Seat },
    PhaseChanged { phase: Phase },
    PriorityPassed { seat: Seat },
    LandPlayed { seat: Seat, object: ObjectId },
    Cast { seat: Seat, object: ObjectId, targets: Vec<Target> },
    Resolved { object: ObjectId },
    Tapped { object: ObjectId },
    Untapped { object: ObjectId },
    ManaAdded { seat: Seat, mana: Mana, amount: u8 },
    Attacked { seat: Seat, attackers: Vec<(ObjectId, AttackTarget)> },
    Blocked { seat: Seat, blocks: Vec<(ObjectId, ObjectId)> },
    DamageAssigned { attacker: ObjectId, assignments: Vec<(DamageTarget, i32)> },
    Damage { source: ObjectId, to: DamageTarget, amount: i32 },
    LifeChanged { seat: Seat, from: i32, to: i32 },
    /// Library→hand is never a `ZoneChange`; it is `Drew`. Hand→library is
    /// `MulliganTaken` / `Bottomed`. Every other transition is public.
    ZoneChange { object: ObjectId, from: Zone, to: Zone },
    Discarded { seat: Seat, objects: Vec<ObjectId> },
    Eliminated { seat: Seat, reason: Elimination },
    GameOver { outcome: Outcome },
    /// Table talk. The engine never produces this; the daemon injects it into
    /// the stream so the type lives with the others.
    Chat { from: Seat, to: Option<Seat>, text: String },
}

/// The full event, as the engine emits it. Holds hidden information.
pub type Event = EventBase<Vec<ObjectId>>;

/// What one seat (or a spectator) is allowed to know about a draw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawnCards {
    Yours(Vec<ObjectId>),
    Hidden { count: u8 },
}

/// The event as seen from one seat.
pub type EventView = EventBase<DrawnCards>;

impl Event {
    /// Filter this event for `viewer` (`None` = spectator). Returns `None` when
    /// the viewer should not receive the event at all (private chat).
    pub fn view(&self, viewer: Option<Seat>) -> Option<EventView> {
        Some(match self {
            EventBase::Drew { seat, cards } => EventBase::Drew {
                seat: *seat,
                cards: if viewer == Some(*seat) {
                    DrawnCards::Yours(cards.clone())
                } else {
                    DrawnCards::Hidden { count: cards.len() as u8 }
                },
            },
            EventBase::Chat { from, to, text } => {
                if let Some(to) = to {
                    if viewer != Some(*from) && viewer != Some(*to) {
                        return None;
                    }
                }
                EventBase::Chat { from: *from, to: *to, text: text.clone() }
            }
            EventBase::GameStarted { starting_player, seats } => EventBase::GameStarted {
                starting_player: *starting_player,
                seats: *seats,
            },
            EventBase::Shuffled { seat } => EventBase::Shuffled { seat: *seat },
            EventBase::MulliganTaken { seat, to } => EventBase::MulliganTaken { seat: *seat, to: *to },
            EventBase::HandKept { seat, size } => EventBase::HandKept { seat: *seat, size: *size },
            EventBase::Bottomed { seat, count } => EventBase::Bottomed { seat: *seat, count: *count },
            EventBase::TurnStarted { turn, active } => EventBase::TurnStarted { turn: *turn, active: *active },
            EventBase::PhaseChanged { phase } => EventBase::PhaseChanged { phase: *phase },
            EventBase::PriorityPassed { seat } => EventBase::PriorityPassed { seat: *seat },
            EventBase::LandPlayed { seat, object } => EventBase::LandPlayed { seat: *seat, object: *object },
            EventBase::Cast { seat, object, targets } => EventBase::Cast {
                seat: *seat,
                object: *object,
                targets: targets.clone(),
            },
            EventBase::Resolved { object } => EventBase::Resolved { object: *object },
            EventBase::Tapped { object } => EventBase::Tapped { object: *object },
            EventBase::Untapped { object } => EventBase::Untapped { object: *object },
            EventBase::ManaAdded { seat, mana, amount } => EventBase::ManaAdded {
                seat: *seat,
                mana: *mana,
                amount: *amount,
            },
            EventBase::Attacked { seat, attackers } => EventBase::Attacked {
                seat: *seat,
                attackers: attackers.clone(),
            },
            EventBase::Blocked { seat, blocks } => EventBase::Blocked { seat: *seat, blocks: blocks.clone() },
            EventBase::DamageAssigned { attacker, assignments } => EventBase::DamageAssigned {
                attacker: *attacker,
                assignments: assignments.clone(),
            },
            EventBase::Damage { source, to, amount } => EventBase::Damage {
                source: *source,
                to: *to,
                amount: *amount,
            },
            EventBase::LifeChanged { seat, from, to } => EventBase::LifeChanged {
                seat: *seat,
                from: *from,
                to: *to,
            },
            EventBase::ZoneChange { object, from, to } => EventBase::ZoneChange {
                object: *object,
                from: *from,
                to: *to,
            },
            EventBase::Discarded { seat, objects } => EventBase::Discarded {
                seat: *seat,
                objects: objects.clone(),
            },
            EventBase::Eliminated { seat, reason } => EventBase::Eliminated {
                seat: *seat,
                reason: reason.clone(),
            },
            EventBase::GameOver { outcome } => EventBase::GameOver { outcome: *outcome },
        })
    }
}
