//! Evaluating IR filters, references, and amounts against the game.

use crate::action::Target;
use crate::game::Game;
use crate::types::{CardType, Keyword, ObjectId, Seat, Zone};
use cardir::{Amount, Filter, PlayerRef, Ref};
use std::collections::BTreeMap;

/// The binding a "when ~ leaves the battlefield" trigger gets for the cards
/// its source had exiled, since the link is cut as it leaves.
pub const EXILED_BIND: &str = "$exiled";

/// What a filter is evaluated relative to.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ctx {
    /// The controller of the spell or ability ("you").
    pub you: Seat,
    /// The source object, if any ("~", "this").
    pub this: Option<ObjectId>,
    /// Chosen targets, grouped by target spec: `Target(i)` is everything
    /// picked for the i-th "target" word (several things for "up to two
    /// target creatures"), with illegal ones already pruned at resolution.
    pub targets: Vec<Vec<Target>>,
    /// The object or player that caused a trigger.
    pub triggering: Option<Target>,
    /// What each `Chosen ... bind` picked as the effect resolved, by name.
    #[serde(default)]
    pub bindings: BTreeMap<String, Vec<Target>>,
    /// Options picked by index ("you may": 0 did it, 1 declined), by name.
    #[serde(default)]
    pub options: BTreeMap<String, u8>,
    /// The value announced for `{X}` when this was cast or activated.
    #[serde(default)]
    pub x: i32,
    /// While one player of an "each opponent may" decides and acts, the
    /// effects run for that player alone.
    #[serde(default)]
    pub chooser: Option<Seat>,
}

impl Ctx {
    pub fn simple(you: Seat, this: Option<ObjectId>) -> Ctx {
        Ctx::new(you, this, Vec::new(), None)
    }

    pub fn new(you: Seat, this: Option<ObjectId>, targets: Vec<Vec<Target>>, triggering: Option<Target>) -> Ctx {
        Ctx {
            you,
            this,
            targets,
            triggering,
            bindings: BTreeMap::new(),
            options: BTreeMap::new(),
            x: 0,
            chooser: None,
        }
    }

    pub fn with_x(mut self, x: u32) -> Ctx {
        self.x = x as i32;
        self
    }
}

impl Game {
    /// Does an object on the battlefield (or on the stack, for spell filters) match?
    pub fn object_matches(&self, id: ObjectId, filter: &Filter, ctx: &Ctx) -> bool {
        let Some(obj) = self.objects.get(id) else {
            return false;
        };
        let def = self.card_def(id);
        match filter {
            Filter::Targets(_, f) => self.object_matches(id, f, ctx),
            Filter::Any => obj.zone == Zone::Battlefield && (def.is_creature() || def.types.contains(&CardType::Planeswalker)),
            Filter::Creature => def.is_creature(),
            Filter::Land => def.is_land(),
            Filter::Artifact => def.types.contains(&CardType::Artifact),
            Filter::Enchantment => def.types.contains(&CardType::Enchantment),
            Filter::Instant => def.types.contains(&CardType::Instant),
            Filter::Sorcery => def.types.contains(&CardType::Sorcery),
            Filter::Permanent => obj.zone == Zone::Battlefield || (obj.zone == Zone::Graveyard && def.is_permanent()),
            Filter::InGraveyard(p) => obj.zone == Zone::Graveyard && self.players_of(p, ctx).contains(&obj.owner),
            Filter::Player | Filter::Opponent => false,
            Filter::Spell => obj.zone == Zone::Stack,
            Filter::Other => ctx.this != Some(id),
            Filter::This => ctx.this == Some(id),
            Filter::Attached => ctx.this.and_then(|t| self.objects.get(t)).and_then(|t| t.attached_to) == Some(id),
            Filter::Token => def.token,
            Filter::Subtype(s) => def.subtypes.iter().any(|x| x.eq_ignore_ascii_case(s)),
            Filter::Color(c) => def.colors.contains(c),
            Filter::ControlledBy(p) => self.players_of(p, ctx).contains(&obj.controller),
            Filter::Tapped => obj.tapped,
            Filter::Untapped => !obj.tapped,
            Filter::Attacking => obj.attacking.is_some(),
            Filter::Blocking => !obj.blocking.is_empty(),
            Filter::PowerAtLeast(n) => self.effective_stats(id).map(|(p, _)| p >= *n).unwrap_or(false),
            Filter::PowerAtMost(n) => self.effective_stats(id).map(|(p, _)| p <= *n).unwrap_or(false),
            Filter::ManaValueAtMost(n) => self.card_def(id).ir.cost.mana_value() as i32 <= *n,
            Filter::HasKeyword(k) => self.has_keyword(id, *k),
            Filter::And(fs) => fs.iter().all(|f| self.object_matches(id, f, ctx)),
            Filter::Or(fs) => fs.iter().any(|f| self.object_matches(id, f, ctx)),
            Filter::Not(f) => !self.object_matches(id, f, ctx),
        }
    }

