//! `legal_actions`, the load-bearing function, plus the rule check for
//! division actions (§3's carve-out).

use crate::action::{Action, AttackTarget, DamageTarget};
use crate::error::RulesError;
use crate::filter::Ctx;
use crate::game::{Game, PendingChoice};
use crate::types::Keyword;
use crate::types::{ObjectId, Seat, Zone};
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
                PendingChoice::EffectDiscard { count, .. } => {
                    let hand = self.sorted_hand(seat);
                    let n = (*count as usize).min(hand.len());
                    for objects in combinations(&hand, n) {
                        acts.push(Action::Discard { objects });
                    }
                }
                PendingChoice::ChooseTargets { specs, trigger, .. } => {
                    let ctx = Ctx {
                        you: seat,
                        this: Some(trigger.source),
                        targets: Vec::new(),
                        triggering: trigger.triggering,
                    };
                    for targets in self.target_combos(specs, &ctx).unwrap_or_default() {
                        acts.push(Action::ChooseTargets { targets });
                    }
                }
                PendingChoice::Sacrifice { filter, count, .. } => {
                    let ctx = Ctx::simple(seat, None);
                    let mut candidates: Vec<ObjectId> = self.players[seat.index()]
                        .battlefield
                        .iter()
                        .copied()
                        .filter(|&c| self.object_matches(c, filter, &ctx))
                        .collect();
                    candidates.sort();
                    let n = (*count as usize).min(candidates.len());
                    for objects in combinations(&candidates, n) {
                        acts.push(Action::ChooseTargets {
                            targets: objects.into_iter().map(crate::action::Target::Object).collect(),
                        });
                    }
                }
            },
            Some(_) => return acts,
            None => {
                if self.priority != Some(seat) {
                    return acts;
                }
                acts.push(Action::PassPriority);
                let sorcery_timing = self.stack.is_empty() && self.phase.is_main() && self.active_player == seat;
                if sorcery_timing && self.players[seat.index()].lands_played_this_turn < 1 {
                    for id in self.sorted_hand(seat) {
                        if self.card_def(id).is_land() {
                            acts.push(Action::PlayLand { object: id });
                        }
                    }
                }
                for id in self.sorted_hand(seat) {
                    let def = self.card_def(id).clone();
                    if def.is_land() {
                        continue;
                    }
                    if !(sorcery_timing || def.ir.has_instant_speed()) {
                        continue;
                    }
                    let payments = self.enumerate_payments(seat, &self.cast_cost(seat, id));
                    if payments.is_empty() {
                        continue;
                    }
                    let specs = self.cast_target_specs(id);
                    let ctx = Ctx::simple(seat, Some(id));
                    let Some(combos) = self.target_combos(&specs, &ctx) else {
                        continue;
                    };
                    for targets in combos {
                        for payment in &payments {
                            acts.push(Action::CastSpell {
                                object: id,
                                targets: targets.clone(),
                                payment: payment.clone(),
                            });
                        }
                    }
                }
                acts.extend(self.ability_actions(seat, sorcery_timing));
            }
        }
        acts.push(Action::Concede);
        acts
    }

    /// Every legal combination of targets for the specs, or `None` if some
    /// spec has no legal target. Capped at `ENUMERATION_CAP` combinations.
    pub(crate) fn target_combos(&self, specs: &[cardir::Filter], ctx: &Ctx) -> Option<Vec<Vec<crate::action::Target>>> {
        if specs.is_empty() {
            return Some(vec![Vec::new()]);
        }
        let per_spec: Vec<Vec<crate::action::Target>> = specs.iter().map(|s| self.targets_for(s, ctx)).collect();
        if per_spec.iter().any(|c| c.is_empty()) {
            return None;
        }
        let mut combos: Vec<Vec<crate::action::Target>> = vec![Vec::new()];
        for candidates in &per_spec {
            let mut next = Vec::new();
            for combo in &combos {
                for &c in candidates {
                    if combo.contains(&c) {
                        continue; // one object can't be chosen twice for the same "target" word
                    }
                    let mut n = combo.clone();
                    n.push(c);
                    next.push(n);
                    if next.len() >= ENUMERATION_CAP {
                        break;
                    }
                }
            }
            combos = next;
            if combos.is_empty() {
                return None;
            }
        }
        Some(combos)
    }

    /// Activated abilities (and equip) of permanents `seat` controls, with costs enumerated.
    fn ability_actions(&self, seat: Seat, sorcery_timing: bool) -> Vec<Action> {
        let mut acts = Vec::new();
        let mut battlefield = self.players[seat.index()].battlefield.clone();
        battlefield.sort();
        let mut graveyard = self.players[seat.index()].graveyard.clone();
        graveyard.sort();
        for id in battlefield.into_iter().chain(graveyard) {
            let def = self.card_def(id).clone();
            let obj = &self.objects[id];
            let in_graveyard = obj.zone == Zone::Graveyard;
            for (index, ability) in def.ir.activated.iter().enumerate() {
                if ability.is_mana_ability() || (ability.sorcery_speed && !sorcery_timing) || ability.from_graveyard != in_graveyard {
                    continue;
                }
                let mut payments = vec![crate::action::ManaPayment::default()];
                let mut ok = true;
                for cost in &ability.cost {
                    match cost {
                        cardir::Cost::Mana(m) => {
                            let self_used = ability
                                .cost
                                .iter()
                                .any(|c| matches!(c, cardir::Cost::Tap | cardir::Cost::SacrificeThis));
                            let ps: Vec<crate::action::ManaPayment> = self
                                .enumerate_payments(seat, m)
                                .into_iter()
                                .filter(|p| !(self_used && p.tap.contains(&id)))
                                .collect();
                            if ps.is_empty() {
                                ok = false;
                            } else {
                                payments = payments
                                    .iter()
                                    .flat_map(|base| {
                                        ps.iter().map(move |p| {
                                            let mut b = base.clone();
                                            b.tap.extend(p.tap.iter().copied());
                                            b.from_pool.extend(p.from_pool.iter().copied());
                                            b
                                        })
                                    })
                                    .collect();
                            }
                        }
                        cardir::Cost::Tap => {
                            if obj.tapped || (def.is_creature() && obj.summoning_sick && !self.has_keyword(id, Keyword::Haste)) {
                                ok = false;
                            }
                        }
                        cardir::Cost::SacrificeThis => {}
                        cardir::Cost::Sacrifice(filter) => {
                            let ctx = Ctx::simple(seat, Some(id));
                            let candidates: Vec<ObjectId> = self.players[seat.index()]
                                .battlefield
                                .iter()
                                .copied()
                                .filter(|&c| self.object_matches(c, filter, &ctx))
                                .collect();
                            if candidates.is_empty() {
                                ok = false;
                            } else {
                                payments = payments
                                    .iter()
                                    .flat_map(|base| {
                                        candidates.iter().map(move |&c| {
                                            let mut b = base.clone();
                                            b.sacrifice = vec![c];
                                            b
                                        })
                                    })
                                    .collect();
                            }
                        }
                        cardir::Cost::PayLife(n) => {
                            if self.players[seat.index()].life < *n {
                                ok = false;
                            }
                        }
                        cardir::Cost::Discard(n) => {
                            let hand = self.sorted_hand(seat);
                            if (hand.len() as i32) < *n {
                                ok = false;
                            } else {
                                let choices = combinations(&hand, *n as usize);
                                payments = payments
                                    .iter()
                                    .flat_map(|base| {
                                        choices.iter().map(move |c| {
                                            let mut b = base.clone();
                                            b.discard = c.clone();
                                            b
                                        })
                                    })
                                    .collect();
                            }
                        }
                    }
                    if !ok {
                        break;
                    }
                }
                if !ok {
                    continue;
                }
                let ctx = Ctx::simple(seat, Some(id));
                let Some(combos) = self.target_combos(&ability.targets, &ctx) else {
                    continue;
                };
                for targets in combos {
                    for payment in &payments {
                        acts.push(Action::ActivateAbility {
                            object: id,
                            ability: index as u8,
                            targets: targets.clone(),
                            payment: payment.clone(),
                        });
                        if acts.len() > ENUMERATION_CAP * 4 {
                            return acts;
                        }
                    }
                }
            }
            if let (Some(equip), true) = (&def.ir.equip, sorcery_timing) {
                let payments = self.enumerate_payments(seat, equip);
                let creatures: Vec<ObjectId> = self.players[seat.index()]
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|&c| self.is_creature(c) && obj.attached_to != Some(c))
                    .collect();
                for c in creatures {
                    for payment in &payments {
                        acts.push(Action::ActivateAbility {
                            object: id,
                            ability: crate::action::EQUIP_ABILITY,
                            targets: vec![crate::action::Target::Object(c)],
                            payment: payment.clone(),
                        });
                    }
                }
            }
        }
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
                    out.push(Action::DeclareAttackers {
                        attackers: cands.iter().map(|&c| (c, t)).collect(),
                    });
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
        let legal = |blocks: &Vec<(ObjectId, ObjectId)>| {
            blocks.iter().all(|(b, a)| self.can_block(*b, *a)) && self.blocks_satisfy_menace(blocks, &attackers)
        };
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
                    if legal(&blocks) {
                        out.push(Action::DeclareBlockers { blocks });
                    }
                }
            }
            _ => {
                // Too many combinations to list: singles, double-blocks, full gangs,
                // and a spread that puts one blocker on each attacker. Any other
                // legal assignment may be sent as a full action.
                for &a in &attackers {
                    let can: Vec<ObjectId> = blockers.iter().copied().filter(|&b| self.can_block(b, a)).collect();
                    let all: Vec<(ObjectId, ObjectId)> = can.iter().map(|&b| (b, a)).collect();
                    for &b in &can {
                        let one = vec![(b, a)];
                        if legal(&one) {
                            out.push(Action::DeclareBlockers { blocks: one });
                        }
                    }
                    for i in 0..can.len() {
                        for j in i + 1..can.len() {
                            let two = vec![(can[i], a), (can[j], a)];
                            if legal(&two) {
                                out.push(Action::DeclareBlockers { blocks: two });
                            }
                        }
                    }
                    if all.len() > 2 && legal(&all) {
                        out.push(Action::DeclareBlockers { blocks: all });
                    }
                }
                // Spread: biggest attackers first, each taking the toughest free blocker that can block it.
                let mut by_power = attackers.clone();
                by_power.sort_by_key(|&a| std::cmp::Reverse(self.power(a)));
                let mut free: Vec<ObjectId> = blockers.clone();
                free.sort_by_key(|&b| std::cmp::Reverse((self.toughness(b), self.power(b))));
                let mut spread = Vec::new();
                for a in by_power {
                    if let Some(pos) = free.iter().position(|&b| self.can_block(b, a)) {
                        spread.push((free.remove(pos), a));
                    }
                }
                if spread.len() > 1 && legal(&spread) {
                    out.push(Action::DeclareBlockers { blocks: spread });
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
            let all = vec![(DamageTarget::Object(b), power)];
            if self.assignment_is_legal(attacker, &all).is_ok() {
                push(all);
            }
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
                            return Err(RulesError::Unsupported {
                                what: "attacking planeswalkers".into(),
                            })
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
                    if !self.can_block(*b, *a) {
                        return Err(RulesError::illegal(format!(
                            "{} {b} can't block {}: it has flying",
                            self.name_or_unknown(*b),
                            self.name_or_unknown(*a)
                        )));
                    }
                }
                if !self.blocks_satisfy_menace(blocks, &attackers) {
                    return Err(RulesError::illegal("a creature with menace can't be blocked by just one creature"));
                }
                Ok(())
            }
            (
                Action::AssignCombatDamage { attacker, assignments },
                Some(PendingChoice::AssignDamage {
                    seat: s,
                    attacker: pending_attacker,
                    ..
                }),
            ) if *s == seat => {
                if attacker != pending_attacker {
                    return Err(RulesError::illegal(format!(
                        "{pending_attacker} is the attacker whose damage needs assigning, not {attacker}"
                    )));
                }
                self.assignment_is_legal(*attacker, assignments).map_err(RulesError::illegal)
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
