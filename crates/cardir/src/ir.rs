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
    pub effects: Vec<Effect>,
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
    Exile {
        target: Ref,
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
    Sequence(Vec<Effect>),
    Conditional {
        if_: Condition,
        then: Box<Effect>,
        #[serde(default)]
        else_: Option<Box<Effect>>,
    },
    /// Emitted by the ingestion tool for text the vocabulary cannot express.
    /// Never valid on a committed card.
    Unsupported {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Amount {
    Const(i32),
    Count(Filter),
    LifeOf(PlayerRef),
    PowerOf(Ref),
    X,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum PlayerRef {
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
    /// "any target": a creature, player, or planeswalker.
    Any,
    Creature,
    Land,
    Artifact,
    Enchantment,
    Permanent,
    Player,
    Opponent,
    Spell,
    /// Objects other than this one.
    Other,
    /// The permanent this aura or equipment is attached to.
    Attached,
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
    HasKeyword(Keyword),
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Trigger {
    Etb {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
    Dies {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
    Attacks {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
    CombatDamageToPlayer {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
    Upkeep {
        whose: PlayerRef,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
    EndStep {
        whose: PlayerRef,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
    BecomesTapped {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targets: Vec<Filter>,
        effects: Vec<Effect>,
    },
}

impl Trigger {
    pub fn targets(&self) -> &[Filter] {
        match self {
            Trigger::Etb { targets, .. }
            | Trigger::Dies { targets, .. }
            | Trigger::Attacks { targets, .. }
            | Trigger::CombatDamageToPlayer { targets, .. }
            | Trigger::Upkeep { targets, .. }
            | Trigger::EndStep { targets, .. }
            | Trigger::BecomesTapped { targets, .. } => targets,
        }
    }

    pub fn effects(&self) -> &[Effect] {
        match self {
            Trigger::Etb { effects, .. }
            | Trigger::Dies { effects, .. }
            | Trigger::Attacks { effects, .. }
            | Trigger::CombatDamageToPlayer { effects, .. }
            | Trigger::Upkeep { effects, .. }
            | Trigger::EndStep { effects, .. }
            | Trigger::BecomesTapped { effects, .. } => effects,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Duration {
    EndOfTurn,
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

    pub fn mana_abilities(&self) -> impl Iterator<Item = &Ability> {
        self.activated.iter().filter(|a| a.is_mana_ability())
    }

    /// The colours of the card's mana cost.
    pub fn colors(&self) -> Vec<Color> {
        self.cost.colors()
    }
}