    /// Does a card being cast (still in hand) match a spell filter? Like
    /// `object_matches`, but `Spell` is true for it wherever it is.
    pub fn spell_matches(&self, id: ObjectId, filter: &Filter, ctx: &Ctx) -> bool {
        match filter {
            Filter::Targets(_, f) => self.spell_matches(id, f, ctx),
            Filter::Spell => true,
            Filter::And(fs) => fs.iter().all(|f| self.spell_matches(id, f, ctx)),
            Filter::Or(fs) => fs.iter().any(|f| self.spell_matches(id, f, ctx)),
            Filter::Not(f) => !self.spell_matches(id, f, ctx),
            other => self.object_matches(id, other, ctx),
        }
    }

    /// Does a player match a player-capable filter?
    pub fn player_matches(&self, seat: Seat, filter: &Filter, ctx: &Ctx) -> bool {
        if !self.turn_order.contains(&seat) {
            return false;
        }
        match filter {
            Filter::Targets(_, f) => self.player_matches(seat, f, ctx),
            Filter::Any | Filter::Player => true,
            Filter::Opponent => seat != ctx.you,
            Filter::ControlledBy(p) => self.players_of(p, ctx).contains(&seat),
            Filter::And(fs) => fs.iter().all(|f| self.player_matches(seat, f, ctx)),
            Filter::Or(fs) => fs.iter().any(|f| self.player_matches(seat, f, ctx)),
            Filter::Not(f) => !self.player_matches(seat, f, ctx),
            _ => false,
        }
    }

    /// Whether a filter can match players at all (it names players somewhere).
    pub fn filter_admits_players(filter: &Filter) -> bool {
        match filter {
            Filter::Targets(_, f) => Self::filter_admits_players(f),
            Filter::Any | Filter::Player | Filter::Opponent => true,
            Filter::And(fs) => fs.iter().all(Self::filter_admits_players),
            Filter::Or(fs) => fs.iter().any(Self::filter_admits_players),
            _ => false,
        }
    }

    /// Whether a filter can match objects (as opposed to only players).
    pub fn filter_admits_objects(filter: &Filter) -> bool {
        match filter {
            Filter::Targets(_, f) => Self::filter_admits_objects(f),
            Filter::Player | Filter::Opponent => false,
            Filter::And(fs) => fs.iter().all(Self::filter_admits_objects),
            Filter::Or(fs) => fs.iter().any(Self::filter_admits_objects),
            _ => true,
        }
    }

    /// The zone a target filter looks in: the stack for spells, else the battlefield.
    pub fn filter_zone(filter: &Filter) -> Zone {
        match filter {
            Filter::Targets(_, f) => Self::filter_zone(f),
            Filter::Spell => Zone::Stack,
            Filter::InGraveyard(_) => Zone::Graveyard,
            Filter::And(fs) if fs.iter().any(|f| Self::filter_zone(f) == Zone::Stack) => Zone::Stack,
            Filter::And(fs) if fs.iter().any(|f| Self::filter_zone(f) == Zone::Graveyard) => Zone::Graveyard,
            _ => Zone::Battlefield,
        }
    }

    /// Every card in every graveyard.
    pub fn graveyard_objects(&self) -> Vec<ObjectId> {
        self.players.iter().flat_map(|p| p.graveyard.iter().copied()).collect()
    }

