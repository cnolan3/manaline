//! `legal_actions`, the load-bearing function, plus the rule check for
//! division actions (§3's carve-out).

use crate::action::{Action, AttackTarget, DamageTarget};
use crate::error::RulesError;
use crate::game::{Game, PendingChoice};
use crate::types::{ObjectId, Seat};
use std::collections::BTreeSet;

/// Above this many combinations, attack and block enumeration falls back to a
/// structured subset. The full space stays legal: these actions are validated by rule.
const ENUMERATION_CAP: usize = 256;

impl Game {
    /// Every action `seat` may take right now. Empty unless `seat` is in `must_act`.
    pub fn legal_actions(&self, seat: Seat) -> Vec<Action> {
        let mut acts = Vec::new();
        if self.outcome.is_some() {
            return acts;
        }
        match &self.pending {
            Some(p) if p.seat() == seat => match p {
                PendingChoice::Mulligan { .. } => {
                    acts.push(Action::Mulligan { keep: true });
                    if self.players[seat.index()].mulligans < self.format.starting_hand {
                        acts.push(Action::Mulligan { keep: false });
                    }
                }
                PendingChoice::BottomCards { count, .. } => {
                    for objects in combinations(&self.sorted_hand(seat), *count as usize) {
                        acts.push(Action::BottomCards { objects });
                    }
                }
                PendingChoice::DeclareAttackers { .. } => acts.extend(self.attack_suggestions(seat)),
                PendingChoice::DeclareBlockers { .. } => acts.extend(self.block_suggestions(seat)),
                PendingChoice::AssignDamage { attacker, .. } => acts.extend(self.damage_suggestions(*attacker)),
                PendingChoice::Discard { count, .. } => {
                    for objects in combinations(&self.sorted_hand(seat), *count as usize) {
                        acts.push(Action::Discard { objects });
                    }
                }
            },
            Some(_) => return acts,
            None => {
                if self.priority != Some(seat) {
                    return acts;
                }
                acts.push(Action::PassPriority);
                if self.stack.is_empty() && self.phase.is_main() && self.active_player == seat {
                    let can_play_land = self.players[seat.index()].lands_played_this_turn < 1;
                    for id in self.sorted_hand(seat) {
                        let def = self.card_def(id);
                        if def.is_land() {
                            if can_play_land {
                                acts.push(Action::PlayLand { object: id });
                            }
                            continue;
                        }
                        if def.is_creature() {
                            for payment in self.enumerate_payments(seat, &def.cost) {
                                acts.push(Action::CastSpell { object: id, targets: Vec::new(), payment });
                            }
                        }
                    }
                }
            }
        }
        acts.push(Action::Concede);
        acts
    }

    fn sorted_hand(&self, seat: Seat) -> Vec<ObjectId> {
        let mut hand = self.players[seat.index()].hand.clone();
        hand.sort();
        hand
    }

    fn attack_targets(&self, seat: Seat) -> Vec<AttackTarget> {
        self.opponents_of(seat).map(AttackTarget::Player).collect()
    }

    fn attack_suggestions(&self, seat: Seat) -> Vec<Action> {
        let cands = self.attack_candidates(seat);
        let targets = self.attack_targets(seat);
        let mut out = vec![Action::DeclareAttackers { attackers: Vec::new() }];
        if cands.is_empty() || targets.is_empty() {
            return out;
        }
        let base = targets.len() + 1;
        let total = base.checked_pow(cands.len() as u32);
        match total {
            Some(total) if total <= ENUMERATION_CAP => {
                for code in 1..total {
                    let mut rest = code;
                    let mut attackers = Vec::new();
                    for &c in &cands {
                        let digit = rest % base;
                        rest /= base;
                        if digit > 0 {
                            attackers.push((c, targets[digit - 1]));
                        }
                    }
                    out.push(Action::DeclareAttackers { attackers });
                }
            }
            _ => {
                for &t in &targets {
                    out.push(Action::DeclareAttackers { attackers: cands.iter().map(|&c| (c, t)).collect() });
                    for &c in &cands {
                        out.push(Action::DeclareAttackers { attackers: vec![(c, t)] });
                    }
                }
            }
        }
        out
    }

    fn block_suggestions(&self, seat: Seat) -> Vec<Action> {
        let blockers = self.block_candidates(seat);
        let attackers = self.attackers_against(seat);
        let mut out = vec![Action::DeclareBlockers { blocks: Vec::new() }];
        if blockers.is_empty() || attackers.is_empty() {
            return out;
        }
        let base = attackers.len() + 1;
        let total = base.checked_pow(blockers.len() as u32);
        match total {
            Some(total) if total <= ENUMERATION_CAP => {
                for code in 1..total {
                    let mut rest = code;
                    let mut blocks = Vec::new();
                    for &b in &blockers {
                        let digit = rest % base;
                        rest /= base;
                        if digit > 0 {
                            blocks.push((b, attackers[digit - 1]));
                        }
                    }
                    out.push(Action::DeclareBlockers { blocks });
                }
            }
            _ => {
                for &a in &attackers {
                    out.push(Action::DeclareBlockers { blocks: blockers.iter().map(|&b| (b, a)).collect() });
                    for &b in &blockers {
                        out.push(Action::DeclareBlockers { blocks: vec![(b, a)] });
                    }
                }
            }
        }
        out
    }

