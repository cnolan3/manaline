//! The TUI's connection path against a real daemon: join, deck, ready,
//! state, pushes, and acting through `execute`.

use daemon::{CreateGame, Daemon, DaemonConfig};
use engine::{ActReason, Action, Seat};
use protocol::{Endpoint, Token};
use std::sync::Arc;
use tui::app::{Command, Mode};

#[tokio::test]
async fn joins_and_plays_the_opening_through_the_protocol() {
    let dir = std::env::temp_dir().join(format!("manaline-tui-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = DaemonConfig {
        socket: Some(dir.join("game.sock")),
        no_socket: false,
        tcp: None,
        parent_pid: None,
        replay_dir: Some(dir.join("games")),
        create: Some(CreateGame { format: "cube".into(), seats: 2, seed: Some(4) }),
        cards: Arc::new(cards::core()),
    };
    let d = Daemon::bind(config).await.unwrap();
    let info = d.info().clone();
    let handle = d.handle();
    let task = tokio::spawn(async move { d.run().await.unwrap() });
    let endpoint = Endpoint::Unix(info.socket.clone().unwrap());

    // Seat 1 is a bot that readies immediately and then waits.
    let bot_endpoint = endpoint.clone();
    let bot_token: Token = info.seat_tokens[1].clone();
    let bot = tokio::spawn(async move {
        let mut c = protocol::Client::connect(&bot_endpoint).await.unwrap();
        c.hello(&bot_token, Some("Bot")).await.unwrap();
        c.set_deck(cards::deck_text("m0-red").unwrap()).await.unwrap().unwrap();
        c.ready().await.unwrap();
        c
    });

    let cfg = tui::TuiConfig {
        endpoint,
        token: info.seat_tokens[0].clone(),
        name: "Connor".into(),
        decklist: Some(cards::deck_text("m0-green").unwrap().to_string()),
        hints: vec!["hello there".into()],
    };
    let tui::Session { client, mut pushes, mut app } = tui::join(cfg).await.unwrap();
    assert_eq!(app.me, Some(Seat(0)));
    assert!(app.log.iter().any(|l| l.text == "hello there"));
    let _bot_client = bot.await.unwrap();

    // Wait for the game to start (lobby push), then refresh like the event loop would.
    let mut status = handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();
    while app.view.is_none() {
        let msg = pushes.recv().await.unwrap();
        app.handle_push(msg);
        if app.needs_refresh {
            app.needs_refresh = false;
            tui::refresh(&client, &mut app).await;
        }
    }
    let view = app.view.as_ref().unwrap();
    assert_eq!(view.players[0].name, "Connor");
    assert_eq!(view.players[1].name, "Bot");

    // Whoever mulligans first: if it is us, the menu opened and Enter keeps.
    if app.my_reason() == Some(ActReason::Mulligan) {
        assert!(matches!(app.mode, Mode::Menu(_)));
        let cmds = app.handle_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Enter,
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(cmds, vec![Command::Act(Action::Mulligan { keep: true })]);
        for c in cmds {
            tui::execute(&client, &mut app, c).await;
        }
        assert!(app.status.is_none(), "{:?}", app.status);
        assert_ne!(app.my_reason(), Some(ActReason::Mulligan));
    } else {
        assert!(app.footer().contains("Waiting on Bot"), "{}", app.footer());
    }

    // A stale act is reported, not fatal.
    tui::execute(&client, &mut app, Command::Act(Action::PassPriority)).await;
    assert!(app.status.is_some());

    // Chat comes back as a push and lands in the log.
    tui::execute(&client, &mut app, Command::Chat("gl".into())).await;
    loop {
        let msg = pushes.recv().await.unwrap();
        app.handle_push(msg);
        if app.log.iter().any(|l| l.text == "Connor: gl") {
            break;
        }
    }

    handle.shutdown();
    task.await.unwrap();
}
