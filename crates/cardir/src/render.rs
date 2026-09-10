//! The English renderer: IR back to templated Oracle text. This is the
//! correctness oracle for ingestion (§4.3) and the display text for cards
//! without a Scryfall entry. It targets current Oracle phrasing ("enters",
//! "any target").

use crate::ir::*;
use crate::types::{CardType, Keyword};
use std::collections::{BTreeMap, BTreeSet};

/// Grammatical number for a noun phrase.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Number {
    Singular,
    Plural,
}

struct R<'a> {
    card: &'a Card,
    /// Filters of the current targets, for rendering `Target(i)`.
    targets: &'a [Filter],
    /// Inside a trigger's effects, `This` is "it".
    in_trigger: bool,
    /// Whether `This` has been mentioned in this sentence group already.
    this_mentioned: bool,
    /// Head noun of every `Chosen` binding seen so far, for "that creature".
    bound: BTreeMap<String, String>,
    /// Bindings introduced in the sentence being rendered, which read as "it".
    bound_this_sentence: BTreeSet<String>,
}

impl<'a> R<'a> {
    fn new(card: &'a Card, targets: &'a [Filter], in_trigger: bool) -> R<'a> {
        R {
            card,
            targets,
            in_trigger,
            this_mentioned: in_trigger,
            bound: BTreeMap::new(),
            bound_this_sentence: BTreeSet::new(),
        }
    }

    fn new_sentence(&mut self) {
        self.bound_this_sentence.clear();
    }
}

pub fn render(card: &Card) -> String {
    let mut lines: Vec<String> = Vec::new();
    if !card.keywords.is_empty() {
        let mut words: Vec<String> = card.keywords.iter().map(|k| k.word().to_string()).collect();
        words[0] = capitalize(&words[0]);
        lines.push(words.join(", "));
    }
    if let Some(f) = &card.enchant {
        let r = R::new(card, &[], false);
        lines.push(format!("Enchant {}", r.noun(f, Number::Singular)));
    }
    for s in &card.statics {
        let mut r = R::new(card, &[], false);
        lines.push(r.static_(s));
    }
    for t in &card.triggers {
        let mut r = R::new(card, &t.targets, t.event.names_this());
        lines.push(r.trigger(t));
    }
    for a in &card.activated {
        let mut r = R::new(card, &a.targets, false);
        let line = r.ability(a);
        // A basic land's mana ability is intrinsic and printed as reminder text.
        if card.is_basic() && a.is_mana_ability() {
            lines.push(format!("({line})"));
        } else {
            lines.push(line);
        }
    }
    if let Some(spell) = &card.spell {
        lines.extend(spell_lines(card, spell));
    }
    if let Some(cost) = &card.equip {
        lines.push(format!("Equip {cost}"));
    }
    lines.join("\n")
}

/// One activated ability as a line of Oracle text.
pub fn render_ability(card: &Card, a: &Ability) -> String {
    let mut r = R::new(card, &a.targets, false);
    r.ability(a)
}

/// One effect as a clause of Oracle text ("return target creature to its
/// owner's hand"), given the target filters it is read against.
pub fn render_clause(card: &Card, targets: &[Filter], effect: &Effect) -> String {
    let mut r = R::new(card, targets, false);
    r.clause(effect)
}

/// One triggered ability as a line of Oracle text.
pub fn render_trigger(card: &Card, t: &Trigger) -> String {
    let mut r = R::new(card, &t.targets, t.event.names_this());
    r.trigger(t)
}

/// A spell's effects as Oracle text.
pub fn render_spell(card: &Card) -> String {
    match &card.spell {
        Some(spell) => spell_lines(card, spell).join("\n"),
        None => String::new(),
    }
}

/// One mode of a modal spell as Oracle text, without its bullet.
pub fn render_mode(card: &Card, mode: &Mode) -> String {
    let mut r = R::new(card, &mode.targets, false);
    r.sentences(&mode.effects)
}

