use cardsearch::{parse, Index, Query, Term};

fn index() -> Index {
    Index::from_db(&cards::core())
}

fn names(index: &Index, q: &str) -> Vec<String> {
    index.query(q, 0).unwrap().iter().map(|e| e.name.clone()).collect()
}

#[test]
fn parses_terms_operators_negation_or_and_groups() {
    let q = parse(r#"t:creature c:r mv<=2 o:"draw a card" -kw:flying (bear or elf)"#).unwrap();
    let Query::And(parts) = q else { panic!("{q:?}") };
    assert_eq!(parts.len(), 6);
    assert_eq!(parts[0], Query::Term(Term::Type("creature".into())));
    assert!(matches!(&parts[2], Query::Term(Term::ManaValue(cardsearch::Cmp::Le, n)) if *n == 2.0));
    assert_eq!(parts[3], Query::Term(Term::Oracle("draw a card".into())));
    assert!(matches!(&parts[4], Query::Not(_)));
    assert!(matches!(&parts[5], Query::Or(_)));
    assert!(parse("").is_err());
    assert!(parse("(t:creature").is_err());
    assert!(parse("frob:1").is_err());
    assert!(parse("c:purple").is_err());
}

#[test]
fn searches_the_engine_set() {
    let ix = index();
    assert!(names(&ix, "bears").contains(&"Grizzly Bears".to_string()));
    let cheap_red_creatures = names(&ix, "t:creature c:r mv<=1");
    assert!(
        cheap_red_creatures.contains(&"Raging Goblin".to_string()),
        "{cheap_red_creatures:?}"
    );
    assert!(!cheap_red_creatures.contains(&"Hill Giant".to_string()));
    let draws = names(&ix, r#"o:"draw a card" t:creature"#);
    assert!(draws.contains(&"Elvish Visionary".to_string()), "{draws:?}");
    let fliers = names(&ix, "kw:flying pow>=4");
    assert!(fliers.contains(&"Serra Angel".to_string()));
    assert!(!fliers.contains(&"Wind Drake".to_string()));
    let elves = names(&ix, "t:elf -o:add");
    assert!(elves.contains(&"Elvish Visionary".to_string()));
    assert!(!elves.contains(&"Llanowar Elves".to_string()), "mana Elves excluded: {elves:?}");
    let either = names(&ix, "t:angel or t:sphinx");
    assert!(either.contains(&"Serra Angel".to_string()) && either.contains(&"Serra Sphinx".to_string()));
    assert!(names(&ix, "c:c t:artifact").contains(&"Bonesplitter".to_string()));
    assert!(names(&ix, "is:vanilla c=g").contains(&"Grizzly Bears".to_string()));
    assert_eq!(
        names(&ix, "t:creature").len(),
        ix.query("t:creature is:implemented", 0).unwrap().len(),
        "everything is implemented without the cache"
    );
    assert_eq!(ix.query("t:creature", 3).unwrap().len(), 3);
}

#[test]
fn the_cache_marks_implementation_and_legality() {
    let fixture = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../carddb/tests/fixtures/oracle-cards.json")).unwrap();
    let dir = std::env::temp_dir().join(format!("manaline-cardsearch-{}", std::process::id()));
    let cache = carddb::Cache::install(&dir, &fixture, String::new()).unwrap();
    let ix = Index::from_cache(&cache, &cards::core());
    assert!(ix.from_cache);
    let all = names(&ix, "t:creature or t:instant or t:artifact");
    assert!(all.contains(&"Sol Ring".to_string()));
    let implemented = names(&ix, "is:implemented");
    assert!(implemented.contains(&"Grizzly Bears".to_string()) && !implemented.contains(&"Sol Ring".to_string()));
    assert_eq!(
        names(&ix, "f:legacy t:artifact"),
        Vec::<String>::new(),
        "Sol Ring is banned in legacy"
    );
    assert_eq!(names(&ix, "f:commander c:c"), vec!["Sol Ring".to_string()]);
    assert!(ix.get("sol ring").unwrap().line().contains("(not implemented)"));
    // Implemented first, then by name.
    let all = ix.query("t:creature or t:instant or t:artifact", 0).unwrap();
    assert!(all[0].implemented && !all.last().unwrap().implemented);
}