    /// Every legal target for a filter right now, as chosen by `ctx.you`:
    /// objects in the filter's zone (hexproof excludes opponents' spells) and players.
    pub fn targets_for(&self, filter: &Filter, ctx: &Ctx) -> Vec<Target> {
        self.matching(filter, ctx, true)
    }

    /// Everything a filter admits right now for a choice that does not target
    /// ("a creature you control"): like `targets_for`, but hexproof is no bar.
    pub fn choosable(&self, filter: &Filter, ctx: &Ctx) -> Vec<Target> {
        self.matching(filter, ctx, false)
    }

    fn matching(&self, filter: &Filter, ctx: &Ctx, targeted: bool) -> Vec<Target> {
        let mut out = Vec::new();
        if Self::filter_admits_objects(filter) {
            let zone = Self::filter_zone(filter);
            let ids: Vec<ObjectId> = match zone {
                Zone::Stack => self.stack.iter().map(|s| s.object).collect(),
                Zone::Graveyard => self.graveyard_objects(),
                _ => self.battlefield_objects(),
            };
            for id in ids {
                if self.object_matches(id, filter, ctx) && (!targeted || self.can_target(id, ctx.you)) {
                    out.push(Target::Object(id));
                }
            }
        }
        if Self::filter_admits_players(filter) {
            for &seat in &self.turn_order {
                if self.player_matches(seat, filter, ctx) {
                    out.push(Target::Player(seat));
                }
            }
        }
        out
    }

    /// Hexproof: an opponent's spells and abilities can't target it.
    pub fn can_target(&self, id: ObjectId, by: Seat) -> bool {
        let Some(obj) = self.objects.get(id) else {
            return false;
        };
        if obj.zone == Zone::Battlefield && self.has_keyword(id, Keyword::Hexproof) && obj.controller != by {
            return false;
        }
        true
    }

    /// Is a chosen target still legal for its filter?
    pub fn target_is_legal(&self, target: Target, filter: &Filter, ctx: &Ctx) -> bool {
        match target {
            Target::Object(id) => {
                let Some(obj) = self.objects.get(id) else {
                    return false;
                };
                obj.zone == Self::filter_zone(filter) && self.object_matches(id, filter, ctx) && self.can_target(id, ctx.you)
            }
            Target::Player(s) => self.player_matches(s, filter, ctx),
        }
    }