fn spell_lines(card: &Card, spell: &Spell) -> Vec<String> {
    if spell.modes.is_empty() {
        let mut r = R::new(card, &spell.targets, false);
        return vec![r.sentences(&spell.effects)];
    }
    let mut lines = vec![format!("Choose {} \u{2014}", spell.choose.word())];
    for mode in &spell.modes {
        lines.push(format!("\u{2022} {}", render_mode(card, mode)));
    }
    lines
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn number_word(n: i32) -> String {
    match n {
        0 => "zero".into(),
        1 => "one".into(),
        2 => "two".into(),
        3 => "three".into(),
        4 => "four".into(),
        5 => "five".into(),
        6 => "six".into(),
        7 => "seven".into(),
        8 => "eight".into(),
        9 => "nine".into(),
        10 => "ten".into(),
        n => n.to_string(),
    }
}

/// "a card" / "two cards"
fn counted(n: &Amount, noun: &str) -> String {
    match n {
        Amount::Const(1) => format!("{} {noun}", article(noun)),
        Amount::Const(k) => format!("{} {noun}s", number_word(*k)),
        other => format!("{} {noun}s", amount_phrase(other)),
    }
}

fn article(noun: &str) -> &'static str {
    match noun.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

fn amount_phrase(a: &Amount) -> String {
    match a {
        Amount::Const(n) => n.to_string(),
        Amount::X => "X".into(),
        Amount::Count(_) | Amount::LifeOf(_) | Amount::PowerOf(_) => "that much".into(),
    }
}

fn signed(a: &Amount) -> String {
    match a {
        Amount::Const(n) if *n >= 0 => format!("+{n}"),
        Amount::Const(n) => n.to_string(),
        Amount::X => "+X".into(),
        _ => "+that much".into(),
    }
}

fn keyword_list(ks: &[Keyword]) -> String {
    let words: Vec<&str> = ks.iter().map(|k| k.word()).collect();
    match words.len() {
        0 => String::new(),
        1 => words[0].into(),
        2 => format!("{} and {}", words[0], words[1]),
        n => format!("{}, and {}", words[..n - 1].join(", "), words[n - 1]),
    }
}

impl R<'_> {
    // ----- noun phrases -----

    /// A noun phrase for a filter: "creature", "tapped creature", "Elf
    /// creatures you control", "artifact or enchantment", "creature with flying".
    fn noun(&self, f: &Filter, number: Number) -> String {
        let (adjectives, head, postfixes) = self.split(f);
        let mut s = String::new();
        for a in &adjectives {
            s.push_str(a);
            s.push(' ');
        }
        let head = match number {
            Number::Singular => head,
            Number::Plural => plural(&head),
        };
        s.push_str(&head);
        for p in &postfixes {
            s.push(' ');
            s.push_str(p);
        }
        s
    }

    /// Split a filter into prefix adjectives, a head noun, and postfix clauses.
    /// A subtype with no card type ("each Elf you control") is its own head.
    fn split(&self, f: &Filter) -> (Vec<String>, String, Vec<String>) {
        let mut adjectives = Vec::new();
        let mut heads: Vec<String> = Vec::new();
        let mut postfixes = Vec::new();
        self.collect(f, &mut adjectives, &mut heads, &mut postfixes);
        if heads.is_empty() {
            if let Some(i) = adjectives.iter().position(|a| a.chars().next().is_some_and(|c| c.is_uppercase())) {
                heads.push(adjectives.remove(i));
            }
        }
        // "creature spell", "instant or sorcery spell": card types qualify a
        // spell rather than alternating with it.
        if heads.len() > 1 && heads.iter().any(|h| h == "spell") {
            let others: Vec<String> = heads.iter().filter(|h| *h != "spell").cloned().collect();
            adjectives.push(others.join(" or "));
            heads = vec!["spell".into()];
        }
        let mut head = match heads.len() {
            0 => "permanent".to_string(),
            1 => heads.remove(0),
            2 => format!("{} or {}", heads[0], heads[1]),
            n => format!("{}, or {}", heads[..n - 1].join(", "), heads[n - 1]),
        };
        if mentions_graveyard(f) {
            // "creature card from your graveyard"; a bare graveyard filter is just "card".
            head = if head == "permanent" && !mentions_permanent(f) {
                "card".into()
            } else {
                format!("{head} card")
            };
        }
        (adjectives, head, postfixes)
    }

    fn collect(&self, f: &Filter, adjectives: &mut Vec<String>, heads: &mut Vec<String>, postfixes: &mut Vec<String>) {
        match f {
            Filter::Targets(_, inner) => self.collect(inner, adjectives, heads, postfixes),
            Filter::Any => heads.push("any target".into()),
            Filter::Creature => heads.push("creature".into()),
            Filter::Land => heads.push("land".into()),
            Filter::Artifact => heads.push("artifact".into()),
            Filter::Enchantment => heads.push("enchantment".into()),
            Filter::Instant => heads.push("instant".into()),
            Filter::Sorcery => heads.push("sorcery".into()),
            Filter::Permanent => heads.push("permanent".into()),
            Filter::Player => heads.push("player".into()),
            Filter::Opponent => heads.push("opponent".into()),
            Filter::Spell => heads.push("spell".into()),
            Filter::Token => heads.push("token".into()),
            Filter::Other => adjectives.insert(0, "other".into()),
            Filter::Attached => heads.push(
                if self.card.is_equipment() {
                    "equipped creature"
                } else {
                    "enchanted creature"
                }
                .into(),
            ),
            Filter::Subtype(s) => adjectives.push(s.clone()),
            Filter::Color(c) => adjectives.push(c.word().into()),
            Filter::InGraveyard(p) => postfixes.push(match p {
                PlayerRef::You => "from your graveyard".into(),
                PlayerRef::EachPlayer => "from a graveyard".into(),
                PlayerRef::EachOpponent => "from an opponent's graveyard".into(),
                PlayerRef::TargetPlayer(_) | PlayerRef::TargetOpponent(_) | PlayerRef::Triggering => "from that player's graveyard".into(),
                _ => "from its owner's graveyard".into(),
            }),
            Filter::ControlledBy(p) => postfixes.push(match p {
                PlayerRef::You => "you control".into(),
                PlayerRef::EachOpponent => "your opponents control".into(),
                PlayerRef::TargetOpponent(_) => "target opponent controls".into(),
                PlayerRef::TargetPlayer(_) => "target player controls".into(),
                _ => "that player controls".into(),
            }),
            Filter::Tapped => adjectives.push("tapped".into()),
            Filter::Untapped => adjectives.push("untapped".into()),
            Filter::Attacking => adjectives.push("attacking".into()),
            Filter::Blocking => adjectives.push("blocking".into()),
            Filter::PowerAtLeast(n) => postfixes.push(format!("with power {n} or greater")),
            Filter::PowerAtMost(n) => postfixes.push(format!("with power {n} or less")),
            Filter::HasKeyword(k) => postfixes.push(format!("with {}", k.word())),
            Filter::Not(inner) => match &**inner {
                Filter::HasKeyword(k) => postfixes.push(format!("without {}", k.word())),
                Filter::Color(c) => adjectives.push(format!("non{}", c.word())),
                Filter::Creature => adjectives.push("noncreature".into()),
                Filter::Land => adjectives.push("nonland".into()),
                Filter::Artifact => adjectives.push("nonartifact".into()),
                Filter::Token => adjectives.push("nontoken".into()),
                Filter::Subtype(s) => adjectives.push(format!("non-{s}")),
                other => adjectives.push(format!("non-{}", self.noun(other, Number::Singular))),
            },
            Filter::And(fs) => {
                for f in fs {
                    self.collect(f, adjectives, heads, postfixes);
                }
            }
            Filter::Or(fs) => {
                // Either "attacking or blocking creature" (adjectives) or "artifact or enchantment" (heads).
                let mut sub_adj = Vec::new();
                let mut sub_heads = Vec::new();
                for f in fs {
                    let mut a = Vec::new();
                    let mut h = Vec::new();
                    let mut p = Vec::new();
                    self.collect(f, &mut a, &mut h, &mut p);
                    sub_adj.extend(a);
                    sub_heads.extend(h);
                    postfixes.extend(p);
                }
                if sub_heads.is_empty() {
                    adjectives.push(sub_adj.join(" or "));
                } else {
                    heads.extend(sub_heads);
                    adjectives.extend(sub_adj);
                }
            }
        }
    }

    fn target_phrase(&self, i: u8) -> String {
        let Some(spec) = self.targets.get(i as usize) else {
            return format!("target #{i}");
        };
        let (f, _, _) = spec.spec_bounds();
        let one = match f {
            Filter::Any => "any target".to_string(),
            f if has_other(f) => format!("another target {}", self.noun(&without_other(f), Number::Singular)),
            f => format!("target {}", self.noun(f, Number::Singular)),
        };
        let many = |r: &Self| match f {
            Filter::Any => "any targets".to_string(),
            f => format!("target {}", r.noun(f, Number::Plural)),
        };
        match spec {
            Filter::Targets(Quantity::Exactly(1), _) | Filter::Targets(Quantity::UpTo(1), _)
                if matches!(spec, Filter::Targets(Quantity::UpTo(1), _)) =>
            {
                format!("up to one {one}")
            }
            Filter::Targets(Quantity::Exactly(n), _) if *n > 1 => format!("{} {}", number_word(*n), many(self)),
            Filter::Targets(Quantity::UpTo(n), _) if *n > 1 => format!("up to {} {}", number_word(*n), many(self)),
            Filter::Targets(Quantity::AnyNumber, _) => format!("any number of {}", many(self)),
            _ => one,
        }
    }

    /// Whether target `i` may be several things, so verbs take "each".
    fn multi_target(&self, r: &Ref) -> bool {
        match r {
            Ref::Target(i) => self.targets.get(*i as usize).map(Filter::is_multi).unwrap_or(false),
            _ => false,
        }
    }

    fn this(&mut self) -> String {
        if self.in_trigger || self.this_mentioned {
            "it".into()
        } else {
            self.this_mentioned = true;
            "~".into()
        }
    }

    /// The object phrase for a `Ref` in object position.
    fn object(&mut self, r: &Ref) -> String {
        match r {
            Ref::Target(i) => self.target_phrase(*i),
            Ref::This => self.this(),
            Ref::Triggering => "that creature".into(),
            Ref::Each(f) => format!("each {}", self.noun(f, Number::Singular)),
            Ref::Player(p) => self.player_object(p),
            Ref::Attached => (if self.card.is_equipment() {
                "equipped creature"
            } else {
                "enchanted creature"
            })
            .into(),
            Ref::Chosen { filter, count, bind, .. } => {
                if let Some(name) = bind {
                    let (_, head, _) = self.split(filter);
                    self.bound.insert(name.clone(), head);
                    self.bound_this_sentence.insert(name.clone());
                }
                self.chosen_phrase(filter, count)
            }
            Ref::Named(name) => {
                if self.bound_this_sentence.contains(name) {
                    "it".into()
                } else {
                    format!("that {}", self.bound.get(name).cloned().unwrap_or_else(|| "one".into()))
                }
            }
        }
    }

    /// "a creature you control", "up to two permanents you control", "any number of Elves".
    fn chosen_phrase(&self, filter: &Filter, count: &Quantity) -> String {
        match count {
            Quantity::Exactly(1) => {
                let n = self.noun(filter, Number::Singular);
                format!("{} {n}", article(&n))
            }
            Quantity::Exactly(k) => format!("{} {}", number_word(*k), self.noun(filter, Number::Plural)),
            Quantity::UpTo(1) => format!("up to one {}", self.noun(filter, Number::Singular)),
            Quantity::UpTo(k) => format!("up to {} {}", number_word(*k), self.noun(filter, Number::Plural)),
            Quantity::AnyNumber => format!("any number of {}", self.noun(filter, Number::Plural)),
        }
    }

    /// The subject phrase for a `Ref` ("Target creature", "Creatures you control", "~").
    fn subject(&mut self, r: &Ref) -> (String, Number) {
        match r {
            Ref::Each(f) => (self.noun(f, Number::Plural), Number::Plural),
            Ref::Chosen { count, .. } if !matches!(count, Quantity::Exactly(1)) => (self.object(r), Number::Plural),
            Ref::Target(_) if self.multi_target(r) => (format!("{} each", self.object(r)), Number::Plural),
            other => (self.object(other), Number::Singular),
        }
    }

    fn player_object(&self, p: &PlayerRef) -> String {
        match p {
            PlayerRef::You => "you".into(),
            PlayerRef::TargetPlayer(_) => "target player".into(),
            PlayerRef::TargetOpponent(_) => "target opponent".into(),
            PlayerRef::EachOpponent => "each opponent".into(),
            PlayerRef::EachPlayer => "each player".into(),
            PlayerRef::Triggering => "that player".into(),
            PlayerRef::Controller(_) => "that creature's controller".into(),
            PlayerRef::Owner(_) => "its owner".into(),
        }
    }

    /// Subject phrase and whether the verb is second person ("you gain") or third ("target player gains").
    fn player_subject(&self, p: &PlayerRef) -> (String, bool) {
        match p {
            PlayerRef::You => ("you".into(), true),
            other => (self.player_object(other), false),
        }
    }

    // ----- sentences -----

    fn sentences(&mut self, effects: &[Effect]) -> String {
        // "~ deals N damage to X and you gain N life" is printed as one sentence.
        if let [Effect::DealDamage { .. }, Effect::GainLife {
            player: PlayerRef::You, ..
        }] = effects
        {
            let a = self.clause(&effects[0]);
            let b = self.clause(&effects[1]);
            return format!("{}.", capitalize(&format!("{a} and {b}")));
        }
        let parts: Vec<String> = effects.iter().map(|e| self.sentence(e)).collect();
        parts.join(" ")
    }

    /// One effect as a full sentence with a capital and a period.
    fn sentence(&mut self, e: &Effect) -> String {
        self.new_sentence();
        match e {
            Effect::Sequence(es) => {
                let c = self.clause(e);
                let _ = es;
                format!("{}.", capitalize(&c))
            }
            Effect::Conditional { if_, then, else_ } => {
                let cond = self.condition(if_);
                let then_s = self.clause(then);
                match else_ {
                    Some(e) => format!("If {cond}, {then_s}. Otherwise, {}.", self.clause(e)),
                    None => format!("If {cond}, {then_s}."),
                }
            }
            other => {
                let c = self.clause(other);
                format!("{}.", capitalize(&c))
            }
        }
    }

    /// "you control an Elf", "you control two or more creatures".
    fn condition(&mut self, c: &Condition) -> String {
        match c {
            Condition::Controls { player, filter, at_least } => {
                let (subj, second) = self.player_subject(player);
                let control = if second { "control" } else { "controls" };
                if *at_least <= 1 {
                    let n = self.noun(filter, Number::Singular);
                    format!("{subj} {control} {} {n}", article(&n))
                } else {
                    format!(
                        "{subj} {control} {} or more {}",
                        number_word(*at_least),
                        self.noun(filter, Number::Plural)
                    )
                }
            }
        }
    }

    /// One effect as a clause without capital or period, e.g. "draw a card".
    fn clause(&mut self, e: &Effect) -> String {
        match e {
            Effect::DealDamage { amount, to } => {
                let src = self.this();
                let to_s = if self.multi_target(to) {
                    format!("each of {}", self.object(to))
                } else {
                    self.object(to)
                };
                match amount {
                    Amount::PowerOf(Ref::This) => {
                        format!("{src} deals damage equal to its power to {to_s}")
                    }
                    a => format!("{src} deals {} damage to {to_s}", amount_phrase(a)),
                }
            }
            Effect::Destroy { target: Ref::Each(f) } => format!("destroy all {}", self.noun(f, Number::Plural)),
            Effect::Exile { target: Ref::Each(f) } => format!("exile all {}", self.noun(f, Number::Plural)),
            Effect::Destroy { target } => format!("destroy {}", self.object(target)),
            Effect::Exile { target } => format!("exile {}", self.object(target)),
            Effect::Draw { player, count } => {
                let (subj, second) = self.player_subject(player);
                let what = counted(count, "card");
                if second {
                    format!("draw {what}")
                } else {
                    format!("{subj} draws {what}")
                }
            }
            Effect::Discard { player, count, random } => {
                let (subj, second) = self.player_subject(player);
                let what = counted(count, "card");
                let tail = if *random { " at random" } else { "" };
                if second {
                    format!("discard {what}{tail}")
                } else {
                    format!("{subj} discards {what}{tail}")
                }
            }
            Effect::GainLife { player, amount } => {
                let (subj, second) = self.player_subject(player);
                let verb = if second { "gain" } else { "gains" };
                format!("{subj} {verb} {} life", amount_phrase(amount))
            }
            Effect::LoseLife { player, amount } => {
                let (subj, second) = self.player_subject(player);
                let verb = if second { "lose" } else { "loses" };
                format!("{subj} {verb} {} life", amount_phrase(amount))
            }
            Effect::ModifyPt {
                target,
                power,
                toughness,
                keywords,
                until,
            } => {
                let (subj, number) = self.subject(target);
                let gets = if number == Number::Plural { "get" } else { "gets" };
                let gains = if number == Number::Plural { "gain" } else { "gains" };
                let mut s = format!("{subj} {gets} {}/{}", signed(power), signed(toughness));
                if !keywords.is_empty() {
                    s.push_str(&format!(" and {gains} {}", keyword_list(keywords)));
                }
                s.push_str(self.until(until));
                s
            }
            Effect::GrantKeyword { target, keyword, until } => {
                let (subj, number) = self.subject(target);
                let gains = if number == Number::Plural { "gain" } else { "gains" };
                format!("{subj} {gains} {}{}", keyword.word(), self.until(until))
            }
            Effect::CreateToken { spec, count } => {
                let n = match count {
                    Amount::Const(1) => String::new(),
                    Amount::Const(k) => format!("{} ", number_word(*k)),
                    a => format!("{} ", amount_phrase(a)),
                };
                let mut desc = String::new();
                if let Some((p, t)) = spec.pt {
                    desc.push_str(&format!("{p}/{t} "));
                }
                let colors: Vec<&str> = spec.colors.iter().map(|c| c.word()).collect();
                if colors.is_empty() {
                    desc.push_str("colorless ");
                } else {
                    desc.push_str(&colors.join(" and "));
                    desc.push(' ');
                }
                for s in &spec.subtypes {
                    desc.push_str(s);
                    desc.push(' ');
                }
                let types: Vec<&str> = spec.types.iter().map(|t| t.word()).collect();
                desc.push_str(&types.join(" "));
                desc.push_str(" token");
                let plural = !matches!(count, Amount::Const(1));
                if plural {
                    desc.push('s');
                }
                let mut s = if plural {
                    format!("create {n}{desc}")
                } else {
                    format!("create {} {desc}", article(&desc))
                };
                if !spec.keywords.is_empty() {
                    s.push_str(&format!(" with {}", keyword_list(&spec.keywords)));
                }
                s
            }
            Effect::AddCounters { target, kind, count } => {
                let kind_s = match kind {
                    CounterKind::Plus1Plus1 => "+1/+1 counter",
                    CounterKind::Minus1Minus1 => "-1/-1 counter",
                };
                let what = counted(count, kind_s);
                format!("put {what} on {}", self.object(target))
            }
            Effect::AddMana { color, amount } => {
                let sym = match color {
                    Some(c) => format!("{{{}}}", c.symbol()),
                    None => "{C}".into(),
                };
                match amount {
                    Amount::Const(n) => format!("add {}", sym.repeat((*n).max(1) as usize)),
                    Amount::Count(f) => {
                        format!("add {sym} for each {}", self.noun(f, Number::Singular))
                    }
                    a => format!("add {} {sym}", amount_phrase(a)),
                }
            }
            Effect::Tap { target } => format!("tap {}", self.object(target)),
            Effect::Untap { target } => format!("untap {}", self.object(target)),
            Effect::ReturnToHand { target } => {
                format!("return {} to its owner's hand", self.object(target))
            }
            Effect::CounterSpell { target } => format!("counter {}", self.object(target)),
            Effect::Sacrifice { player, filter, count } => {
                let (subj, second) = self.player_subject(player);
                let what = match count {
                    Amount::Const(1) => {
                        let n = self.noun(filter, Number::Singular);
                        format!("{} {n}", article(&n))
                    }
                    Amount::Const(k) => {
                        format!("{} {}", number_word(*k), self.noun(filter, Number::Plural))
                    }
                    a => format!("{} {}", amount_phrase(a), self.noun(filter, Number::Plural)),
                };
                if second {
                    format!("sacrifice {what}")
                } else {
                    format!("{subj} sacrifices {what} of their choice")
                }
            }
            Effect::Mill { player, count } => {
                let (subj, second) = self.player_subject(player);
                let what = counted(count, "card");
                if second {
                    format!("mill {what}")
                } else {
                    format!("{subj} mills {what}")
                }
            }
            Effect::ReturnFromGraveyard { target, to } => {
                let obj = match target {
                    Ref::This => format!("{} from your graveyard", self.this()),
                    other => self.object(other),
                };
                match to {
                    ReturnZone::Hand => format!("return {obj} to your hand"),
                    ReturnZone::Battlefield => format!("return {obj} to the battlefield"),
                }
            }
            Effect::ReturnExiled { target, to } => {
                let obj = match target {
                    Ref::Target(_) | Ref::Named(_) | Ref::Triggering => "that card".to_string(),
                    other => self.object(other),
                };
                match to {
                    ReturnZone::Hand => format!("return {obj} to its owner's hand"),
                    ReturnZone::Battlefield => format!("return {obj} to the battlefield under its owner's control"),
                }
            }
            Effect::Restrict {
                target,
                restriction,
                until,
            } => {
                let (subj, _) = self.subject(target);
                let what = match restriction {
                    Restriction::CantAttack => "can't attack",
                    Restriction::CantBlock => "can't block",
                    Restriction::CantAttackOrBlock => "can't attack or block",
                };
                let when = match until {
                    Duration::EndOfTurn => " this turn",
                };
                format!("{subj} {what}{when}")
            }
            Effect::Delayed { at, effects } => {
                let parts: Vec<String> = effects.iter().map(|e| self.clause(e)).collect();
                let when = match at {
                    DelayedAt::NextEndStep => "at the beginning of the next end step",
                };
                format!("{} {when}", parts.join(", then "))
            }
            Effect::Sequence(es) => {
                let parts: Vec<String> = es.iter().map(|e| self.clause(e)).collect();
                parts.join(", then ")
            }
            Effect::Conditional { .. } => self.sentence(e).trim_end_matches('.').to_string(),
            Effect::May { effect, then, otherwise } => {
                let mut s = format!("you may {}", self.clause(effect));
                for (lead, effects) in [("If you do", then), ("If you don't", otherwise)] {
                    for (i, e) in effects.iter().enumerate() {
                        let c = self.clause(e);
                        if i == 0 {
                            s.push_str(&format!(". {lead}, {c}"));
                        } else {
                            s.push_str(&format!(". {}", capitalize(&c)));
                        }
                    }
                }
                s
            }
            Effect::Unsupported { reason } => format!("[unsupported: {reason}]"),
        }
    }

    fn until(&self, d: &Duration) -> &'static str {
        match d {
            Duration::EndOfTurn => " until end of turn",
        }
    }

    // ----- statics, triggers, abilities -----

    fn static_(&mut self, s: &Static) -> String {
        match s {
            Static::PtBoost {
                filter,
                power,
                toughness,
                keywords,
            } => {
                let (subj, number) = self.static_subject(filter);
                let get = if number == Number::Plural { "get" } else { "gets" };
                let have = if number == Number::Plural { "have" } else { "has" };
                let mut out = format!("{subj} {get} {}/{}", signed(power), signed(toughness));
                if !keywords.is_empty() {
                    out.push_str(&format!(" and {have} {}", keyword_list(keywords)));
                }
                out.push('.');
                capitalize(&out)
            }
            Static::GrantKeyword { filter, keyword } => {
                let (subj, number) = self.static_subject(filter);
                let have = if number == Number::Plural { "have" } else { "has" };
                capitalize(&format!("{subj} {have} {}.", keyword.word()))
            }
            Static::CostReduction { filter, amount } => {
                let subj = self.noun(filter, Number::Plural);
                capitalize(&format!(
                    "{subj} you cast cost {} less to cast.",
                    match amount {
                        Amount::Const(n) => format!("{{{n}}}"),
                        a => amount_phrase(a),
                    }
                ))
            }
        }
    }

    /// "Enchanted creature" is singular; everything else a static applies to is plural.
    fn static_subject(&self, f: &Filter) -> (String, Number) {
        if matches!(f, Filter::Attached) {
            (self.noun(f, Number::Singular), Number::Singular)
        } else {
            (self.noun(f, Number::Plural), Number::Plural)
        }
    }

    fn trigger(&mut self, t: &Trigger) -> String {
        let head = self.event_head(&t.event);
        let cond = match &t.condition {
            Some(c) => format!(", if {}", self.condition(c)),
            None => String::new(),
        };
        let body = self.trigger_body(&t.effects);
        format!("{head}{cond}, {body}")
    }

    /// "When ~ enters", "Whenever another creature you control dies", ...
    fn event_head(&self, e: &EventPattern) -> String {
        if let Some((word, verb)) = this_verb(e) {
            return format!("{word} ~ {verb}");
        }
        match e {
            EventPattern::Enters(f) => format!("Whenever {} enters", self.some_noun(f)),
            EventPattern::Dies(f) => format!("Whenever {} dies", self.some_noun(f)),
            EventPattern::Upkeep(whose) => format!("At the beginning of {}", self.step_owner(whose, "upkeep")),
            EventPattern::EndStep(whose) => format!("At the beginning of {}", self.step_owner(whose, "end step")),
            EventPattern::BeginCombat(whose) => match whose {
                PlayerRef::You => "At the beginning of combat on your turn".into(),
                PlayerRef::EachPlayer => "At the beginning of each combat".into(),
                other => format!("At the beginning of combat on {}'s turn", self.player_object(other)),
            },
            EventPattern::Cast { who, filter } => {
                let n = self.noun(filter, Number::Singular);
                format!("Whenever {} {} {n}", self.event_player(who, "cast", "casts"), article(&n))
            }
            EventPattern::GainsLife(whose) => format!("Whenever {} life", self.event_player(whose, "gain", "gains")),
            EventPattern::Discards(whose) => format!("Whenever {} a card", self.event_player(whose, "discard", "discards")),
            EventPattern::Any(es) => {
                let verbs: Vec<(&str, &str)> = es.iter().filter_map(this_verb).collect();
                if verbs.len() == es.len() && !verbs.is_empty() {
                    let word = if verbs.iter().any(|(w, _)| *w == "Whenever") {
                        "Whenever"
                    } else {
                        "When"
                    };
                    let list: Vec<&str> = verbs.iter().map(|(_, v)| *v).collect();
                    format!("{word} ~ {}", list.join(" or "))
                } else {
                    let heads: Vec<String> = es.iter().map(|e| self.event_head(e)).collect();
                    heads.join(" or ")
                }
            }
            _ => unreachable!("this_verb covers the This* events"),
        }
    }

    /// "another creature you control" / "a creature".
    fn some_noun(&self, f: &Filter) -> String {
        if has_other(f) {
            format!("another {}", self.noun(&without_other(f), Number::Singular))
        } else {
            let n = self.noun(f, Number::Singular);
            format!("{} {n}", article(&n))
        }
    }

    /// The subject of an event head: "you cast", "an opponent casts", "a player casts".
    fn event_player(&self, p: &PlayerRef, second: &str, third: &str) -> String {
        match p {
            PlayerRef::You => format!("you {second}"),
            PlayerRef::EachOpponent => format!("an opponent {third}"),
            PlayerRef::EachPlayer => format!("a player {third}"),
            other => format!("{} {third}", self.player_object(other)),
        }
    }

    fn step_owner(&self, whose: &PlayerRef, step: &str) -> String {
        match whose {
            PlayerRef::You => format!("your {step}"),
            PlayerRef::EachOpponent => format!("each opponent's {step}"),
            PlayerRef::EachPlayer => format!("each {step}"),
            other => format!("{}'s {step}", self.player_object(other)),
        }
    }

    /// Effects after a trigger head: the first clause lowercase, later ones as sentences.
    fn trigger_body(&mut self, effects: &[Effect]) -> String {
        if let [Effect::DealDamage { .. }, Effect::GainLife {
            player: PlayerRef::You, ..
        }] = effects
        {
            let a = self.clause(&effects[0]);
            let b = self.clause(&effects[1]);
            return format!("{a} and {b}.");
        }
        let mut out = String::new();
        for (i, e) in effects.iter().enumerate() {
            self.new_sentence();
            let c = self.clause(e);
            if i == 0 {
                out.push_str(&c);
                out.push('.');
            } else {
                out.push(' ');
                out.push_str(&capitalize(&c));
                out.push('.');
            }
        }
        out
    }

    fn ability(&mut self, a: &Ability) -> String {
        let costs: Vec<String> = a
            .cost
            .iter()
            .map(|c| match c {
                Cost::Mana(m) => m.to_string(),
                Cost::Tap => "{T}".into(),
                Cost::SacrificeThis => "Sacrifice ~".into(),
                Cost::Sacrifice(f) if has_other(f) => format!("Sacrifice another {}", self.noun(&without_other(f), Number::Singular)),
                Cost::Sacrifice(f) => {
                    let n = self.noun(f, Number::Singular);
                    format!("Sacrifice {} {n}", article(&n))
                }
                Cost::PayLife(n) => format!("Pay {n} life"),
                Cost::Discard(n) => format!("Discard {}", counted(&Amount::Const(*n), "card")),
            })
            .collect();
        // "Sacrifice ~: It deals ..." — the cost already named the card.
        if a.cost.contains(&Cost::SacrificeThis) {
            self.this_mentioned = true;
        }
        let mut body = String::new();
        for (i, e) in a.effects.iter().enumerate() {
            self.new_sentence();
            let c = self.clause(e);
            if i > 0 {
                body.push(' ');
            }
            body.push_str(&capitalize(&c));
            body.push('.');
        }
        let mut s = format!("{}: {body}", costs.join(", "));
        if a.sorcery_speed {
            s.push_str(" Activate only as a sorcery.");
        }
        s
    }
}

