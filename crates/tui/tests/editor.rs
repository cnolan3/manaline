use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use std::sync::Arc;
use tui::editor::{Editor, EditorCommand, EditorSetup, Focus};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn type_text(ed: &mut Editor, s: &str) {
    for c in s.chars() {
        ed.handle_key(key(KeyCode::Char(c)));
    }
}

fn render(ed: &Editor, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| tui::editor_ui::draw(f, ed)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn setup(path: Option<std::path::PathBuf>, text: &str) -> Editor {
    let db = Arc::new(cards::core());
    let index = Arc::new(cardsearch::Index::from_db(&db));
    Editor::new(EditorSetup {
        banner: None,
        path,
        text: text.into(),
        format: engine::Format::cube(),
        db,
        index,
        known: None,
        theme: Default::default(),
    })
    .unwrap()
}

fn tempfile(tag: &str, text: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("manaline-editor-{tag}-{}.txt", std::process::id()));
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn search_add_remove_undo_and_save_in_canonical_order() {
    let path = tempfile("edit", "// my deck\n4 Grizzly Bears\n17 Forest\n\nSideboard\n1 Plummet\n");
    let mut ed = setup(Some(path.clone()), &std::fs::read_to_string(&path).unwrap());
    assert_eq!(ed.card_count(), 21);
    assert!(!ed.report.is_legal(), "21 cards is short");
    assert!(ed.legality_line().contains("needs at least 40"), "{}", ed.legality_line());

    // Search narrows as you type; Enter adds one.
    type_text(&mut ed, "t:creature kw:trample c:g mv<=5");
    let names: Vec<&str> = ed.results.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"Stampeding Rhino"), "{names:?}");
    let pick = ed.results_cursor;
    let picked = ed.results[pick].name.clone();
    ed.handle_key(key(KeyCode::Enter));
    assert!(ed.dirty);
    assert_eq!(ed.main.iter().find(|(n, _)| *n == picked).unwrap().1, 1);
    ed.handle_key(key(KeyCode::Enter));
    assert_eq!(ed.main.iter().find(|(n, _)| *n == picked).unwrap().1, 2);

    // Deck pane: rows are grouped by type; +/- adjust; x removes; u undoes.
    ed.handle_key(key(KeyCode::Tab));
    assert_eq!(ed.focus, Focus::Deck);
    let s = render(&ed, 120, 36);
    assert!(s.contains("Creatures (6)") && s.contains("Lands (17)"), "{s}");
    let first = ed.selected_deck_card().unwrap();
    ed.handle_key(key(KeyCode::Char('+')));
    let after = ed.main.iter().find(|(n, _)| *n == first).unwrap().1;
    ed.handle_key(key(KeyCode::Char('-')));
    assert_eq!(ed.main.iter().find(|(n, _)| *n == first).unwrap().1, after - 1);
    ed.handle_key(key(KeyCode::Char('x')));
    assert!(!ed.main.iter().any(|(n, _)| *n == first));
    ed.handle_key(key(KeyCode::Char('u')));
    assert_eq!(ed.main.iter().find(|(n, _)| *n == first).unwrap().1, after - 1);

    // Save writes canonical order and keeps the sideboard.
    let cmds = ed.handle_key(key(KeyCode::Char('s')));
    assert!(matches!(cmds.as_slice(), [EditorCommand::Saved(_)]));
    assert!(!ed.dirty);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("Deck\n"), "{text}");
    assert!(text.contains("\nSideboard\n1 Plummet\n"), "{text}");
    let deck_lines: Vec<&str> = text.lines().skip(1).take_while(|l| !l.is_empty()).collect();
    assert_eq!(deck_lines.last().unwrap(), &"17 Forest", "lands last: {deck_lines:?}");
    assert!(deck_lines.iter().any(|l| l.ends_with(&picked)));
}

#[test]
fn quit_confirms_when_dirty_and_stats_pane_shows_hands() {
    let mut ed = setup(None, &cards::deck_text("green").unwrap());
    assert!(ed.report.is_legal());
    ed.handle_key(key(KeyCode::Tab));
    assert_eq!(
        ed.handle_key(key(KeyCode::Char('q'))),
        vec![EditorCommand::Quit],
        "clean: quits at once"
    );
    ed.handle_key(key(KeyCode::Char('+')));
    assert!(ed.dirty);
    assert!(ed.handle_key(key(KeyCode::Char('q'))).is_empty());
    assert!(ed.confirm_quit);
    let s = render(&ed, 100, 30);
    assert!(s.contains("Unsaved changes"), "{s}");
    ed.handle_key(key(KeyCode::Esc));
    assert!(!ed.confirm_quit);
    let cmds = ed.handle_key(ctrl('q'));
    assert!(cmds.is_empty() && ed.confirm_quit);
    assert_eq!(ed.handle_key(key(KeyCode::Char('q'))), vec![EditorCommand::Quit]);

    ed.confirm_quit = false;
    ed.handle_key(key(KeyCode::Char('t')));
    assert!(ed.show_stats);
    assert_eq!(ed.hands.len(), 3);
    let s = render(&ed, 120, 40);
    assert!(s.contains("curve") && s.contains("sample opening hands"), "{s}");
    let before = ed.hands.clone();
    ed.handle_key(key(KeyCode::Char('h')));
    assert_ne!(ed.hands, before, "new seed, new hands");
}

