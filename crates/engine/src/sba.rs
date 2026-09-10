//! State-based actions (rule 704) and elimination (rule 800.4a), N-player from day one.

use crate::action::AttackTarget;
use crate::event::Event;
use crate::format::FormatRule;
use crate::game::{Elimination, Game, Outcome, PendingChoice};
use crate::types::{ObjectId, Seat, Zone};

impl Game {
    pub(crate) fn check_state_based_actions(&mut self) {
        loop {
            let mut acted = false;

            for seat in self.turn_order.clone() {
                let p = &self.players[seat.index()];
                let reason = if p.life <= 0 {
                    Some(Elimination::LifeZero)
                } else if p.drew_from_empty {
                    Some(Elimination::DrewFromEmptyLibrary)
                } else if p.poison >= self.poison_threshold() {
                    Some(Elimination::Poison)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    self.eliminate(seat, reason);
                    acted = true;
                }
            }

            for id in self.battlefield_objects() {
                let def = self.card_def(id).clone();
                if let Some((_, toughness)) = self.effective_stats(id) {
                    let obj = &self.objects[id];
                    if toughness <= 0 {
                        self.move_object(id, Zone::Graveyard);
                        acted = true;
                        continue;
                    }
                    if (obj.damage >= toughness || obj.deathtouch_damaged) && !self.has_keyword(id, crate::types::Keyword::Indestructible) {
                        self.move_object(id, Zone::Graveyard);
                        acted = true;
                        continue;
                    }
                }
                // +1/+1 and -1/-1 counters annihilate (rule 704.5q).
                let c = &self.objects[id].counters;
                if c.plus1 > 0 && c.minus1 > 0 {
                    let n = c.plus1.min(c.minus1);
                    let c = &mut self.objects[id].counters;
                    c.plus1 -= n;
                    c.minus1 -= n;
                    acted = true;
                }
                // An aura attached to nothing legal goes to the graveyard (704.5m);
                // equipment attached illegally becomes unattached (704.5n).
                if def.is_aura() || def.is_equipment() {
                    let attached = self.objects[id].attached_to;
                    let legal = attached
                        .and_then(|t| self.objects.get(t))
                        .map(|t| t.zone == Zone::Battlefield)
                        .unwrap_or(false)
                        && attached
                            .map(|t| {
                                let ctx = crate::filter::Ctx::simple(self.objects[id].controller, Some(id));
                                match &def.ir.enchant {
                                    Some(f) => self.object_matches(t, f, &ctx),
                                    None => self.is_creature(t) && self.objects[t].controller == self.objects[id].controller,
                                }
                            })
                            .unwrap_or(false);
                    if !legal {
                        if def.is_aura() {
                            self.move_object(id, Zone::Graveyard);
                            acted = true;
                        } else if attached.is_some() {
                            self.objects[id].attached_to = None;
                            acted = true;
                        }
                    }
                }
            }
            // Tokens anywhere but the battlefield cease to exist (704.5d).
            let stray_tokens: Vec<ObjectId> = self
                .objects
                .iter()
                .filter(|(_, o)| o.zone != Zone::Battlefield && o.zone != Zone::OutOfGame && o.zone != Zone::Stack)
                .filter(|(_, o)| self.card_by_id(o.card).token)
                .map(|(id, _)| id)
                .collect();
            for id in stray_tokens {
                self.move_object(id, Zone::OutOfGame);
                acted = true;
            }

            if !acted {
                break;
            }
        }

        if self.outcome.is_none() {
            match self.turn_order.len() {
                0 => self.end_game(Outcome::Draw),
                1 => self.end_game(Outcome::Winner(self.turn_order[0])),
                _ => {}
            }
        }
    }

    fn poison_threshold(&self) -> u8 {
        self.format
            .rules
            .iter()
            .find_map(|r| match r {
                FormatRule::Poison(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(10)
    }

    fn end_game(&mut self, outcome: Outcome) {
        self.outcome = Some(outcome);
        self.pending = None;
        self.priority = None;
        self.emit(Event::GameOver { outcome });
    }

    /// A player leaves the game: they leave `turn_order`, every object they
    /// own leaves the game, and anything they control on the stack ceases to
    /// exist. If it was their turn, the turn ends.
    pub(crate) fn eliminate(&mut self, seat: Seat, reason: Elimination) {
        if self.is_eliminated(seat) {
            return;
        }
        self.players[seat.index()].eliminated = Some(reason.clone());
        self.emit(Event::Eliminated { seat, reason });
        self.turn_order.retain(|&s| s != seat);

        let mut owned: Vec<ObjectId> = self
            .objects
            .iter()
            .filter(|(_, o)| o.owner == seat && o.zone != Zone::OutOfGame)
            .map(|(id, _)| id)
            .collect();
        owned.sort();
        for id in owned {
            self.move_object(id, Zone::OutOfGame);
        }
        self.stack.retain(|s| s.controller != seat);
        self.fired.retain(|f| f.controller != seat);
        self.delayed.retain(|d| d.controller != seat);

        // Creatures attacking a player who left are removed from combat.
        for (_, o) in self.objects.iter_mut() {
            if o.attacking == Some(AttackTarget::Player(seat)) {
                o.attacking = None;
                o.blocked = false;
                o.blocked_by.clear();
            }
        }

        if self.priority == Some(seat) {
            self.priority = self.next_in_turn_order_after(seat);
        }
        if seat == self.active_player {
            self.turn_aborted = true;
            self.pending = None;
            self.priority = None;
            return;
        }
        if self.pending.as_ref().map(|p| p.seat()) == Some(seat) {
            match self.pending.take() {
                Some(PendingChoice::DeclareBlockers { remaining, .. }) => self.continue_blockers(remaining),
                Some(PendingChoice::Mulligan { .. }) | Some(PendingChoice::BottomCards { .. }) => self.advance_mulligan(seat),
                Some(PendingChoice::ChooseTargets { .. }) => {
                    let placed = self.place_triggers();
                    if placed && self.priority.is_none() {
                        self.give_priority_to_active();
                    }
                }
                // The chooser left: the resolution continues with nothing picked.
                Some(PendingChoice::Choose { resume, .. }) | Some(PendingChoice::ChooseOption { resume, .. }) => self.resume(resume),
                _ => {}
            }
        }
    }
}
