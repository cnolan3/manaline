//! Mana sources, payment enumeration (§10: by colour combination, engine picks
//! the permanents), and payment.

use crate::action::ManaPayment;
use crate::error::RulesError;
use crate::event::Event;
use crate::game::Game;
use crate::types::{Color, Mana, ManaCost, ObjectId, Seat};

impl Game {
    /// What casting `object` costs `seat` right now: the printed cost less any
    /// `CostReduction` statics they control that apply to it (generic only).
    pub fn cast_cost(&self, seat: Seat, object: ObjectId) -> ManaCost {
        let mut cost = self.card_def(object).cost.clone();
        let ctx = crate::filter::Ctx::simple(seat, Some(object));
        let mut reduction = 0i32;
        for (source, s) in self.active_statics() {
            if self.objects[source].controller != seat {
                continue;
            }
            if let cardir::Static::CostReduction { filter, amount } = s {
                if self.spell_matches(object, filter, &ctx) {
                    reduction += self.eval_amount(amount, &ctx);
                }
            }
        }
        cost.generic = (cost.generic as i32 - reduction).max(0) as u8;
        cost
    }

    /// Untapped permanents `seat` controls with a mana ability: the mana
    /// each makes and how much (board-dependent amounts evaluated now).
    /// Only single-kind producers are supported in v1.
    pub fn mana_sources(&self, seat: Seat) -> Vec<(ObjectId, Mana)> {
        self.mana_sources_with_amounts(seat).into_iter().map(|(id, c, _)| (id, c)).collect()
    }

    pub fn mana_sources_with_amounts(&self, seat: Seat) -> Vec<(ObjectId, Mana, i32)> {
        let mut out = Vec::new();
        for &id in &self.players[seat.index()].battlefield {
            let obj = &self.objects[id];
            if obj.tapped {
                continue;
            }
            let def = self.card_def(id);
            if def.is_creature() && obj.summoning_sick && !self.has_keyword(id, crate::types::Keyword::Haste) {
                continue;
            }
            let abilities = def.mana_abilities();
            let Some((color, amount, _)) = abilities.first() else {
                continue;
            };
            let mana = match color {
                Some(c) => Mana::Colored(*c),
                None => Mana::Colorless,
            };
            let amount = match amount {
                Some(n) => *n,
                None => {
                    // Evaluate the IR amount (e.g. "for each Elf you control") right now.
                    let ability = &def.ir.activated[abilities[0].2];
                    let ctx = crate::filter::Ctx {
                        you: seat,
                        this: Some(id),
                        targets: Vec::new(),
                        triggering: None,
                    };
                    ability
                        .effects
                        .iter()
                        .filter_map(|e| match e {
                            cardir::Effect::AddMana { amount, .. } => Some(self.eval_amount(amount, &ctx)),
                            _ => None,
                        })
                        .sum()
                }
            };
            if amount > 0 {
                out.push((id, mana, amount));
            }
        }
        out.sort();
        out
    }

    /// Every distinct way `seat` could pay `cost` right now: one colour
    /// combination at a time (§10), realised as concrete permanents in up to
    /// two ways — lands first (fewest creatures tapped), and producers first
    /// (biggest mana creatures, leaving lands up). Any other set of untapped
    /// sources that covers the cost is also accepted by `apply`.
    pub fn enumerate_payments(&self, seat: Seat, cost: &ManaCost) -> Vec<ManaPayment> {
        if cost.is_free() {
            return vec![ManaPayment::default()];
        }
        let sources_full = self.mana_sources_with_amounts(seat);
        let pool = &self.players[seat.index()].mana_pool;

        // Capacity per kind of mana counts every unit a source can make;
        // slot 5 is colourless, which only pays generic.
        let mut counts = [0u8; 6];
        for (_, m, n) in &sources_full {
            counts[mana_slot(*m)] = counts[mana_slot(*m)].saturating_add((*n).max(1) as u8);
        }
        let mut pool_counts = [0u8; 6];
        for m in Mana::ALL {
            pool_counts[mana_slot(m)] = pool.get(m);
        }
        let mut pips = [0u8; 5];
        for c in &cost.pips {
            pips[*c as usize] += 1;
        }

        let mut solutions = Vec::new();
        let mut tapped = [0u8; 6];
        let mut pooled = [0u8; 6];
        search(
            0,
            &counts,
            &pool_counts,
            &pips,
            cost.generic as i32,
            &mut tapped,
            &mut pooled,
            &mut solutions,
        );

        let mut out: Vec<ManaPayment> = Vec::new();
        for (tapped, pooled) in solutions {
            for strategy in [Pick::LandsFirst, Pick::ProducersFirst] {
                let mut payment = ManaPayment::default();
                let mut feasible = true;
                for m in Mana::ALL {
                    let slot = mana_slot(m);
                    for _ in 0..pooled[slot] {
                        payment.from_pool.push(m);
                    }
                    let need = tapped[slot] as i32;
                    if need == 0 {
                        continue;
                    }
                    let of_kind: Vec<(ObjectId, i32, bool)> = sources_full
                        .iter()
                        .filter(|(_, sm, _)| *sm == m)
                        .map(|(id, _, n)| (*id, *n, self.card_def(*id).is_creature()))
                        .collect();
                    match self.pick_sources(&of_kind, need, strategy) {
                        Some(ids) => payment.tap.extend(ids),
                        None => feasible = false,
                    }
                }
                if !feasible {
                    continue;
                }
                payment.tap.sort();
                payment.tap.dedup();
                payment.from_pool.sort();
                if !out.contains(&payment) {
                    out.push(payment);
                }
            }
        }
        out
    }

