//! `engine::interp`: one arm per IR effect (§4.1). Adding a primitive means
//! an enum variant, an arm here, a renderer arm, and a test.
//!
//! `step` applies one effect. Effects that unfold into more work (a
//! `Sequence`, a branch of a `Conditional`, "each player discards") hand back
//! a `Frame` for the evaluator in `stack.rs` to push, so a choice raised
//! anywhere inside them pauses the whole resolution and resumes it intact.

use crate::action::{DamageTarget, Target};
use crate::card::CardDef;
use crate::event::Event;
use crate::filter::Ctx;
use crate::game::ActReason;
use crate::game::{Expiry, Game, Modifier, ModifierKind};
use crate::stack::{Frame, MAY_BIND};
use crate::types::{Keyword, Mana, ObjectId, Seat, Zone};
use cardir::{Condition, CounterKind, Effect, Filter, Quantity, Ref};

/// The `Ref` positions where a `Chosen` may sit: an effect's own target.
fn direct_refs_mut(e: &mut Effect) -> Vec<&mut Ref> {
    match e {
        Effect::DealDamage { to, .. } => vec![to],
        Effect::Destroy { target }
        | Effect::Exile { target }
        | Effect::Tap { target }
        | Effect::Untap { target }
        | Effect::ReturnToHand { target }
        | Effect::CounterSpell { target }
        | Effect::ModifyPt { target, .. }
        | Effect::GrantKeyword { target, .. }
        | Effect::AddCounters { target, .. }
        | Effect::ReturnFromGraveyard { target, .. } => vec![target],
        _ => Vec::new(),
    }
}

/// The verb of an effect, for the menu that asks a player to choose for it.
fn verb_of(e: &Effect) -> &'static str {
    match e {
        Effect::DealDamage { .. } => "Deal damage to",
        Effect::Destroy { .. } => "Destroy",
        Effect::Exile { .. } => "Exile",
        Effect::Tap { .. } => "Tap",
        Effect::Untap { .. } => "Untap",
        Effect::ReturnToHand { .. } | Effect::ReturnFromGraveyard { .. } => "Return",
        Effect::CounterSpell { .. } => "Counter",
        Effect::AddCounters { .. } => "Put counters on",
        _ => "Choose",
    }
}

impl Game {
    /// Apply one effect. Returns frames for work that must run (and may
    /// pause) after this step, in push order: the last is done first.
    pub(crate) fn step(&mut self, ctx: &Ctx, specs: &[Filter], effect: &Effect) -> Vec<Frame> {
        // An effect that picks its own object ("a creature you control") first
        // asks for the pick, then runs again with the answer bound by name.
        let mut resolved = effect.clone();
        for r in direct_refs_mut(&mut resolved) {
            if let Ref::Chosen { who, filter, count, bind } = r {
                let name = bind.clone().unwrap_or_else(|| format!("$chosen{}", ctx.bindings.len()));
                let seat = self.players_of(who, ctx).first().copied().unwrap_or(ctx.you);
                let options = self.choosable(filter, ctx);
                let (min, max) = match count {
                    Quantity::Exactly(n) => (*n as usize, *n as usize),
                    Quantity::UpTo(n) => (0, *n as usize),
                    Quantity::AnyNumber => (0, options.len()),
                };
                let ask = Frame::Choose {
                    seat,
                    options,
                    min,
                    max,
                    prompt: verb_of(effect).into(),
                    reason: ActReason::Choice,
                    bind: name.clone(),
                };
                *r = Ref::Named(name);
                return vec![
                    Frame::Effects {
                        effects: vec![resolved],
                        next: 0,
                    },
                    ask,
                ];
            }
        }
        self.apply_effect(ctx, specs, effect)
    }

