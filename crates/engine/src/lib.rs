//! manaline's pure rules engine: no I/O, no async, no networking.
//!
//! `Game` is a state machine driven by `apply`; `legal_actions` and `must_act`
//! are the contract every client builds on. See `docs/SPEC.md` §3.

pub mod action;
pub mod bot;
pub mod card;
pub mod error;
pub mod event;
pub mod format;
pub mod game;
pub mod objects;
pub mod testing;
pub mod text;
pub mod types;
pub mod view;

mod combat;
mod legal;
mod mana;
mod mulligan;
mod sba;
mod turn;

pub use action::{Action, AttackTarget, DamageTarget, ManaPayment, Target};
pub use card::{CardDb, CardDef, CardId};
pub use error::RulesError;
pub use event::{DrawnCards, Event, EventBase, EventView};
pub use format::{CardPool, Format, FormatRule, Violation};
pub use game::{
    ActReason, Elimination, Game, GameConfig, GameObject, Outcome, PendingChoice, PlayerSetup, PlayerState,
    StackObject,
};
pub use objects::Objects;
pub use types::{CardType, Color, Mana, ManaCost, ManaPool, ObjectId, Phase, Seat, Zone};
pub use view::{GameView, HandView, ObjectView, PlayerView, StackObjectView};
