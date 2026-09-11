//! Casting in steps (§10's two-step flow): a modal spell, or one with a
//! caster-chosen number of targets, is paid for first and then asks for its
//! modes and each target spec in turn before it goes on the stack. Also the
//! target bookkeeping every resolution shares: grouping a flat target list
//! by spec and pruning targets that became illegal.

use crate::action::Target;
use crate::card::CardDef;
use crate::error::RulesError;
use crate::event::Event;
use crate::filter::Ctx;
use crate::game::{Game, PendingChoice, StackKind, StackObject};
use crate::types::{ObjectId, Seat, Zone};
use cardir::Filter;

/// The `ChooseMode` value that ends mode selection for "choose one or both".
pub const NO_MORE_MODES: u8 = u8::MAX;

impl Game {
    /// Whether this card is cast in steps rather than with its targets listed up front.
    pub fn is_two_step(def: &CardDef) -> bool {
        def.is_modal() || Self::spell_specs(def).iter().any(Filter::is_variable)
    }

    /// A non-modal spell's target specs: an aura's `enchant`, else the spell's list.
    fn spell_specs(def: &CardDef) -> Vec<Filter> {
        if let Some(f) = &def.ir.enchant {
            return vec![f.clone()];
        }
        def.ir.spell.as_ref().map(|s| s.targets.clone()).unwrap_or_default()
    }

    /// The target specs a cast asks for, in order: the chosen modes' specs
    /// one mode after another, or the spell's own.
    pub fn cast_specs(def: &CardDef, modes: &[u8]) -> Vec<Filter> {
        if !def.is_modal() {
            return Self::spell_specs(def);
        }
        let spell = def.ir.spell.as_ref().expect("modal cards are spells");
        modes
            .iter()
            .filter_map(|&m| spell.modes.get(m as usize))
            .flat_map(|m| m.targets.iter().cloned())
            .collect()
    }

    /// Split a flat target list into one group per spec. Fixed-count specs
    /// take their share in order; the one variable spec takes the rest.
    pub fn group_targets(specs: &[Filter], targets: &[Target]) -> Vec<Vec<Target>> {
        let fixed: usize = specs.iter().filter_map(|s| s.spec_bounds().2.filter(|_| !s.is_variable())).sum();
        let variable = targets.len().saturating_sub(fixed);
        let mut groups = Vec::with_capacity(specs.len());
        let mut at = 0;
        for spec in specs {
            let take = if spec.is_variable() {
                variable
            } else {
                spec.spec_bounds().2.unwrap_or(0)
            };
            let end = (at + take).min(targets.len());
            groups.push(targets[at..end].to_vec());
            at = end;
        }
        groups
    }

    /// Drop targets that are no longer legal for their spec (rule 608.2b).
    pub fn prune_targets(&self, specs: &[Filter], groups: Vec<Vec<Target>>, ctx: &Ctx) -> Vec<Vec<Target>> {
        groups
            .into_iter()
            .enumerate()
            .map(|(i, group)| {
                let Some(spec) = specs.get(i) else { return Vec::new() };
                let (inner, _, _) = spec.spec_bounds();
                group.into_iter().filter(|t| self.target_is_legal(*t, inner, ctx)).collect()
            })
            .collect()
    }

    /// Whether `seat` could pick this mode now: every fixed target spec has
    /// enough legal targets.
    pub fn mode_castable(&self, seat: Seat, object: ObjectId, mode: usize) -> bool {
        let def = self.card_def(object);
        let Some(m) = def.ir.spell.as_ref().and_then(|s| s.modes.get(mode)) else {
            return false;
        };
        let ctx = Ctx::simple(seat, Some(object));
        m.targets.iter().all(|spec| {
            let (inner, min, _) = spec.spec_bounds();
            min == 0 || self.targets_for(inner, &ctx).len() >= min
        })
    }

    /// Whether a two-step spell can be cast at all right now.
    pub fn two_step_castable(&self, seat: Seat, object: ObjectId) -> bool {
        let def = self.card_def(object);
        if def.is_modal() {
            let spell = def.ir.spell.as_ref().expect("modal cards are spells");
            let castable = (0..spell.modes.len()).filter(|&m| self.mode_castable(seat, object, m)).count();
            castable >= spell.choose.bounds().0
        } else {
            let ctx = Ctx::simple(seat, Some(object));
            Self::spell_specs(def).iter().all(|spec| {
                let (inner, min, _) = spec.spec_bounds();
                min == 0 || self.targets_for(inner, &ctx).len() >= min
            })
        }
    }

