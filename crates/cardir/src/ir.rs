//! The card IR (§4.1): a card is a composition of a fixed vocabulary of
//! primitives and nothing else. The engine interprets these; the renderer
//! turns them back into Oracle text; the validator checks what the schema
//! cannot. There is no singular "opponent": Oracle text is already written
//! for N players and the IR mirrors it.

use crate::types::{CardType, Color, Keyword, ManaCost, Supertype};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Card {
    pub name: String,
    #[serde(default)]
    pub cost: ManaCost,
    pub types: Vec<CardType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supertypes: Vec<Supertype>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtypes: Vec<String>,
    /// Present iff the card is a creature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pt: Option<(i32, i32)>,
    /// Oracle text, for display and the round-trip check.
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<Keyword>,
    /// Instants and sorceries: what happens on resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spell: Option<Spell>,
    /// Auras: what the aura may enchant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enchant: Option<Filter>,
    /// Equipment: the equip cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equip: Option<ManaCost>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statics: Vec<Static>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activated: Vec<Ability>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Spell {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Filter>,
    /// Empty for a modal spell, whose effects live in its modes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<Effect>,
    /// "Choose one —": the caster picks modes as they cast.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<Mode>,
    #[serde(default, skip_serializing_if = "ModeChoice::is_default")]
    pub choose: ModeChoice,
}

/// One bullet of a modal spell, with its own targets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Mode {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Filter>,
    pub effects: Vec<Effect>,
}

/// How many modes a modal spell's caster picks.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ModeChoice {
    #[default]
    One,
    Two,
    OneOrBoth,
}

impl ModeChoice {
    fn is_default(&self) -> bool {
        *self == ModeChoice::One
    }

