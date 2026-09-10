//! The manaline card IR (docs/SPEC.md §4): schema types, a RON loader, a
//! validator, and the English renderer with its round-trip check.
//!
//! Compiles without tokio or any networking dependency.

pub mod ir;
pub mod render;
pub mod types;
pub mod validate;

pub use ir::*;
pub use render::{normalise, render, render_ability, render_clause, render_mode, render_spell, render_trigger, round_trips};
pub use types::{CardType, Color, Keyword, ManaCost, Supertype};
pub use validate::{validate, ValidationError};

/// RON options for card files: `Some(...)` may be omitted around optional
/// fields, so `pt: (2, 2)` and `spell: Spell(...)` read naturally.
pub fn ron_options() -> ron::Options {
    ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
}

/// Parse one card from RON text and validate it.
pub fn load(text: &str) -> Result<Card, String> {
    let card: Card = ron_options().from_str(text).map_err(|e| e.to_string())?;
    validate(&card).map_err(|e| e.to_string())?;
    Ok(card)
}

/// Serialize a card as RON, for tools that write card files.
pub fn to_ron(card: &Card) -> String {
    let pretty = ron::ser::PrettyConfig::new().struct_names(true).depth_limit(6);
    ron_options().to_string_pretty(card, pretty).expect("card serializes")
}

