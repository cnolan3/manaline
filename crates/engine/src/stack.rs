//! The stack: casting spells, activating abilities, placing triggers, and
//! resolving all three (§3.3, §3.5). Resolution runs on an explicit frame
//! stack so it can pause on a player's choice at any depth and continue
//! later from serialisable state.

use crate::action::{ManaPayment, Target, EQUIP_ABILITY};
use crate::error::RulesError;
use crate::event::Event;
use crate::filter::Ctx;
use crate::game::{ActReason, Game, PendingChoice, StackKind, StackObject};
use crate::types::{ObjectId, Seat, Zone};
use cardir::{Cost, Effect, Filter};

/// Binding names the evaluator uses for its own choices; card-written names
/// never start with `$`.
const SACRIFICE_BIND: &str = "$sacrifice";
const DISCARD_BIND: &str = "$discard";
pub(crate) const MAY_BIND: &str = "$may";

/// One step of work the evaluator still has to do. Frames are pushed as an
/// effect unfolds (a `Sequence` becomes an `Effects` frame, "each player
/// discards" becomes a `Discard` frame over the remaining seats) and popped
/// as they finish, so pausing at any depth loses nothing.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Frame {
    /// Run `effects` in order; `next` is the index of the one to run next.
    Effects { effects: Vec<Effect>, next: usize },
    /// Ask `seat` to pick between `min` and `max` of `options`; what they
    /// pick is bound to `bind`. Decided on the spot when there is no choice.
    Choose {
        seat: Seat,
        options: Vec<Target>,
        min: usize,
        max: usize,
        /// The verb, for menus: "Sacrifice", "Discard", "Return".
        prompt: String,
        reason: ActReason,
        bind: String,
    },
    /// Ask `seat` to pick one of `labels`; the index is stored under `bind`.
    ChooseOption { seat: Seat, labels: Vec<String>, bind: String },
    /// Continue with the branch whose index is stored under `bind`.
    Branch { bind: String, branches: Vec<Vec<Effect>> },
    /// Each of `seats`, in order, discards `count` cards (at random, or by choice).
    Discard { seats: Vec<Seat>, count: i32, random: bool },
    /// Each of `seats`, in order, sacrifices `count` permanents matching `filter`.
    Sacrifice { seats: Vec<Seat>, filter: Filter, count: i32 },
    /// Sacrifice what `bind` holds (the answer to a `Choose`).
    SacrificeBound { seat: Seat, bind: String },
    /// Discard what `bind` holds.
    DiscardBound { seat: Seat, bind: String },
}

/// A resolution in progress: what it is evaluating relative to, and the
/// frames still to run (top of the stack last). Stored inside a
/// `PendingChoice` while a player decides, and picked up again afterwards.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Continuation {
    pub ctx: Ctx,
    /// The target filters the effects were written against, for rendering
    /// "target creature" in option labels.
    pub specs: Vec<Filter>,
    pub frames: Vec<Frame>,
}

impl Continuation {
    /// Run `effects` in order under `ctx`.
    pub fn new(ctx: Ctx, specs: Vec<Filter>, effects: Vec<Effect>) -> Continuation {
        Continuation {
            ctx,
            specs,
            frames: vec![Frame::Effects { effects, next: 0 }],
        }
    }
}

impl Game {
    /// Target filters for casting a card: an aura's `enchant`, a spell's targets, else none.
    pub fn cast_target_specs(&self, id: ObjectId) -> Vec<Filter> {
        let def = self.card_def(id);
        if let Some(f) = &def.ir.enchant {
            return vec![f.clone()];
        }
        def.ir.spell.as_ref().map(|s| s.targets.clone()).unwrap_or_default()
    }

    pub(crate) fn cast_spell(&mut self, seat: Seat, object: ObjectId, targets: &[Target], payment: &ManaPayment) -> Result<(), RulesError> {
        let cost = self.cast_cost(seat, object);
        self.pay_mana(seat, payment, &cost)?;
        self.objects[object].controller = seat;
        self.move_object(object, Zone::Stack);
        self.stack.push(StackObject {
            object,
            controller: seat,
            targets: targets.to_vec(),
            kind: StackKind::Spell,
        });
        self.emit(Event::Cast {
            seat,
            object,
            targets: targets.to_vec(),
        });
        self.give_priority(seat);
        Ok(())
    }

