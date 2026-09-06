//! What the schema cannot check: target indices in range, `Triggering` only
//! inside triggers, `X` only with an X cost (none yet), P/T iff creature,
//! auras enchant something, equipment has an equip cost, no `Unsupported`.

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
            Ref::This => {}
        }
    }

    fn filter(&mut self, f: &Filter) {
        match f {
            Filter::ControlledBy(p) => self.player_ref(p),
            Filter::And(fs) | Filter::Or(fs) => {
                if fs.is_empty() {
                    self.err("empty And/Or filter");
                }
                for f in fs {
                    self.filter(f);
                }
            }
            Filter::Not(f) => self.filter(f),
            Filter::Attached => {
                if !self.card.is_aura() && !self.card.is_equipment() {
                    self.err("Filter::Attached on a card that is neither an aura nor equipment");
                }
            }
            Filter::Subtype(s) if s.trim().is_empty() => self.err("empty subtype"),
            _ => {}
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
                self.reference(to);
            }
            Effect::Destroy { target } | Effect::Exile { target } | Effect::Tap { target } | Effect::Untap { target } => {
                self.reference(target)
            }
            Effect::ReturnToHand { target } | Effect::CounterSpell { target } => self.reference(target),
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
            Effect::ModifyPt { target, power, toughness, .. } => {
                self.reference(target);
                self.amount(power);
                self.amount(toughness);
            }
            Effect::GrantKeyword { target, .. } => self.reference(target),
            Effect::CreateToken { spec, count } => {
                self.amount(count);
                if spec.types.contains(&CardType::Creature) != spec.pt.is_some() {
                    self.err("token P/T must be present iff the token is a creature");
                }
            }
            Effect::AddCounters { target, count, .. } => {
                self.reference(target);
                self.amount(count);
            }
            Effect::AddMana { amount, .. } => self.amount(amount),
            Effect::Sacrifice { player, filter, count } => {
                self.player_ref(player);
                self.filter(filter);
                self.amount(count);
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
                match if_ {
                    Condition::Controls { player, filter, .. } => {
                        self.player_ref(player);
                        self.filter(filter);
                    }
                }
                self.effect(then);
                if let Some(e) = else_ {
                    self.effect(e);
                }
            }
            Effect::Unsupported { reason } => self.err(format!("unsupported effect: {reason}")),
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
    let mut ctx = Ctx { card, targets: 0, in_trigger: false, errors: Vec::new() };

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
        ctx.targets = spell.targets.len();
        for f in &spell.targets {
            ctx.filter(f);
        }
        if spell.effects.is_empty() {
            ctx.err("a spell needs at least one effect");
        }
        for e in &spell.effects {
            ctx.effect(e);
        }
    }
    if let Some(f) = &card.enchant {
        ctx.targets = 0;
        ctx.filter(f);
    }
    for s in &card.statics {
        ctx.targets = 0;
        match s {
            Static::PtBoost { filter, power, toughness, .. } => {
                ctx.filter(filter);
                ctx.amount(power);
                ctx.amount(toughness);
            }
            Static::GrantKeyword { filter, .. } => ctx.filter(filter),
            Static::CostReduction { filter, amount } => {
                ctx.filter(filter);
                ctx.amount(amount);
            }
        }
    }
    for t in &card.triggers {
        ctx.in_trigger = true;
        ctx.targets = t.targets().len();
        for f in t.targets() {
            ctx.filter(f);
        }
        if let Trigger::Upkeep { whose, .. } | Trigger::EndStep { whose, .. } = t {
            ctx.player_ref(whose);
        }
        if t.effects().is_empty() {
            ctx.err("a trigger needs at least one effect");
        }
        for e in t.effects() {
            ctx.effect(e);
        }
        ctx.in_trigger = false;
    }
    for a in &card.activated {
        ctx.targets = a.targets.len();
        for f in &a.targets {
            ctx.filter(f);
        }
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
        Err(ValidationError { card: card.name.clone(), problem: ctx.errors.join("; ") })
    }
}

use crate::types::Keyword;
