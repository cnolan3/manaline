//! Triggered abilities (§3.5): collected from emitted events, matched against
//! each card's event patterns, then put on the stack in APNAP order, asking
//! for targets where a trigger has them. Delayed triggers that effects set
//! up ("at the beginning of the next end step") wait in `Game::delayed`.

use crate::action::{DamageTarget, Target};
use crate::event::{Event, EventBase};
use crate::filter::Ctx;
use crate::game::{Game, PendingChoice, StackKind, StackObject};
use crate::types::{Keyword, ObjectId, Phase, Seat, Zone};
use cardir::{DelayedAt, Effect, EventPattern, Filter};

/// Which ability of the source fired.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FiredKind {
    /// Index into the source's `triggers`.
    Card(u8),
    Prowess,
    /// A delayed trigger: effects to run under the context they were created in.
    Delayed {
        effects: Vec<Effect>,
        ctx: Ctx,
    },
}

/// A trigger that has fired but is not yet on the stack.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FiredTrigger {
    pub source: ObjectId,
    pub controller: Seat,
    pub kind: FiredKind,
    pub triggering: Option<Target>,
}

/// A trigger an effect set up for later.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DelayedTrigger {
    pub at: DelayedAt,
    pub source: ObjectId,
    pub controller: Seat,
    pub ctx: Ctx,
    pub effects: Vec<Effect>,
}

/// One thing that just happened, in the terms event patterns are written in.
enum Occurred {
    Enters(ObjectId),
    Dies(ObjectId),
    Attacks(ObjectId),
    Blocks { blocker: ObjectId, attacker: ObjectId },
    BecomesBlocked { attacker: ObjectId, blocker: ObjectId },
    CombatDamageToPlayer { source: ObjectId, player: Seat },
    Tapped(ObjectId),
    Step { phase: Phase, active: Seat },
    Cast { seat: Seat, spell: ObjectId },
    GainsLife(Seat),
    Discards(Seat),
}

impl Occurred {
    /// What `Triggering` refers to for this occurrence.
    fn triggering(&self) -> Target {
        match self {
            Occurred::Enters(id) | Occurred::Dies(id) | Occurred::Attacks(id) | Occurred::Tapped(id) => Target::Object(*id),
            Occurred::Blocks { attacker, .. } => Target::Object(*attacker),
            Occurred::BecomesBlocked { blocker, .. } => Target::Object(*blocker),
            Occurred::CombatDamageToPlayer { player, .. } => Target::Player(*player),
            Occurred::Step { active, .. } => Target::Player(*active),
            Occurred::Cast { spell, .. } => Target::Object(*spell),
            Occurred::GainsLife(s) | Occurred::Discards(s) => Target::Player(*s),
        }
    }

    /// An object that watches this occurrence even though it has left the
    /// battlefield (a dying creature's own "when ~ dies").
    fn extra_watcher(&self) -> Option<ObjectId> {
        match self {
            Occurred::Dies(id) => Some(*id),
            _ => None,
        }
    }
}

impl Game {
    /// Scan events emitted since the last scan and queue the triggers they cause.
    pub(crate) fn collect_triggers(&mut self) {
        let start = self.trigger_cursor;
        self.trigger_cursor = self.log.len();
        for i in start..self.log.len() {
            let event = self.log[i].clone();
            self.triggers_for_event(&event);
        }
    }

