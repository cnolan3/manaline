//! Mana sources, payment enumeration (§10: by colour combination, engine picks
//! the permanents), and payment.

use crate::action::ManaPayment;
use crate::error::RulesError;
use crate::event::Event;
use crate::game::Game;
use crate::types::{Color, Mana, ManaCost, ObjectId, Seat};

impl Game {
    /// Untapped permanents `seat` controls with an intrinsic mana ability, and the colour each makes.
    pub fn mana_sources(&self, seat: Seat) -> Vec<(ObjectId, Color)> {
        let mut out = Vec::new();
        for &id in &self.players[seat.index()].battlefield {
            let obj = &self.objects[id];
            if obj.tapped {
                continue;
            }
            let def = self.cards.get(obj.card);
            if def.produces.is_empty() || (def.is_creature() && obj.summoning_sick) {
                continue;
            }
            out.push((id, def.produces[0]));
        }
        out.sort();
        out
    }

    /// Every distinct way `seat` could pay `cost` right now, one payment per
    /// colour combination. Concrete permanents are chosen by the engine.
    pub fn enumerate_payments(&self, seat: Seat, cost: &ManaCost) -> Vec<ManaPayment> {
        if cost.is_free() {
            return vec![ManaPayment::default()];
        }
        let sources = self.mana_sources(seat);
        let pool = &self.players[seat.index()].mana_pool;

        let mut counts = [0u8; 5];
        for (_, c) in &sources {
            counts[*c as usize] += 1;
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
        let mut tapped = [0u8; 5];
        let mut pooled = [0u8; 6];
        search(0, &counts, &pool_counts, &pips, cost.generic as i32, &mut tapped, &mut pooled, &mut solutions);

        solutions
            .into_iter()
            .map(|(tapped, pooled)| {
                let mut payment = ManaPayment::default();
                for c in Color::ALL {
                    let want = tapped[c as usize] as usize;
                    payment.tap.extend(sources.iter().filter(|(_, sc)| *sc == c).map(|(id, _)| *id).take(want));
                    for _ in 0..pooled[c as usize] {
                        payment.from_pool.push(Mana::Colored(c));
                    }
                }
                for _ in 0..pooled[5] {
                    payment.from_pool.push(Mana::Colorless);
                }
                payment.tap.sort();
                payment.from_pool.sort();
                payment
            })
            .collect()
    }

    /// Validate and execute a payment: tap the named permanents, spend the named
    /// pool mana, pay `cost`, and return any surplus to the pool.
    pub(crate) fn pay_mana(&mut self, seat: Seat, payment: &ManaPayment, cost: &ManaCost) -> Result<(), RulesError> {
        let sources = self.mana_sources(seat);
        let mut paying: Vec<Mana> = Vec::new();
        let mut seen = Vec::new();
        for &id in &payment.tap {
            if seen.contains(&id) {
                return Err(RulesError::illegal(format!("{id} is listed twice in the payment")));
            }
            seen.push(id);
            match sources.iter().find(|(s, _)| *s == id) {
                Some((_, c)) => paying.push(Mana::Colored(*c)),
                None => {
                    return Err(RulesError::illegal(format!(
                        "{id} is not an untapped mana source {seat} controls"
                    )))
                }
            }
        }
        {
            let mut pool = self.players[seat.index()].mana_pool.clone();
            for &m in &payment.from_pool {
                if !pool.remove(m, 1) {
                    return Err(RulesError::illegal(format!("{seat} has no {m} in their mana pool to spend")));
                }
                paying.push(m);
            }
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

fn mana_slot(m: Mana) -> usize {
    match m {
        Mana::Colored(c) => c as usize,
        Mana::Colorless => 5,
    }
}

#[allow(clippy::too_many_arguments)]
fn search(
    c: usize,
    counts: &[u8; 5],
    pool: &[u8; 6],
    pips: &[u8; 5],
    generic_left: i32,
    tapped: &mut [u8; 5],
    pooled: &mut [u8; 6],
    out: &mut Vec<([u8; 5], [u8; 6])>,
) {
    if generic_left < 0 {
        return;
    }
    if c == 5 {
        // Whatever generic remains must come from colourless pool mana.
        let need = generic_left as u8;
        if pool[5] >= need {
            pooled[5] = need;
            out.push((*tapped, *pooled));
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