fn mentions_graveyard(f: &Filter) -> bool {
    match f {
        Filter::InGraveyard(_) => true,
        Filter::And(fs) | Filter::Or(fs) => fs.iter().any(mentions_graveyard),
        _ => false,
    }
}

fn mentions_permanent(f: &Filter) -> bool {
    match f {
        Filter::Permanent => true,
        Filter::And(fs) | Filter::Or(fs) => fs.iter().any(mentions_permanent),
        _ => false,
    }
}

/// The filter without its `Other`, for "another <noun>" phrasings.
fn without_other(f: &Filter) -> Filter {
    match f {
        Filter::And(fs) => Filter::And(fs.iter().filter(|x| !matches!(x, Filter::Other)).cloned().collect()),
        other => other.clone(),
    }
}

fn has_other(f: &Filter) -> bool {
    match f {
        Filter::Other => true,
        Filter::And(fs) => fs.iter().any(has_other),
        _ => false,
    }
}

fn plural(head: &str) -> String {
    if let Some(rest) = head.strip_suffix(" or enchantment") {
        return format!("{}s or enchantments", rest);
    }
    match head {
        "any target" => "any targets".into(),
        "Elf" => "Elves".into(),
        "Dwarf" => "Dwarves".into(),
        "Wolf" => "Wolves".into(),
        "Sphinx" => "Sphinxes".into(),
        "Fox" => "Foxes".into(),
        "Mouse" => "Mice".into(),
        h => format!("{h}s"),
    }
}

