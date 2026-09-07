use carddb::Cache;
use engine::LegalitySource;

fn fixture() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/oracle-cards.json")).unwrap()
}

#[test]
fn install_trims_tokens_and_looks_up_by_name_and_face() {
    let dir = tempdir("install");
    let cache = Cache::install(&dir, &fixture(), "2026-08-30T09:00:00+00:00".into()).unwrap();
    assert_eq!(cache.len(), 4, "the token layout is dropped");
    assert!(cache.get("grizzly bears").is_some());
    assert_eq!(
        cache.get("Lightning Bolt").unwrap().oracle_text,
        "Lightning Bolt deals 3 damage to any target."
    );
    assert!(cache.get("Delver of Secrets").is_some(), "front face resolves");
    assert!(cache.get("Goblin Token").is_none());
    assert!(
        cache.age_text().starts_with("card data as of 2026-08-30 (fetched today)"),
        "{}",
        cache.age_text()
    );

    // Reloading from disk gives the same thing.
    let again = Cache::load_from(&dir).unwrap().expect("cache present");
    assert_eq!(again.len(), 4);
    assert_eq!(again.meta.updated_at, "2026-08-30T09:00:00+00:00");
    assert!(Cache::load_from(&dir.join("nope")).unwrap().is_none());
}

#[test]
fn legality_answers_per_format_and_unknown_cards_are_none() {
    let dir = tempdir("legality");
    let cache = Cache::install(&dir, &fixture(), String::new()).unwrap();
    assert_eq!(cache.legal_in("Sol Ring", "commander"), Some(true));
    assert_eq!(cache.legal_in("Sol Ring", "legacy"), Some(false));
    assert!(cache.get("Sol Ring").unwrap().is_banned("legacy"));
    assert_eq!(cache.legal_in("Grizzly Bears", "standard"), Some(false));
    assert_eq!(cache.legal_in("Grizzly Bears", "modern"), Some(true));
    assert_eq!(cache.legal_in("Made Up Card", "modern"), None);
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("manaline-carddb-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn json_lines_and_gzip_are_accepted() {
    let cards: Vec<serde_json::Value> = serde_json::from_slice(&fixture()).unwrap();
    let jsonl: String = cards.iter().map(|c| c.to_string() + "\n").collect();
    assert_eq!(carddb::parse_bulk(jsonl.as_bytes()).unwrap().len(), 5);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, jsonl.as_bytes()).unwrap();
    let bytes = gz.finish().unwrap();
    assert_eq!(carddb::parse_bulk(&bytes).unwrap().len(), 5);
    let dir = tempdir("gz");
    assert_eq!(Cache::install(&dir, &bytes, String::new()).unwrap().len(), 4);
}