    fn triggers_for_event(&mut self, event: &Event) {
        match event {
            EventBase::ZoneChange {
                object,
                to: Zone::Battlefield,
                ..
            } => self.fire(Occurred::Enters(*object)),
            EventBase::ZoneChange {
                object,
                from: Zone::Battlefield,
                to: Zone::Graveyard,
            } => self.fire(Occurred::Dies(*object)),
            EventBase::Attacked { attackers, .. } => {
                for (a, _) in attackers {
                    self.fire(Occurred::Attacks(*a));
                }
            }
            EventBase::Blocked { blocks, .. } => {
                let mut blocked_attackers: Vec<ObjectId> = Vec::new();
                for (blocker, attacker) in blocks {
                    self.fire(Occurred::Blocks {
                        blocker: *blocker,
                        attacker: *attacker,
                    });
                    if !blocked_attackers.contains(attacker) {
                        blocked_attackers.push(*attacker);
                        self.fire(Occurred::BecomesBlocked {
                            attacker: *attacker,
                            blocker: *blocker,
                        });
                    }
                }
            }
            EventBase::Damage {
                source,
                to: DamageTarget::Player(s),
                amount,
                combat: true,
            } if *amount > 0 => self.fire(Occurred::CombatDamageToPlayer {
                source: *source,
                player: *s,
            }),
            EventBase::Tapped { object } => self.fire(Occurred::Tapped(*object)),
            EventBase::PhaseChanged { phase } if matches!(phase, Phase::Upkeep | Phase::End | Phase::BeginCombat) => {
                self.fire(Occurred::Step {
                    phase: *phase,
                    active: self.active_player,
                });
                if *phase == Phase::End {
                    self.fire_delayed(DelayedAt::NextEndStep);
                }
            }
            EventBase::Cast { seat, object, .. } => {
                if !self.card_def(*object).is_creature() {
                    // Prowess on each creature the caster controls.
                    for id in self.players[seat.index()].battlefield.clone() {
                        if self.is_creature(id) && self.has_keyword(id, Keyword::Prowess) {
                            self.fired.push(FiredTrigger {
                                source: id,
                                controller: *seat,
                                kind: FiredKind::Prowess,
                                triggering: Some(Target::Object(*object)),
                            });
                        }
                    }
                }
                self.fire(Occurred::Cast {
                    seat: *seat,
                    spell: *object,
                });
            }
            EventBase::LifeChanged { seat, from, to } if to > from => self.fire(Occurred::GainsLife(*seat)),
            EventBase::Discarded { seat, objects } => {
                for _ in objects {
                    self.fire(Occurred::Discards(*seat));
                }
            }
            _ => {}
        }
    }

    /// Queue every trigger on the battlefield (plus the occurrence's own
    /// extra watcher) whose event pattern matches, if its intervening "if" holds.
    fn fire(&mut self, o: Occurred) {
        let mut watchers = self.battlefield_objects();
        if let Some(extra) = o.extra_watcher() {
            if !watchers.contains(&extra) {
                watchers.push(extra);
            }
        }
        let triggering = o.triggering();
        for id in watchers {
            let controller = self.objects[id].controller;
            let def = self.card_def(id).clone();
            for (i, t) in def.ir.triggers.iter().enumerate() {
                let ctx = Ctx::new(controller, Some(id), Vec::new(), Some(triggering));
                if !self.event_matches(&t.event, &o, id, &ctx) {
                    continue;
                }
                if let Some(c) = &t.condition {
                    if !self.condition_holds(c, &ctx) {
                        continue;
                    }
                }
                self.fired.push(FiredTrigger {
                    source: id,
                    controller,
                    kind: FiredKind::Card(i as u8),
                    triggering: Some(triggering),
                });
            }
        }
    }

    fn event_matches(&self, pattern: &EventPattern, o: &Occurred, watcher: ObjectId, ctx: &Ctx) -> bool {
        match (pattern, o) {
            (EventPattern::ThisEnters, Occurred::Enters(id))
            | (EventPattern::ThisDies, Occurred::Dies(id))
            | (EventPattern::ThisAttacks, Occurred::Attacks(id))
            | (EventPattern::ThisBecomesTapped, Occurred::Tapped(id)) => *id == watcher,
            (EventPattern::ThisBlocks, Occurred::Blocks { blocker, .. }) => *blocker == watcher,
            (EventPattern::ThisBecomesBlocked, Occurred::BecomesBlocked { attacker, .. }) => *attacker == watcher,
            (EventPattern::ThisDealsCombatDamageToPlayer, Occurred::CombatDamageToPlayer { source, .. }) => *source == watcher,
            (EventPattern::Enters(f), Occurred::Enters(id)) | (EventPattern::Dies(f), Occurred::Dies(id)) => {
                self.object_matches(*id, f, ctx)
            }
            (
                EventPattern::Upkeep(whose),
                Occurred::Step {
                    phase: Phase::Upkeep,
                    active,
                },
            )
            | (EventPattern::EndStep(whose), Occurred::Step { phase: Phase::End, active })
            | (
                EventPattern::BeginCombat(whose),
                Occurred::Step {
                    phase: Phase::BeginCombat,
                    active,
                },
            ) => self.players_of(whose, ctx).contains(active),
            (EventPattern::Cast { who, filter }, Occurred::Cast { seat, spell }) => {
                self.players_of(who, ctx).contains(seat) && self.spell_matches(*spell, filter, ctx)
            }
            (EventPattern::GainsLife(whose), Occurred::GainsLife(seat)) | (EventPattern::Discards(whose), Occurred::Discards(seat)) => {
                self.players_of(whose, ctx).contains(seat)
            }
            (EventPattern::Any(es), _) => es.iter().any(|e| self.event_matches(e, o, watcher, ctx)),
            _ => false,
        }
    }