    /// The modes still on offer while casting: `(index, text)`, plus
    /// `(NO_MORE_MODES, ...)` once "one or both" may stop.
    pub fn mode_options(&self, seat: Seat, object: ObjectId, chosen: &[u8]) -> Vec<(u8, String)> {
        let def = self.card_def(object);
        let Some(spell) = def.ir.spell.as_ref() else { return Vec::new() };
        let (min, _) = spell.choose.bounds();
        let mut out: Vec<(u8, String)> = spell
            .modes
            .iter()
            .enumerate()
            .filter(|(i, _)| !chosen.contains(&(*i as u8)) && self.mode_castable(seat, object, *i))
            .map(|(i, m)| (i as u8, cardir::render_mode(&def.ir, m)))
            .collect();
        if chosen.len() >= min && chosen.len() < spell.modes.len() {
            out.push((NO_MORE_MODES, "No more modes".into()));
        }
        out
    }

    pub(crate) fn modes_complete(def: &CardDef, chosen: &[u8], done: bool) -> bool {
        if !def.is_modal() {
            return true;
        }
        let (min, max) = def.ir.spell.as_ref().map(|s| s.choose.bounds()).unwrap_or((0, 0));
        chosen.len() >= max || (done && chosen.len() >= min)
    }

    /// Begin casting a two-step spell: mana is paid, the card stays in hand
    /// while its modes and targets are chosen, nobody holds priority.
    pub(crate) fn begin_two_step_cast(&mut self, seat: Seat, object: ObjectId, x: u32) {
        self.pending = Some(PendingChoice::Casting {
            seat,
            object,
            modes: Vec::new(),
            modes_done: false,
            targets: Vec::new(),
            spec: 0,
            x,
        });
        self.priority = None;
        self.advance_casting();
    }

    /// Move the cast along: wait for a mode or a target spec that needs a
    /// decision, skip specs with nothing to pick, and finish when done.
    fn advance_casting(&mut self) {
        loop {
            let Some(PendingChoice::Casting {
                seat,
                object,
                modes,
                modes_done,
                targets,
                spec,
                x,
            }) = &self.pending
            else {
                return;
            };
            let def = self.card_def(*object).clone();
            if !Self::modes_complete(&def, modes, *modes_done) {
                return;
            }
            let specs = Self::cast_specs(&def, modes);
            if *spec >= specs.len() {
                let (seat, object, modes, targets, x) = (*seat, *object, modes.clone(), targets.clone(), *x);
                self.pending = None;
                self.finish_cast(seat, object, modes, targets, x);
                return;
            }
            let (inner, min, _) = specs[*spec].spec_bounds();
            let ctx = Ctx::simple(*seat, Some(*object));
            if min == 0 && self.targets_for(inner, &ctx).is_empty() {
                // Nothing to pick: the spec takes no targets.
                if let Some(PendingChoice::Casting { spec, .. }) = &mut self.pending {
                    *spec += 1;
                }
                continue;
            }
            return;
        }
    }

    pub(crate) fn answer_cast_mode(&mut self, mode: u8) -> Result<(), RulesError> {
        match &mut self.pending {
            Some(PendingChoice::Casting { modes, modes_done, .. }) => {
                if mode == NO_MORE_MODES {
                    *modes_done = true;
                } else {
                    modes.push(mode);
                }
            }
            _ => return Err(RulesError::illegal("no modes to choose")),
        }
        self.advance_casting();
        Ok(())
    }

    pub(crate) fn answer_cast_targets(&mut self, chosen: &[Target]) -> Result<(), RulesError> {
        match &mut self.pending {
            Some(PendingChoice::Casting { targets, spec, .. }) => {
                targets.extend(chosen.iter().copied());
                *spec += 1;
            }
            _ => return Err(RulesError::illegal("no targets to choose")),
        }
        self.advance_casting();
        Ok(())
    }

    /// The spell goes on the stack with everything chosen; the caster gets priority.
    fn finish_cast(&mut self, seat: Seat, object: ObjectId, modes: Vec<u8>, targets: Vec<Target>, x: u32) {
        self.objects[object].controller = seat;
        self.move_object(object, Zone::Stack);
        self.stack.push(StackObject {
            object,
            controller: seat,
            targets: targets.clone(),
            kind: StackKind::Spell,
            modes,
            x,
        });
        self.emit(Event::Cast { seat, object, targets });
        self.give_priority(seat);
    }
}