    /// The players a `PlayerRef` denotes right now.
    pub fn players_of(&self, p: &PlayerRef, ctx: &Ctx) -> Vec<Seat> {
        if let (Some(c), PlayerRef::EachOpponent | PlayerRef::EachPlayer) = (ctx.chooser, p) {
            return vec![c];
        }
        match p {
            PlayerRef::You => vec![ctx.you],
            PlayerRef::TargetPlayer(i) | PlayerRef::TargetOpponent(i) => ctx
                .targets
                .get(*i as usize)
                .map(|group| {
                    group
                        .iter()
                        .filter_map(|t| match t {
                            Target::Player(s) => Some(*s),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            PlayerRef::EachOpponent => self.opponents_of(ctx.you).collect(),
            PlayerRef::EachPlayer => self.turn_order.clone(),
            PlayerRef::Triggering => match ctx.triggering {
                Some(Target::Player(s)) => vec![s],
                Some(Target::Object(o)) => self.objects.get(o).map(|o| vec![o.controller]).unwrap_or_default(),
                None => Vec::new(),
            },
            PlayerRef::Controller(r) => self
                .objects_of(r, ctx)
                .iter()
                .filter_map(|id| self.objects.get(*id).map(|o| o.controller))
                .collect(),
            PlayerRef::Owner(r) => self
                .objects_of(r, ctx)
                .iter()
                .filter_map(|id| self.objects.get(*id).map(|o| o.owner))
                .collect(),
        }
    }

    /// The objects a `Ref` denotes right now (players are handled separately).
    pub fn objects_of(&self, r: &Ref, ctx: &Ctx) -> Vec<ObjectId> {
        match r {
            Ref::Target(i) => ctx
                .targets
                .get(*i as usize)
                .map(|group| {
                    group
                        .iter()
                        .filter_map(|t| match t {
                            Target::Object(id) => Some(*id),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            Ref::This => ctx.this.into_iter().collect(),
            // Bound when the source left the battlefield (its "when ~ leaves"
            // trigger fires after the link is gone); live before that.
            Ref::ExiledWithThis => match ctx.bindings.get(EXILED_BIND) {
                Some(ts) => ts
                    .iter()
                    .filter_map(|t| match t {
                        Target::Object(id) => Some(*id),
                        _ => None,
                    })
                    .collect(),
                None => ctx.this.map(|s| self.exiled_by(s)).unwrap_or_default(),
            },
            Ref::Triggering => match ctx.triggering {
                Some(Target::Object(id)) => vec![id],
                _ => Vec::new(),
            },
            Ref::Each(f) => {
                let zone = Self::filter_zone(f);
                let ids: Vec<ObjectId> = match zone {
                    Zone::Stack => self.stack.iter().map(|s| s.object).collect(),
                    Zone::Graveyard => self.graveyard_objects(),
                    _ => self.battlefield_objects(),
                };
                ids.into_iter().filter(|id| self.object_matches(*id, f, ctx)).collect()
            }
            Ref::Player(_) => Vec::new(),
            Ref::Attached => ctx
                .this
                .and_then(|t| self.objects.get(t))
                .and_then(|t| t.attached_to)
                .into_iter()
                .collect(),
            Ref::Named(name) => ctx
                .bindings
                .get(name)
                .map(|ts| {
                    ts.iter()
                        .filter_map(|t| match t {
                            Target::Object(id) => Some(*id),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // A `Chosen` is turned into a `Named` by the evaluator before any effect runs.
            Ref::Chosen { .. } => Vec::new(),
        }
    }

    /// Everything a `Ref` denotes: objects and players.
    pub fn refs_of(&self, r: &Ref, ctx: &Ctx) -> Vec<Target> {
        match r {
            Ref::Target(i) => ctx.targets.get(*i as usize).cloned().unwrap_or_default(),
            Ref::Player(p) => self.players_of(p, ctx).into_iter().map(Target::Player).collect(),
            Ref::Triggering => ctx.triggering.into_iter().collect(),
            Ref::Named(name) => ctx.bindings.get(name).cloned().unwrap_or_default(),
            Ref::Each(f) => {
                let mut out: Vec<Target> = self.objects_of(r, ctx).into_iter().map(Target::Object).collect();
                if Self::filter_admits_players(f) {
                    out.extend(
                        self.turn_order
                            .iter()
                            .copied()
                            .filter(|s| self.player_matches(*s, f, ctx))
                            .map(Target::Player),
                    );
                }
                out
            }
            other => self.objects_of(other, ctx).into_iter().map(Target::Object).collect(),
        }
    }

    /// Whether a condition ("if you control an Elf") holds right now.
    pub fn condition_holds(&self, c: &cardir::Condition, ctx: &Ctx) -> bool {
        match c {
            cardir::Condition::Controls { player, filter, at_least } => self.players_of(player, ctx).iter().any(|&s| {
                let sub = Ctx { you: s, ..ctx.clone() };
                let n = self.players[s.index()]
                    .battlefield
                    .iter()
                    .filter(|id| self.object_matches(**id, filter, &sub))
                    .count();
                n as i32 >= *at_least
            }),
            cardir::Condition::LifeAtLeast { player, amount } => {
                self.players_of(player, ctx).iter().any(|s| self.players[s.index()].life >= *amount)
            }
            cardir::Condition::LifeAtMost { player, amount } => {
                self.players_of(player, ctx).iter().any(|s| self.players[s.index()].life <= *amount)
            }
        }
    }

    pub fn eval_amount(&self, a: &Amount, ctx: &Ctx) -> i32 {
        match a {
            Amount::Const(n) => *n,
            Amount::Count(f) => {
                let objects = self
                    .battlefield_objects()
                    .into_iter()
                    .filter(|id| self.object_matches(*id, f, ctx))
                    .count();
                objects as i32
            }
            Amount::LifeOf(p) => self.players_of(p, ctx).first().map(|s| self.players[s.index()].life).unwrap_or(0),
            Amount::PowerOf(r) => self.objects_of(r, ctx).first().map(|id| self.power(*id)).unwrap_or(0),
            Amount::X => ctx.x,
            Amount::Neg(a) => -self.eval_amount(a, ctx),
        }
    }
}