/// The JSON Schema for a card, handed to the ingestion model as its contract.
pub fn json_schema() -> schemars::Schema {
    schemars::schema_for!(Card)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHTNING_STRIKE: &str = r#"
Card(
    name: "Lightning Strike",
    cost: "{1}{R}",
    types: [Instant],
    text: "Lightning Strike deals 3 damage to any target.",
    spell: Spell(
        targets: [Any],
        effects: [DealDamage(amount: Const(3), to: Target(0))],
    ),
)"#;

    const ARCHDRUID: &str = r#"
Card(
    name: "Elvish Archdruid",
    cost: "{1}{G}{G}",
    types: [Creature], subtypes: ["Elf", "Druid"], pt: (2, 2),
    text: "Other Elf creatures you control get +1/+1.\n{T}: Add {G} for each Elf you control.",
    statics: [
        PtBoost(
            filter: And([Other, Subtype("Elf"), Creature, ControlledBy(You)]),
            power: Const(1), toughness: Const(1),
        ),
    ],
    activated: [
        Ability(
            cost: [Tap],
            effects: [AddMana(color: Some(Green), amount: Count(And([Subtype("Elf"), ControlledBy(You)])))],
        ),
    ],
)"#;

    #[test]
    fn loads_and_round_trips_the_spec_examples() {
        let strike = load(LIGHTNING_STRIKE).unwrap();
        assert_eq!(
            render(&strike),
            "Lightning Strike deals 3 damage to any target.".replace("Lightning Strike", "~")
        );
        round_trips(&strike).unwrap();
        let druid = load(ARCHDRUID).unwrap();
        assert_eq!(
            render(&druid),
            "Other Elf creatures you control get +1/+1.\n{T}: Add {G} for each Elf you control."
        );
        round_trips(&druid).unwrap();
        assert!(druid.activated[0].is_mana_ability());
    }

    #[test]
    fn validator_catches_bad_targets_and_shapes() {
        let bad = r#"Card(name: "X", cost: "{R}", types: [Instant], text: "", spell: Spell(targets: [Any], effects: [DealDamage(amount: Const(3), to: Target(1))]))"#;
        let e = load(bad).unwrap_err();
        assert!(e.contains("Target(1)"), "{e}");
        let bad = r#"Card(name: "Y", cost: "{R}", types: [Creature], text: "")"#;
        assert!(load(bad).unwrap_err().contains("P/T"));
        let bad = r#"Card(name: "Z", cost: "{R}", types: [Sorcery], text: "", spell: Spell(effects: [Unsupported(reason: "copy")]))"#;
        assert!(load(bad).unwrap_err().contains("unsupported"));
    }

    #[test]
    fn chosen_named_and_may_render_like_oracle() {
        let skyfisher = r#"Card(name: "Kor Skyfisher", cost: "{1}{W}", types: [Creature], subtypes: ["Kor", "Soldier"], pt: (2, 3), text: "Flying\nWhen this creature enters, return a permanent you control to its owner's hand.", keywords: [Flying], triggers: [Trigger(event: ThisEnters, effects: [ReturnToHand(target: Chosen(who: You, filter: And([Permanent, ControlledBy(You)]), count: Exactly(1)))])])"#;
        round_trips(&load(skyfisher).unwrap()).unwrap();
        let cull = r#"Card(name: "Cull", cost: "{W}", types: [Sorcery], text: "Exile up to two creatures you control.", spell: Spell(effects: [Exile(target: Chosen(who: You, filter: And([Creature, ControlledBy(You)]), count: UpTo(2)))]))"#;
        round_trips(&load(cull).unwrap()).unwrap();
        let any = r#"Card(name: "Rally", cost: "{W}", types: [Instant], text: "Untap any number of creatures you control.", spell: Spell(effects: [Untap(target: Chosen(who: You, filter: And([Creature, ControlledBy(You)]), count: AnyNumber))]))"#;
        round_trips(&load(any).unwrap()).unwrap();
        let it = r#"Card(name: "Rest", cost: "{G}", types: [Instant], text: "Tap a creature you control, then untap it. Untap that creature.", spell: Spell(effects: [Sequence([Tap(target: Chosen(who: You, filter: And([Creature, ControlledBy(You)]), count: Exactly(1), bind: "c")), Untap(target: Named("c"))]), Untap(target: Named("c"))]))"#;
        round_trips(&load(it).unwrap()).unwrap();
        let may = r#"Card(name: "Reap", cost: "{B}", types: [Sorcery], text: "You may sacrifice a creature. If you do, draw two cards. If you don't, you lose 2 life.", spell: Spell(effects: [May(effect: Sacrifice(player: You, filter: Creature, count: Const(1)), then: [Draw(player: You, count: Const(2))], otherwise: [LoseLife(player: You, amount: Const(2))])]))"#;
        round_trips(&load(may).unwrap()).unwrap();
        let digger = r#"Card(name: "Gravedigger", cost: "{3}{B}", types: [Creature], subtypes: ["Zombie"], pt: (2, 2), text: "When this creature enters, you may return target creature card from your graveyard to your hand.", triggers: [Trigger(event: ThisEnters, targets: [And([Creature, InGraveyard(You)])], effects: [May(effect: ReturnFromGraveyard(target: Target(0), to: Hand))])])"#;
        round_trips(&load(digger).unwrap()).unwrap();
    }

    #[test]
    fn event_patterns_and_intervening_ifs_render_like_oracle() {
        let cases = [
            r#"Card(name: "A", cost: "{G}", types: [Creature], pt: (2, 2), text: "Whenever this creature attacks, if you control an Elf, it gets +2/+2 until end of turn.", triggers: [Trigger(event: ThisAttacks, condition: Controls(player: You, filter: Subtype("Elf"), at_least: 1), effects: [ModifyPt(target: This, power: Const(2), toughness: Const(2), until: EndOfTurn)])])"#,
            r#"Card(name: "B", cost: "{R}", types: [Creature], pt: (2, 2), text: "Whenever this creature attacks or blocks, it gets +1/+1 until end of turn.", triggers: [Trigger(event: Any([ThisAttacks, ThisBlocks]), effects: [ModifyPt(target: This, power: Const(1), toughness: Const(1), until: EndOfTurn)])])"#,
            r#"Card(name: "C", cost: "{R}", types: [Enchantment], text: "At the beginning of combat on your turn, target creature gets +1/+0 until end of turn.", triggers: [Trigger(event: BeginCombat(You), targets: [Creature], effects: [ModifyPt(target: Target(0), power: Const(1), toughness: Const(0), until: EndOfTurn)])])"#,
            r#"Card(name: "D", cost: "{B}", types: [Enchantment], text: "At the beginning of your upkeep, you lose 1 life.", triggers: [Trigger(event: Upkeep(You), effects: [LoseLife(player: You, amount: Const(1))])])"#,
            r#"Card(name: "E", cost: "{W}", types: [Creature], pt: (1, 1), text: "Whenever another creature you control enters, you gain 1 life.", triggers: [Trigger(event: Enters(And([Other, Creature, ControlledBy(You)])), effects: [GainLife(player: You, amount: Const(1))])])"#,
            r#"Card(name: "F", cost: "{U}", types: [Creature], pt: (1, 1), text: "Whenever you cast a noncreature spell, draw a card.", triggers: [Trigger(event: Cast(who: You, filter: And([Spell, Not(Creature)])), effects: [Draw(player: You, count: Const(1))])])"#,
            r#"Card(name: "G", cost: "{B}", types: [Enchantment], text: "Whenever a player discards a card, you gain 1 life.", triggers: [Trigger(event: Discards(EachPlayer), effects: [GainLife(player: You, amount: Const(1))])])"#,
            r#"Card(name: "H", cost: "{W}", types: [Instant], text: "Exile target creature. Return that card to its owner's hand at the beginning of the next end step.", spell: Spell(targets: [Creature], effects: [Exile(target: Target(0)), Delayed(at: NextEndStep, effects: [ReturnExiled(target: Target(0), to: Hand)])]))"#,
            r#"Card(name: "I", cost: "{R}", types: [Instant], text: "Target creature can't attack or block this turn.", spell: Spell(targets: [Creature], effects: [Restrict(target: Target(0), restriction: CantAttackOrBlock, until: EndOfTurn)]))"#,
        ];
        for text in cases {
            let card = load(text).unwrap_or_else(|e| panic!("{e}"));
            if let Err((want, got)) = round_trips(&card) {
                panic!("{}:\n  oracle:   {want}\n  rendered: {got}", card.name);
            }
        }
    }

    #[test]
    fn modal_spells_and_multiple_targets_render_like_oracle() {
        let cases = [
            r#"Card(name: "Dual Shot", cost: "{R}", types: [Instant], text: "Dual Shot deals 1 damage to each of up to two target creatures.", spell: Spell(targets: [Targets(UpTo(2), Creature)], effects: [DealDamage(amount: Const(1), to: Target(0))]))"#,
            r#"Card(name: "Tandem Tactics", cost: "{1}{W}", types: [Instant], text: "Up to two target creatures each get +1/+2 until end of turn. You gain 2 life.", spell: Spell(targets: [Targets(UpTo(2), Creature)], effects: [ModifyPt(target: Target(0), power: Const(1), toughness: Const(2), until: EndOfTurn), GainLife(player: You, amount: Const(2))]))"#,
            r#"Card(name: "Sweep", cost: "{W}", types: [Sorcery], text: "Destroy any number of target artifacts.", spell: Spell(targets: [Targets(AnyNumber, Artifact)], effects: [Destroy(target: Target(0))]))"#,
            r#"Card(name: "Pair", cost: "{G}", types: [Instant], text: "Two target creatures each gain trample until end of turn.", spell: Spell(targets: [Targets(Exactly(2), Creature)], effects: [GrantKeyword(target: Target(0), keyword: Trample, until: EndOfTurn)]))"#,
            r#"Card(name: "Selesnya Charm", cost: "{G}{W}", types: [Instant], text: "Choose one —\n• Target creature gets +2/+2 and gains trample until end of turn.\n• Exile target creature with power 5 or greater.\n• Create a 2/2 white Knight creature token with vigilance.", spell: Spell(modes: [Mode(targets: [Creature], effects: [ModifyPt(target: Target(0), power: Const(2), toughness: Const(2), keywords: [Trample], until: EndOfTurn)]), Mode(targets: [And([Creature, PowerAtLeast(5)])], effects: [Exile(target: Target(0))]), Mode(effects: [CreateToken(spec: TokenSpec(name: "Knight", colors: [White], types: [Creature], subtypes: ["Knight"], pt: (2, 2), keywords: [Vigilance]), count: Const(1))])]))"#,
            r#"Card(name: "Kolaghan's Command", cost: "{1}{B}{R}", types: [Instant], text: "Choose two —\n• Target player discards a card.\n• Return target creature card from your graveyard to your hand.\n• Destroy target artifact.\n• Kolaghan's Command deals 2 damage to any target.", spell: Spell(choose: Two, modes: [Mode(targets: [Player], effects: [Discard(player: TargetPlayer(0), count: Const(1))]), Mode(targets: [And([Creature, InGraveyard(You)])], effects: [ReturnFromGraveyard(target: Target(0), to: Hand)]), Mode(targets: [Artifact], effects: [Destroy(target: Target(0))]), Mode(targets: [Any], effects: [DealDamage(amount: Const(2), to: Target(0))])]))"#,
        ];
        for text in cases {
            let card = load(text).unwrap_or_else(|e| panic!("{e}"));
            if let Err((want, got)) = round_trips(&card) {
                panic!("{}:\n  oracle:   {want}\n  rendered: {got}", card.name);
            }
        }
        let bad = r#"Card(name: "X", cost: "{R}", types: [Instant], text: "", spell: Spell(targets: [Targets(UpTo(2), Creature), Targets(AnyNumber, Artifact)], effects: [Destroy(target: Target(0))]))"#;
        assert!(load(bad).unwrap_err().contains("at most one"));
        let bad = r#"Card(name: "Y", cost: "{R}", types: [Instant], text: "", spell: Spell(targets: [And([Targets(UpTo(2), Creature), Tapped])], effects: [Destroy(target: Target(0))]))"#;
        assert!(load(bad).unwrap_err().contains("whole of one target spec"));
        let bad = r#"Card(name: "Z", cost: "{R}", types: [Instant], text: "", spell: Spell(modes: [Mode(effects: [Draw(player: You, count: Const(1))])]))"#;
        assert!(load(bad).unwrap_err().contains("at least two modes"));
    }

    #[test]
    fn chosen_is_only_a_direct_target_and_named_needs_its_binding() {
        let bad = r#"Card(name: "B", cost: "{R}", types: [Instant], text: "", spell: Spell(effects: [DealDamage(amount: PowerOf(Chosen(who: You, filter: Creature, count: Exactly(1))), to: Player(You))]))"#;
        assert!(load(bad).unwrap_err().contains("direct target"), "{}", load(bad).unwrap_err());
        let bad = r#"Card(name: "C", cost: "{R}", types: [Instant], text: "", spell: Spell(effects: [Destroy(target: Named("x"))]))"#;
        assert!(load(bad).unwrap_err().contains("Named"));
        let bad = r#"Card(name: "D", cost: "{R}", types: [Instant], text: "", spell: Spell(effects: [Destroy(target: Chosen(who: You, filter: Creature, count: UpTo(0)))]))"#;
        assert!(load(bad).unwrap_err().contains("at least one"));
    }

    #[test]
    fn schema_exists() {
        let s = serde_json::to_string(&json_schema()).unwrap();
        assert!(s.contains("DealDamage"));
    }

    #[test]
    fn normalisation_ignores_reminder_text_and_keyword_line_breaks() {
        let a = normalise(
            "Flying\nDeathtouch (Any amount of damage this deals to a creature is enough to destroy it.)\nLifelink",
            "X",
        );
        let b = normalise("Flying, deathtouch, lifelink", "X");
        assert_eq!(a, b);
    }
}
