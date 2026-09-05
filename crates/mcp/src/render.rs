//! Compact text renderings for models (§7.2). Every state-returning tool
//! includes both structured JSON and this.

use crate::session::Session;
use engine::text::render_view;
use engine::{ActReason, GameView};
use protocol::LegalAction;
use std::fmt::Write;

pub fn reason_text(r: ActReason) -> &'static str {
    match r {
        ActReason::Priority => "you have priority: cast, play a land, or pass",
        ActReason::DeclareAttackers => "declare attackers",
        ActReason::DeclareBlockers => "declare blockers",
        ActReason::AssignDamage => "assign combat damage among blockers",
        ActReason::Mulligan => "decide whether to keep your opening hand",
        ActReason::BottomCards => "choose cards to put on the bottom of your library",
        ActReason::Discard => "discard down to your maximum hand size",
        ActReason::Choice => "make a choice",
    }
}

/// The state view plus a line saying whether it is your turn, and the
/// numbered legal actions if it is.
pub fn render_state(session: &Session, view: &GameView, legal: &[LegalAction]) -> String {
    let mut s = render_view(view);
    if let Some(o) = view.outcome {
        let _ = writeln!(s, "\nGAME OVER: {}", outcome_text(session, o));
        return s;
    }
    match view.must_act.get(&session.me) {
        Some(r) => {
            let _ = writeln!(s, "\nIT IS YOUR TURN TO ACT: {}.", reason_text(*r));
        }
        None => {
            let who: Vec<String> = view
                .must_act
                .iter()
                .map(|(seat, r)| format!("{} to {}", session.seat_name(*seat), reason_text(*r)))
                .collect();
            let _ = writeln!(s, "\nNot your turn. Waiting on {}. Call wait_for_turn.", who.join(", "));
        }
    }
    if !legal.is_empty() {
        s.push_str(&render_legal(legal));
    }
    s
}

pub fn render_legal(legal: &[LegalAction]) -> String {
    let mut s = String::from("\nLEGAL ACTIONS\n");
    for a in legal {
        let _ = writeln!(s, "  {}. {}", a.id, a.description);
    }
    s
}

pub fn outcome_text(session: &Session, o: engine::Outcome) -> String {
    match o {
        engine::Outcome::Winner(s) if s == session.me => "you won".into(),
        engine::Outcome::Winner(s) => format!("{} won", session.seat_name(s)),
        engine::Outcome::Draw => "the game was a draw".into(),
    }
}
