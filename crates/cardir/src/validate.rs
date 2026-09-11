//! What the schema cannot check: target indices in range, `Triggering` only
//! inside triggers, `X` only with an X cost (none yet), P/T iff creature,
//! auras enchant something, equipment has an equip cost, `Chosen` only as an
//! effect's direct target, `Named` only after the `Chosen` that binds it, no
//! `Unsupported`.

use crate::ir::*;
use crate::types::CardType;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{card}: {problem}")]
pub struct ValidationError {
    pub card: String,
    pub problem: String,
}

struct Ctx<'a> {
    card: &'a Card,
    targets: usize,
    in_trigger: bool,
    /// Whether the `Ref` being checked is an effect's direct target, the
    /// one place a `Chosen` may appear.
    direct: bool,
    /// Names bound by earlier `Chosen`s in the current effect list.
    bound: Vec<String>,
    errors: Vec<String>,
}

impl Ctx<'_> {
    fn err(&mut self, s: impl Into<String>) {
        self.errors.push(s.into());
    }

    fn target(&mut self, i: u8) {
        if i as usize >= self.targets {
            self.err(format!("Target({i}) but only {} target(s) declared", self.targets));
        }
    }

    fn player_ref(&mut self, p: &PlayerRef) {
        match p {
            PlayerRef::TargetPlayer(i) | PlayerRef::TargetOpponent(i) => self.target(*i),
            PlayerRef::Triggering => {
                if !self.in_trigger {
                    self.err("PlayerRef::Triggering outside a trigger");
                }
            }
            PlayerRef::Controller(r) | PlayerRef::Owner(r) => self.reference(r),
            PlayerRef::You | PlayerRef::EachOpponent | PlayerRef::EachPlayer => {}
        }
    }

    fn reference(&mut self, r: &Ref) {
        match r {
            Ref::Target(i) => self.target(*i),
            Ref::Triggering => {
                if !self.in_trigger {
                    self.err("Ref::Triggering outside a trigger");
                }
            }
            Ref::Each(f) => self.filter(f),
            Ref::Player(p) => self.player_ref(p),
            Ref::Attached => {
                if !self.card.is_aura() && !self.card.is_equipment() {
                    self.err("Ref::Attached on a card that is neither an aura nor equipment");
                }
            }
            Ref::Chosen { who, filter, count, bind } => {
                if !self.direct {
                    self.err("Chosen may only be the direct target of an effect");
                }
                self.player_ref(who);
                self.filter(filter);
                match count {
                    Quantity::Exactly(n) | Quantity::UpTo(n) if *n < 1 => self.err("Chosen count must be at least one"),
                    _ => {}
                }
                if let Some(name) = bind {
                    if name.trim().is_empty() {
                        self.err("empty bind name");
                    } else if self.bound.contains(name) {
                        self.err(format!("{name:?} is bound twice"));
                    } else {
                        self.bound.push(name.clone());
                    }
                }
            }
            Ref::Named(name) => {
                if !self.bound.contains(name) {
                    self.err(format!("Named({name:?}) refers to nothing a Chosen bound earlier"));
                }
            }
            Ref::This => {}
        }
    }

    /// An effect's own target: the one position where `Chosen` is allowed.
    fn direct_ref(&mut self, r: &Ref) {
        self.direct = true;
        self.reference(r);
        self.direct = false;
    }

    fn filter(&mut self, f: &Filter) {
        match f {
            Filter::ControlledBy(p) | Filter::InGraveyard(p) => self.player_ref(p),
            Filter::And(fs) | Filter::Or(fs) => {
                if fs.is_empty() {
                    self.err("empty And/Or filter");
                }
                for f in fs {
                    self.filter(f);
                }
            }
            Filter::Not(f) => self.filter(f),
            Filter::Attached if !self.card.is_aura() && !self.card.is_equipment() => {
                self.err("Filter::Attached on a card that is neither an aura nor equipment")
            }
            Filter::Subtype(s) if s.trim().is_empty() => self.err("empty subtype"),
            Filter::Targets(..) => self.err("Targets(count, ...) may only be the whole of one target spec"),
            _ => {}
        }
    }

    /// One entry of a `targets` list: a filter, or `Targets(count, filter)`.
    fn target_spec(&mut self, f: &Filter) {
        match f {
            Filter::Targets(count, inner) => {
                match count {
                    Quantity::Exactly(n) | Quantity::UpTo(n) if *n < 1 => self.err("a target count must be at least one"),
                    _ => {}
                }
                if matches!(**inner, Filter::Targets(..)) {
                    self.err("nested Targets");
                }
                self.filter(inner);
            }
            other => self.filter(other),
        }
    }

    fn targets(&mut self, specs: &[Filter]) {
        self.targets = specs.len();
        self.bound.clear();
        for f in specs {
            self.target_spec(f);
        }
        if specs.iter().filter(|f| f.is_variable()).count() > 1 {
            self.err("at most one target spec may take a variable number of targets");
        }
    }

    fn amount(&mut self, a: &Amount) {
        match a {
            Amount::Const(_) => {}
            Amount::Count(f) => self.filter(f),
            Amount::LifeOf(p) => self.player_ref(p),
            Amount::PowerOf(r) => self.reference(r),
            Amount::X => self.err("X amounts are not supported yet (no X costs)"),
        }
    }

    fn effect(&mut self, e: &Effect) {
        match e {
            Effect::DealDamage { amount, to } => {
                self.amount(amount);
                self.direct_ref(to);
            }
            Effect::Destroy { target } | Effect::Exile { target } | Effect::Tap { target } | Effect::Untap { target } => {
                self.direct_ref(target)
            }
            Effect::ReturnToHand { target } | Effect::CounterSpell { target } => self.direct_ref(target),
            Effect::Draw { player, count } => {
                self.player_ref(player);
                self.amount(count);
            }
            Effect::Discard { player, count, .. } => {
                self.player_ref(player);
                self.amount(count);
            }
            Effect::GainLife { player, amount } | Effect::LoseLife { player, amount } => {
                self.player_ref(player);
                self.amount(amount);
            }
            Effect::ModifyPt {
                target, power, toughness, ..
            } => {
                self.direct_ref(target);
                self.amount(power);
                self.amount(toughness);
            }
            Effect::GrantKeyword { target, .. } => self.direct_ref(target),
            Effect::CreateToken { spec, count } => {
                self.amount(count);
                if spec.types.contains(&CardType::Creature) != spec.pt.is_some() {
                    self.err("token P/T must be present iff the token is a creature");
                }
            }
            Effect::AddCounters { target, count, .. } => {
                self.direct_ref(target);
                self.amount(count);
            }
            Effect::AddMana { amount, .. } => self.amount(amount),
            Effect::Sacrifice { player, filter, count } => {
                self.player_ref(player);
                self.filter(filter);
                self.amount(count);
            }
            Effect::Mill { player, count } => {
                self.player_ref(player);
                self.amount(count);
            }
            Effect::ReturnFromGraveyard { target, .. }
            | Effect::ReturnExiled { target, .. }
            | Effect::Restrict { target, .. }
            | Effect::SkipUntap { target } => self.direct_ref(target),
            Effect::Delayed { effects, .. } => {
                if effects.is_empty() {
                    self.err("a delayed trigger needs at least one effect");
                }
                for e in effects {
                    self.effect(e);
                }
            }
            Effect::Sequence(es) => {
                if es.len() < 2 {
                    self.err("Sequence needs at least two effects");
                }
                for e in es {
                    self.effect(e);
                }
            }
            Effect::Conditional { if_, then, else_ } => {
                self.condition(if_);
                self.effect(then);
                if let Some(e) = else_ {
                    self.effect(e);
                }
            }
            Effect::May { effect, then, otherwise } => {
                self.effect(effect);
                for e in then {
                    self.effect(e);
                }
                for e in otherwise {
                    self.effect(e);
                }
            }
            Effect::Unsupported { reason } => self.err(format!("unsupported effect: {reason}")),
        }
    }

    fn condition(&mut self, c: &Condition) {
        match c {
            Condition::Controls { player, filter, .. } => {
                self.player_ref(player);
                self.filter(filter);
            }
            Condition::LifeAtLeast { player, .. } | Condition::LifeAtMost { player, .. } => self.player_ref(player),
        }
    }

    /// A condition on a static ability may not read power, toughness, or
    /// keywords: those are computed from statics, which would loop.
    fn static_condition(&mut self, c: &Condition) {
        self.condition(c);
        if let Condition::Controls { filter, .. } = c {
            if reads_stats(filter) {
                self.err("a static's condition may not depend on power, toughness, or keywords");
            }
        }
    }

    fn static_(&mut self, s: &Static) {
        match s {
            Static::PtBoost {
                filter, power, toughness, ..
            } => {
                self.filter(filter);
                self.amount(power);
                self.amount(toughness);
            }
            Static::GrantKeyword { filter, .. } => self.filter(filter),
            Static::CostReduction { filter, amount } => {
                self.filter(filter);
                self.amount(amount);
            }
            Static::AsLongAs { condition, static_, .. } => {
                self.static_condition(condition);
                if matches!(**static_, Static::AsLongAs { .. }) {
                    self.err("nested AsLongAs");
                }
                self.static_(static_);
            }
        }
    }

    fn event(&mut self, e: &EventPattern) {
        match e {
            EventPattern::Enters(f) | EventPattern::Dies(f) => self.filter(f),
            EventPattern::Upkeep(p)
            | EventPattern::EndStep(p)
            | EventPattern::BeginCombat(p)
            | EventPattern::GainsLife(p)
            | EventPattern::Discards(p) => self.player_ref(p),
            EventPattern::Cast { who, filter } => {
                self.player_ref(who);
                self.filter(filter);
            }
            EventPattern::Any(es) => {
                if es.is_empty() {
                    self.err("empty Any event");
                }
                for e in es {
                    self.event(e);
                }
            }
            _ => {}
        }
    }

    fn cost(&mut self, c: &Cost) {
        match c {
            Cost::Sacrifice(f) => self.filter(f),
            Cost::PayLife(n) | Cost::Discard(n) if *n <= 0 => self.err("cost amounts must be positive"),
            _ => {}
        }
    }
}

