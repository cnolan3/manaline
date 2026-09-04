//! Combat: attackers, sequential multiplayer blockers, and damage assignment
//! under the current (post-Foundations) rules — no damage assignment order.

use crate::action::{AttackTarget, DamageTarget};
use crate::event::Event;
use crate::game::{Game, PendingChoice};
use crate::types::{ObjectId, Seat, Zone};

impl Game {
    /// Creatures `seat` controls that could be declared as attackers right now.
    pub fn attack_candidates(&self, seat: Seat) -> Vec<ObjectId> {
        let mut out: Vec<ObjectId> = self.players[seat.index()]
            .battlefield
            .iter()
            .copied()
            .filter(|&id| {
                let o = &self.objects[id];
                self.is_creature(id) && !o.tapped && !o.summoning_sick
            })
            .collect();
        out.sort();
        out
    }

    /// Creatures `seat` controls that could block right now.
    pub fn block_candidates(&self, seat: Seat) -> Vec<ObjectId> {
        let mut out: Vec<ObjectId> = self.players[seat.index()]
            .battlefield
            .iter()
            .copied()
            .filter(|&id| self.is_creature(id) && !self.objects[id].tapped)
            .collect();
        out.sort();
        out
    }

    /// Attackers currently attacking `seat` (or a planeswalker they control).
    pub fn attackers_against(&self, seat: Seat) -> Vec<ObjectId> {
        self.battlefield_objects()
            .into_iter()
            .filter(|&id| match self.objects[id].attacking {
                Some(AttackTarget::Player(s)) => s == seat,
                Some(AttackTarget::Planeswalker(pw)) => {
                    self.objects.get(pw).map(|o| o.controller == seat).unwrap_or(false)
                }
                None => false,
            })
            .collect()
    }

    pub fn attacking_creatures(&self) -> Vec<ObjectId> {
        self.battlefield_objects()
            .into_iter()
            .filter(|&id| self.objects[id].attacking.is_some())
            .collect()
    }

    pub fn any_attackers(&self) -> bool {
        self.objects
            .iter()
            .any(|(_, o)| o.zone == Zone::Battlefield && o.attacking.is_some())
    }

    /// Blockers still on the battlefield and still blocking `attacker`, in declared order.
    pub fn live_blockers(&self, attacker: ObjectId) -> Vec<ObjectId> {
        self.objects[attacker]
            .blocked_by
            .iter()
            .copied()
            .filter(|&b| {
                let o = &self.objects[b];
                o.zone == Zone::Battlefield && o.blocking.contains(&attacker)
            })
            .collect()
    }

    /// Seats that were attacked, in APNAP order (rule 802.3): they declare blockers one at a time.
    pub(crate) fn defending_seats(&self) -> Vec<Seat> {
        self.apnap()
            .into_iter()
            .filter(|&s| s != self.active_player && !self.attackers_against(s).is_empty())
            .collect()
    }

    pub(crate) fn declare_attackers(&mut self, seat: Seat, attackers: &[(ObjectId, AttackTarget)]) {
        for &(id, target) in attackers {
            let obj = &mut self.objects[id];
            obj.attacking = Some(target);
            if !obj.tapped {
                obj.tapped = true;
                self.emit(Event::Tapped { object: id });
            }
        }
        self.emit(Event::Attacked { seat, attackers: attackers.to_vec() });
        self.pending = None;
        self.give_priority_to_active();
    }

    pub(crate) fn declare_blockers(&mut self, seat: Seat, blocks: &[(ObjectId, ObjectId)]) {
        for &(blocker, attacker) in blocks {
            self.objects[blocker].blocking.push(attacker);
            let a = &mut self.objects[attacker];
            a.blocked = true;
            a.blocked_by.push(blocker);
        }
        self.emit(Event::Blocked { seat, blocks: blocks.to_vec() });
        let remaining = match self.pending.take() {
            Some(PendingChoice::DeclareBlockers { remaining, .. }) => remaining,
            _ => Vec::new(),
        };
        self.continue_blockers(remaining);
    }

    pub(crate) fn continue_blockers(&mut self, mut remaining: Vec<Seat>) {
        remaining.retain(|s| self.turn_order.contains(s));
        if remaining.is_empty() {
            self.pending = None;
            self.give_priority_to_active();
        } else {
            let head = remaining.remove(0);
            self.pending = Some(PendingChoice::DeclareBlockers { seat: head, remaining });
        }
    }

