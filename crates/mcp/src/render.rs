//! Compact text renderings for models (§7.2). Every state-returning tool
//! includes both structured JSON and this.

use crate::session::Session;
use engine::text::render_view;
use engine::{ActReason, GameView};
use protocol::LegalAction;
use std::fmt::Write;

/// Second person: what *you* must do. Only ever used after "IT IS YOUR TURN
/// TO ACT" and friends, never behind somebody else's name.
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

/// Third person, for "Waiting on <name> to …". `reason_text` is written from
/// the acting seat's own point of view and turns into nonsense the moment
/// another player's name is put in front of it.
pub fn waiting_text(name: &str, r: ActReason) -> String {
    let what = match r {
        ActReason::Priority => "act: they have priority",
        ActReason::DeclareAttackers => "declare attackers",
        ActReason::DeclareBlockers => "declare blockers",
        ActReason::AssignDamage => "assign combat damage among blockers",
        ActReason::Mulligan => "decide whether to keep their opening hand",
        ActReason::BottomCards => "choose cards to put on the bottom of their library",
        ActReason::Discard => "discard down to their maximum hand size",
        ActReason::Choice => "make a choice",
    };
    format!("{name} to {what}")
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
                .map(|(seat, r)| waiting_text(&session.seat_name(*seat), *r))
                .collect();
            let _ = writeln!(
                s,
                "\nNot your turn (state_version {}). Waiting on {}. Call wait_for_turn.",
                view.state_version,
                who.join(", ")
            );
        }
    }
    if !legal.is_empty() {
        s.push_str(&render_legal(legal, view.state_version));
    }
    s
}

/// The numbered list, headed by the state version its ids belong to. An id
/// only means anything for that version and the text half is all an agent
/// reads, so the number has to be here and not only in the JSON.
pub fn render_legal(legal: &[LegalAction], state_version: u64) -> String {
    let mut s = format!("\nLEGAL ACTIONS (state_version {state_version})\n");
    for a in legal {
        let _ = writeln!(s, "  {}. {}", a.id, a.description);
    }
    s.push_str(&combat_composition_hint(legal));
    s
}

/// What the table has said since the agent's last reply, as a section to hang
/// off the end of one. Empty when nobody has spoken.
pub fn render_chat(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut s = String::from("\nTABLE CHAT (new, oldest first — reply with `say`)\n");
    for l in lines {
        let _ = writeln!(s, "  {l}");
    }
    s
}

/// Attack and block declarations are validated by rule, not by list: the
/// listed shapes are suggestions and any legal assignment may be sent as a
/// full `action`. Spell this out, with the legal blocker→attacker pairs.
pub fn combat_composition_hint(legal: &[LegalAction]) -> String {
    let mut s = String::new();
    let singles: Vec<(engine::ObjectId, engine::ObjectId)> = legal
        .iter()
        .filter_map(|a| match &a.action {
            engine::Action::DeclareBlockers { blocks } if blocks.len() == 1 => Some(blocks[0]),
            _ => None,
        })
        .collect();
    if !singles.is_empty() {
        s.push_str("\nBLOCKING: the list above is only the common shapes. You may block with ANY assignment of blockers to attackers ");
        s.push_str("(several blockers on one attacker, different blockers on different attackers, some staying home) by passing a full action to take_action, e.g.\n");
        let example = engine::Action::DeclareBlockers {
            blocks: singles.iter().take(2).copied().collect(),
        };
        let _ = writeln!(s, "  action: {}", serde_json::to_string(&example).unwrap_or_default());
        s.push_str("Each pair is [blocker_id, attacker_id]; a blocker may appear once. Legal pairs:\n");
        let mut blockers: Vec<engine::ObjectId> = singles.iter().map(|(b, _)| *b).collect();
        blockers.sort();
        blockers.dedup();
        for b in blockers {
            let can: Vec<String> = singles.iter().filter(|(x, _)| *x == b).map(|(_, a)| a.to_string()).collect();
            let _ = writeln!(s, "  {b} can block {}", can.join(", "));
        }
    }
    let attack_example = legal.iter().find_map(|a| match &a.action {
        engine::Action::DeclareAttackers { attackers } if attackers.len() == 1 => Some(a.action.clone()),
        _ => None,
    });
    if let Some(example) = attack_example {
        s.push_str(
            "\nATTACKING: any subset of your untapped creatures may attack; pass a full action with every attacker you want, e.g.\n",
        );
        let _ = writeln!(s, "  action: {}", serde_json::to_string(&example).unwrap_or_default());
    }
    s
}

pub fn outcome_text(session: &Session, o: engine::Outcome) -> String {
    let why: Vec<String> = session
        .view()
        .map(|v| {
            v.players
                .iter()
                .filter_map(|p| {
                    p.elimination
                        .as_ref()
                        .map(|e| engine::text::elimination_clause(&p.name, p.seat == session.me, e))
                })
                .collect()
        })
        .unwrap_or_default();
    let why = if why.is_empty() {
        String::new()
    } else {
        format!(" ({})", why.join(", "))
    };
    match o {
        engine::Outcome::Winner(s) if s == session.me => format!("you won{why}"),
        engine::Outcome::Winner(s) => format!("{} won{why}", session.seat_name(s)),
        engine::Outcome::Draw => format!("the game was a draw{why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_on_another_seat_is_written_in_the_third_person() {
        // The bug: "Waiting on Claude 2 to you have priority: cast, …".
        assert_eq!(waiting_text("Claude 2", ActReason::Priority), "Claude 2 to act: they have priority");
        assert_eq!(waiting_text("Claude 2", ActReason::DeclareBlockers), "Claude 2 to declare blockers");
        assert_eq!(waiting_text("Connor", ActReason::Choice), "Connor to make a choice");
        assert_eq!(
            waiting_text("Connor", ActReason::Mulligan),
            "Connor to decide whether to keep their opening hand"
        );
        for r in [
            ActReason::Priority,
            ActReason::DeclareAttackers,
            ActReason::DeclareBlockers,
            ActReason::AssignDamage,
            ActReason::Mulligan,
            ActReason::BottomCards,
            ActReason::Discard,
            ActReason::Choice,
        ] {
            let line = waiting_text("Bo", r);
            assert!(!line.contains("you"), "second person leaked into {line:?}");
        }
    }

    #[test]
    fn the_legal_actions_header_carries_the_state_version() {
        let legal = vec![LegalAction {
            id: 0,
            description: "Pass priority".into(),
            action: engine::Action::PassPriority,
        }];
        let s = render_legal(&legal, 62);
        assert!(s.starts_with("\nLEGAL ACTIONS (state_version 62)\n"), "{s}");
        assert!(s.contains("  0. Pass priority"), "{s}");
    }
}
