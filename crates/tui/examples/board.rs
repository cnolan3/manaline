//! Print the TUI's rendering of a scenario board at a given size, for eyeballing
//! the layout without a live game: `cargo run -p manaline-tui --example board -- 100 32`.

use engine::testing::TestGame;
use engine::Seat;
use protocol::{LegalAction, LobbyView};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::sync::Arc;
use tui::app::App;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let w: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
    let h: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(32);
    let game = TestGame::new(Arc::new(cards::core()), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield_tapped(Seat(0), "Mountain")
        .battlefield(Seat(0), "Grizzly Bears")
        .battlefield(Seat(0), "Balduvian Barbarians")
        .battlefield(Seat(0), "Serra Angel")
        .hand(Seat(0), "Centaur Courser")
        .hand(Seat(0), "Forest")
        .hand(Seat(0), "Craw Wurm")
        .battlefield(Seat(1), "Hill Giant")
        .battlefield(Seat(1), "Vampire Nighthawk")
        .battlefield(Seat(1), "Mountain")
        .battlefield(Seat(1), "Mountain")
        .battlefield_tapped(Seat(1), "Mountain")
        .build();
    let seat = Seat(0);
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
            description: engine::text::describe_action(&game, &action),
            action,
        })
        .collect();
    app.set_legal(legal, game.state_version(), game.must_act().get(&seat).copied());
    for e in &game.log {
        if let Some(v) = e.view(Some(seat)) {
            app.handle_push(protocol::ServerMessage::Event {
                event: v,
                state_version: 1,
            });
        }
    }
    app.handle_push(protocol::ServerMessage::Event {
        event: engine::EventBase::Chat {
            from: Seat(1),
            to: None,
            text: "Hm, fair enough.".into(),
        },
        state_version: 1,
    });
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| tui::ui::draw(f, &app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        println!("{}", line.trim_end());
    }
}