    /// Pay an activated ability's costs and put it on the stack.
    pub(crate) fn activate_ability(
        &mut self,
        seat: Seat,
        object: ObjectId,
        index: u8,
        targets: &[Target],
        payment: &ManaPayment,
    ) -> Result<(), RulesError> {
        if index == EQUIP_ABILITY {
            let cost = self
                .card_def(object)
                .ir
                .equip
                .clone()
                .ok_or_else(|| RulesError::illegal("not equipment"))?;
            self.pay_mana(seat, payment, &cost)?;
            self.stack.push(StackObject {
                object,
                controller: seat,
                targets: targets.to_vec(),
                kind: StackKind::Equip { source: object },
            });
            self.emit(Event::Activated {
                seat,
                object,
                ability: index,
                targets: targets.to_vec(),
            });
            self.give_priority(seat);
            return Ok(());
        }
        let ability = self
            .card_def(object)
            .ir
            .activated
            .get(index as usize)
            .cloned()
            .ok_or_else(|| RulesError::illegal("no such ability"))?;
        for cost in &ability.cost {
            match cost {
                Cost::Mana(m) => self.pay_mana(seat, payment, m)?,
                Cost::Tap => {
                    if self.objects[object].tapped {
                        return Err(RulesError::illegal(format!("{object} is already tapped")));
                    }
                    self.objects[object].tapped = true;
                    self.emit(Event::Tapped { object });
                }
                Cost::SacrificeThis => self.sacrifice(seat, object),
                Cost::Sacrifice(filter) => {
                    let ctx = Ctx::simple(seat, Some(object));
                    let mut paid = false;
                    for &id in &payment.sacrifice {
                        if self
                            .objects
                            .get(id)
                            .map(|o| o.controller == seat && o.zone == Zone::Battlefield)
                            .unwrap_or(false)
                            && self.object_matches(id, filter, &ctx)
                        {
                            self.sacrifice(seat, id);
                            paid = true;
                            break;
                        }
                    }
                    if !paid {
                        return Err(RulesError::illegal(
                            "the payment names nothing that can be sacrificed for this cost",
                        ));
                    }
                }
                Cost::PayLife(n) => {
                    let p = &mut self.players[seat.index()];
                    if p.life < *n {
                        return Err(RulesError::illegal(format!("{seat} can't pay {n} life")));
                    }
                    let from = p.life;
                    p.life -= n;
                    self.emit(Event::LifeChanged { seat, from, to: from - n });
                }
                Cost::Discard(n) => {
                    let mut discarded = 0;
                    for &id in &payment.discard {
                        if self.players[seat.index()].hand.contains(&id) && discarded < *n {
                            self.move_object(id, Zone::Graveyard);
                            discarded += 1;
                        }
                    }
                    if discarded < *n {
                        return Err(RulesError::illegal(format!("the payment must name {n} card(s) in hand to discard")));
                    }
                    self.emit(Event::Discarded {
                        seat,
                        objects: payment.discard.clone(),
                    });
                }
            }
        }
        self.stack.push(StackObject {
            object,
            controller: seat,
            targets: targets.to_vec(),
            kind: StackKind::Ability { source: object, index },
        });
        self.emit(Event::Activated {
            seat,
            object,
            ability: index,
            targets: targets.to_vec(),
        });
        self.give_priority(seat);
        Ok(())
    }

    pub(crate) fn sacrifice(&mut self, seat: Seat, id: ObjectId) {
        self.emit(Event::Sacrificed { seat, object: id });
        self.move_object(id, Zone::Graveyard);
    }

