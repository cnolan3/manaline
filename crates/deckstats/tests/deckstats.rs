use deckstats::{parse, CardStatus, Section, Stats};
use engine::Format;
use std::sync::Arc;

#[test]
fn parses_every_common_export_shape() {
    let text = "// my deck\nDeck\n4 Lightning Strike (FDN) 154\n4x Grizzly Bears\nForest # a single one\n\nSideboard\n2 Shock\nSB: 1 Negate\n\nCommander\n1 Elvish Archdruid\n";
    let list = parse(text).unwrap();
    let main: Vec<_> = list.main().collect();
    assert_eq!(main.len(), 3);
    assert_eq!(main[0].count, 4);
    assert_eq!(main[0].name, "Lightning Strike");
    assert_eq!(main[0].set.as_deref(), Some("fdn"));
    assert_eq!(main[0].collector_number.as_deref(), Some("154"));
    assert_eq!(main[1].count, 4);
    assert_eq!(main[2].count, 1);
    assert_eq!(main[2].name, "Forest");
    assert_eq!(list.main_count(), 9);
    let side: Vec<_> = list.section(Section::Sideboard).collect();
    assert_eq!(side.len(), 2);
    assert_eq!(side[1].name, "Negate");
    assert_eq!(list.section(Section::Commander).count(), 1);
    assert!(parse("0 Forest").is_err());
}

#[test]
fn resolution_suggests_close_names_and_canonical_text_sorts_by_type() {
    let db = cards::core();
    let list = parse("2 grizly bears\n3 Forest\n1 Lightning Strike\n1 Frobnicator\n").unwrap();
    let res = list.resolve(&db);
    assert_eq!(res.deck.len(), 4, "the resolvable lines resolve");
    assert_eq!(res.unresolved.len(), 2);
    assert_eq!(res.unresolved[0].suggestion.as_deref(), Some("Grizzly Bears"));
    assert_eq!(res.unresolved[1].suggestion, None);
    let text = parse("3 Forest\n1 Lightning Strike\n2 Grizzly Bears\n").unwrap().to_text(&db);
    assert_eq!(text, "Deck\n2 Grizzly Bears\n1 Lightning Strike\n3 Forest\n");
}

#[test]
fn check_classifies_each_line() {
    let db = cards::core();
    let format = Format::cube();
    let list = parse("17 Forest\n4 Grizzly Bears\n4 Grizly Bears\n4 Black Lotus\n").unwrap();
    let report = deckstats::check::check(&list, &format, &db, None);
    assert!(!report.is_legal());
    let statuses: Vec<&CardStatus> = report.lines.iter().map(|l| &l.status).collect();
    assert_eq!(statuses[0], &CardStatus::Ok);
    assert_eq!(statuses[1], &CardStatus::Ok);
    assert_eq!(
        statuses[2],
        &CardStatus::Unknown {
            suggestion: Some("Grizzly Bears".into())
        }
    );
    assert_eq!(statuses[3], &CardStatus::Unknown { suggestion: None });
    assert!(report
        .deck
        .iter()
        .any(|v| matches!(v, engine::Violation::TooFewCards { have: 21, .. })));

    // With card data, a real card the engine lacks is "not implemented", and a
    // Scryfall-pool format asks the data about legality.
    struct Known;
    fn legal(name: &str, format: &str) -> Option<bool> {
        Some(format == "legacy" && name != "Grizzly Bears")
    }
    impl deckstats::KnownCards for Known {
        fn is_card(&self, name: &str) -> bool {
            name.eq_ignore_ascii_case("Black Lotus") || cards::core().lookup(name).is_some()
        }
        fn as_legality(&self) -> &dyn engine::LegalitySource {
            &(legal as fn(&str, &str) -> Option<bool>)
        }
    }
    let report = deckstats::check::check(&list, &format, &db, Some(&Known));
    assert_eq!(report.lines[3].status, CardStatus::NotImplemented);
    let mut legacy = Format::cube();
    legacy.legality.pool = engine::CardPool::Scryfall { format: "legacy".into() };
    let list = parse("4 Grizzly Bears\n36 Forest\n").unwrap();
    let report = deckstats::check::check(&list, &legacy, &db, Some(&Known));
    assert_eq!(report.lines[0].status, CardStatus::NotInPool);
    let report = deckstats::check::check(&list, &legacy, &db, None);
    assert!(
        report.deck.iter().any(|v| matches!(v, engine::Violation::NeedsCardData { .. })),
        "{:?}",
        report.deck
    );
}

#[test]
fn stats_and_sample_hands() {
    let db = Arc::new(cards::core());
    let list = parse(cards::deck_text("green").unwrap()).unwrap();
    let deck = list.resolve(&db).deck;
    let s = Stats::compute(&deck, &db);
    assert_eq!(s.cards, 40);
    assert_eq!(s.lands, 17);
    assert_eq!(s.creatures + s.noncreature_spells + s.lands, 40);
    assert!(s.average_mv > 1.5 && s.average_mv < 3.0, "{}", s.average_mv);
    assert_eq!(
        s.sources[&cardir::Color::Green],
        17 + 4 + 2 + 2,
        "Forests, Llanowar Elves, Mystics, Archdruids"
    );
    assert!(s.pips[&cardir::Color::Green] >= 20);
    assert!(s.interaction >= 1, "Plummet");
    // Hypergeometric sanity: 17 lands in 40, 7 cards, at least 2 lands ≈ 89%.
    let p = deckstats::hypergeometric_at_least(40, 17, 7, 2);
    assert!((p - 0.89).abs() < 0.02, "{p}");
    let hands = deckstats::sample_hands(&deck, &db, &Format::cube(), 3, 5);
    assert_eq!(hands.len(), 3);
    assert!(hands.iter().all(|h| h.len() == 7));
    let text = deckstats::stats::render(&s, "Starter Cube");
    assert!(text.contains("curve") && text.contains("green"), "{text}");
}

#[test]
fn the_cube_allows_four_copies_and_unlimited_basics() {
    let db = cards::core();
    let format = Format::cube();
    let list = parse("36 Serra Angel\n24 Plains\n").unwrap();
    let report = deckstats::check::check(&list, &format, &db, None);
    assert!(!report.is_legal());
    assert_eq!(report.lines[0].status, CardStatus::TooManyCopies { count: 36, max: 4 });
    assert_eq!(report.lines[1].status, CardStatus::Ok, "basic lands are exempt");
    let list = parse("4 Serra Angel\n36 Plains\n").unwrap();
    assert!(deckstats::check::check(&list, &format, &db, None).is_legal());
}