pub fn validate(card: &Card) -> Result<(), ValidationError> {
    let mut ctx = Ctx {
        card,
        targets: 0,
        in_trigger: false,
        direct: false,
        bound: Vec::new(),
        errors: Vec::new(),
    };

    if card.name.trim().is_empty() {
        ctx.err("missing name");
    }
    if card.types.is_empty() {
        ctx.err("no card types");
    }
    if card.is_creature() != card.pt.is_some() {
        ctx.err("P/T must be present iff the card is a creature");
    }
    let is_spell_card = card.types.contains(&CardType::Instant) || card.types.contains(&CardType::Sorcery);
    if is_spell_card != card.spell.is_some() {
        ctx.err("instants and sorceries must have `spell`, and only they may");
    }
    if is_spell_card && card.is_permanent() {
        ctx.err("a card cannot be both a spell and a permanent type");
    }
    if card.enchant.is_some() && !(card.types.contains(&CardType::Enchantment) && card.subtypes.iter().any(|s| s == "Aura")) {
        ctx.err("`enchant` requires the Enchantment type and the Aura subtype");
    }
    if card.types.contains(&CardType::Enchantment) && card.subtypes.iter().any(|s| s == "Aura") && card.enchant.is_none() {
        ctx.err("an Aura needs `enchant`");
    }
    if card.equip.is_some() != (card.types.contains(&CardType::Artifact) && card.subtypes.iter().any(|s| s == "Equipment")) {
        ctx.err("`equip` iff Artifact — Equipment");
    }
    if card.is_land() && !card.cost.is_free() {
        ctx.err("lands have no mana cost");
    }
    if card.keywords.contains(&Keyword::Reach) && card.keywords.contains(&Keyword::Flying) {
        ctx.err("flying and reach together is redundant");
    }
    for kw in &card.keywords {
        if card.keywords.iter().filter(|k| *k == kw).count() > 1 {
            ctx.err(format!("duplicate keyword {}", kw.word()));
        }
    }

    if let Some(spell) = &card.spell {
        if spell.modes.is_empty() {
            ctx.targets(&spell.targets);
            if spell.effects.is_empty() {
                ctx.err("a spell needs at least one effect");
            }
            for e in &spell.effects {
                ctx.effect(e);
            }
        } else {
            if !spell.effects.is_empty() || !spell.targets.is_empty() {
                ctx.err("a modal spell keeps its targets and effects in its modes");
            }
            if spell.modes.len() < 2 {
                ctx.err("a modal spell needs at least two modes");
            }
            if spell.modes.len() < spell.choose.bounds().0 {
                ctx.err("not enough modes to choose that many");
            }
            for mode in &spell.modes {
                ctx.targets(&mode.targets);
                if mode.effects.is_empty() {
                    ctx.err("a mode needs at least one effect");
                }
                for e in &mode.effects {
                    ctx.effect(e);
                }
            }
        }
    }
    if let Some(f) = &card.enchant {
        ctx.targets = 0;
        ctx.filter(f);
    }
    for s in &card.statics {
        ctx.targets = 0;
        ctx.static_(s);
    }
    for t in &card.triggers {
        ctx.in_trigger = true;
        ctx.targets(&t.targets);
        ctx.event(&t.event);
        if let Some(c) = &t.condition {
            ctx.condition(c);
        }
        if t.effects.is_empty() {
            ctx.err("a trigger needs at least one effect");
        }
        for e in &t.effects {
            ctx.effect(e);
        }
        ctx.in_trigger = false;
    }
    for a in &card.activated {
        ctx.targets(&a.targets);
        if a.cost.is_empty() {
            ctx.err("an activated ability needs a cost");
        }
        for c in &a.cost {
            ctx.cost(c);
        }
        if a.effects.is_empty() {
            ctx.err("an activated ability needs at least one effect");
        }
        for e in &a.effects {
            ctx.effect(e);
        }
    }

    if ctx.errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError {
            card: card.name.clone(),
            problem: ctx.errors.join("; "),
        })
    }
}

use crate::types::Keyword;

/// Whether a filter looks at power, toughness, or keywords.
fn reads_stats(f: &Filter) -> bool {
    match f {
        Filter::PowerAtLeast(_) | Filter::PowerAtMost(_) | Filter::HasKeyword(_) => true,
        Filter::And(fs) | Filter::Or(fs) => fs.iter().any(reads_stats),
        Filter::Not(f) | Filter::Targets(_, f) => reads_stats(f),
        _ => false,
    }
}