    /// Resolve the top object. Returns `false` if resolution paused on a choice.
    pub(crate) fn resolve(&mut self, so: StackObject) -> bool {
        let StackObject {
            object,
            controller,
            targets,
            kind,
        } = so;
        match kind {
            StackKind::Spell => {
                let def = self.card_def(object).clone();
                let specs = self.cast_target_specs(object);
                let ctx = Ctx::new(controller, Some(object), targets.clone(), None);
                if self.all_targets_illegal(&specs, &ctx) {
                    // Fizzle: the spell does nothing and goes to the graveyard.
                    self.move_object(object, Zone::Graveyard);
                    self.emit(Event::Resolved { object });
                    return true;
                }
                if def.is_permanent() {
                    self.objects[object].controller = controller;
                    self.move_object(object, Zone::Battlefield);
                    self.objects[object].summoning_sick = true;
                    if def.is_aura() {
                        if let Some(Target::Object(t)) = targets.first() {
                            self.objects[object].attached_to = Some(*t);
                            self.emit(Event::Attached { object, to: *t });
                        }
                    }
                    self.emit(Event::Resolved { object });
                    return true;
                }
                // An instant or sorcery leaves the stack as it resolves; nothing in
                // v1 can observe the difference, and it keeps every object in exactly
                // one zone list while a resolution is paused on a choice.
                let effects = def.ir.spell.as_ref().map(|s| s.effects.clone()).unwrap_or_default();
                self.move_object(object, Zone::Graveyard);
                self.emit(Event::Resolved { object });
                self.run(Continuation::new(ctx, specs, effects))
            }
            StackKind::Ability { source, index } => {
                let def = self.card_def(source).clone();
                let ability = def.ir.activated[index as usize].clone();
                let ctx = Ctx::new(controller, Some(source), targets.clone(), None);
                if self.all_targets_illegal(&ability.targets, &ctx) {
                    return true;
                }
                self.run(Continuation::new(ctx, ability.targets, ability.effects))
            }
            StackKind::Equip { source } => {
                if let Some(Target::Object(t)) = targets.first() {
                    let legal = self
                        .objects
                        .get(*t)
                        .map(|o| o.zone == Zone::Battlefield && o.controller == controller)
                        .unwrap_or(false)
                        && self.is_creature(*t)
                        && self.objects[source].zone == Zone::Battlefield;
                    if legal {
                        self.objects[source].attached_to = Some(*t);
                        self.emit(Event::Attached { object: source, to: *t });
                    }
                }
                true
            }
            StackKind::Trigger { source, index, triggering } => {
                let def = self.card_def(source).clone();
                let trigger = def.ir.triggers[index as usize].clone();
                let ctx = Ctx::new(controller, Some(source), targets.clone(), triggering);
                if self.all_targets_illegal(trigger.targets(), &ctx) {
                    return true;
                }
                self.run(Continuation::new(ctx, trigger.targets().to_vec(), trigger.effects().to_vec()))
            }
            StackKind::Prowess { source } => {
                if self.objects[source].zone == Zone::Battlefield {
                    self.objects[source].modifiers.push(crate::game::Modifier {
                        kind: crate::game::ModifierKind::Pt { power: 1, toughness: 1 },
                        expires: crate::game::Expiry::EndOfTurn,
                    });
                }
                true
            }
        }
    }

    /// A spell or ability with targets whose targets are all illegal on
    /// resolution doesn't resolve (rule 608.2b).
    fn all_targets_illegal(&self, specs: &[Filter], ctx: &Ctx) -> bool {
        if specs.is_empty() {
            return false;
        }
        !specs
            .iter()
            .enumerate()
            .any(|(i, spec)| ctx.targets.get(i).map(|t| self.target_is_legal(*t, spec, ctx)).unwrap_or(false))
    }

    /// Counter a spell: it leaves the stack for its owner's graveyard.
    pub(crate) fn counter_spell(&mut self, object: ObjectId) {
        if let Some(pos) = self.stack.iter().position(|s| s.object == object && s.kind == StackKind::Spell) {
            self.stack.remove(pos);
            self.move_object(object, Zone::Graveyard);
            self.emit(Event::Countered { object });
        }
    }

    /// Continue a resolution that paused on a choice.
    pub(crate) fn resume(&mut self, k: Continuation) {
        if self.run(k) {
            self.give_priority_to_active();
        }
    }