    pub fn word(self) -> &'static str {
        match self {
            ModeChoice::One => "one",
            ModeChoice::Two => "two",
            ModeChoice::OneOrBoth => "one or both",
        }
    }

    /// The fewest and most modes the caster may pick.
    pub fn bounds(self) -> (usize, usize) {
        match self {
            ModeChoice::One => (1, 1),
            ModeChoice::Two => (2, 2),
            ModeChoice::OneOrBoth => (1, 2),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ability {
    pub cost: Vec<Cost>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Filter>,
    pub effects: Vec<Effect>,
    /// Only while you could cast a sorcery.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sorcery_speed: bool,
    /// Activated from the graveyard rather than the battlefield
    /// ("{2}{B}: Return this card from your graveyard to your hand").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_graveyard: bool,
}

impl Ability {
    /// A mana ability: costs only a tap and only adds mana. Paid through the
    /// mana solver rather than offered as an action.
    pub fn is_mana_ability(&self) -> bool {
        self.cost == [Cost::Tap]
            && self.targets.is_empty()
            && !self.effects.is_empty()
            && self.effects.iter().all(|e| matches!(e, Effect::AddMana { .. }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Cost {
    Mana(ManaCost),
    Tap,
    SacrificeThis,
    Sacrifice(Filter),
    PayLife(i32),
    Discard(i32),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Effect {
    DealDamage {
        amount: Amount,
        to: Ref,
    },
    Destroy {
        target: Ref,
    },
    /// "exile target creature", or with `until`, "exile target creature
    /// until ~ leaves the battlefield": the card comes back on its own when
    /// the exiling permanent leaves.
    Exile {
        target: Ref,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<Duration>,
    },
    Draw {
        player: PlayerRef,
        count: Amount,
    },
    Discard {
        player: PlayerRef,
        count: Amount,
        #[serde(default)]
        random: bool,
    },
    GainLife {
        player: PlayerRef,
        amount: Amount,
    },
    LoseLife {
        player: PlayerRef,
        amount: Amount,
    },
    /// "gets +N/+N [and gains K] until end of turn"
    ModifyPt {
        target: Ref,
        power: Amount,
        toughness: Amount,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keywords: Vec<Keyword>,
        until: Duration,
    },
    GrantKeyword {
        target: Ref,
        keyword: Keyword,
        until: Duration,
    },
    CreateToken {
        spec: TokenSpec,
        count: Amount,
    },
    AddCounters {
        target: Ref,
        kind: CounterKind,
        count: Amount,
    },
    /// `None` colour is colourless `{C}`.
    AddMana {
        color: Option<Color>,
        amount: Amount,
    },
    Tap {
        target: Ref,
    },
    Untap {
        target: Ref,
    },
    ReturnToHand {
        target: Ref,
    },
    CounterSpell {
        target: Ref,
    },
    Sacrifice {
        player: PlayerRef,
        filter: Filter,
        count: Amount,
    },
    /// "target player mills three cards": library top to graveyard.
    Mill {
        player: PlayerRef,
        count: Amount,
    },
    /// "return target creature card from your graveyard to your hand / to the battlefield".
    ReturnFromGraveyard {
        target: Ref,
        to: ReturnZone,
    },
    Sequence(Vec<Effect>),
    Conditional {
        if_: Condition,
        then: Box<Effect>,
        #[serde(default)]
        else_: Option<Box<Effect>>,
    },
    /// "return that card to the battlefield under its owner's control" /
    /// "to its owner's hand": a card in exile comes back.
    ReturnExiled {
        target: Ref,
        to: ReturnZone,
    },
    /// "It doesn't untap during its controller's next untap step."
    SkipUntap {
        target: Ref,
    },
    /// "[target] can't block this turn".
    Restrict {
        target: Ref,
        restriction: Restriction,
        until: Duration,
    },
    /// "[effects] at the beginning of the next end step": a delayed trigger
    /// that remembers this resolution's targets and bindings.
    Delayed {
        at: DelayedAt,
        effects: Vec<Effect>,
    },
    /// "[Who] may [effect]. If they do, [then]. If they don't, [otherwise]."
    /// `who` decides at resolution (each of them in turn for "each opponent").
    /// With `unless`, rendered the other way round: "[otherwise] unless
    /// [who] pays [cost]", for a `PayMana` or `PayLife` effect.
    May {
        #[serde(default, skip_serializing_if = "PlayerRef::is_you")]
        who: PlayerRef,
        effect: Box<Effect>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        then: Vec<Effect>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        otherwise: Vec<Effect>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unless: bool,
    },
    /// "pay {3}": the player spends mana, if they can.
    PayMana {
        player: PlayerRef,
        cost: ManaCost,
    },
    /// "pay 2 life": the player loses that much life, if they have it.
    PayLife {
        player: PlayerRef,
        amount: Amount,
    },
    /// Emitted by the ingestion tool for text the vocabulary cannot express.
    /// Never valid on a committed card.
    Unsupported {
        reason: String,
    },
}

/// What a creature is stopped from doing.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Restriction {
    CantAttack,
    CantBlock,
    CantAttackOrBlock,
}

/// When a delayed trigger fires.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum DelayedAt {
    /// The beginning of the next end step, whoever's turn it is.
    NextEndStep,
}

/// Where a card returned from a graveyard or exile goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ReturnZone {
    /// Its owner's hand.
    Hand,
    /// The battlefield under the controller's control.
    Battlefield,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Amount {
    Const(i32),
    Count(Filter),
    LifeOf(PlayerRef),
    PowerOf(Ref),
    /// The value announced for the spell's or ability's `{X}`.
    X,
    /// The negative of an amount ("-X/-X").
    Neg(Box<Amount>),
}

/// Something an effect acts on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Ref {
    /// The n-th target of the spell, ability, or trigger.
    Target(u8),
    /// The card itself.
    This,
    /// The object that caused the trigger (the creature that died, ...).
    Triggering,
    /// Every object matching the filter.
    Each(Filter),
    Player(PlayerRef),
    /// The permanent this aura or equipment is attached to.
    Attached,
    /// Objects a player picks as the effect resolves, without targeting:
    /// "a creature you control", "up to two permanents you control". Allowed
    /// only as the direct target of an effect. With `bind`, later effects may
    /// refer to what was picked as `Named`.
    Chosen {
        who: PlayerRef,
        filter: Filter,
        count: Quantity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bind: Option<String>,
    },
    /// What an earlier `Chosen` with this `bind` picked ("it", "that creature").
    Named(String),
    /// "the exiled card": whatever this permanent's abilities exiled while it
    /// was on the battlefield (linked abilities, rule 607).
    ExiledWithThis,
}

/// How many things a `Chosen` picks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Quantity {
    Exactly(i32),
    UpTo(i32),
    AnyNumber,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PlayerRef {
    #[default]
    You,
    TargetPlayer(u8),
    TargetOpponent(u8),
    EachOpponent,
    EachPlayer,
    /// The player who caused the trigger.
    Triggering,
    Controller(Box<Ref>),
    Owner(Box<Ref>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Filter {
    /// How many things one "target" word picks: "up to two target
    /// creatures", "any number of target Elves". Only at the top of a
    /// target spec; a bare filter means exactly one.
    Targets(Quantity, Box<Filter>),
    /// "any target": a creature, player, or planeswalker.
    Any,
    Creature,
    Land,
    Artifact,
    Enchantment,
    Instant,
    Sorcery,
    Permanent,
    Player,
    Opponent,
    Spell,
    /// Objects other than this one.
    Other,
    /// This card itself ("~ gets +1/+2 as long as ...").
    This,
    /// The permanent this aura or equipment is attached to.
    Attached,
    /// A card in the named player's graveyard ("creature card from your graveyard").
    InGraveyard(PlayerRef),
    Token,
    Subtype(String),
    Color(Color),
    ControlledBy(PlayerRef),
    Tapped,
    Untapped,
    Attacking,
    Blocking,
    PowerAtLeast(i32),
    PowerAtMost(i32),
    ManaValueAtMost(i32),
    HasKeyword(Keyword),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

/// A triggered ability: when `event` happens (and `condition` holds, checked
/// when it triggers and again as it resolves, rule 603.4), choose `targets`
/// and run `effects`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Trigger {
    pub event: EventPattern,
    /// An intervening "if": "Whenever ~ attacks, if you control an Elf, ...".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<Filter>,
    pub effects: Vec<Effect>,
}

/// What a trigger listens for. The `This*` events are about the card
/// itself; the others watch the whole game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EventPattern {
    /// "When ~ enters"
    ThisEnters,
    /// "When ~ dies"
    ThisDies,
    /// "When ~ leaves the battlefield": any zone change off the battlefield.
    ThisLeaves,
    /// "Whenever ~ attacks"
    ThisAttacks,
    /// "Whenever ~ blocks"
    ThisBlocks,
    /// "Whenever ~ becomes blocked"
    ThisBecomesBlocked,
    /// "Whenever ~ deals combat damage to a player"
    ThisDealsCombatDamageToPlayer,
    /// "Whenever ~ becomes tapped"
    ThisBecomesTapped,
    /// "Whenever another creature you control enters": a permanent matching
    /// the filter (`Other` excludes this card) enters the battlefield.
    Enters(Filter),
    /// "Whenever another Zombie you control dies".
    Dies(Filter),
    /// "At the beginning of your upkeep"
    Upkeep(PlayerRef),
    /// "At the beginning of your end step"
    EndStep(PlayerRef),
    /// "At the beginning of combat on your turn"
    BeginCombat(PlayerRef),
    /// "Whenever you cast a noncreature spell"
    Cast { who: PlayerRef, filter: Filter },
    /// "Whenever you gain life"
    GainsLife(PlayerRef),
    /// "Whenever an opponent discards a card" (once per card)
    Discards(PlayerRef),
    /// Any of these: "When ~ enters or dies", "Whenever ~ attacks or blocks".
    Any(Vec<EventPattern>),
}

impl EventPattern {
    /// Whether the head names the card ("When ~ enters"), so the body says "it".
    pub fn names_this(&self) -> bool {
        match self {
            EventPattern::ThisEnters
            | EventPattern::ThisDies
            | EventPattern::ThisLeaves
            | EventPattern::ThisAttacks
            | EventPattern::ThisBlocks
            | EventPattern::ThisBecomesBlocked
            | EventPattern::ThisDealsCombatDamageToPlayer
            | EventPattern::ThisBecomesTapped => true,
            EventPattern::Any(es) => es.iter().all(EventPattern::names_this),
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Static {
    /// "[filter] get +N/+N [and have K]."
    PtBoost {
        filter: Filter,
        power: Amount,
        toughness: Amount,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keywords: Vec<Keyword>,
    },
    /// "[filter] have K."
    GrantKeyword {
        filter: Filter,
        keyword: Keyword,
    },
    CostReduction {
        filter: Filter,
        amount: Amount,
    },
    /// "~ gets +1/+2 as long as you control a Forest": the static applies
    /// only while the condition holds.
    AsLongAs {
        condition: Condition,
        static_: Box<Static>,
        /// Rendering only: "As long as ..., ~ gets ..." rather than trailing.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        leading: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Duration {
    EndOfTurn,
    /// "until your next turn": ends as the controller's next turn begins.
    UntilYourNextTurn,
    /// "until ~ leaves the battlefield" / "for as long as ~ remains on the
    /// battlefield": ends when the permanent that made the effect leaves.
    UntilThisLeaves,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TokenSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<Color>,
    pub types: Vec<CardType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtypes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pt: Option<(i32, i32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<Keyword>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CounterKind {
    Plus1Plus1,
    Minus1Minus1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Condition {
    /// The player controls at least `at_least` objects matching the filter.
    Controls { player: PlayerRef, filter: Filter, at_least: i32 },
    /// "you have 30 or more life"
    LifeAtLeast { player: PlayerRef, amount: i32 },
    /// "you have 10 or less life"
    LifeAtMost { player: PlayerRef, amount: i32 },
}

impl PlayerRef {
    pub fn is_you(&self) -> bool {
        *self == PlayerRef::You
    }
}

impl Card {
    pub fn is_creature(&self) -> bool {
        self.types.contains(&CardType::Creature)
    }

    pub fn is_land(&self) -> bool {
        self.types.contains(&CardType::Land)
    }

    pub fn is_permanent(&self) -> bool {
        self.types.iter().any(|t| t.is_permanent())
    }

    pub fn is_basic(&self) -> bool {
        self.supertypes.contains(&Supertype::Basic)
    }

    pub fn is_aura(&self) -> bool {
        self.enchant.is_some()
    }

    pub fn is_equipment(&self) -> bool {
        self.equip.is_some()
    }

    /// Instants, and anything with flash.
    pub fn has_instant_speed(&self) -> bool {
        self.types.contains(&CardType::Instant) || self.keywords.contains(&Keyword::Flash)
    }

    /// Whether the spell is cast in steps (modes and each target spec chosen
    /// after paying) rather than with every target enumerated up front.
    pub fn is_modal(&self) -> bool {
        self.spell.as_ref().map(|s| !s.modes.is_empty()).unwrap_or(false)
    }

    pub fn mana_abilities(&self) -> impl Iterator<Item = &Ability> {
        self.activated.iter().filter(|a| a.is_mana_ability())
    }

    /// The colours of the card's mana cost.
    pub fn colors(&self) -> Vec<Color> {
        self.cost.colors()
    }
}

impl Filter {
    /// A target spec's filter and how many it picks: `(filter, min, max)`,
    /// `max` `None` for any number.
    pub fn spec_bounds(&self) -> (&Filter, usize, Option<usize>) {
        match self {
            Filter::Targets(Quantity::Exactly(n), f) => (f, (*n).max(0) as usize, Some((*n).max(0) as usize)),
            Filter::Targets(Quantity::UpTo(n), f) => (f, 0, Some((*n).max(0) as usize)),
            Filter::Targets(Quantity::AnyNumber, f) => (f, 0, None),
            f => (f, 1, Some(1)),
        }
    }

    /// Whether the spec picks a caster-chosen number of targets.
    pub fn is_variable(&self) -> bool {
        matches!(self, Filter::Targets(Quantity::UpTo(_) | Quantity::AnyNumber, _))
    }

    /// Whether the spec may pick more than one target.
    pub fn is_multi(&self) -> bool {
        !matches!(self.spec_bounds(), (_, _, Some(1)))
    }
}