    /// The common splits: everything to each blocker, and lethal in declared order.
    fn damage_suggestions(&self, attacker: ObjectId) -> Vec<Action> {
        let blockers = self.live_blockers(attacker);
        let power = self.power(attacker).max(0);
        let mut out: Vec<Action> = Vec::new();
        let mut push = |assignments: Vec<(DamageTarget, i32)>| {
            let a = Action::AssignCombatDamage { attacker, assignments };
            if !out.iter().any(|x| x.canonical() == a.canonical()) {
                out.push(a);
            }
        };
        push(self.lethal_in_order(attacker, &blockers));
        for &b in &blockers {
            push(vec![(DamageTarget::Object(b), power)]);
        }
        out
    }

    /// Rule check for division actions. Anything satisfying the rule is accepted
    /// whether or not `legal_actions` listed it.
    pub(crate) fn validate_division(&self, seat: Seat, action: &Action) -> Result<(), RulesError> {
        match (action, &self.pending) {
            (Action::DeclareAttackers { attackers }, Some(PendingChoice::DeclareAttackers { seat: s })) if *s == seat => {
                let cands = self.attack_candidates(seat);
                let mut seen = BTreeSet::new();
                for (id, target) in attackers {
                    if !cands.contains(id) {
                        return Err(RulesError::illegal(format!("{} {id} can't attack", self.name_or_unknown(*id))));
                    }
                    if !seen.insert(*id) {
                        return Err(RulesError::illegal(format!("{id} is declared twice")));
                    }
                    match target {
                        AttackTarget::Player(t) => {
                            if *t == seat || !self.turn_order.contains(t) {
                                return Err(RulesError::illegal(format!("{t} can't be attacked")));
                            }
                        }
                        AttackTarget::Planeswalker(_) => {
                            return Err(RulesError::Unsupported { what: "attacking planeswalkers".into() })
                        }
                    }
                }
                Ok(())
            }
            (Action::DeclareBlockers { blocks }, Some(PendingChoice::DeclareBlockers { seat: s, .. })) if *s == seat => {
                let blockers = self.block_candidates(seat);
                let attackers = self.attackers_against(seat);
                let mut seen = BTreeSet::new();
                for (b, a) in blocks {
                    if !blockers.contains(b) {
                        return Err(RulesError::illegal(format!("{} {b} can't block", self.name_or_unknown(*b))));
                    }
                    if !seen.insert(*b) {
                        return Err(RulesError::illegal(format!("{b} can block only one attacker")));
                    }
                    if !attackers.contains(a) {
                        return Err(RulesError::illegal(format!("{a} is not attacking {seat}")));
                    }
                }
                Ok(())
            }
            (
                Action::AssignCombatDamage { attacker, assignments },
                Some(PendingChoice::AssignDamage { seat: s, attacker: pending_attacker, .. }),
            ) if *s == seat => {
                if attacker != pending_attacker {
                    return Err(RulesError::illegal(format!(
                        "{pending_attacker} is the attacker whose damage needs assigning, not {attacker}"
                    )));
                }
                let blockers = self.live_blockers(*attacker);
                let power = self.power(*attacker).max(0);
                let mut seen = BTreeSet::new();
                let mut sum = 0;
                for (to, amount) in assignments {
                    if *amount < 0 {
                        return Err(RulesError::illegal("damage amounts can't be negative"));
                    }
                    match to {
                        DamageTarget::Object(o) => {
                            if !blockers.contains(o) {
                                return Err(RulesError::illegal(format!("{o} is not blocking {attacker}")));
                            }
                        }
                        DamageTarget::Player(_) => {
                            return Err(RulesError::illegal("only a creature with trample can assign damage to the player it's attacking"));
                        }
                    }
                    if !seen.insert(*to) {
                        return Err(RulesError::illegal("each recipient may appear only once"));
                    }
                    sum += amount;
                }
                if sum != power {
                    return Err(RulesError::illegal(format!(
                        "{attacker} must assign exactly {power} damage, got {sum}"
                    )));
                }
                Ok(())
            }
            _ => Err(RulesError::illegal(format!(
                "{} is not the choice pending for {seat}",
                crate::text::describe_action(self, action)
            ))),
        }
    }

    fn name_or_unknown(&self, id: ObjectId) -> String {
        self.objects
            .get(id)
            .map(|o| self.cards.get(o.card).name.clone())
            .unwrap_or_else(|| "unknown object".into())
    }
}

/// All `k`-element subsets of `items`, in lexicographic order of positions.
pub(crate) fn combinations(items: &[ObjectId], k: usize) -> Vec<Vec<ObjectId>> {
    let n = items.len();
    if k > n {
        return Vec::new();
    }
    if k == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    let mut idx: Vec<usize> = (0..k).collect();
    loop {
        out.push(idx.iter().map(|&i| items[i]).collect());
        // Advance to the next combination.
        let mut i = k;
        loop {
            if i == 0 {
                return out;
            }
            i -= 1;
            if idx[i] != i + n - k {
                break;
            }
            if i == 0 {
                return out;
            }
        }
        idx[i] += 1;
        for j in i + 1..k {
            idx[j] = idx[j - 1] + 1;
        }
    }
}