/// Normalise Oracle text for comparison: lowercase, reminder text removed,
/// the card's own name replaced by `~`, line breaks and commas treated as
/// spaces, whitespace collapsed.
pub fn normalise(text: &str, name: &str) -> String {
    let mut s = text.replace(name, "~");
    // Current Oracle text refers to a permanent by its type ("this creature")
    // rather than by name; both mean the card itself.
    for word in [
        "creature",
        "artifact",
        "enchantment",
        "land",
        "permanent",
        "card",
        "Aura",
        "Equipment",
    ] {
        s = s.replace(&format!("this {word}"), "~").replace(&format!("This {word}"), "~");
    }
    // Strip reminder text in parentheses.
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for ch in s.drain(..) {
        match ch {
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let lowered = out.to_lowercase();
    let spaced: String = lowered
        .chars()
        .map(|c| match c {
            '\n' | ',' | '\u{2014}' | '\u{2019}' => ' ',
            '\'' => ' ',
            c => c,
        })
        .collect();
    let mut result = String::new();
    for word in spaced.split_whitespace() {
        // A sentence-final period on a keyword line is optional in Oracle text.
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(word);
    }
    result
}

/// Whether the renderer reproduces the card's Oracle text.
pub fn round_trips(card: &Card) -> Result<(), (String, String)> {
    let want = normalise(&card.text, &card.name);
    let got = normalise(&render(card), &card.name);
    if want == got {
        Ok(())
    } else {
        Err((want, got))
    }
}

pub fn card_type_word(t: CardType) -> &'static str {
    t.word()
}

/// The trigger word and verb phrase of an event about the card itself.
fn this_verb(e: &EventPattern) -> Option<(&'static str, &'static str)> {
    Some(match e {
        EventPattern::ThisEnters => ("When", "enters"),
        EventPattern::ThisDies => ("When", "dies"),
        EventPattern::ThisAttacks => ("Whenever", "attacks"),
        EventPattern::ThisBlocks => ("Whenever", "blocks"),
        EventPattern::ThisBecomesBlocked => ("Whenever", "becomes blocked"),
        EventPattern::ThisDealsCombatDamageToPlayer => ("Whenever", "deals combat damage to a player"),
        EventPattern::ThisBecomesTapped => ("Whenever", "becomes tapped"),
        _ => return None,
    })
}
