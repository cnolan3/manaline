//! Turn structure and priority (§3.3), and the auto-advance loop that
//! guarantees the engine never returns in a state where nobody can act.

use crate::event::Event;
use crate::game::{Expiry, Game, ModifierKind, PendingChoice};
use crate::types::{ObjectId, Phase};

impl Game {
    /// Run state-based actions, turn-based actions and phase advancement until
    /// some seat has a legal action or the game is over.
    pub(crate) fn settle(&mut self) {
        let mut guard = 0u32;
        loop {
            if self.outcome.is_some() {
                return;
            }
            self.check_state_based_actions();
            if self.outcome.is_some() {
                return;
            }
            // Triggers go on the stack before anyone receives priority (rule 117.5).
            if self.pending.is_none() {
                self.collect_triggers();
                if !self.place_triggers() {
                    return; // a controller must choose targets
                }
                if self.pending.is_none() && !self.fired.is_empty() {
                    continue;
                }
            }
            if self.pending.is_some() || self.priority.is_some() {
                return;
            }
            self.step_forward();
            guard += 1;
            assert!(
                guard < 10_000,
                "engine failed to reach a state where a seat can act (turn {}, {:?})",
                self.turn,
                self.phase
            );
        }
    }

    /// Nobody can act and nothing is pending: move the game forward one notch.
    fn step_forward(&mut self) {
        if !self.started {
            self.started = true;
            let first = self.turn_order.first().copied().unwrap_or(self.active_player);
            self.begin_turn(first);
            return;
        }
        if self.turn_aborted {
            self.turn_aborted = false;
            self.advance_turn();
            return;
        }
        // After first-strike damage and a round of priority, regular damage is dealt (rule 510.4).
        if self.phase == Phase::CombatDamage && self.combat_round == crate::game::CombatRound::FirstStrikeDone {
            self.begin_combat_damage(false);
            return;
        }
        match self.phase.next() {
            None => self.advance_turn(),
            Some(next) => {
                // Skip the blocker and damage steps when nothing attacked (rule 508.8).
                let next = match next {
                    Phase::DeclareBlockers | Phase::CombatDamage if !self.any_attackers() => Phase::EndCombat,
                    p => p,
                };
                self.enter_phase(next);
            }
        }
    }

    fn advance_turn(&mut self) {
        let next = self.next_in_turn_order_after(self.active_player).unwrap_or(self.active_player);
        self.begin_turn(next);
    }

    fn begin_turn(&mut self, seat: crate::types::Seat) {
        self.turn += 1;
        self.active_player = seat;
        self.turn_aborted = false;
        // "Until your next turn" effects this player created end now.
        for (_, obj) in self.objects.iter_mut() {
            obj.modifiers.retain(|m| m.expires != Expiry::TurnOf(seat));
        }
        self.emit(Event::TurnStarted {
            turn: self.turn,
            active: seat,
        });
        self.enter_phase(Phase::Untap);
    }

    pub(crate) fn enter_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.priority = None;
        self.passed_in_succession = 0;
        // Mana empties at the end of every step and phase (rule 500.4).
        for p in &mut self.players {
            p.mana_pool.clear();
        }
        self.emit(Event::PhaseChanged { phase });
        let active = self.active_player;
        match phase {
            Phase::Untap => self.untap_step(),
            Phase::Draw => {
                // In a two-player game the starting player skips their first draw
                // (rule 103.8a); in larger games nobody does (103.8c).
                if !(self.turn == 1 && self.seating.len() == 2) {
                    self.draw(active, 1);
                }
                self.give_priority_to_active();
            }
            Phase::DeclareAttackers => {
                if self.attack_candidates(active).is_empty() {
                    self.give_priority_to_active();
                } else {
                    self.pending = Some(PendingChoice::DeclareAttackers { seat: active });
                }
            }
            Phase::DeclareBlockers => {
                let mut defenders = self.defending_seats();
                if defenders.is_empty() {
                    self.give_priority_to_active();
                } else {
                    let head = defenders.remove(0);
                    self.pending = Some(PendingChoice::DeclareBlockers {
                        seat: head,
                        remaining: defenders,
                    });
                }
            }
            Phase::CombatDamage => {
                let first_strike = self.combat_has_first_strike();
                self.begin_combat_damage(first_strike);
            }
            Phase::Main2 => {
                self.remove_all_from_combat();
                self.combat_round = crate::game::CombatRound::None;
                self.give_priority_to_active();
            }
            Phase::Cleanup => self.cleanup_step(),
            Phase::Upkeep | Phase::Main1 | Phase::BeginCombat | Phase::EndCombat | Phase::End => {
                self.give_priority_to_active();
            }
        }
    }

    fn untap_step(&mut self) {
        let active = self.active_player;
        let ids: Vec<ObjectId> = self.players[active.index()].battlefield.clone();
        for id in ids {
            let obj = &mut self.objects[id];
            obj.summoning_sick = false;
            // "Doesn't untap during its controller's next untap step": skip once, then forget.
            let hold = |m: &crate::game::Modifier| m.kind == ModifierKind::SkipUntap && m.expires == Expiry::NextUntapOf(active);
            let held = obj.modifiers.iter().any(hold);
            obj.modifiers.retain(|m| !hold(m));
            if obj.tapped && !held {
                obj.tapped = false;
                self.emit(Event::Untapped { object: id });
            }
        }
        self.players[active.index()].lands_played_this_turn = 0;
    }

    fn cleanup_step(&mut self) {
        let active = self.active_player;
        let max = self.format.max_hand_size as usize;
        let hand = self.players[active.index()].hand.len();
        if hand > max && !self.is_eliminated(active) {
            self.pending = Some(PendingChoice::Discard {
                seat: active,
                count: (hand - max) as u8,
            });
            return;
        }
        self.finish_cleanup();
    }

    /// Rule 514.2: remove damage and end "until end of turn" effects. No priority
    /// is given in cleanup in v1 (no triggers can fire here yet), so the settle
    /// loop advances straight to the next turn.
    pub(crate) fn finish_cleanup(&mut self) {
        for (_, obj) in self.objects.iter_mut() {
            obj.damage = 0;
            obj.deathtouch_damaged = false;
            obj.modifiers.retain(|m| m.expires != Expiry::EndOfTurn);
        }
    }

    pub(crate) fn pass_priority(&mut self, seat: crate::types::Seat) {
        self.emit(Event::PriorityPassed { seat });
        self.passed_in_succession += 1;
        if self.passed_in_succession as usize >= self.turn_order.len() {
            self.passed_in_succession = 0;
            if let Some(top) = self.stack.pop() {
                if self.resolve(top) {
                    self.give_priority_to_active();
                }
            } else {
                self.priority = None;
            }
        } else {
            self.priority = self.next_in_turn_order_after(seat);
        }
    }
}
