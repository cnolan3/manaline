//! The stack: casting spells, activating abilities, placing triggers, and
//! resolving all three (§3.3, §3.5). Resolution may suspend on a choice.

use crate::action::{ManaPayment, Target, EQUIP_ABILITY};
use crate::error::RulesError;
use crate::event::Event;
use crate::filter::Ctx;
use crate::game::{Game, PendingChoice, StackKind, StackObject};
use crate::types::{ObjectId, Seat, Zone};
use cardir::{Cost, Effect, Filter};
use std::collections::VecDeque;

/// What to do once an effect list finishes running.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum After {
    Nothing,
    /// An instant or sorcery goes to its owner's graveyard.
    SpellToGraveyard(ObjectId),
}

/// A resolution paused on a player's choice.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Resume {
    pub ctx: Ctx,
    pub remaining: Vec<Effect>,
    pub after: After,
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

    pub(crate) fn cast_spell(
        &mut self,
        seat: Seat,
        object: ObjectId,
        targets: &[Target],
        payment: &ManaPayment,
    ) -> Result<(), RulesError> {
        let cost = self.card_def(object).cost.clone();
        self.pay_mana(seat, payment, &cost)?;
        self.objects[object].controller = seat;
        self.move_object(object, Zone::Stack);
        self.stack.push(StackObject { object, controller: seat, targets: targets.to_vec(), kind: StackKind::Spell });
        self.emit(Event::Cast { seat, object, targets: targets.to_vec() });
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
            let cost = self.card_def(object).ir.equip.clone().ok_or_else(|| RulesError::illegal("not equipment"))?;
            self.pay_mana(seat, payment, &cost)?;
            self.stack.push(StackObject { object, controller: seat, targets: targets.to_vec(), kind: StackKind::Equip { source: object } });
            self.emit(Event::Activated { seat, object, ability: index, targets: targets.to_vec() });
            self.give_priority(seat);
            return Ok(());
        }
        let ability = self.card_def(object).ir.activated.get(index as usize).cloned().ok_or_else(|| RulesError::illegal("no such ability"))?;
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
                        if self.objects.get(id).map(|o| o.controller == seat && o.zone == Zone::Battlefield).unwrap_or(false)
                            && self.object_matches(id, filter, &ctx)
                        {
                            self.sacrifice(seat, id);
                            paid = true;
                            break;
                        }
                    }
                    if !paid {
                        return Err(RulesError::illegal("the payment names nothing that can be sacrificed for this cost"));
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
                    self.emit(Event::Discarded { seat, objects: payment.discard.clone() });
                }
            }
        }
        self.stack.push(StackObject { object, controller: seat, targets: targets.to_vec(), kind: StackKind::Ability { source: object, index } });
        self.emit(Event::Activated { seat, object, ability: index, targets: targets.to_vec() });
        self.give_priority(seat);
        Ok(())
    }

    pub(crate) fn sacrifice(&mut self, seat: Seat, id: ObjectId) {
        self.emit(Event::Sacrificed { seat, object: id });
        self.move_object(id, Zone::Graveyard);
    }

    /// Resolve the top object. Returns `false` if resolution suspended on a choice.
    pub(crate) fn resolve(&mut self, so: StackObject) -> bool {
        let StackObject { object, controller, targets, kind } = so;
        match kind {
            StackKind::Spell => {
                let def = self.card_def(object).clone();
                let specs = self.cast_target_specs(object);
                let ctx = Ctx { you: controller, this: Some(object), targets: targets.clone(), triggering: None };
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
                // one zone list while a resolution is suspended on a choice.
                let effects = def.ir.spell.as_ref().map(|s| s.effects.clone()).unwrap_or_default();
                self.move_object(object, Zone::Graveyard);
                self.emit(Event::Resolved { object });
                self.run_effects(ctx, effects.into(), After::Nothing)
            }
            StackKind::Ability { source, index } => {
                let def = self.card_def(source).clone();
                let ability = def.ir.activated[index as usize].clone();
                let ctx = Ctx { you: controller, this: Some(source), targets: targets.clone(), triggering: None };
                if self.all_targets_illegal(&ability.targets, &ctx) {
                    return true;
                }
                self.run_effects(ctx, ability.effects.into(), After::Nothing)
            }
            StackKind::Equip { source } => {
                if let Some(Target::Object(t)) = targets.first() {
                    let legal = self.objects.get(*t).map(|o| o.zone == Zone::Battlefield && o.controller == controller).unwrap_or(false)
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
                let ctx = Ctx { you: controller, this: Some(source), targets: targets.clone(), triggering };
                if self.all_targets_illegal(trigger.targets(), &ctx) {
                    return true;
                }
                self.run_effects(ctx, trigger.effects().to_vec().into(), After::Nothing)
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
        !specs.iter().enumerate().any(|(i, spec)| {
            ctx.targets.get(i).map(|t| self.target_is_legal(*t, spec, ctx)).unwrap_or(false)
        })
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
    pub(crate) fn resume(&mut self, r: Resume) {
        let done = self.run_effects(r.ctx, r.remaining.into(), r.after);
        if done {
            self.give_priority_to_active();
        }
    }

    /// Run effects front to back. Returns `true` when finished; `false` when
    /// a player must choose something first (the state carries the resume).
    pub(crate) fn run_effects(&mut self, ctx: Ctx, mut work: VecDeque<Effect>, after: After) -> bool {
        while let Some(effect) = work.pop_front() {
            match self.apply_effect(&ctx, &effect) {
                Ok(()) => {}
                Err(pending) => {
                    let remaining: Vec<Effect> = work.into_iter().collect();
                    let resume = Resume { ctx: ctx.clone(), remaining, after };
                    self.pending = Some(match pending {
                        Suspend::Sacrifice { seat, filter, count } => PendingChoice::Sacrifice { seat, filter, count, resume },
                        Suspend::Discard { seat, count } => PendingChoice::EffectDiscard { seat, count, resume },
                    });
                    self.priority = None;
                    return false;
                }
            }
        }
        if let After::SpellToGraveyard(id) = after {
            if self.objects.get(id).map(|o| o.zone == Zone::Stack).unwrap_or(false) {
                self.move_object(id, Zone::Graveyard);
            }
        }
        true
    }
}

/// A choice an effect needs before it can continue.
pub(crate) enum Suspend {
    Sacrifice { seat: Seat, filter: Filter, count: i32 },
    Discard { seat: Seat, count: i32 },
}
