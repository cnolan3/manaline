//! Layout tests: render real game views at 100×32 and 80×24 and check the
//! panes, and drive the pickers with keys.

use engine::testing::{advance_until, TestGame};
use engine::{ActReason, Action, AttackTarget, PendingChoice, Seat};
use protocol::{LegalAction, LobbyView};
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use std::sync::Arc;
use tui::app::{App, Command, Mode};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn app_for(game: &engine::Game, seat: Seat) -> App {
    let mut app = App::new(Some(seat), "TEST42".into(), "Scenario".into(), LobbyView { seats: vec![], started: true });
    app.set_view(game.view(seat));
    let legal: Vec<LegalAction> = game
        .legal_actions(seat)
        .into_iter()
        .enumerate()
        .map(|(i, action)| LegalAction { id: i as u32, description: engine::text::describe_action(game, &action), action })
        .collect();
    let reason = game.must_act().get(&seat).copied();
    app.set_legal(legal, game.state_version(), reason);
    app
}

fn render(app: &App, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| tui::ui::draw(f, app)).unwrap();
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

fn board() -> engine::Game {
    TestGame::new(Arc::new(cards::core()), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield_tapped(Seat(0), "Mountain")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Centaur Courser")
        .hand(Seat(0), "Forest")
        .hand(Seat(0), "Craw Wurm")
        .battlefield(Seat(1), "Hill Giant")
        .battlefield(Seat(1), "Mountain")
        .build()
}

#[test]
fn full_layout_at_100x32() {
    let game = board();
    let app = app_for(&game, Seat(0));
    let s = render(&app, 100, 32);
    assert!(s.contains("Turn 1 · Main 1 · You have priority"), "{s}");
    assert!(s.contains("P1 (seat 1)"));
    assert!(s.contains("YOU — P0 (seat 0)"));
    assert!(s.contains("Turn 1 · Main 1 · your turn"), "centre strip");
    assert!(s.contains("YOU HAVE PRIORITY  ·  [Space] pass"));
    // Stack and log are hidden until toggled.
    assert!(!s.contains("STACK"));
    assert!(!s.contains("LOG"));
    let mut toggled = app_for(&game, Seat(0));
    toggled.handle_key(key(KeyCode::Char('l')));
    toggled.handle_key(key(KeyCode::Char('s')));
    let t = render(&toggled, 100, 32);
    assert!(t.contains("STACK") && t.contains("LOG"), "{t}");
    assert!(s.contains("Giant"), "{s}");
    assert!(s.contains("Grizzly"));
    assert!(s.contains("Bears"));
    assert!(s.contains("[1] Centaur Courser {2}{G}"));
    assert!(s.contains("[2] Forest"));
    assert!(s.contains("[Space] pass"));
    assert!(s.contains("[1-9] play/cast"));
    // Card boxes: a tapped Mountain shows T and its mana symbol.
    assert!(s.contains("{R}"));
}

#[test]
fn compact_layout_at_80x24_keeps_every_pane() {
    let game = board();
    let app = app_for(&game, Seat(0));
    let s = render(&app, 80, 24);
    assert!(s.contains("YOU — P0"));
    // Too short for card boxes: rows fall back to one-line chips.
    assert!(s.contains("[Grizzly Bears 2/2]"), "{s}");
    assert!(s.contains("[Mountain{R} T]"));
    assert!(s.contains("[1] Centaur Courser"));
    assert!(s.contains("[Space] pass"));
    let opp = app_for(&game, Seat(1));
    let s = render(&opp, 80, 24);
    assert!(s.contains("Waiting on P0"), "{s}");
}

#[test]
fn spectator_and_lobby_render() {
    let game = board();
    let mut spec = App::new(None, "TEST42".into(), "Scenario".into(), LobbyView { seats: vec![], started: true });
    spec.set_view(game.view_spectator());
    let s = render(&spec, 100, 32);
    assert!(s.contains("Spectating"));
    let lobby = App::new(
        Some(Seat(0)),
        "TEST42".into(),
        "Starter Cube".into(),
        LobbyView {
            seats: vec![
                protocol::SeatStatus { seat: Seat(0), name: Some("Connor".into()), connected: true, deck_ok: true, ready: true },
                protocol::SeatStatus { seat: Seat(1), name: None, connected: false, deck_ok: false, ready: false },
            ],
            started: false,
        },
    );
    let s = render(&lobby, 80, 24);
    assert!(s.contains("Connor (you)  —  ready"));
    assert!(s.contains("seat 1  (empty)  —  not connected"));
    assert!(s.contains("Waiting for every seat"));
}

#[test]
fn hand_keys_play_lands_and_open_payment_menus() {
    let game = board();
    let mut app = app_for(&game, Seat(0));
    // [2] is a Forest: one legal way to play it.
    let cmds = app.handle_key(key(KeyCode::Char('2')));
    assert!(matches!(cmds.as_slice(), [Command::Act(Action::PlayLand { .. })]));
    // [1] Centaur Courser {2}{G} with three Forests (Mountain tapped): exactly one payment → act.
    let cmds = app.handle_key(key(KeyCode::Char('1')));
    assert!(matches!(cmds.as_slice(), [Command::Act(Action::CastSpell { .. })]), "{cmds:?}");
    // [3] Craw Wurm is unaffordable.
    let cmds = app.handle_key(key(KeyCode::Char('3')));
    assert!(cmds.is_empty());
    assert!(app.status.as_ref().unwrap().0.contains("can't be played"));
    // Space passes.
    assert_eq!(app.handle_key(key(KeyCode::Char(' '))), vec![Command::Act(Action::PassPriority)]);
    // q quits.
    assert_eq!(app.handle_key(key(KeyCode::Char('q'))), vec![Command::Quit]);
}

#[test]
fn attack_picker_auto_opens_and_declares() {
    let mut game = board();
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    let mut app = app_for(&game, Seat(0));
    assert_eq!(app.my_reason(), Some(ActReason::DeclareAttackers));
    assert!(matches!(app.mode, Mode::Attack(_)), "picker opens on its own");
    let s = render(&app, 100, 32);
    assert!(s.contains("Declare attackers"));
    assert!(s.contains("stays home"));
    app.handle_key(key(KeyCode::Char(' ')));
    let s = render(&app, 100, 32);
    assert!(s.contains("→ P1"), "{s}");
    let cmds = app.handle_key(key(KeyCode::Enter));
    let bears = game.players[0].battlefield.iter().copied().find(|id| game.object_name(*id) == "Grizzly Bears").unwrap();
    assert_eq!(
        cmds,
        vec![Command::Act(Action::DeclareAttackers { attackers: vec![(bears, AttackTarget::Player(Seat(1)))] })]
    );
    // Escape closes; 'a' reopens.
    app.mode = Mode::Normal;
    app.handle_key(key(KeyCode::Char('a')));
    assert!(matches!(app.mode, Mode::Attack(_)));
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn block_and_damage_pickers() {
    let mut game = TestGame::new(Arc::new(cards::core()), 2)
        .battlefield(Seat(0), "Hill Giant")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    let giant = game.players[0].battlefield[0];
    game.apply(Seat(0), &Action::DeclareAttackers { attackers: vec![(giant, AttackTarget::Player(Seat(1)))] }).unwrap();
    for _ in 0..2 {
        let s = game.priority.unwrap();
        game.apply(s, &Action::PassPriority).unwrap();
    }
    assert!(matches!(game.pending, Some(PendingChoice::DeclareBlockers { .. })));
    let mut app = app_for(&game, Seat(1));
    assert!(matches!(app.mode, Mode::Block(_)));
    let s = render(&app, 100, 32);
    assert!(s.contains("Declare blockers"));
    assert!(s.contains("[ ] Grizzly Bears"), "{s}");
    assert!(s.contains("doesn't block"));
    // Both bears block the giant: Space toggles, like the attack picker.
    app.handle_key(key(KeyCode::Char(' ')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    let s = render(&app, 100, 32);
    assert!(s.contains("[x] Grizzly Bears"), "{s}");
    let cmds = app.handle_key(key(KeyCode::Enter));
    let Command::Act(action) = &cmds[0] else { panic!() };
    let Action::DeclareBlockers { blocks } = action else { panic!() };
    assert_eq!(blocks.len(), 2);
    game.apply(Seat(1), action).unwrap();
    for _ in 0..2 {
        let s = game.priority.unwrap();
        game.apply(s, &Action::PassPriority).unwrap();
    }
    assert!(matches!(game.pending, Some(PendingChoice::AssignDamage { .. })));

    let mut app = app_for(&game, Seat(0));
    assert!(matches!(app.mode, Mode::Damage(_)));
    let s = render(&app, 100, 32);
    assert!(s.contains("Assign combat damage"));
    assert!(s.contains("deals 3 damage; 3 assigned"), "{s}");
    // Move one point from the first bear to the second.
    app.handle_key(key(KeyCode::Char('-')));
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char('+')));
    let cmds = app.handle_key(key(KeyCode::Enter));
    let Command::Act(action) = &cmds[0] else { panic!() };
    let Action::AssignCombatDamage { assignments, .. } = action else { panic!() };
    assert_eq!(assignments.iter().map(|(_, n)| *n).sum::<i32>(), 3);
    game.apply(Seat(0), action).unwrap();
}

#[test]
fn mulligan_menu_and_log_lines() {
    let db = Arc::new(cards::core());
    let green = cards::parse_decklist(cards::deck_text("m0-green").unwrap(), &db).unwrap();
    let config = engine::GameConfig {
        format: engine::Format::cube(),
        players: vec![
            engine::PlayerSetup { name: "Connor".into(), deck: green.clone() },
            engine::PlayerSetup { name: "Bot".into(), deck: green },
        ],
        cards: db,
        starting_player: Some(Seat(0)),
    };
    let game = engine::Game::new(config, 1).unwrap();
    let mut app = app_for(&game, Seat(0));
    assert!(matches!(app.mode, Mode::Menu(_)));
    let s = render(&app, 100, 32);
    assert!(s.contains("Mulligan"));
    assert!(s.contains("Keep this hand"));
    assert!(s.contains("HAND [1]"), "the hand stays visible under the mulligan menu: {s}");
    let cmds = app.handle_key(key(KeyCode::Enter));
    assert_eq!(cmds, vec![Command::Act(Action::Mulligan { keep: true })]);

    // Pushed events become log lines with names from the view; hidden draws stay hidden.
    for e in &game.log {
        if let Some(v) = e.view(Some(Seat(0))) {
            app.handle_push(protocol::ServerMessage::Event { event: v, state_version: 1 });
        }
    }
    let text: Vec<&str> = app.log.iter().map(|l| l.text.as_str()).collect();
    assert!(text.iter().any(|t| t.contains("Connor draws 7 card(s)")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("Bot draws 7 card(s)")));
    app.handle_push(protocol::ServerMessage::Event {
        event: engine::EventBase::Chat { from: Seat(1), to: None, text: "gl hf".into() },
        state_version: 1,
    });
    assert_eq!(app.log.last().unwrap().text, "Bot: gl hf");
    let s = render(&app, 100, 32);
    assert!(!s.contains("Bot: gl hf"), "log hidden by default");
    app.show_log = true;
    let s = render(&app, 100, 32);
    assert!(s.contains("Bot: gl hf"));
}