#[test]
fn external_edits_are_noticed() {
    let path = tempfile("disk", "4 Grizzly Bears\n17 Forest\n");
    let mut ed = setup(Some(path.clone()), &std::fs::read_to_string(&path).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&path, "4 Grizzly Bears\n4 Llanowar Elves\n17 Forest\n").unwrap();
    ed.check_disk();
    assert_eq!(ed.card_count(), 25, "clean editor reloads silently");
    ed.handle_key(key(KeyCode::Tab));
    ed.handle_key(key(KeyCode::Char('+')));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&path, "17 Forest\n").unwrap();
    ed.check_disk();
    assert!(ed.disk_changed && ed.dirty, "dirty editor keeps the edits and warns");
    assert!(ed.status.as_deref().unwrap_or("").contains("changed on disk"));
    ed.handle_key(key(KeyCode::Char('r')));
    assert_eq!(ed.card_count(), 17);
    assert!(!ed.dirty);
}

#[test]
fn the_search_pane_only_offers_playable_cards_and_shows_a_card() {
    let mut ed = setup(None, "");
    assert!(ed.results.iter().all(|e| e.implemented));
    type_text(&mut ed, "serra angel");
    assert_eq!(ed.results.len(), 1);
    let s = render(&ed, 120, 36);
    assert!(s.contains("Serra Angel") && s.contains("4/4") && s.contains("Flying"), "{s}");
    type_text(&mut ed, " frob:1");
    assert!(
        ed.status.as_deref().unwrap_or("").starts_with("search:"),
        "bad queries are reported"
    );
    assert!(ed.results.is_empty());
}

#[test]
fn agent_requests_apply_like_keystrokes_and_are_marked() {
    use protocol::editor::{EditorReply, EditorRequest};
    let path = tempfile("agent", "17 Forest\n4 Grizzly Bears\n");
    let mut ed = setup(Some(path.clone()), &std::fs::read_to_string(&path).unwrap());
    match ed.apply_request(EditorRequest::Status) {
        EditorReply::Status(st) => {
            assert_eq!(st.cards, 21);
            assert!(!st.dirty && st.last_agent_action.is_none());
        }
        other => panic!("{other:?}"),
    }
    match ed.apply_request(EditorRequest::AddCard {
        name: "llanowar elves".into(),
        count: 4,
    }) {
        EditorReply::Changed { message, status } => {
            assert_eq!(message, "added 4 Llanowar Elves (now 4)");
            assert!(status.dirty && status.cards == 25);
        }
        other => panic!("{other:?}"),
    }
    assert!(ed.agent_marked("Llanowar Elves") && !ed.agent_marked("Forest"));
    assert_eq!(ed.status.as_deref(), Some("agent added 4 Llanowar Elves (now 4)"));
    let s = render(&ed, 120, 36);
    assert!(s.contains("◆ agent"), "{s}");
    assert!(matches!(
        ed.apply_request(EditorRequest::AddCard {
            name: "Black Lotus".into(),
            count: 1
        }),
        EditorReply::Error { .. }
    ));
    assert!(matches!(
        ed.apply_request(EditorRequest::RemoveCard {
            name: "Plummet".into(),
            count: 1,
            all: false
        }),
        EditorReply::Error { .. }
    ));
    match ed.apply_request(EditorRequest::SetCount {
        name: "Grizzly Bears".into(),
        count: 0,
    }) {
        EditorReply::Changed { status, .. } => assert_eq!(status.cards, 21),
        other => panic!("{other:?}"),
    }
    assert!(!ed.main.iter().any(|(n, _)| n == "Grizzly Bears"));
    match ed.apply_request(EditorRequest::Deck) {
        EditorReply::Deck(d) => {
            assert!(d.groups.iter().any(|g| g.title == "Creatures" && g.count == 4));
            assert!(d.groups.iter().any(|g| g.title == "Lands" && g.cards[0].name == "Forest"));
        }
        other => panic!("{other:?}"),
    }
    // The human's undo key undoes the agent's change too.
    ed.handle_key(key(KeyCode::Tab));
    ed.handle_key(key(KeyCode::Char('u')));
    assert!(ed.main.iter().any(|(n, _)| n == "Grizzly Bears"));
    assert!(matches!(ed.apply_request(EditorRequest::Undo), EditorReply::Changed { .. }));
    assert!(!ed.main.iter().any(|(n, _)| n == "Llanowar Elves"));
    match ed.apply_request(EditorRequest::Save) {
        EditorReply::Saved { path: p, status } => {
            assert_eq!(p, path);
            assert!(!status.dirty);
        }
        other => panic!("{other:?}"),
    }
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .starts_with("Deck\n4 Grizzly Bears\n17 Forest\n"));
}
