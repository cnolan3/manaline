//! Layout tests: render real game views at 100×32 and 80×24 and check the
//! panes, and drive the pickers with keys.

use engine::testing::{advance_to, advance_until, TestGame};
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
    let mut app = App::new(
        Some(seat),
        "TEST42".into(),
        "Scenario".into(),
        LobbyView {
            seats: vec![],
            started: true,
        },
    );
    app.set_view(game.view(seat));
    let legal: Vec<LegalAction> = game
        .legal_actions(seat)
        .into_iter()
        .enumerate()
        .map(|(i, action)| LegalAction {
            id: i as u32,
            description: engine::text::describe_action(game, &action),
            action,
        })
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
    let mut spec = App::new(
        None,
        "TEST42".into(),
        "Scenario".into(),
        LobbyView {
            seats: vec![],
            started: true,
        },
    );
    spec.set_view(game.view_spectator());
    let s = render(&spec, 100, 32);
    assert!(s.contains("Spectating"));
    let lobby = App::new(
        Some(Seat(0)),
        "TEST42".into(),
        "Starter Cube".into(),
        LobbyView {
            seats: vec![
                protocol::SeatStatus {
                    seat: Seat(0),
                    name: Some("Connor".into()),
                    connected: true,
                    deck_ok: true,
                    ready: true,
                },
                protocol::SeatStatus {
                    seat: Seat(1),
                    name: None,
                    connected: false,
                    deck_ok: false,
                    ready: false,
                },
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
    let bears = game.players[0]
        .battlefield
        .iter()
        .copied()
        .find(|id| game.object_name(*id) == "Grizzly Bears")
        .unwrap();
    assert_eq!(
        cmds,
        vec![Command::Act(Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(1)))]
        })]
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
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(giant, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
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
    let Action::AssignCombatDamage { assignments, .. } = action else {
        panic!()
    };
    assert_eq!(assignments.iter().map(|(_, n)| *n).sum::<i32>(), 3);
    game.apply(Seat(0), action).unwrap();
}

#[test]
fn mulligan_menu_and_log_lines() {
    let db = Arc::new(cards::core());
    let green = cards::parse_decklist(&cards::deck_text("green").unwrap(), &db).unwrap();
    let config = engine::GameConfig {
        format: engine::Format::cube(),
        players: vec![
            engine::PlayerSetup {
                name: "Connor".into(),
                deck: green.clone(),
            },
            engine::PlayerSetup {
                name: "Bot".into(),
                deck: green,
            },
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
            app.handle_push(protocol::ServerMessage::Event {
                event: v,
                state_version: 1,
            });
        }
    }
    let text: Vec<&str> = app.log.iter().map(|l| l.text.as_str()).collect();
    assert!(text.iter().any(|t| t.contains("Connor draws 7 card(s)")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("Bot draws 7 card(s)")));
    app.handle_push(protocol::ServerMessage::Event {
        event: engine::EventBase::Chat {
            from: Seat(1),
            to: None,
            text: "gl hf".into(),
        },
        state_version: 1,
    });
    assert_eq!(app.log.last().unwrap().text, "Bot: gl hf");
    let s = render(&app, 100, 32);
    assert!(!s.contains("Bot: gl hf"), "log hidden by default");
    app.show_log = true;
    let s = render(&app, 100, 32);
    assert!(s.contains("Bot: gl hf"));
}