    /// Choose sources of one colour whose output covers `need`.
    fn pick_sources(&self, sources: &[(ObjectId, i32, bool)], need: i32, strategy: Pick) -> Option<Vec<ObjectId>> {
        let mut ordered: Vec<(ObjectId, i32, bool)> = sources.to_vec();
        match strategy {
            // Lands (basic first, by id), then creatures with the biggest output first.
            Pick::LandsFirst => ordered.sort_by_key(|(id, n, creature)| (*creature, if *creature { -*n } else { 0 }, *id)),
            // Biggest producers first, then lands.
            Pick::ProducersFirst => ordered.sort_by_key(|(id, n, creature)| (-*n, !*creature, *id)),
        }
        let mut chosen = Vec::new();
        let mut have = 0;
        for (id, n, _) in ordered {
            if have >= need {
                break;
            }
            chosen.push(id);
            have += n.max(1);
        }
        if have >= need {
            Some(chosen)
        } else {
            None
        }
    }

    /// Whether a payment covers `cost` with sources `seat` controls, without changing anything.
    pub fn payment_covers(&self, seat: Seat, payment: &ManaPayment, cost: &ManaCost) -> Result<(), RulesError> {
        let sources = self.mana_sources_with_amounts(seat);
        let mut paying: Vec<Mana> = Vec::new();
        let mut seen = Vec::new();
        for &id in &payment.tap {
            if seen.contains(&id) {
                return Err(RulesError::illegal(format!("{id} is listed twice in the payment")));
            }
            seen.push(id);
            match sources.iter().find(|(s, _, _)| *s == id) {
                Some((_, m, n)) => paying.extend(std::iter::repeat_n(*m, (*n).max(1) as usize)),
                None => return Err(RulesError::illegal(format!("{id} is not an untapped mana source {seat} controls"))),
            }
        }
        let mut pool = self.players[seat.index()].mana_pool.clone();
        for &m in &payment.from_pool {
            if !pool.remove(m, 1) {
                return Err(RulesError::illegal(format!("{seat} has no {m} in their mana pool to spend")));
            }
            paying.push(m);
        }
        for c in Color::ALL {
            let have = paying.iter().filter(|m| **m == Mana::Colored(c)).count() as u8;
            if have < cost.pips_of(c) {
                return Err(RulesError::illegal(format!("payment does not cover {cost}")));
            }
        }
        if paying.len() < cost.mana_value() as usize {
            return Err(RulesError::illegal(format!("payment does not cover {cost}")));
        }
        Ok(())
    }

    /// Validate and execute a payment: tap the named permanents, spend the named
    /// pool mana, pay `cost`, and return any surplus to the pool.
    pub(crate) fn pay_mana(&mut self, seat: Seat, payment: &ManaPayment, cost: &ManaCost) -> Result<(), RulesError> {
        self.payment_covers(seat, payment, cost)?;
        let sources = self.mana_sources_with_amounts(seat);
        let mut paying: Vec<Mana> = Vec::new();
        for &id in &payment.tap {
            if let Some((_, m, n)) = sources.iter().find(|(s, _, _)| *s == id) {
                paying.extend(std::iter::repeat_n(*m, (*n).max(1) as usize));
            }
        }
        paying.extend(payment.from_pool.iter().copied());

        // Execute.
        for &m in &payment.from_pool {
            self.players[seat.index()].mana_pool.remove(m, 1);
        }
        for &id in &payment.tap {
            self.objects[id].tapped = true;
            self.emit(Event::Tapped { object: id });
        }
        for c in &cost.pips {
            let pos = paying.iter().position(|m| *m == Mana::Colored(*c)).expect("checked above");
            paying.remove(pos);
        }
        paying.sort();
        for _ in 0..cost.generic {
            // Spend colourless first, then whatever is left; remainder returns to the pool.
            paying.pop();
        }
        for m in paying {
            self.players[seat.index()].mana_pool.add(m, 1);
        }
        Ok(())
    }
}

#[derive(Copy, Clone)]
enum Pick {
    LandsFirst,
    ProducersFirst,
}

fn mana_slot(m: Mana) -> usize {
    match m {
        Mana::Colored(c) => c as usize,
        Mana::Colorless => 5,
    }
}

#[allow(clippy::too_many_arguments)]
fn search(
    c: usize,
    counts: &[u8; 6],
    pool: &[u8; 6],
    pips: &[u8; 5],
    generic_left: i32,
    tapped: &mut [u8; 6],
    pooled: &mut [u8; 6],
    out: &mut Vec<([u8; 6], [u8; 6])>,
) {
    if generic_left < 0 {
        return;
    }
    if c == 5 {
        // Whatever generic remains comes from colourless sources, then colourless pool mana.
        let need = generic_left as u8;
        let t = need.min(counts[5]);
        let p = need - t;
        if pool[5] >= p {
            tapped[5] = t;
            pooled[5] = p;
            out.push((*tapped, *pooled));
            tapped[5] = 0;
            pooled[5] = 0;
        }
        return;
    }
    for t in 0..=counts[c] {
        for p in 0..=pool[c] {
            let have = t + p;
            if have < pips[c] {
                continue;
            }
            let extra = (have - pips[c]) as i32;
            if extra > generic_left {
                continue;
            }
            tapped[c] = t;
            pooled[c] = p;
            search(c + 1, counts, pool, pips, generic_left - extra, tapped, pooled, out);
        }
    }
    tapped[c] = 0;
    pooled[c] = 0;
}
