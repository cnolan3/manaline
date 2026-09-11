//! The rules engine state machine (§3). Everything else in the project is a
//! consequence of this interface: `new`, `legal_actions`, `must_act`, `apply`,
//! `view`, `is_over`.

use crate::action::{Action, AttackTarget, DamageTarget, Target};
use crate::card::{CardDb, CardDef, CardId};
use crate::error::RulesError;
use crate::event::Event;
use crate::format::Format;
use crate::objects::Objects;
use crate::types::{Keyword, ManaPool, ObjectId, Phase, Seat, Zone};
use crate::view::{GameView, HandView, LibraryView, ObjectView, PlayerView, StackObjectView};
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct PlayerSetup {
    pub name: String,
    /// Already validated against the format; `Game::new` checks again.
    pub deck: Vec<CardId>,
}

#[derive(Clone)]
pub struct GameConfig {
    pub format: Format,
    /// One per seat, in seat order.
    pub players: Vec<PlayerSetup>,
    pub cards: Arc<CardDb>,
    /// `None` chooses at random from the seed (rule 103.1).
    pub starting_player: Option<Seat>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Elimination {
    LifeZero,
    Poison,
    DrewFromEmptyLibrary,
    Conceded,
}

impl Elimination {
    /// "conceded", "was reduced to 0 life", ...
    pub fn phrase(&self) -> &'static str {
        match self {
            Elimination::LifeZero => "was reduced to 0 life",
            Elimination::Poison => "took ten poison counters",
            Elimination::DrewFromEmptyLibrary => "drew from an empty library",
            Elimination::Conceded => "conceded",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Winner(Seat),
    Draw,
}

/// Why a seat must act right now. Computed once by the engine and carried
/// unchanged to the daemon's watch channel, the TUI header, and the MCP tool.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActReason {
    Priority,
    DeclareAttackers,
    DeclareBlockers,
    AssignDamage,
    Mulligan,
    BottomCards,
    Discard,
    Choice,
}

/// A decision the game is waiting on that is not "someone has priority".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingChoice {
    Mulligan {
        seat: Seat,
    },
    BottomCards {
        seat: Seat,
        count: u8,
    },
    DeclareAttackers {
        seat: Seat,
    },
    /// `seat` declares now; `remaining` declare afterwards, in APNAP order.
    DeclareBlockers {
        seat: Seat,
        remaining: Vec<Seat>,
    },
    /// `attacker` needs a damage division now; `queue` holds the rest.
    AssignDamage {
        seat: Seat,
        attacker: ObjectId,
        queue: Vec<ObjectId>,
    },
    /// Cleanup-step hand size.
    Discard {
        seat: Seat,
        count: u8,
    },
    /// A trigger needs targets before it goes on the stack.
    ChooseTargets {
        seat: Seat,
        specs: Vec<cardir::Filter>,
        trigger: crate::triggers::FiredTrigger,
    },
    /// A resolving effect asks `seat` to pick between `min` and `max` of
    /// `options` (what to sacrifice, discard, return, ...). Answered with
    /// `ChooseTargets`; the pick is bound to `bind` and the resolution continues.
    Choose {
        seat: Seat,
        options: Vec<Target>,
        min: usize,
        max: usize,
        /// The verb, for menus: "Sacrifice", "Discard", "Return".
        prompt: String,
        reason: ActReason,
        bind: String,
        resume: crate::stack::Continuation,
    },
    /// A resolving effect asks `seat` to pick one of `labels` ("you may":
    /// do it, or don't). Answered with `ChooseMode`.
    ChooseOption {
        seat: Seat,
        labels: Vec<String>,
        bind: String,
        resume: crate::stack::Continuation,
    },
    /// `seat` is casting `object` in steps: mana is paid; modes (answered
    /// with `ChooseMode`) and then each target spec (answered with
    /// `ChooseTargets`) are still being chosen. The card is still in hand.
    Casting {
        seat: Seat,
        object: ObjectId,
        /// Modes chosen so far, in order.
        modes: Vec<u8>,
        /// "One or both": the caster said they are done choosing modes.
        modes_done: bool,
        /// Targets chosen so far, flat, in spec order.
        targets: Vec<Target>,
        /// Index of the next spec to choose for.
        spec: usize,
        /// The value announced for `{X}`.
        x: u32,
    },
}