#[test]
fn minor_priority_moments_auto_pass_and_main_phases_wait() {
    use engine::Phase;
    use tui::settings::Settings;
    // Main phase on my turn: never a countdown.
    let game = board();
    let mut app = app_for(&game, Seat(0)).with_settings(Settings::default());
    app.set_legal(app.legal.clone(), app.legal_version, app.reason);
    assert!(!app.priority_is_minor());
    assert!(app.auto_pass_at.is_none());

    // Begin combat with only pass and concede available: minor, countdown armed.
    let mut game = board();
    advance_to(&mut game, Phase::BeginCombat).unwrap();
    assert_eq!(game.phase, Phase::BeginCombat);
    let mut app = app_for(&game, Seat(0)).with_settings(Settings {
        auto_pass_ms: 500,
        ..Settings::default()
    });
    app.set_legal(app.legal.clone(), app.legal_version, app.reason);
    assert!(app.priority_is_minor());
    assert!(app.auto_pass_at.is_some());
    let s = render(&app, 100, 32);
    assert!(s.contains("Passing in"), "{s}");
    assert!(app.footer().contains("Auto-passing"));
    assert!(!app.auto_pass_due());
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(app.auto_pass_due());

    // Esc holds for this moment and it does not re-arm at the same version.
    app.handle_key(key(KeyCode::Esc));
    assert!(app.auto_pass_at.is_none());
    app.set_legal(app.legal.clone(), app.legal_version, app.reason);
    assert!(app.auto_pass_at.is_none());
    assert!(app.footer().contains("[Space] pass"));

    // Turning auto-pass off in the settings menu disarms it.
    let mut app = app_for(&game, Seat(0)).with_settings(Settings::default());
    app.set_legal(app.legal.clone(), app.legal_version, app.reason);
    assert!(app.auto_pass_at.is_some());
    app.handle_key(key(KeyCode::Char('o')));
    assert!(matches!(app.mode, Mode::Settings { selected: 0 }));
    let s = render(&app, 100, 32);
    assert!(s.contains("Auto-pass minor priority moments   [on]"), "{s}");
    app.handle_key(key(KeyCode::Enter));
    assert!(!app.settings.auto_pass);
    assert!(app.auto_pass_at.is_none());
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.settings.auto_pass_ms, 2000);
    app.handle_key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
}

