//! `engine::interp`: one arm per IR effect (§4.1). Adding a primitive means
//! an enum variant, an arm here, a renderer arm, and a test.

use crate::action::{DamageTarget, Target};
use crate::card::CardDef;
use crate::event::Event;
use crate::filter::Ctx;
use crate::game::{Expiry, Game, Modifier, ModifierKind};
use crate::stack::Suspend;
use crate::types::{Keyword, Mana, ObjectId, Seat, Zone};
use cardir::{Condition, CounterKind, Effect};

impl Game {
    /// Apply one effect, or report the choice it needs first.
    pub(crate) fn apply_effect(&mut self, ctx: &Ctx, effect: &Effect) -> Result<(), Suspend> {
        match effect {
            Effect::DealDamage { amount, to } => {
                let n = self.eval_amount(amount, ctx);
                let Some(source) = ctx.this else { return Ok(()) };
                for target in self.refs_of(to, ctx) {
                    let dt = match target {
                        Target::Object(o) => DamageTarget::Object(o),
                        Target::Player(s) => DamageTarget::Player(s),
                    };
                    self.deal_damage(source, dt, n, false);
                }
            }
            Effect::Destroy { target } => {
                for id in self.objects_of(target, ctx) {
                    self.destroy(id);
                }
            }
            Effect::Exile { target } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone == Zone::Battlefield {
                        self.move_object(id, Zone::Exile);
                    }
                }
            }
            Effect::Draw { player, count } => {
                let n = self.eval_amount(count, ctx).max(0) as usize;
                for seat in self.players_of(player, ctx) {
                    self.draw(seat, n);
                }
            }
            Effect::Discard { player, count, random } => {
                let n = self.eval_amount(count, ctx).max(0);
                let seats = self.players_of(player, ctx);
                // Several players discarding by choice would need several suspensions;
                // v1 cards only ever name one player or discard at random.
                for seat in seats {
                    let hand = self.players[seat.index()].hand.len() as i32;
                    if hand == 0 || n == 0 {
                        continue;
                    }
                    if *random || hand <= n {
                        let mut hand: Vec<ObjectId> = self.players[seat.index()].hand.clone();
                        if *random {
                            use rand::seq::SliceRandom;
                            hand.shuffle(&mut self.rng);
                        }
                        let chosen: Vec<ObjectId> = hand.into_iter().take(n as usize).collect();
                        self.discard(seat, &chosen);
                    } else {
                        return Err(Suspend::Discard { seat, count: n });
                    }
                }
            }
            Effect::GainLife { player, amount } => {
                let n = self.eval_amount(amount, ctx);
                for seat in self.players_of(player, ctx) {
                    self.gain_life(seat, n);
                }
            }
            Effect::LoseLife { player, amount } => {
                let n = self.eval_amount(amount, ctx);
                for seat in self.players_of(player, ctx) {
                    let from = self.players[seat.index()].life;
                    self.players[seat.index()].life = from - n;
                    self.emit(Event::LifeChanged { seat, from, to: from - n });
                }
            }
            Effect::ModifyPt { target, power, toughness, keywords, until: _ } => {
                let p = self.eval_amount(power, ctx);
                let t = self.eval_amount(toughness, ctx);
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone != Zone::Battlefield {
                        continue;
                    }
                    let obj = &mut self.objects[id];
                    obj.modifiers.push(Modifier { kind: ModifierKind::Pt { power: p, toughness: t }, expires: Expiry::EndOfTurn });
                    for k in keywords {
                        obj.modifiers.push(Modifier { kind: ModifierKind::Keyword(*k), expires: Expiry::EndOfTurn });
                    }
                }
            }
            Effect::GrantKeyword { target, keyword, until: _ } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone == Zone::Battlefield {
                        self.objects[id].modifiers.push(Modifier { kind: ModifierKind::Keyword(*keyword), expires: Expiry::EndOfTurn });
                    }
                }
            }
            Effect::CreateToken { spec, count } => {
                let n = self.eval_amount(count, ctx).max(0);
                for _ in 0..n {
                    self.create_token(ctx.you, spec);
                }
            }
            Effect::AddCounters { target, kind, count } => {
                let n = self.eval_amount(count, ctx).max(0) as u8;
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone != Zone::Battlefield {
                        continue;
                    }
                    match kind {
                        CounterKind::Plus1Plus1 => self.objects[id].counters.plus1 = self.objects[id].counters.plus1.saturating_add(n),
                        CounterKind::Minus1Minus1 => self.objects[id].counters.minus1 = self.objects[id].counters.minus1.saturating_add(n),
                    }
                    self.emit(Event::CountersAdded { object: id, counter: format!("{kind:?}"), count: n as i32 });
                }
            }
            Effect::AddMana { color, amount } => {
                let n = self.eval_amount(amount, ctx).max(0) as u8;
                let mana = match color {
                    Some(c) => Mana::Colored(*c),
                    None => Mana::Colorless,
                };
                self.players[ctx.you.index()].mana_pool.add(mana, n);
                self.emit(Event::ManaAdded { seat: ctx.you, mana, amount: n });
            }
            Effect::Tap { target } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone == Zone::Battlefield && !self.objects[id].tapped {
                        self.objects[id].tapped = true;
                        self.emit(Event::Tapped { object: id });
                    }
                }
            }
            Effect::Untap { target } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone == Zone::Battlefield && self.objects[id].tapped {
                        self.objects[id].tapped = false;
                        self.emit(Event::Untapped { object: id });
                    }
                }
            }
            Effect::ReturnToHand { target } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone == Zone::Battlefield {
                        self.move_object(id, Zone::Hand);
                    }
                }
            }
            Effect::CounterSpell { target } => {
                for id in self.objects_of(target, ctx) {
                    self.counter_spell(id);
                }
            }
            Effect::Sacrifice { player, filter, count } => {
                let n = self.eval_amount(count, ctx).max(0);
                for seat in self.players_of(player, ctx) {
                    let sub = Ctx { you: seat, ..ctx.clone() };
                    let candidates: Vec<ObjectId> = self.players[seat.index()]
                        .battlefield
                        .clone()
                        .into_iter()
                        .filter(|id| self.object_matches(*id, filter, &sub))
                        .collect();
                    if candidates.is_empty() || n == 0 {
                        continue;
                    }
                    if candidates.len() as i32 <= n {
                        for id in candidates {
                            self.sacrifice(seat, id);
                        }
                    } else {
                        return Err(Suspend::Sacrifice { seat, filter: filter.clone(), count: n });
                    }
                }
            }
            Effect::Sequence(es) => {
                // Flattened by the caller's work list; if we get here, run inline.
                for e in es {
                    self.apply_effect(ctx, e)?;
                }
            }
            Effect::Conditional { if_, then, else_ } => {
                let holds = match if_ {
                    Condition::Controls { player, filter, at_least } => {
                        let seats = self.players_of(player, ctx);
                        seats.iter().any(|&s| {
                            let sub = Ctx { you: s, ..ctx.clone() };
                            let n = self.players[s.index()].battlefield.iter().filter(|id| self.object_matches(**id, filter, &sub)).count();
                            n as i32 >= *at_least
                        })
                    }
                };
                if holds {
                    self.apply_effect(ctx, then)?;
                } else if let Some(e) = else_ {
                    self.apply_effect(ctx, e)?;
                }
            }
            Effect::Unsupported { .. } => {}
        }
        Ok(())
    }

    /// Damage from a source: marks it on creatures (deathtouch remembered),
    /// subtracts life from players, and handles lifelink.
    pub(crate) fn deal_damage(&mut self, source: ObjectId, to: DamageTarget, amount: i32, combat: bool) {
        if amount <= 0 {
            return;
        }
        match to {
            DamageTarget::Player(s) => {
                if !self.turn_order.contains(&s) {
                    return;
                }
                let from = self.players[s.index()].life;
                self.players[s.index()].life = from - amount;
                self.emit(Event::Damage { source, to, amount, combat });
                self.emit(Event::LifeChanged { seat: s, from, to: from - amount });
            }
            DamageTarget::Object(o) => {
                let Some(obj) = self.objects.get(o) else { return };
                if obj.zone != Zone::Battlefield || !self.is_creature(o) {
                    return;
                }
                let deathtouch = self.has_keyword(source, Keyword::Deathtouch);
                let obj = &mut self.objects[o];
                obj.damage += amount;
                if deathtouch {
                    obj.deathtouch_damaged = true;
                }
                self.emit(Event::Damage { source, to, amount, combat });
            }
        }
        if self.has_keyword(source, Keyword::Lifelink) {
            let controller = self.objects[source].controller;
            self.gain_life(controller, amount);
        }
    }

    pub(crate) fn gain_life(&mut self, seat: Seat, n: i32) {
        if n <= 0 || !self.turn_order.contains(&seat) {
            return;
        }
        let from = self.players[seat.index()].life;
        self.players[seat.index()].life = from + n;
        self.emit(Event::LifeChanged { seat, from, to: from + n });
    }

    /// Destroy a permanent: to the graveyard unless indestructible.
    pub(crate) fn destroy(&mut self, id: ObjectId) {
        let Some(obj) = self.objects.get(id) else { return };
        if obj.zone != Zone::Battlefield || self.has_keyword(id, Keyword::Indestructible) {
            return;
        }
        self.move_object(id, Zone::Graveyard);
    }

    pub(crate) fn discard(&mut self, seat: Seat, objects: &[ObjectId]) {
        for &id in objects {
            if self.players[seat.index()].hand.contains(&id) {
                self.move_object(id, Zone::Graveyard);
            }
        }
        self.emit(Event::Discarded { seat, objects: objects.to_vec() });
    }

    pub(crate) fn create_token(&mut self, seat: Seat, spec: &cardir::TokenSpec) -> ObjectId {
        let def = CardDef::token(spec);
        let card_id = (self.cards.len() + self.tokens.len()) as u32;
        self.tokens.push(def);
        let id = self.objects.insert_with_key(|id| {
            let mut o = crate::game::GameObject::new(id, card_id, seat);
            o.zone = Zone::Battlefield;
            o.summoning_sick = true;
            o
        });
        self.players[seat.index()].battlefield.push(id);
        self.emit(Event::TokenCreated { seat, object: id });
        self.emit(Event::ZoneChange { object: id, from: Zone::OutOfGame, to: Zone::Battlefield });
        id
    }

    /// Answer a pending sacrifice or effect-driven discard and resume.
    pub(crate) fn answer_choice(&mut self, seat: Seat, objects: &[ObjectId]) {
        let pending = self.pending.take();
        match pending {
            Some(crate::game::PendingChoice::Sacrifice { resume, .. }) => {
                for &id in objects {
                    self.sacrifice(seat, id);
                }
                self.resume(resume);
            }
            Some(crate::game::PendingChoice::EffectDiscard { resume, .. }) => {
                self.discard(seat, objects);
                self.resume(resume);
            }
            other => self.pending = other,
        }
    }
}
