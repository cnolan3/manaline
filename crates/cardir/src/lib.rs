//! The manaline card IR (docs/SPEC.md §4): schema types, a RON loader, a
//! validator, and the English renderer with its round-trip check.
//!
//! Compiles without tokio or any networking dependency.

pub mod ir;
pub mod render;
pub mod types;
pub mod validate;

pub use ir::*;
pub use render::{normalise, render, render_ability, render_spell, render_trigger, round_trips};
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