    /// Fire every delayed trigger whose moment this is.
    fn fire_delayed(&mut self, at: DelayedAt) {
        let due: Vec<DelayedTrigger> = {
            let (due, rest): (Vec<_>, Vec<_>) = self.delayed.drain(..).partition(|d| d.at == at);
            self.delayed = rest;
            due
        };
        for d in due {
            self.fired.push(FiredTrigger {
                source: d.source,
                controller: d.controller,
                kind: FiredKind::Delayed {
                    effects: d.effects,
                    ctx: d.ctx,
                },
                triggering: None,
            });
        }
    }

    /// Put fired triggers on the stack in APNAP order. Stops (returns `false`)
    /// when a trigger needs its controller to choose targets.
    pub(crate) fn place_triggers(&mut self) -> bool {
        if self.fired.is_empty() {
            return true;
        }
        // APNAP: active player's triggers first, then each other seat in turn order.
        let order = self.apnap();
        let mut fired = std::mem::take(&mut self.fired);
        fired.retain(|f| self.objects.get(f.source).is_some() && self.turn_order.contains(&f.controller));
        fired.sort_by_key(|f| order.iter().position(|s| *s == f.controller).unwrap_or(usize::MAX));
        let mut queue: std::collections::VecDeque<FiredTrigger> = fired.into();
        while let Some(f) = queue.pop_front() {
            let specs: Vec<Filter> = match &f.kind {
                FiredKind::Card(i) => self.card_def(f.source).ir.triggers[*i as usize].targets.clone(),
                _ => Vec::new(),
            };
            if specs.is_empty() {
                self.push_trigger(&f, Vec::new());
                continue;
            }
            let ctx = Ctx::new(f.controller, Some(f.source), Vec::new(), f.triggering);
            let candidates: Vec<Vec<Target>> = specs.iter().map(|s| self.targets_for(s, &ctx)).collect();
            if candidates.iter().any(|c| c.is_empty()) {
                continue; // no legal target: the trigger is removed from the stack (rule 603.3d)
            }
            // Ask the controller.
            self.fired = queue.into_iter().collect();
            self.pending = Some(PendingChoice::ChooseTargets {
                seat: f.controller,
                specs,
                trigger: f.clone(),
            });
            return false;
        }
        true
    }

    fn push_trigger(&mut self, f: &FiredTrigger, targets: Vec<Target>) {
        let kind = match &f.kind {
            FiredKind::Card(i) => StackKind::Trigger {
                source: f.source,
                index: *i,
                triggering: f.triggering,
            },
            FiredKind::Prowess => StackKind::Prowess { source: f.source },
            FiredKind::Delayed { effects, ctx } => StackKind::Delayed {
                source: f.source,
                effects: effects.clone(),
                ctx: ctx.clone(),
            },
        };
        let description = self.describe_stack_kind(&kind);
        self.stack.push(StackObject {
            object: f.source,
            controller: f.controller,
            targets: targets.clone(),
            kind,
            modes: Vec::new(),
        });
        self.emit(Event::Triggered {
            source: f.source,
            description,
            targets,
        });
    }

    /// Answer a `ChooseTargets` for a trigger, then keep placing the rest.
    pub(crate) fn choose_trigger_targets(&mut self, targets: &[Target]) {
        if let Some(PendingChoice::ChooseTargets { trigger, .. }) = self.pending.take() {
            self.push_trigger(&trigger, targets.to_vec());
        }
        if self.place_triggers() {
            // Triggers went on the stack while someone held priority; the
            // active player gets priority to respond (rule 117.3b).
            if self.priority.is_none() {
                self.give_priority_to_active();
            }
        }
    }
}
