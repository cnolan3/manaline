//! Triggered abilities (§3.5): collected from emitted events, then put on
//! the stack in APNAP order, asking for targets where a trigger has them.

use crate::action::{DamageTarget, Target};
use crate::event::{Event, EventBase};
use crate::filter::Ctx;
use crate::game::{Game, PendingChoice, StackKind, StackObject};
use crate::types::{Keyword, ObjectId, Phase, Seat, Zone};
use cardir::{Filter, Trigger};

/// A trigger that has fired but is not yet on the stack.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FiredTrigger {
    pub source: ObjectId,
    pub controller: Seat,
    /// Index into the source's `triggers`, or `None` for prowess.
    pub index: Option<u8>,
    pub triggering: Option<Target>,
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
            } => {
                self.fire_matching(
                    *object,
                    |t| matches!(t, Trigger::Etb { .. } | Trigger::EtbOrDies { .. }),
                    Some(Target::Object(*object)),
                );
            }
            EventBase::ZoneChange {
                object,
                from: Zone::Battlefield,
                to: Zone::Graveyard,
            } => {
                self.fire_matching(
                    *object,
                    |t| matches!(t, Trigger::Dies { .. } | Trigger::EtbOrDies { .. }),
                    Some(Target::Object(*object)),
                );
                if self.is_creature(*object) {
                    self.fire_creature_dies(*object);
                }
            }
            EventBase::Attacked { attackers, .. } => {
                for (a, _) in attackers {
                    self.fire_matching(*a, |t| matches!(t, Trigger::Attacks { .. }), Some(Target::Object(*a)));
                }
            }
            EventBase::Damage {
                source,
                to: DamageTarget::Player(s),
                amount,
                combat: true,
            } if *amount > 0 => {
                self.fire_matching(
                    *source,
                    |t| matches!(t, Trigger::CombatDamageToPlayer { .. }),
                    Some(Target::Player(*s)),
                );
            }
            EventBase::Tapped { object } => {
                self.fire_matching(
                    *object,
                    |t| matches!(t, Trigger::BecomesTapped { .. }),
                    Some(Target::Object(*object)),
                );
            }
            EventBase::PhaseChanged { phase: Phase::Upkeep } => self.fire_step_triggers(true),
            EventBase::PhaseChanged { phase: Phase::End } => self.fire_step_triggers(false),
            EventBase::Cast { seat, object, .. } if !self.card_def(*object).is_creature() => {
                // Prowess on each creature the caster controls.
                for id in self.players[seat.index()].battlefield.clone() {
                    if self.is_creature(id) && self.has_keyword(id, Keyword::Prowess) {
                        self.fired.push(FiredTrigger {
                            source: id,
                            controller: *seat,
                            index: None,
                            triggering: Some(Target::Object(*object)),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn fire_matching(&mut self, source: ObjectId, pred: impl Fn(&Trigger) -> bool, triggering: Option<Target>) {
        let Some(obj) = self.objects.get(source) else {
            return;
        };
        let controller = obj.controller;
        let def = self.card_def(source).clone();
        for (i, t) in def.ir.triggers.iter().enumerate() {
            if pred(t) {
                self.fired.push(FiredTrigger {
                    source,
                    controller,
                    index: Some(i as u8),
                    triggering,
                });
            }
        }
    }

    /// "Whenever a creature dies" triggers on everything watching, including
    /// the dying creature's own (leaves-the-battlefield abilities look back).
    fn fire_creature_dies(&mut self, dying: ObjectId) {
        let mut watchers = self.battlefield_objects();
        if !watchers.contains(&dying) {
            watchers.push(dying);
        }
        for id in watchers {
            let controller = self.objects[id].controller;
            let def = self.card_def(id).clone();
            for (i, t) in def.ir.triggers.iter().enumerate() {
                let Trigger::CreatureDies { filter, .. } = t else { continue };
                let ctx = Ctx::simple(controller, Some(id));
                if self.object_matches(dying, filter, &ctx) {
                    self.fired.push(FiredTrigger {
                        source: id,
                        controller,
                        index: Some(i as u8),
                        triggering: Some(Target::Object(dying)),
                    });
                }
            }
        }
    }

    /// Upkeep and end-step triggers whose "whose" includes the active player.
    fn fire_step_triggers(&mut self, upkeep: bool) {
        let active = self.active_player;
        for id in self.battlefield_objects() {
            let controller = self.objects[id].controller;
            let def = self.card_def(id).clone();
            for (i, t) in def.ir.triggers.iter().enumerate() {
                let whose = match (t, upkeep) {
                    (Trigger::Upkeep { whose, .. }, true) | (Trigger::EndStep { whose, .. }, false) => whose,
                    _ => continue,
                };
                let ctx = Ctx::simple(controller, Some(id));
                if self.players_of(whose, &ctx).contains(&active) {
                    self.fired.push(FiredTrigger {
                        source: id,
                        controller,
                        index: Some(i as u8),
                        triggering: Some(Target::Player(active)),
                    });
                }
            }
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
        // Objects that left the battlefield before their non-dies trigger was
        // placed still trigger (the ability exists independently), except ETB
        // triggers of things that left again — rare; keep it simple.
        let mut queue: std::collections::VecDeque<FiredTrigger> = fired.into();
        while let Some(f) = queue.pop_front() {
            let specs: Vec<Filter> = match f.index {
                Some(i) => self.card_def(f.source).ir.triggers[i as usize].targets().to_vec(),
                None => Vec::new(),
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
        let kind = match f.index {
            Some(i) => StackKind::Trigger {
                source: f.source,
                index: i,
                triggering: f.triggering,
            },
            None => StackKind::Prowess { source: f.source },
        };
        let description = self.describe_stack_kind(&kind);
        self.stack.push(StackObject {
            object: f.source,
            controller: f.controller,
            targets: targets.clone(),
            kind,
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