impl PendingChoice {
    pub fn seat(&self) -> Seat {
        match self {
            PendingChoice::Mulligan { seat }
            | PendingChoice::BottomCards { seat, .. }
            | PendingChoice::DeclareAttackers { seat }
            | PendingChoice::DeclareBlockers { seat, .. }
            | PendingChoice::AssignDamage { seat, .. }
            | PendingChoice::Discard { seat, .. }
            | PendingChoice::ChooseTargets { seat, .. }
            | PendingChoice::Choose { seat, .. }
            | PendingChoice::ChooseOption { seat, .. }
            | PendingChoice::Casting { seat, .. } => *seat,
        }
    }

    pub fn reason(&self) -> ActReason {
        match self {
            PendingChoice::Mulligan { .. } => ActReason::Mulligan,
            PendingChoice::BottomCards { .. } => ActReason::BottomCards,
            PendingChoice::DeclareAttackers { .. } => ActReason::DeclareAttackers,
            PendingChoice::DeclareBlockers { .. } => ActReason::DeclareBlockers,
            PendingChoice::AssignDamage { .. } => ActReason::AssignDamage,
            PendingChoice::Discard { .. } => ActReason::Discard,
            PendingChoice::Choose { reason, .. } => *reason,
            PendingChoice::ChooseTargets { .. } | PendingChoice::ChooseOption { .. } | PendingChoice::Casting { .. } => ActReason::Choice,
        }
    }
}