#[test]
fn bottoming_after_two_mulligans_uses_a_hand_picker() {
    let db = Arc::new(cards::core());
    let green = cards::parse_decklist(&cards::deck_text("green").unwrap(), &db).unwrap();
    let config = engine::GameConfig {
        format: engine::Format::cube(),
        players: vec![
            engine::PlayerSetup {
                name: "Connor".into(),
                deck: green.clone(),
            },
            engine::PlayerSetup {
                name: "Bot".into(),
                deck: green,
            },
        ],
        cards: db,
        starting_player: Some(Seat(0)),
    };
    let mut game = engine::Game::new(config, 1).unwrap();
    game.apply(Seat(0), &Action::Mulligan { keep: false }).unwrap();
    game.apply(Seat(0), &Action::Mulligan { keep: false }).unwrap();
    game.apply(Seat(0), &Action::Mulligan { keep: true }).unwrap();
    assert!(matches!(game.pending, Some(PendingChoice::BottomCards { count: 2, .. })));
    let mut app = app_for(&game, Seat(0));
    assert!(matches!(app.mode, Mode::Pick(_)), "{:?}", app.mode);
    let s = render(&app, 100, 40);
    assert!(s.contains("Put 2 on the bottom"), "{s}");
    assert!(s.contains("0/2 marked"), "{s}");
    // Exactly the seven cards in hand are listed, once each.
    assert_eq!(s.matches("[ ] ").count(), 7, "{s}");
    assert!(app.handle_key(key(KeyCode::Char(' '))).is_empty());
    assert!(app.handle_key(key(KeyCode::Enter)).is_empty(), "one marked card is not enough");
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Char(' ')));
    let cmds = app.handle_key(key(KeyCode::Enter));
    match cmds.as_slice() {
        [Command::Act(Action::BottomCards { objects })] => {
            assert_eq!(objects.len(), 2);
            game.apply(Seat(0), &cmds_action(&cmds[0])).unwrap();
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(game.players[0].hand.len(), 5);
}

fn cmds_action(c: &Command) -> Action {
    match c {
        Command::Act(a) => a.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn card_boxes_get_a_keyword_row_when_there_is_room_and_it_is_on() {
    let game = TestGame::new(Arc::new(cards::core()), 2)
        .battlefield(Seat(0), "Serra Angel")
        .battlefield(Seat(1), "Mountain")
        .build();
    let mut app = app_for(&game, Seat(0));
    app.mode = Mode::Normal;
    // Tall terminal: boxes with the glyph row.
    let s = render(&app, 100, 40);
    assert!(s.contains("│✈ Vg     │"), "{s}");
    // Not enough room for the extra row: plain boxes, no glyphs.
    let s = render(&app, 100, 32);
    assert!(s.contains("│Angel    │") && !s.contains("Vg"), "{s}");
    // Toggled off in settings: plain boxes even when tall, and chips drop glyphs too.
    let mut app = app.with_settings(tui::settings::Settings {
        card_keywords: false,
        ..Default::default()
    });
    app.mode = Mode::Normal;
    let s = render(&app, 100, 40);
    assert!(s.contains("│Angel    │") && !s.contains("Vg"), "{s}");
    let s = render(&app, 100, 20);
    assert!(s.contains("[Serra Angel 4/4]"), "{s}");
    assert_eq!(tui::settings::Settings::default().auto_pass_ms, 1500);
}

#[test]
fn summoning_sick_creatures_show_a_star_hollow_when_hasty() {
    let mut game = TestGame::new(Arc::new(cards::core()), 2)
        .battlefield(Seat(0), "Grizzly Bears")
        .battlefield(Seat(0), "Raging Goblin")
        .battlefield(Seat(1), "Mountain")
        .build();
    for id in game.players[0].battlefield.clone() {
        game.objects[id].summoning_sick = true;
    }
    let mut app = app_for(&game, Seat(0));
    app.mode = Mode::Normal;
    let s = render(&app, 100, 20);
    assert!(s.contains("[Grizzly Bears 2/2★]"), "{s}");
    assert!(s.contains("[Raging Goblin 1/1 Hs☆]"), "{s}");
    let s = render(&app, 100, 40);
    assert!(s.contains("2/2★") && s.contains("1/1☆"), "{s}");
    assert!(engine::text::render_view(&game.view(Seat(0))).contains("sick but hasty"));
}

#[test]
fn blockers_show_which_attacker_they_block() {
    let mut game = board();
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    let bears = game.players[0]
        .battlefield
        .iter()
        .copied()
        .find(|id| game.object_name(*id) == "Grizzly Bears")
        .unwrap();
    let giant = game.players[1]
        .battlefield
        .iter()
        .copied()
        .find(|id| game.object_name(*id) == "Hill Giant")
        .unwrap();
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareBlockers { .. }))).unwrap();
    game.apply(
        Seat(1),
        &Action::DeclareBlockers {
            blocks: vec![(giant, bears)],
        },
    )
    .unwrap();
    let mut app = app_for(&game, Seat(0));
    app.mode = Mode::Normal;
    let tag = format!("⊣ #{}", bears.0);
    let s = render(&app, 100, 40);
    assert!(s.contains(&tag), "box shows the blocked attacker: {s}");
    assert!(
        s.contains(&format!("Blocks: Hill Giant #{} blocks Grizzly Bears #{}", giant.0, bears.0)),
        "{s}"
    );
    let s = render(&app, 100, 20);
    assert!(s.contains(&format!("[Hill Giant 3/3 ⊣#{}]", bears.0)), "chip shows it too: {s}");
}

#[test]
fn the_lobby_opens_the_deckbuilder_and_resubmits_on_save() {
    let mut app = App::new(
        Some(Seat(0)),
        "TEST42".into(),
        "Starter Cube".into(),
        LobbyView {
            seats: vec![],
            started: false,
        },
    );
    app.deck_source = Some(tui::app::DeckSource {
        path: None,
        text: cards::deck_text("green").unwrap(),
        format: engine::Format::cube(),
    });
    assert!(app.footer().contains("[d] edit your deck"));
    app.handle_key(key(KeyCode::Char('d')));
    assert!(app.editor.is_some());
    let s = render(&app, 120, 36);
    assert!(s.contains("Deck · 40 cards"), "{s}");
    // Add a card in the deck pane and save: the deck goes back to the daemon.
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(key(KeyCode::Char('+')));
    let cmds = app.handle_key(key(KeyCode::Char('s')));
    match cmds.as_slice() {
        [Command::SetDeck(text)] => assert!(text.starts_with("Deck\n") && text.contains("Forest")),
        other => panic!("{other:?}"),
    }
    app.handle_key(key(KeyCode::Char('q')));
    assert!(app.editor.is_none());
}

