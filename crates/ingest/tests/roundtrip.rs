#[test]
fn the_core_set_round_trips_and_a_bad_file_is_reported() {
    let r = ingest::roundtrip_core();
    assert!(r.is_clean(), "{}", r.render());
    assert_eq!(r.total, r.exact);

    let dir = std::env::temp_dir().join(format!("manaline-ingest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("ok.ron"),
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../cards/data/core/grizzly_bears.ron")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("wrong_text.ron"),
        r#"Card(name: "Bear", cost: "{1}{G}", types: [Creature], pt: (2, 2), text: "Trample")"#,
    )
    .unwrap();
    std::fs::write(dir.join("invalid.ron"), r#"Card(name: "Broken", types: [Creature])"#).unwrap();
    let r = ingest::roundtrip_dir(&dir).unwrap();
    assert_eq!(r.total, 3);
    assert_eq!(r.exact, 1);
    assert_eq!(r.mismatches.len(), 1);
    assert_eq!(r.invalid.len(), 1, "{}", r.render());
    assert!(r.render().contains("MISMATCH  Bear"));
}