/// What an entry on the stack is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackKind {
    /// The card itself is on the stack.
    Spell,
    /// An activated ability of `source` (which stays where it is).
    Ability { source: ObjectId, index: u8 },
    /// The equip ability of an Equipment.
    Equip { source: ObjectId },
    /// A triggered ability of `source`.
    Trigger {
        source: ObjectId,
        index: u8,
        triggering: Option<Target>,
    },
    /// The prowess trigger.
    Prowess { source: ObjectId },
    /// A delayed trigger set up by an effect of `source`, run under the
    /// context (targets, bindings) that effect resolved with.
    Delayed {
        source: ObjectId,
        effects: Vec<cardir::Effect>,
        ctx: crate::filter::Ctx,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackObject {
    /// The spell card, or the source of an ability or trigger.
    pub object: ObjectId,
    pub controller: Seat,
    /// Flat, in spec order; grouped per spec at resolution.
    pub targets: Vec<Target>,
    pub kind: StackKind,
    /// A modal spell's chosen modes, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<u8>,
    /// The value announced for `{X}`.
    #[serde(default)]
    pub x: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    pub plus1: u8,
    pub minus1: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModifierKind {
    Pt {
        power: i32,
        toughness: i32,
    },
    Keyword(Keyword),
    /// "can't block this turn"
    Restriction(cardir::Restriction),
    /// "doesn't untap during its controller's next untap step"
    SkipUntap,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Expiry {
    EndOfTurn,
    /// Ends as this seat's next turn begins ("until your next turn").
    TurnOf(Seat),
    /// Consumed by this seat's next untap step.
    NextUntapOf(Seat),
}

impl Expiry {
    /// The expiry for an effect `you` created with this duration.
    pub fn from_duration(d: &cardir::Duration, you: Seat) -> Expiry {
        match d {
            cardir::Duration::EndOfTurn => Expiry::EndOfTurn,
            cardir::Duration::UntilYourNextTurn => Expiry::TurnOf(you),
        }
    }
}

/// An "until end of turn" style effect stored on the object it modifies (§3.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifier {
    pub kind: ModifierKind,
    pub expires: Expiry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameObject {
    pub id: ObjectId,
    pub card: CardId,
    pub owner: Seat,
    pub controller: Seat,
    pub zone: Zone,
    pub tapped: bool,
    pub summoning_sick: bool,
    pub damage: i32,
    pub counters: Counters,
    pub attached_to: Option<ObjectId>,
    pub attacking: Option<AttackTarget>,
    /// Attackers this creature is blocking.
    pub blocking: Vec<ObjectId>,
    /// Once blocked, an attacker stays blocked even if its blockers leave (rule 509.1h).
    pub blocked: bool,
    /// Blockers in the order they were declared.
    pub blocked_by: Vec<ObjectId>,
    pub modifiers: Vec<Modifier>,
    /// Took damage from a deathtouch source this turn (rule 704.5h).
    #[serde(default)]
    pub deathtouch_damaged: bool,
}

impl GameObject {
    pub(crate) fn new(id: ObjectId, card: CardId, owner: Seat) -> GameObject {
        GameObject {
            id,
            card,
            owner,
            controller: owner,
            zone: Zone::Library,
            tapped: false,
            summoning_sick: false,
            damage: 0,
            counters: Counters::default(),
            attached_to: None,
            attacking: None,
            blocking: Vec::new(),
            blocked: false,
            blocked_by: Vec::new(),
            modifiers: Vec::new(),
            deathtouch_damaged: false,
        }
    }

    /// An object that changes zones becomes a new object: nothing carries over.
    pub(crate) fn reset_zone_state(&mut self) {
        self.tapped = false;
        self.summoning_sick = false;
        self.damage = 0;
        self.counters = Counters::default();
        self.attached_to = None;
        self.attacking = None;
        self.blocking.clear();
        self.blocked = false;
        self.blocked_by.clear();
        self.modifiers.clear();
        self.deathtouch_damaged = false;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerState {
    pub name: String,
    pub life: i32,
    pub eliminated: Option<Elimination>,
    /// Index 0 is the bottom of the library; the last element is the top.
    pub library: Vec<ObjectId>,
    pub hand: Vec<ObjectId>,
    pub graveyard: Vec<ObjectId>,
    pub exile: Vec<ObjectId>,
    pub battlefield: Vec<ObjectId>,
    /// Command zone; empty outside commander formats.
    pub command: Vec<ObjectId>,
    /// Damage taken from each commander (rule 903.10a).
    pub commander_damage: BTreeMap<ObjectId, i32>,
    pub commander_casts: BTreeMap<ObjectId, u8>,
    pub mana_pool: ManaPool,
    pub lands_played_this_turn: u8,
    pub poison: u8,
    pub mulligans: u8,
    /// Set when a draw from an empty library was attempted; checked as a state-based action.
    pub drew_from_empty: bool,
}

/// Full game state, including every player's hidden information. Never leaves
/// the engine except through [`Game::view`] and [`Game::view_spectator`].
#[derive(Clone)]
pub struct Game {
    pub format: Format,
    pub turn: u32,
    /// Every seat in turn order from the starting player, eliminated seats included.
    pub seating: Vec<Seat>,
    /// Seats still in the game, in turn order from the starting player.
    pub turn_order: Vec<Seat>,
    pub active_player: Seat,
    pub phase: Phase,
    /// `None` during untap and cleanup, and while a `PendingChoice` is open.
    pub priority: Option<Seat>,
    /// Equals `turn_order.len()` with an empty stack → advance the step.
    pub passed_in_succession: u8,
    /// Bottom to top.
    pub stack: Vec<StackObject>,
    /// Indexed by seat; eliminated players stay (their objects still reference them).
    pub players: Vec<PlayerState>,
    pub objects: Objects,
    pub pending: Option<PendingChoice>,
    pub log: Vec<Event>,
    pub(crate) rng: ChaCha8Rng,
    pub(crate) cards: Arc<CardDb>,
    pub(crate) seed: u64,
    pub(crate) state_version: u64,
    pub(crate) history: Vec<(Seat, Action)>,
    pub(crate) outcome: Option<Outcome>,
    pub(crate) started: bool,
    /// Set when the active player leaves the game: the turn ends at the next opportunity.
    pub(crate) turn_aborted: bool,
    pub(crate) damage_assignments: BTreeMap<ObjectId, Vec<(DamageTarget, i32)>>,
    /// Tokens created this game; `CardId`s from `cards.len()` upwards index here.
    pub(crate) tokens: Vec<CardDef>,
    /// Triggers that fired but are not yet on the stack.
    pub(crate) fired: Vec<crate::triggers::FiredTrigger>,
    /// Delayed triggers waiting for their moment.
    pub(crate) delayed: Vec<crate::triggers::DelayedTrigger>,
    /// Events before this index have been scanned for triggers.
    pub(crate) trigger_cursor: usize,
    /// Which combat damage round is next / done.
    pub(crate) combat_round: CombatRound,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CombatRound {
    /// Not in the combat damage step.
    None,
    /// First-strike damage was dealt; regular damage is still to come.
    FirstStrikeDone,
    /// Regular damage dealt (or no first strike this combat).
    Done,
}

impl std::fmt::Debug for Game {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Game")
            .field("seed", &self.seed)
            .field("turn", &self.turn)
            .field("active_player", &self.active_player)
            .field("phase", &self.phase)
            .field("priority", &self.priority)
            .field("pending", &self.pending)
            .field("turn_order", &self.turn_order)
            .field("outcome", &self.outcome)
            .field("state_version", &self.state_version)
            .finish_non_exhaustive()
    }
}

impl Game {
    pub fn new(config: GameConfig, seed: u64) -> Result<Game, RulesError> {
        let GameConfig {
            format,
            players,
            cards,
            starting_player,
        } = config;
        let n = players.len();
        if !format.allows_player_count(n) {
            return Err(RulesError::setup(format!(
                "{} needs {}–{} players, got {n}",
                format.name, format.players.min, format.players.max
            )));
        }
        let unsupported = format.unsupported_rules();
        if !unsupported.is_empty() {
            let list: Vec<String> = unsupported.iter().map(ToString::to_string).collect();
            return Err(RulesError::setup(list.join("; ")));
        }
        if let Some(s) = starting_player {
            if s.index() >= n {
                return Err(RulesError::setup(format!("{s} is not at this table")));
            }
        }

        let mut objects = Objects::new();
        let mut states = Vec::with_capacity(n);
        for (i, p) in players.iter().enumerate() {
            let seat = Seat(i as u8);
            let violations = format.check_deck(&p.deck, &cards);
            if !violations.is_empty() {
                let list: Vec<String> = violations.iter().map(ToString::to_string).collect();
                return Err(RulesError::setup(format!(
                    "{}'s deck is not legal in {}: {}",
                    p.name,
                    format.name,
                    list.join("; ")
                )));
            }
            let library: Vec<ObjectId> = p
                .deck
                .iter()
                .map(|&card| objects.insert_with_key(|id| GameObject::new(id, card, seat)))
                .collect();
            states.push(PlayerState {
                name: p.name.clone(),
                life: format.starting_life,
                library,
                ..PlayerState::default()
            });
        }

        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let starting = starting_player.unwrap_or_else(|| Seat(rng.gen_range(0..n) as u8));
        let seating: Vec<Seat> = (0..n).map(|k| Seat(((starting.index() + k) % n) as u8)).collect();
        let starting_hand = format.starting_hand;

        let mut game = Game {
            format,
            turn: 0,
            seating: seating.clone(),
            turn_order: seating.clone(),
            active_player: starting,
            phase: Phase::Untap,
            priority: None,
            passed_in_succession: 0,
            stack: Vec::new(),
            players: states,
            objects,
            pending: None,
            log: Vec::new(),
            rng,
            cards,
            seed,
            state_version: 0,
            history: Vec::new(),
            outcome: None,
            started: false,
            turn_aborted: false,
            damage_assignments: BTreeMap::new(),
            tokens: Vec::new(),
            fired: Vec::new(),
            delayed: Vec::new(),
            trigger_cursor: 0,
            combat_round: CombatRound::None,
        };
        game.emit(Event::GameStarted {
            starting_player: starting,
            seats: n as u8,
        });
        for seat in seating {
            game.shuffle_library(seat);
            game.draw(seat, starting_hand as usize);
        }
        if starting_hand > 0 {
            game.pending = Some(PendingChoice::Mulligan { seat: game.seating[0] });
        }
        game.trigger_cursor = game.log.len();
        game.settle();
        Ok(game)
    }

    /// Reconstruct a game from its seed and action log (§3.6).
    pub fn replay(config: GameConfig, seed: u64, actions: &[(Seat, Action)]) -> Result<Game, RulesError> {
        let mut game = Game::new(config, seed)?;
        for (seat, action) in actions {
            game.apply(*seat, action)?;
        }
        Ok(game)
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn cards(&self) -> &CardDb {
        &self.cards
    }

    pub fn state_version(&self) -> u64 {
        self.state_version
    }

    /// Every accepted action, in order. With the seed, fully determines this game.
    pub fn history(&self) -> &[(Seat, Action)] {
        &self.history
    }

    pub fn is_over(&self) -> Option<Outcome> {
        self.outcome
    }

    /// Seats whose `legal_actions` is non-empty right now, and why. Empty iff the game is over.
    pub fn must_act(&self) -> BTreeMap<Seat, ActReason> {
        let mut m = BTreeMap::new();
        if self.outcome.is_some() {
            return m;
        }
        if let Some(p) = &self.pending {
            m.insert(p.seat(), p.reason());
        } else if let Some(s) = self.priority {
            m.insert(s, ActReason::Priority);
        }
        m
    }

    /// Apply one action for one seat. Rejects anything `legal_actions` would not
    /// list (or, for division actions, anything that fails the rule check), then
    /// runs turn-based actions, state-based actions and phase advancement until
    /// some seat can act again or the game is over.
    pub fn apply(&mut self, seat: Seat, action: &Action) -> Result<Vec<Event>, RulesError> {
        if let Some(outcome) = self.outcome {
            return Err(RulesError::GameOver { outcome });
        }
        // A player may concede at any time (rule 104.3a), whether or not they must act.
        let conceding = matches!(action, Action::Concede);
        if conceding {
            if self.is_eliminated(seat) {
                return Err(RulesError::illegal(format!("{seat} has already left the game")));
            }
        } else if !self.must_act().contains_key(&seat) {
            return Err(RulesError::NotYourTurnToAct { seat });
        }
        if conceding {
            // Always legal; skip the list check.
        } else if action.is_division() {
            self.validate_division(seat, action)?;
        } else {
            let canon = action.canonical();
            let legal = self.legal_actions(seat);
            if !legal.iter().any(|a| a.canonical() == canon) {
                // A cast or activation may name its own mana sources: accept it
                // if everything but the tapped permanents matches a listed
                // action and the payment covers the cost (§10).
                let listed_twin = legal.iter().find(|a| a.same_except_mana(action));
                match (listed_twin, action.mana_cost_in(self)) {
                    (Some(_), Some(cost)) => {
                        let payment = action.payment().expect("cast or activation");
                        self.payment_covers(seat, payment, &cost.with_x(payment.x))?;
                    }
                    _ => {
                        return Err(RulesError::illegal(format!(
                            "{} is not a legal action for {seat} right now",
                            crate::text::describe_action(self, action)
                        )))
                    }
                }
            }
        }
        let start = self.log.len();
        self.perform(seat, action)?;
        self.history.push((seat, action.clone()));
        self.settle();
        self.state_version += 1;
        Ok(self.log[start..].to_vec())
    }

    fn perform(&mut self, seat: Seat, action: &Action) -> Result<(), RulesError> {
        match action {
            Action::PassPriority => self.pass_priority(seat),
            Action::PlayLand { object } => self.play_land(seat, *object),
            Action::CastSpell { object, targets, payment } => self.cast_spell(seat, *object, targets, payment)?,
            Action::ActivateAbility {
                object,
                ability,
                targets,
                payment,
            } => self.activate_ability(seat, *object, *ability, targets, payment)?,
            Action::ChooseTargets { targets } => match &self.pending {
                Some(PendingChoice::ChooseTargets { .. }) => self.choose_trigger_targets(targets),
                Some(PendingChoice::Choose { .. }) => self.answer_choice(targets),
                Some(PendingChoice::Casting { .. }) => self.answer_cast_targets(targets)?,
                _ => return Err(RulesError::illegal("nothing to choose")),
            },
            Action::ChooseMode { mode } => match &self.pending {
                Some(PendingChoice::ChooseOption { .. }) => self.answer_option(*mode),
                Some(PendingChoice::Casting { .. }) => self.answer_cast_mode(*mode)?,
                _ => return Err(RulesError::illegal("no option to choose")),
            },
            Action::DeclareAttackers { attackers } => self.declare_attackers(seat, attackers),
            Action::DeclareBlockers { blocks } => self.declare_blockers(seat, blocks),
            Action::AssignCombatDamage { attacker, assignments } => self.assign_combat_damage(seat, *attacker, assignments),
            Action::Discard { objects } => self.discard_to_hand_size(seat, objects),
            Action::Mulligan { keep } => self.mulligan(seat, *keep),
            Action::BottomCards { objects } => self.bottom_cards(seat, objects),
            Action::Concede => self.eliminate(seat, Elimination::Conceded),
            other => {
                return Err(RulesError::Unsupported {
                    what: format!("{other:?}"),
                });
            }
        }
        Ok(())
    }

    // ----- views -----

    /// What `seat` is allowed to know: hidden information of other seats removed.
    pub fn view(&self, seat: Seat) -> GameView {
        self.build_view(Some(seat))
    }

    /// All hidden information removed.
    pub fn view_spectator(&self) -> GameView {
        self.build_view(None)
    }

    fn build_view(&self, viewer: Option<Seat>) -> GameView {
        let castable: BTreeSet<ObjectId> = viewer
            .map(|s| {
                self.legal_actions(s)
                    .iter()
                    .filter_map(|a| match a {
                        Action::PlayLand { object } | Action::CastSpell { object, .. } | Action::ActivateAbility { object, .. } => {
                            Some(*object)
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let players = self
            .players
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let seat = Seat(i as u8);
                let is_you = viewer == Some(seat);
                PlayerView {
                    seat,
                    name: p.name.clone(),
                    life: p.life,
                    eliminated: p.eliminated.is_some(),
                    elimination: p.eliminated.clone(),
                    hand: if is_you {
                        HandView::Yours(p.hand.clone())
                    } else {
                        HandView::Hidden { count: p.hand.len() as u8 }
                    },
                    library: LibraryView {
                        count: p.library.len() as u16,
                    },
                    graveyard: p.graveyard.clone(),
                    exile: p.exile.clone(),
                    battlefield: p.battlefield.clone(),
                    command: p.command.clone(),
                    commander_damage: p.commander_damage.clone(),
                    poison: p.poison,
                    lands_played_this_turn: p.lands_played_this_turn,
                    mana_pool: is_you.then(|| p.mana_pool.clone()),
                    pool: None,
                }
            })
            .collect();

        let mut objects = BTreeMap::new();
        for (id, obj) in &self.objects {
            let visible = obj.zone.is_public() || (obj.zone == Zone::Hand && viewer == Some(obj.owner));
            if !visible {
                continue;
            }
            let def = self.card_by_id(obj.card);
            let mut abilities: Vec<String> = def
                .ir
                .activated
                .iter()
                .filter(|a| !a.is_mana_ability())
                .map(|a| crate::text::render_ability(def, a))
                .collect();
            if let Some(e) = &def.ir.equip {
                abilities.push(format!("Equip {e}"));
            }
            objects.insert(
                id,
                ObjectView {
                    id,
                    card: obj.card,
                    name: def.name.clone(),
                    cost: def.cost.clone(),
                    types: def.types.clone(),
                    subtypes: def.subtypes.clone(),
                    text: def.text.clone(),
                    produces: def.produces(),
                    owner: obj.owner,
                    controller: obj.controller,
                    zone: obj.zone,
                    tapped: obj.tapped,
                    summoning_sick: obj.summoning_sick,
                    damage: obj.damage,
                    pt: self.effective_stats(id),
                    attacking: obj.attacking,
                    blocking: obj.blocking.clone(),
                    castable: castable.contains(&id),
                    keywords: if obj.zone == Zone::Battlefield {
                        self.keywords_of(id)
                    } else {
                        def.keywords.clone()
                    },
                    attached_to: obj.attached_to,
                    token: def.token,
                    counters: obj.counters.plus1 as i32 - obj.counters.minus1 as i32,
                    abilities,
                },
            );
        }

        let stack = self
            .stack
            .iter()
            .map(|s| StackObjectView {
                object: s.object,
                name: self.object_name(s.object).to_string(),
                controller: s.controller,
                targets: s.targets.clone(),
                modes: s.modes.clone(),
                x: s.x,
                kind: match s.kind {
                    StackKind::Spell => "spell",
                    StackKind::Ability { .. } => "ability",
                    StackKind::Equip { .. } => "equip",
                    StackKind::Trigger { .. } | StackKind::Prowess { .. } | StackKind::Delayed { .. } => "trigger",
                }
                .into(),
                description: self.describe_stack_object(s),
            })
            .collect();

        GameView {
            you: viewer,
            turn: self.turn,
            active_player: self.active_player,
            phase: self.phase,
            priority: self.priority,
            must_act: self.must_act(),
            state_version: self.state_version,
            outcome: self.outcome,
            stack,
            players,
            objects,
        }
    }

    // ----- queries -----

    pub fn card_def(&self, id: ObjectId) -> &CardDef {
        self.card_by_id(self.objects[id].card)
    }

    /// A card definition by id: from the database, or a token created this game.
    pub fn card_by_id(&self, card: CardId) -> &CardDef {
        let n = self.cards.len() as u32;
        if card < n {
            self.cards.get(card)
        } else {
            &self.tokens[(card - n) as usize]
        }
    }

    pub fn object_name(&self, id: ObjectId) -> &str {
        &self.card_def(id).name
    }

    pub fn player_name(&self, seat: Seat) -> &str {
        &self.players[seat.index()].name
    }

    pub fn is_creature(&self, id: ObjectId) -> bool {
        self.card_def(id).is_creature()
    }

    pub fn is_eliminated(&self, seat: Seat) -> bool {
        self.players[seat.index()].eliminated.is_some()
    }

    /// Every seat still in the game other than `seat`. Never `1 - seat`.
    pub fn opponents_of(&self, seat: Seat) -> impl Iterator<Item = Seat> + '_ {
        self.turn_order.iter().copied().filter(move |&s| s != seat)
    }

    /// Effective power and toughness (§3.4, layer 7 only): base, then static
    /// boosts from permanents on the battlefield, then counters and
    /// "until end of turn" modifiers. Recomputed on every call, never cached.
    /// `None` for non-creatures.
    pub fn effective_stats(&self, id: ObjectId) -> Option<(i32, i32)> {
        let obj = self.objects.get(id)?;
        let (mut p, mut t) = self.card_by_id(obj.card).pt?;
        if obj.zone == Zone::Battlefield {
            for (source, static_) in self.active_statics() {
                if let cardir::Static::PtBoost {
                    filter, power, toughness, ..
                } = static_
                {
                    let ctx = crate::filter::Ctx::simple(self.objects[source].controller, Some(source));
                    if self.object_matches(id, filter, &ctx) {
                        p += self.eval_amount(power, &ctx);
                        t += self.eval_amount(toughness, &ctx);
                    }
                }
            }
        }
        p += obj.counters.plus1 as i32 - obj.counters.minus1 as i32;
        t += obj.counters.plus1 as i32 - obj.counters.minus1 as i32;
        for m in &obj.modifiers {
            if let ModifierKind::Pt { power, toughness } = m.kind {
                p += power;
                t += toughness;
            }
        }
        Some((p, t))
    }

    /// Does the object have the keyword right now: printed, granted by a
    /// static on the battlefield, or granted until end of turn?
    pub fn has_keyword(&self, id: ObjectId, kw: Keyword) -> bool {
        let Some(obj) = self.objects.get(id) else {
            return false;
        };
        if self.card_def(id).has_keyword(kw) {
            return true;
        }
        if obj.modifiers.iter().any(|m| m.kind == ModifierKind::Keyword(kw)) {
            return true;
        }
        if obj.zone != Zone::Battlefield {
            return false;
        }
        for (source, static_) in self.active_statics() {
            let ctx = crate::filter::Ctx::simple(self.objects[source].controller, Some(source));
            match static_ {
                cardir::Static::PtBoost { filter, keywords, .. } if keywords.contains(&kw) && self.object_matches(id, filter, &ctx) => {
                    return true;
                }
                cardir::Static::GrantKeyword { filter, keyword } if *keyword == kw && self.object_matches(id, filter, &ctx) => {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Every keyword the object has right now.
    pub fn keywords_of(&self, id: ObjectId) -> Vec<Keyword> {
        Keyword::ALL.iter().copied().filter(|k| self.has_keyword(id, *k)).collect()
    }

    /// The static abilities of every permanent on the battlefield, with their
    /// source. An aura or equipment's statics apply only while it is attached;
    /// a conditional static ("as long as ...") only while its condition holds.
    pub(crate) fn active_statics(&self) -> Vec<(ObjectId, &cardir::Static)> {
        let mut out = Vec::new();
        for id in self.battlefield_objects() {
            let def = self.card_def(id);
            if (def.is_aura() || def.is_equipment()) && self.objects[id].attached_to.is_none() {
                continue;
            }
            for s in &def.ir.statics {
                match s {
                    cardir::Static::AsLongAs { condition, static_, .. } => {
                        let ctx = crate::filter::Ctx::simple(self.objects[id].controller, Some(id));
                        if self.condition_holds(condition, &ctx) {
                            out.push((id, &**static_));
                        }
                    }
                    s => out.push((id, s)),
                }
            }
        }
        out
    }

    pub fn power(&self, id: ObjectId) -> i32 {
        self.effective_stats(id).map(|(p, _)| p).unwrap_or(0)
    }

    pub fn toughness(&self, id: ObjectId) -> i32 {
        self.effective_stats(id).map(|(_, t)| t).unwrap_or(0)
    }

    /// The next seat still in the game after `seat` in turn order, or `None` if there is none.
    pub fn next_in_turn_order_after(&self, seat: Seat) -> Option<Seat> {
        let n = self.seating.len();
        let pos = self.seating.iter().position(|&s| s == seat)?;
        (1..=n)
            .map(|k| self.seating[(pos + k) % n])
            .find(|s| *s != seat && self.turn_order.contains(s))
    }

    /// Seats in the game in APNAP order: the active player first, then turn order.
    pub fn apnap(&self) -> Vec<Seat> {
        let mut out = Vec::with_capacity(self.turn_order.len());
        let first = if self.turn_order.contains(&self.active_player) {
            Some(self.active_player)
        } else {
            self.next_in_turn_order_after(self.active_player)
        };
        let Some(first) = first else { return out };
        out.push(first);
        let mut cur = first;
        while let Some(next) = self.next_in_turn_order_after(cur) {
            if next == first {
                break;
            }
            out.push(next);
            cur = next;
        }
        out
    }

    /// Objects on the battlefield, in id order.
    pub(crate) fn battlefield_objects(&self) -> Vec<ObjectId> {
        let mut ids: Vec<ObjectId> = self
            .objects
            .iter()
            .filter(|(_, o)| o.zone == Zone::Battlefield)
            .map(|(id, _)| id)
            .collect();
        ids.sort();
        ids
    }

    // ----- mutation primitives -----

    pub(crate) fn emit(&mut self, event: Event) {
        self.log.push(event);
    }

    fn zone_list_mut(&mut self, id: ObjectId, zone: Zone) -> Option<&mut Vec<ObjectId>> {
        let obj = &self.objects[id];
        let (owner, controller) = (obj.owner, obj.controller);
        let p = match zone {
            Zone::Battlefield => &mut self.players[controller.index()],
            Zone::Library | Zone::Hand | Zone::Graveyard | Zone::Exile | Zone::Command => &mut self.players[owner.index()],
            Zone::Stack | Zone::OutOfGame => return None,
        };
        Some(match zone {
            Zone::Library => &mut p.library,
            Zone::Hand => &mut p.hand,
            Zone::Battlefield => &mut p.battlefield,
            Zone::Graveyard => &mut p.graveyard,
            Zone::Exile => &mut p.exile,
            Zone::Command => &mut p.command,
            Zone::Stack | Zone::OutOfGame => unreachable!(),
        })
    }

    /// Move an object between zones, keeping the zone lists and the object's
    /// zone field in step. The caller pushes onto `stack` itself when moving
    /// there. A hidden-to-hidden move emits nothing; anything touching a
    /// public zone emits `ZoneChange`.
    pub(crate) fn move_object(&mut self, id: ObjectId, to: Zone) {
        let from = self.objects[id].zone;
        if from == to {
            return;
        }
        match from {
            Zone::Stack => self.stack.retain(|s| s.object != id),
            Zone::OutOfGame => {}
            z => {
                if let Some(list) = self.zone_list_mut(id, z) {
                    list.retain(|&o| o != id);
                }
            }
        }
        {
            let obj = &mut self.objects[id];
            obj.reset_zone_state();
            obj.zone = to;
        }
        match to {
            Zone::Stack | Zone::OutOfGame => {}
            z => {
                if let Some(list) = self.zone_list_mut(id, z) {
                    list.push(id);
                }
            }
        }
        if from.is_public() || to.is_public() {
            self.emit(Event::ZoneChange { object: id, from, to });
        }
    }

    pub(crate) fn shuffle_library(&mut self, seat: Seat) {
        let mut lib = std::mem::take(&mut self.players[seat.index()].library);
        lib.shuffle(&mut self.rng);
        self.players[seat.index()].library = lib;
        self.emit(Event::Shuffled { seat });
    }

    /// Draw `n` cards. Drawing from an empty library sets the flag the
    /// state-based action checks; it does not end the game here.
    pub(crate) fn draw(&mut self, seat: Seat, n: usize) {
        let i = seat.index();
        let mut drawn = Vec::with_capacity(n);
        for _ in 0..n {
            match self.players[i].library.pop() {
                Some(id) => {
                    self.objects[id].zone = Zone::Hand;
                    self.players[i].hand.push(id);
                    drawn.push(id);
                }
                None => self.players[i].drew_from_empty = true,
            }
        }
        if !drawn.is_empty() {
            self.emit(Event::Drew { seat, cards: drawn });
        }
    }

    pub(crate) fn give_priority(&mut self, seat: Seat) {
        self.priority = Some(seat);
        self.passed_in_succession = 0;
    }

    /// Priority to the active player, or to whoever follows them if they have left.
    pub(crate) fn give_priority_to_active(&mut self) {
        self.passed_in_succession = 0;
        self.priority = if self.turn_order.contains(&self.active_player) {
            Some(self.active_player)
        } else {
            self.next_in_turn_order_after(self.active_player)
        };
    }

    pub(crate) fn play_land(&mut self, seat: Seat, object: ObjectId) {
        self.objects[object].controller = seat;
        self.move_object(object, Zone::Battlefield);
        self.players[seat.index()].lands_played_this_turn += 1;
        self.emit(Event::LandPlayed { seat, object });
        self.give_priority(seat);
    }

    pub(crate) fn discard_to_hand_size(&mut self, seat: Seat, objects: &[ObjectId]) {
        for &id in objects {
            self.move_object(id, Zone::Graveyard);
        }
        self.emit(Event::Discarded {
            seat,
            objects: objects.to_vec(),
        });
        self.pending = None;
        self.finish_cleanup();
    }
}