#[test]
fn the_mono_theme_uses_no_colour_and_the_menu_cycles_themes() {
    use ratatui::style::Color;
    let mut game = board();
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    let bears = game.players[0]
        .battlefield
        .iter()
        .copied()
        .find(|id| game.object_name(*id) == "Grizzly Bears")
        .unwrap();
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    let mut app = app_for(&game, Seat(0)).with_settings(tui::settings::Settings {
        theme: "mono".into(),
        ..Default::default()
    });
    app.mode = Mode::Normal;
    app.show_log = true;
    let backend = TestBackend::new(100, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| tui::ui::draw(f, &app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let coloured = buf
        .content()
        .iter()
        .filter(|c| c.fg != Color::Reset || c.bg != Color::Reset)
        .count();
    assert_eq!(coloured, 0, "mono renders with modifiers only");
    let s = render(&app, 100, 40);
    assert!(s.contains("Grizzly"), "the board still reads: {s}");

    // The settings row cycles through every theme and back.
    app.handle_key(key(KeyCode::Char('o')));
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Down));
    }
    let s = render(&app, 100, 40);
    assert!(s.contains("Colour theme                        [mono]"), "{s}");
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.settings.theme, "high-contrast");
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.settings.theme, "default");
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.settings.theme, "high-contrast");
    assert!(tui::theme_flag(Some("purple")).is_err());
    assert_eq!(tui::theme_flag(Some("mono")).unwrap().as_deref(), Some("mono"));
}

#[test]
fn replay_stepping_moves_through_a_game() {
    let db = Arc::new(cards::core());
    let green = cards::parse_decklist(&cards::deck_text("green").unwrap(), &db).unwrap();
    let red = cards::parse_decklist(&cards::deck_text("red").unwrap(), &db).unwrap();
    let config = engine::GameConfig {
        format: engine::Format::cube(),
        players: vec![
            engine::PlayerSetup {
                name: "A".into(),
                deck: green,
            },
            engine::PlayerSetup {
                name: "B".into(),
                deck: red,
            },
        ],
        cards: db,
        starting_player: Some(Seat(0)),
    };
    let mut game = engine::Game::new(config, 3).unwrap();
    let mut bot = engine::bot::RandomBot::new(1);
    let mut views = vec![game.view_spectator()];
    let mut events = vec![game.log.iter().filter_map(|e| e.view(None)).collect::<Vec<_>>()];
    for _ in 0..120 {
        if game.is_over().is_some() {
            break;
        }
        let seat = *game.must_act().keys().next().unwrap();
        let action = bot.choose(&game, seat).unwrap();
        let produced = game.apply(seat, &action).unwrap();
        views.push(game.view_spectator());
        events.push(produced.iter().filter_map(|e| e.view(None)).collect());
    }
    let n = views.len();
    let last_turn = views[n - 1].turn;
    let mut app = App::new(
        None,
        "R".into(),
        String::new(),
        LobbyView {
            seats: vec![],
            started: true,
        },
    );
    app.load_replay(tui::app::ReplayState {
        title: "test".into(),
        views,
        events,
        index: 0,
        playing: false,
    });
    assert_eq!(app.replay.as_ref().unwrap().index, 0);
    let s = render(&app, 100, 32);
    assert!(s.contains("REPLAY test") && s.contains(&format!("action 1/{n}")), "{s}");
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Right));
    assert_eq!(app.replay.as_ref().unwrap().index, 2);
    app.handle_key(key(KeyCode::Left));
    assert_eq!(app.replay.as_ref().unwrap().index, 1);
    let t0 = app.view.as_ref().unwrap().turn;
    app.handle_key(key(KeyCode::Char(']')));
    let t1 = app.view.as_ref().unwrap().turn;
    assert!(t1 > t0, "next turn boundary: {t0} -> {t1}");
    app.handle_key(key(KeyCode::Char('[')));
    assert_eq!(app.view.as_ref().unwrap().turn, t0, "back to the start of the previous turn");
    let i = app.replay.as_ref().unwrap().index;
    assert!(
        i == 0 || app.replay.as_ref().unwrap().views[i - 1].turn != t0,
        "at the first action of that turn"
    );
    app.handle_key(key(KeyCode::End));
    assert_eq!(app.replay.as_ref().unwrap().index, n - 1);
    assert_eq!(app.view.as_ref().unwrap().turn, last_turn);
    assert!(
        app.log.iter().any(|l| l.text.contains("draws")),
        "the log is rebuilt up to the position"
    );
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Char(' ')));
    assert!(app.replay.as_ref().unwrap().playing);
    app.replay_tick();
    assert_eq!(app.replay.as_ref().unwrap().index, 1);
    assert!(app.footer().starts_with("REPLAY 2/"), "{}", app.footer());
    // Space plays or pauses; it never passes priority in a replay.
    assert!(app.handle_key(key(KeyCode::Char(' '))).is_empty());
}