    /// Turn-based action at the start of the combat damage step (rule 510.1):
    /// ask the attacking player to divide damage wherever a real choice exists,
    /// then deal it all at once.
    pub(crate) fn begin_combat_damage(&mut self) {
        let queue: Vec<ObjectId> = self
            .attacking_creatures()
            .into_iter()
            .filter(|&a| self.power(a) > 0 && self.live_blockers(a).len() >= 2)
            .collect();
        self.continue_damage_assignment(queue);
    }

    pub(crate) fn assign_combat_damage(
        &mut self,
        _seat: Seat,
        attacker: ObjectId,
        assignments: &[(DamageTarget, i32)],
    ) {
        self.damage_assignments.insert(attacker, assignments.to_vec());
        self.emit(Event::DamageAssigned { attacker, assignments: assignments.to_vec() });
        let queue = match self.pending.take() {
            Some(PendingChoice::AssignDamage { queue, .. }) => queue,
            _ => Vec::new(),
        };
        self.continue_damage_assignment(queue);
    }

    fn continue_damage_assignment(&mut self, mut queue: Vec<ObjectId>) {
        if queue.is_empty() {
            self.pending = None;
            self.deal_combat_damage();
            self.give_priority_to_active();
        } else {
            let attacker = queue.remove(0);
            let seat = self.objects[attacker].controller;
            self.pending = Some(PendingChoice::AssignDamage { seat, attacker, queue });
        }
    }

    /// "Lethal to each blocker in declared order, remainder to the last one"
    /// (or to the player, with trample — not in v1). The TUI's default and
    /// the engine's fallback.
    pub(crate) fn lethal_in_order(&self, attacker: ObjectId, blockers: &[ObjectId]) -> Vec<(DamageTarget, i32)> {
        let mut remaining = self.power(attacker).max(0);
        let mut out: Vec<(DamageTarget, i32)> = Vec::new();
        for &b in blockers {
            if remaining == 0 {
                break;
            }
            let lethal = (self.toughness(b) - self.objects[b].damage).max(1);
            let give = remaining.min(lethal);
            out.push((DamageTarget::Object(b), give));
            remaining -= give;
        }
        if remaining > 0 {
            if let Some(last) = out.last_mut() {
                last.1 += remaining;
            }
        }
        out
    }

    fn deal_combat_damage(&mut self) {
        let mut packets: Vec<(ObjectId, DamageTarget, i32)> = Vec::new();

        for a in self.attacking_creatures() {
            let p = self.power(a);
            if p <= 0 {
                continue;
            }
            let obj = &self.objects[a];
            let target = obj.attacking.expect("attacking creature has a target");
            if !obj.blocked {
                match target {
                    AttackTarget::Player(s) => {
                        if self.turn_order.contains(&s) {
                            packets.push((a, DamageTarget::Player(s), p));
                        }
                    }
                    AttackTarget::Planeswalker(pw) => packets.push((a, DamageTarget::Object(pw), p)),
                }
                continue;
            }
            let blockers = self.live_blockers(a);
            match blockers.len() {
                0 => {} // blocked, blockers gone, no trample: no damage (rule 509.1h)
                1 => packets.push((a, DamageTarget::Object(blockers[0]), p)),
                _ => {
                    let split = match self.damage_assignments.get(&a) {
                        Some(s) => s.clone(),
                        None => self.lethal_in_order(a, &blockers),
                    };
                    for (to, amt) in split {
                        if amt > 0 {
                            packets.push((a, to, amt));
                        }
                    }
                }
            }
        }

        for b in self.battlefield_objects() {
            if self.objects[b].blocking.is_empty() {
                continue;
            }
            let p = self.power(b);
            if p <= 0 {
                continue;
            }
            for a in self.objects[b].blocking.clone() {
                let alive = self.objects.get(a).map(|o| o.zone == Zone::Battlefield && o.attacking.is_some());
                if alive == Some(true) {
                    packets.push((b, DamageTarget::Object(a), p));
                }
            }
        }

        // All combat damage is dealt simultaneously (rule 510.2).
        for (source, to, amount) in packets {
            match to {
                DamageTarget::Player(s) => {
                    let from = self.players[s.index()].life;
                    self.players[s.index()].life = from - amount;
                    self.emit(Event::Damage { source, to, amount });
                    self.emit(Event::LifeChanged { seat: s, from, to: from - amount });
                }
                DamageTarget::Object(o) => {
                    self.objects[o].damage += amount;
                    self.emit(Event::Damage { source, to, amount });
                }
            }
        }
        self.damage_assignments.clear();
    }

    pub(crate) fn remove_all_from_combat(&mut self) {
        for (_, o) in self.objects.iter_mut() {
            o.attacking = None;
            o.blocking.clear();
            o.blocked = false;
            o.blocked_by.clear();
        }
        self.damage_assignments.clear();
    }
}