    /// Run the continuation to completion. Returns `true` when finished;
    /// `false` when a player must choose something first, in which case the
    /// continuation (with the frame that asked still on it) is stored in
    /// `pending` and nobody holds priority.
    pub(crate) fn run(&mut self, mut k: Continuation) -> bool {
        while let Some(frame) = k.frames.pop() {
            match frame {
                Frame::Effects { effects, next } => {
                    let Some(effect) = effects.get(next).cloned() else {
                        continue;
                    };
                    if next + 1 < effects.len() {
                        k.frames.push(Frame::Effects { effects, next: next + 1 });
                    }
                    for pushed in self.step(&k.ctx, &k.specs, &effect) {
                        k.frames.push(pushed);
                    }
                }
                Frame::Choose {
                    seat,
                    options,
                    min,
                    max,
                    prompt,
                    reason,
                    bind,
                } => {
                    let max = max.min(options.len());
                    let min = min.min(max);
                    if options.is_empty() || max == 0 {
                        k.ctx.bindings.insert(bind, Vec::new());
                        continue;
                    }
                    if options.len() <= min {
                        // Everything must be taken: no decision to make.
                        k.ctx.bindings.insert(bind, options);
                        continue;
                    }
                    self.pending = Some(PendingChoice::Choose {
                        seat,
                        options,
                        min,
                        max,
                        prompt,
                        reason,
                        bind,
                        resume: k,
                    });
                    self.priority = None;
                    return false;
                }
                Frame::ChooseOption { seat, labels, bind } => {
                    if labels.len() <= 1 {
                        k.ctx.options.insert(bind, 0);
                        continue;
                    }
                    self.pending = Some(PendingChoice::ChooseOption {
                        seat,
                        labels,
                        bind,
                        resume: k,
                    });
                    self.priority = None;
                    return false;
                }
                Frame::Branch { bind, branches } => {
                    if let Some(effects) = k.ctx.options.get(&bind).and_then(|&i| branches.get(i as usize)) {
                        if !effects.is_empty() {
                            k.frames.push(Frame::Effects {
                                effects: effects.clone(),
                                next: 0,
                            });
                        }
                    }
                }
                Frame::Discard { mut seats, count, random } => {
                    if count <= 0 {
                        continue;
                    }
                    let Some(seat) = (!seats.is_empty()).then(|| seats.remove(0)) else {
                        continue;
                    };
                    if !seats.is_empty() {
                        k.frames.push(Frame::Discard { seats, count, random });
                    }
                    let hand: Vec<ObjectId> = self.players[seat.index()].hand.clone();
                    if hand.is_empty() {
                        continue;
                    }
                    if random {
                        let mut hand = hand;
                        use rand::seq::SliceRandom;
                        hand.shuffle(&mut self.rng);
                        let chosen: Vec<ObjectId> = hand.into_iter().take(count as usize).collect();
                        self.discard(seat, &chosen);
                        continue;
                    }
                    let mut hand = hand;
                    hand.sort();
                    k.frames.push(Frame::DiscardBound {
                        seat,
                        bind: DISCARD_BIND.into(),
                    });
                    k.frames.push(Frame::Choose {
                        seat,
                        options: hand.into_iter().map(Target::Object).collect(),
                        min: count as usize,
                        max: count as usize,
                        prompt: "Discard".into(),
                        reason: ActReason::Discard,
                        bind: DISCARD_BIND.into(),
                    });
                }
                Frame::Sacrifice { mut seats, filter, count } => {
                    if count <= 0 {
                        continue;
                    }
                    let Some(seat) = (!seats.is_empty()).then(|| seats.remove(0)) else {
                        continue;
                    };
                    if !seats.is_empty() {
                        k.frames.push(Frame::Sacrifice {
                            seats,
                            filter: filter.clone(),
                            count,
                        });
                    }
                    // The filter is read from the sacrificing player's side ("a creature you control").
                    let sub = Ctx {
                        you: seat,
                        ..k.ctx.clone()
                    };
                    let mut candidates: Vec<ObjectId> = self.players[seat.index()]
                        .battlefield
                        .clone()
                        .into_iter()
                        .filter(|id| self.object_matches(*id, &filter, &sub))
                        .collect();
                    candidates.sort();
                    if candidates.is_empty() {
                        continue;
                    }
                    k.frames.push(Frame::SacrificeBound {
                        seat,
                        bind: SACRIFICE_BIND.into(),
                    });
                    k.frames.push(Frame::Choose {
                        seat,
                        options: candidates.into_iter().map(Target::Object).collect(),
                        min: count as usize,
                        max: count as usize,
                        prompt: "Sacrifice".into(),
                        reason: ActReason::Choice,
                        bind: SACRIFICE_BIND.into(),
                    });
                }
                Frame::SacrificeBound { seat, bind } => {
                    for id in bound_objects(&k.ctx, &bind) {
                        if self
                            .objects
                            .get(id)
                            .map(|o| o.zone == Zone::Battlefield && o.controller == seat)
                            .unwrap_or(false)
                        {
                            self.sacrifice(seat, id);
                        }
                    }
                }
                Frame::DiscardBound { seat, bind } => {
                    let ids = bound_objects(&k.ctx, &bind);
                    if !ids.is_empty() {
                        self.discard(seat, &ids);
                    }
                }
            }
        }
        true
    }
}

/// The objects a binding holds.
fn bound_objects(ctx: &Ctx, bind: &str) -> Vec<ObjectId> {
    ctx.bindings
        .get(bind)
        .map(|ts| {
            ts.iter()
                .filter_map(|t| match t {
                    Target::Object(id) => Some(*id),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