#[test]
fn narrow_terminals_wrap_the_footer_and_show_recent_log_lines() {
    let mut app = app_for(&board(), Seat(0));
    app.mode = Mode::Normal;
    app.push_log(tui::app::LogKind::Game, "something happened".into());
    let s = render(&app, 80, 24);
    assert!(s.contains("[x] concede"), "the whole footer is visible: {s}");
    assert!(s.contains("recent") && s.contains("something happened"), "{s}");
}

#[test]
fn conceding_is_explained_and_graveyards_can_be_browsed() {
    let mut game = TestGame::new(Arc::new(cards::core()), 2)
        .battlefield(Seat(0), "Forest")
        .graveyard(Seat(0), "Grizzly Bears")
        .graveyard(Seat(0), "Giant Growth")
        .graveyard(Seat(1), "Shock")
        .build();
    let mut app = app_for(&game, Seat(0));
    app.mode = Mode::Normal;
    // g opens my graveyard, newest first; Enter inspects; Tab moves to the opponent's.
    app.handle_key(key(KeyCode::Char('g')));
    assert!(matches!(app.mode, Mode::Graveyard { seat: Seat(0), cursor: 0 }));
    let s = render(&app, 100, 32);
    assert!(
        s.contains("Your graveyard") && s.contains("Giant Growth") && s.contains("Grizzly Bears"),
        "{s}"
    );
    let giant_growth = s.find("Giant Growth").unwrap();
    let bears = s.find("Grizzly Bears").unwrap();
    assert!(giant_growth < bears, "newest first");
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Inspect(_)));
    let s = render(&app, 100, 32);
    assert!(s.contains("Creature — Bear"), "{s}");
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Char('g')));
    app.handle_key(key(KeyCode::Tab));
    assert!(matches!(app.mode, Mode::Graveyard { seat: Seat(1), .. }));
    let s = render(&app, 100, 32);
    assert!(
        s.contains("P1's graveyard") && s.contains("Shock"),
        "opponents' graveyards are public: {s}"
    );

    // The opponent concedes: the winner is told why.
    game.apply(Seat(1), &Action::Concede).unwrap();
    let mut app = app_for(&game, Seat(0));
    app.mode = Mode::Normal;
    let s = render(&app, 100, 32);
    assert!(s.contains("YOU WIN — P1 conceded"), "{s}");
    assert!(s.contains("GAME OVER — P0 wins, P1 conceded"), "header: {s}");
    let text: Vec<String> = game.log.iter().map(|e| engine::text::describe_event(&game, e)).collect();
    assert!(text.iter().any(|t| t.contains("conceded and leaves the game")), "{text:?}");
}