    fn apply_effect(&mut self, ctx: &Ctx, specs: &[Filter], effect: &Effect) -> Vec<Frame> {
        match effect {
            Effect::DealDamage { amount, to } => {
                let n = self.eval_amount(amount, ctx);
                let Some(source) = ctx.this else {
                    return Vec::new();
                };
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
                return vec![Frame::Discard {
                    seats: self.players_of(player, ctx),
                    count: self.eval_amount(count, ctx),
                    random: *random,
                }];
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
            Effect::ModifyPt {
                target,
                power,
                toughness,
                keywords,
                until: _,
            } => {
                let p = self.eval_amount(power, ctx);
                let t = self.eval_amount(toughness, ctx);
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone != Zone::Battlefield {
                        continue;
                    }
                    let obj = &mut self.objects[id];
                    obj.modifiers.push(Modifier {
                        kind: ModifierKind::Pt { power: p, toughness: t },
                        expires: Expiry::EndOfTurn,
                    });
                    for k in keywords {
                        obj.modifiers.push(Modifier {
                            kind: ModifierKind::Keyword(*k),
                            expires: Expiry::EndOfTurn,
                        });
                    }
                }
            }
            Effect::GrantKeyword { target, keyword, until: _ } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone == Zone::Battlefield {
                        self.objects[id].modifiers.push(Modifier {
                            kind: ModifierKind::Keyword(*keyword),
                            expires: Expiry::EndOfTurn,
                        });
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
                    self.emit(Event::CountersAdded {
                        object: id,
                        counter: format!("{kind:?}"),
                        count: n as i32,
                    });
                }
            }
            Effect::AddMana { color, amount } => {
                let n = self.eval_amount(amount, ctx).max(0) as u8;
                let mana = match color {
                    Some(c) => Mana::Colored(*c),
                    None => Mana::Colorless,
                };
                self.players[ctx.you.index()].mana_pool.add(mana, n);
                self.emit(Event::ManaAdded {
                    seat: ctx.you,
                    mana,
                    amount: n,
                });
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
                return vec![Frame::Sacrifice {
                    seats: self.players_of(player, ctx),
                    filter: filter.clone(),
                    count: self.eval_amount(count, ctx),
                }];
            }
            Effect::Mill { player, count } => {
                let n = self.eval_amount(count, ctx).max(0) as usize;
                for seat in self.players_of(player, ctx) {
                    for _ in 0..n {
                        let Some(id) = self.players[seat.index()].library.pop() else {
                            break;
                        };
                        self.move_object(id, Zone::Graveyard);
                    }
                }
            }
            Effect::ReturnFromGraveyard { target, to } => {
                for id in self.objects_of(target, ctx) {
                    if self.objects[id].zone != Zone::Graveyard {
                        continue;
                    }
                    match to {
                        cardir::ReturnZone::Hand => self.move_object(id, Zone::Hand),
                        cardir::ReturnZone::Battlefield => {
                            self.objects[id].controller = ctx.you;
                            self.move_object(id, Zone::Battlefield);
                            self.objects[id].summoning_sick = true;
                        }
                    }
                }
            }
            Effect::Sequence(es) => {
                return vec![Frame::Effects {
                    effects: es.clone(),
                    next: 0,
                }];
            }
            Effect::Conditional { if_, then, else_ } => {
                let holds = match if_ {
                    Condition::Controls { player, filter, at_least } => {
                        let seats = self.players_of(player, ctx);
                        seats.iter().any(|&s| {
                            let sub = Ctx { you: s, ..ctx.clone() };
                            let n = self.players[s.index()]
                                .battlefield
                                .iter()
                                .filter(|id| self.object_matches(**id, filter, &sub))
                                .count();
                            n as i32 >= *at_least
                        })
                    }
                };
                let branch = if holds { Some(then) } else { else_.as_ref() };
                return branch
                    .map(|e| Frame::Effects {
                        effects: vec![(**e).clone()],
                        next: 0,
                    })
                    .into_iter()
                    .collect();
            }
            Effect::May { effect, then, otherwise } => {
                let label = match ctx.this {
                    Some(this) => capitalize(&cardir::render_clause(&self.card_def(this).ir, specs, effect)),
                    None => "Do it".into(),
                };
                let mut did = vec![(**effect).clone()];
                did.extend(then.iter().cloned());
                // The option is asked first (pushed last); the branch it picked runs after.
                return vec![
                    Frame::Branch {
                        bind: MAY_BIND.into(),
                        branches: vec![did, otherwise.clone()],
                    },
                    Frame::ChooseOption {
                        seat: ctx.you,
                        labels: vec![label, "Don't".into()],
                        bind: MAY_BIND.into(),
                    },
                ];
            }
            Effect::Unsupported { .. } => {}
        }
        Vec::new()
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
                self.emit(Event::Damage {
                    source,
                    to,
                    amount,
                    combat,
                });
                self.emit(Event::LifeChanged {
                    seat: s,
                    from,
                    to: from - amount,
                });
            }
            DamageTarget::Object(o) => {
                let Some(obj) = self.objects.get(o) else {
                    return;
                };
                if obj.zone != Zone::Battlefield || !self.is_creature(o) {
                    return;
                }
                let deathtouch = self.has_keyword(source, Keyword::Deathtouch);
                let obj = &mut self.objects[o];
                obj.damage += amount;
                if deathtouch {
                    obj.deathtouch_damaged = true;
                }
                self.emit(Event::Damage {
                    source,
                    to,
                    amount,
                    combat,
                });
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
        let Some(obj) = self.objects.get(id) else {
            return;
        };
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
        self.emit(Event::Discarded {
            seat,
            objects: objects.to_vec(),
        });
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
        self.emit(Event::ZoneChange {
            object: id,
            from: Zone::OutOfGame,
            to: Zone::Battlefield,
        });
        id
    }

    /// Answer a pending pick with what was chosen, then continue the
    /// resolution that asked.
    pub(crate) fn answer_choice(&mut self, targets: &[Target]) {
        match self.pending.take() {
            Some(crate::game::PendingChoice::Choose { bind, mut resume, .. }) => {
                resume.ctx.bindings.insert(bind, targets.to_vec());
                self.resume(resume);
            }
            other => self.pending = other,
        }
    }

    /// Answer a pending option ("you may": do it, or don't), then continue.
    pub(crate) fn answer_option(&mut self, mode: u8) {
        match self.pending.take() {
            Some(crate::game::PendingChoice::ChooseOption { bind, mut resume, .. }) => {
                resume.ctx.options.insert(bind, mode);
                self.resume(resume);
            }
            other => self.pending = other,
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
