//! Combat: attackers, sequential multiplayer blockers, evasion keywords, and
//! damage assignment under the current (post-Foundations) rules, with a
//! first-strike round when needed.

use crate::action::{AttackTarget, DamageTarget};
use crate::event::Event;
use crate::game::{CombatRound, Game, PendingChoice};
use crate::types::{Keyword, ObjectId, Seat, Zone};

impl Game {
    /// Creatures `seat` controls that could be declared as attackers right now.
    pub fn attack_candidates(&self, seat: Seat) -> Vec<ObjectId> {
        let mut out: Vec<ObjectId> = self.players[seat.index()]
            .battlefield
            .iter()
            .copied()
            .filter(|&id| {
                let o = &self.objects[id];
                self.is_creature(id)
                    && !o.tapped
                    && (!o.summoning_sick || self.has_keyword(id, Keyword::Haste))
                    && !self.has_keyword(id, Keyword::Defender)
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

    /// Can `blocker` block `attacker` at all (flying and reach)?
    pub fn can_block(&self, blocker: ObjectId, attacker: ObjectId) -> bool {
        if self.has_keyword(attacker, Keyword::Flying)
            && !(self.has_keyword(blocker, Keyword::Flying) || self.has_keyword(blocker, Keyword::Reach))
        {
            return false;
        }
        true
    }

    /// Menace: an attacker can't be blocked by exactly one creature.
    pub fn blocks_satisfy_menace(&self, blocks: &[(ObjectId, ObjectId)], attackers: &[ObjectId]) -> bool {
        attackers.iter().all(|&a| {
            let n = blocks.iter().filter(|(_, x)| *x == a).count();
            !(self.has_keyword(a, Keyword::Menace) && n == 1)
        })
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
            let vigilance = self.has_keyword(id, Keyword::Vigilance);
            let obj = &mut self.objects[id];
            obj.attacking = Some(target);
            if !obj.tapped && !vigilance {
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

    /// Whether any creature in combat has first or double strike.
    pub(crate) fn combat_has_first_strike(&self) -> bool {
        self.battlefield_objects().into_iter().any(|id| {
            let o = &self.objects[id];
            (o.attacking.is_some() || !o.blocking.is_empty())
                && (self.has_keyword(id, Keyword::FirstStrike) || self.has_keyword(id, Keyword::DoubleStrike))
        })
    }

    /// Does this creature deal combat damage in the given round?
    fn deals_damage_in_round(&self, id: ObjectId, first_strike_round: bool) -> bool {
        let fs = self.has_keyword(id, Keyword::FirstStrike);
        let ds = self.has_keyword(id, Keyword::DoubleStrike);
        if first_strike_round {
            fs || ds
        } else {
            ds || !fs
        }
    }

    /// Lethal damage to assign to a blocker: 1 from a deathtouch source, else remaining toughness.
    fn lethal_for(&self, attacker: ObjectId, blocker: ObjectId) -> i32 {
        if self.has_keyword(attacker, Keyword::Deathtouch) {
            return 1;
        }
        (self.toughness(blocker) - self.objects[blocker].damage).max(1)
    }

    /// Turn-based action at the start of a combat damage round (rule 510.1):
    /// ask the attacking player to divide damage wherever a real choice exists,
    /// then deal it all at once.
    pub(crate) fn begin_combat_damage(&mut self, first_strike_round: bool) {
        self.combat_round = if first_strike_round { CombatRound::FirstStrikeDone } else { CombatRound::Done };
        let queue: Vec<ObjectId> = self
            .attacking_creatures()
            .into_iter()
            .filter(|&a| {
                if self.power(a) <= 0 || !self.deals_damage_in_round(a, first_strike_round) {
                    return false;
                }
                let blockers = self.live_blockers(a);
                blockers.len() >= 2 || (self.has_keyword(a, Keyword::Trample) && !blockers.is_empty())
            })
            .collect();
        self.continue_damage_assignment(queue, first_strike_round);
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
        let first_strike_round = self.combat_round == CombatRound::FirstStrikeDone;
        self.continue_damage_assignment(queue, first_strike_round);
    }

    fn continue_damage_assignment(&mut self, mut queue: Vec<ObjectId>, first_strike_round: bool) {
        if queue.is_empty() {
            self.pending = None;
            self.deal_combat_damage(first_strike_round);
            self.give_priority_to_active();
        } else {
            let attacker = queue.remove(0);
            let seat = self.objects[attacker].controller;
            self.pending = Some(PendingChoice::AssignDamage { seat, attacker, queue });
        }
    }

    /// "Lethal to each blocker in declared order, remainder to the last one"
    /// (or to the player, with trample). The TUI's default and the engine's fallback.
    pub(crate) fn lethal_in_order(&self, attacker: ObjectId, blockers: &[ObjectId]) -> Vec<(DamageTarget, i32)> {
        let mut remaining = self.power(attacker).max(0);
        let mut out: Vec<(DamageTarget, i32)> = Vec::new();
        for &b in blockers {
            if remaining == 0 {
                break;
            }
            let give = remaining.min(self.lethal_for(attacker, b));
            out.push((DamageTarget::Object(b), give));
            remaining -= give;
        }
        if remaining > 0 {
            if let (true, Some(AttackTarget::Player(s))) = (self.has_keyword(attacker, Keyword::Trample), self.objects[attacker].attacking) {
                out.push((DamageTarget::Player(s), remaining));
            } else if let Some(last) = out.last_mut() {
                last.1 += remaining;
            }
        }
        out
    }

    /// Whether a division of `attacker`'s damage is legal: sums to its power,
    /// names only its blockers (plus the defending player with trample, only
    /// once every blocker has lethal).
    pub(crate) fn assignment_is_legal(&self, attacker: ObjectId, assignments: &[(DamageTarget, i32)]) -> Result<(), String> {
        let blockers = self.live_blockers(attacker);
        let power = self.power(attacker).max(0);
        let trample = self.has_keyword(attacker, Keyword::Trample);
        let defending = match self.objects[attacker].attacking {
            Some(AttackTarget::Player(s)) => Some(s),
            _ => None,
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut sum = 0;
        let mut to_player = 0;
        for (to, amount) in assignments {
            if *amount < 0 {
                return Err("damage amounts can't be negative".into());
            }
            match to {
                DamageTarget::Object(o) => {
                    if !blockers.contains(o) {
                        return Err(format!("{o} is not blocking {attacker}"));
                    }
                }
                DamageTarget::Player(s) => {
                    if !trample || Some(*s) != defending {
                        return Err("only a creature with trample can assign damage to the player it's attacking".into());
                    }
                    to_player += amount;
                }
            }
            if !seen.insert(*to) {
                return Err("each recipient may appear only once".into());
            }
            sum += amount;
        }
        if sum != power {
            return Err(format!("{attacker} must assign exactly {power} damage, got {sum}"));
        }
        if to_player > 0 {
            for b in &blockers {
                let got = assignments.iter().find(|(t, _)| *t == DamageTarget::Object(*b)).map(|(_, n)| *n).unwrap_or(0);
                if got < self.lethal_for(attacker, *b) {
                    return Err(format!("trample: {b} must be assigned lethal damage before any goes to the player"));
                }
            }
        }
        Ok(())
    }

    fn deal_combat_damage(&mut self, first_strike_round: bool) {
        let mut packets: Vec<(ObjectId, DamageTarget, i32)> = Vec::new();

        for a in self.attacking_creatures() {
            let p = self.power(a);
            if p <= 0 || !self.deals_damage_in_round(a, first_strike_round) {
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
            let trample = self.has_keyword(a, Keyword::Trample);
            match blockers.len() {
                0 => {
                    // Blocked but the blockers are gone: trample still hits the player (rule 702.19c).
                    if let (true, AttackTarget::Player(s)) = (trample, target) {
                        packets.push((a, DamageTarget::Player(s), p));
                    }
                }
                1 if !trample => packets.push((a, DamageTarget::Object(blockers[0]), p)),
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
            if self.objects[b].blocking.is_empty() || !self.deals_damage_in_round(b, first_strike_round) {
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

        // All combat damage in a round is dealt simultaneously (rule 510.2).
        for (source, to, amount) in packets {
            self.deal_damage(source, to, amount, true);
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
