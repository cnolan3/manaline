//! Plain-text rendering: events for logs, actions for menus, and the compact
//! state view models read (§7.2).

use crate::action::{Action, AttackTarget, DamageTarget};
use crate::event::{Event, EventBase};
use crate::game::Game;
use crate::types::{CardType, Seat, Zone};
use crate::view::{GameView, HandView};
use std::fmt::Write;

fn obj(game: &Game, id: crate::types::ObjectId) -> String {
    match game.objects.get(id) {
        Some(o) => format!("{} {id}", game.cards().get(o.card).name),
        None => format!("{id}"),
    }
}

fn who(game: &Game, seat: Seat) -> String {
    format!("{} ({seat})", game.player_name(seat))
}

fn damage_target(game: &Game, t: DamageTarget) -> String {
    match t {
        DamageTarget::Player(s) => who(game, s),
        DamageTarget::Object(o) => obj(game, o),
    }
}

fn attack_target(game: &Game, t: AttackTarget) -> String {
    match t {
        AttackTarget::Player(s) => who(game, s),
        AttackTarget::Planeswalker(o) => obj(game, o),
    }
}

/// One line per event, with card names, from inside the engine.
pub fn describe_event(game: &Game, e: &Event) -> String {
    describe_event_with(e, |c| c.len(), &|id| obj(game, id), &|s| who(game, s))
}

/// One line per seat-filtered event, for clients that only hold a view.
/// `names` resolves object ids (clients keep a cache from the views they
/// have seen); `seats` resolves seat names.
pub fn describe_event_view(
    e: &crate::event::EventView,
    names: &dyn Fn(crate::types::ObjectId) -> String,
    seats: &dyn Fn(Seat) -> String,
) -> String {
    describe_event_with(
        e,
        |c| match c {
            crate::event::DrawnCards::Yours(v) => v.len(),
            crate::event::DrawnCards::Hidden { count } => *count as usize,
        },
        names,
        seats,
    )
}

fn describe_event_with<D>(
    e: &EventBase<D>,
    drawn: impl Fn(&D) -> usize,
    names: &dyn Fn(crate::types::ObjectId) -> String,
    seats: &dyn Fn(Seat) -> String,
) -> String {
    let obj = |id: crate::types::ObjectId| names(id);
    let who = |s: Seat| seats(s);
    let damage_target = |t: DamageTarget| match t {
        DamageTarget::Player(s) => who(s),
        DamageTarget::Object(o) => obj(o),
    };
    let attack_target = |t: AttackTarget| match t {
        AttackTarget::Player(s) => who(s),
        AttackTarget::Planeswalker(o) => obj(o),
    };
    match e {
        EventBase::GameStarted { starting_player, seats } => {
            format!("Game started with {seats} seats; {} plays first", who(*starting_player))
        }
        EventBase::Drew { seat, cards } => format!("{} draws {} card(s)", who(*seat), drawn(cards)),
        EventBase::Shuffled { seat } => format!("{} shuffles", who(*seat)),
        EventBase::MulliganTaken { seat, to } => format!("{} mulligans to {to}", who(*seat)),
        EventBase::HandKept { seat, size } => format!("{} keeps {size}", who(*seat)),
        EventBase::Bottomed { seat, count } => format!("{} puts {count} card(s) on the bottom", who(*seat)),
        EventBase::TurnStarted { turn, active } => format!("--- Turn {turn}: {} ---", who(*active)),
        EventBase::PhaseChanged { phase } => format!("[{phase}]"),
        EventBase::PriorityPassed { seat } => format!("{} passes", who(*seat)),
        EventBase::LandPlayed { seat, object } => format!("{} plays {}", who(*seat), obj(*object)),
        EventBase::Cast { seat, object, .. } => format!("{} casts {}", who(*seat), obj(*object)),
        EventBase::Resolved { object } => format!("{} resolves", obj(*object)),
        EventBase::Tapped { object } => format!("{} taps", obj(*object)),
        EventBase::Untapped { object } => format!("{} untaps", obj(*object)),
        EventBase::ManaAdded { seat, mana, amount } => format!("{} adds {amount}×{mana}", who(*seat)),
        EventBase::Attacked { seat, attackers } => {
            if attackers.is_empty() {
                return format!("{} declares no attackers", who(*seat));
            }
            let list: Vec<String> = attackers
                .iter()
                .map(|(a, t)| format!("{} → {}", obj(*a), attack_target(*t)))
                .collect();
            format!("{} attacks: {}", who(*seat), list.join(", "))
        }
        EventBase::Blocked { seat, blocks } => {
            if blocks.is_empty() {
                return format!("{} declares no blockers", who(*seat));
            }
            let list: Vec<String> = blocks
                .iter()
                .map(|(b, a)| format!("{} blocks {}", obj(*b), obj(*a)))
                .collect();
            format!("{}: {}", who(*seat), list.join(", "))
        }
        EventBase::DamageAssigned { attacker, assignments } => {
            let list: Vec<String> = assignments
                .iter()
                .map(|(t, n)| format!("{n} to {}", damage_target(*t)))
                .collect();
            format!("{} assigns {}", obj(*attacker), list.join(", "))
        }
        EventBase::Damage { source, to, amount } => {
            format!("{} deals {amount} to {}", obj(*source), damage_target(*to))
        }
        EventBase::LifeChanged { seat, from, to } => format!("{}: {from} → {to} life", who(*seat)),
        EventBase::ZoneChange { object, from, to } => {
            let verb = match (from, to) {
                (Zone::Battlefield, Zone::Graveyard) => "dies".to_string(),
                (_, Zone::Battlefield) => "enters the battlefield".to_string(),
                (_, Zone::OutOfGame) => "leaves the game".to_string(),
                (f, t) => format!("moves from {f:?} to {t:?}"),
            };
            format!("{} {verb}", obj(*object))
        }
        EventBase::Discarded { seat, objects } => {
            let list: Vec<String> = objects.iter().map(|o| obj(*o)).collect();
            format!("{} discards {}", who(*seat), list.join(", "))
        }
        EventBase::Eliminated { seat, reason } => format!("{} loses ({reason:?})", who(*seat)),
        EventBase::GameOver { outcome } => match outcome {
            crate::game::Outcome::Winner(s) => format!("=== {} wins ===", who(*s)),
            crate::game::Outcome::Draw => "=== The game is a draw ===".into(),
        },
        EventBase::Chat { from, to, text } => match to {
            Some(t) => format!("{} → {}: \"{text}\"", who(*from), who(*t)),
            None => format!("{}: \"{text}\"", who(*from)),
        },
    }
}

/// A short human-readable description of an action, for menus and error messages.
pub fn describe_action(game: &Game, a: &Action) -> String {
    match a {
        Action::PassPriority => "Pass priority".into(),
        Action::PlayLand { object } => format!("Play {}", obj(game, *object)),
        Action::CastSpell { object, payment, .. } | Action::CastCommander { object, payment, .. } => {
            let cost = game.objects.get(*object).map(|o| game.cards().get(o.card).cost.to_string()).unwrap_or_default();
            let mut s = format!("Cast {} {cost}", obj(game, *object));
            if !payment.tap.is_empty() {
                let taps: Vec<String> = payment.tap.iter().map(|t| t.to_string()).collect();
                write!(s, " (tap {})", taps.join(", ")).unwrap();
            }
            if !payment.from_pool.is_empty() {
                let pool: String = payment.from_pool.iter().map(|m| m.to_string()).collect();
                write!(s, " (from pool {pool})").unwrap();
            }
            s
        }
        Action::ActivateAbility { object, ability, .. } => format!("Activate ability {ability} of {}", obj(game, *object)),
        Action::DeclareAttackers { attackers } => {
            if attackers.is_empty() {
                return "Attack with nothing".into();
            }
            let list: Vec<String> = attackers
                .iter()
                .map(|(a, t)| format!("{} → {}", obj(game, *a), attack_target(game, *t)))
                .collect();
            format!("Attack: {}", list.join(", "))
        }
        Action::DeclareBlockers { blocks } => {
            if blocks.is_empty() {
                return "Block with nothing".into();
            }
            let list: Vec<String> = blocks
                .iter()
                .map(|(b, a)| format!("{} blocks {}", obj(game, *b), obj(game, *a)))
                .collect();
            format!("Block: {}", list.join(", "))
        }
        Action::AssignCombatDamage { attacker, assignments } => {
            let list: Vec<String> = assignments
                .iter()
                .map(|(t, n)| format!("{n} to {}", damage_target(game, *t)))
                .collect();
            format!("{} assigns {}", obj(game, *attacker), list.join(", "))
        }
        Action::ChooseTargets { .. } => "Choose targets".into(),
        Action::ChooseMode { mode } => format!("Choose mode {mode}"),
        Action::Discard { objects } => {
            let list: Vec<String> = objects.iter().map(|o| obj(game, *o)).collect();
            format!("Discard {}", list.join(", "))
        }
        Action::Mulligan { keep: true } => "Keep this hand".into(),
        Action::Mulligan { keep: false } => "Mulligan".into(),
        Action::BottomCards { objects } => {
            let list: Vec<String> = objects.iter().map(|o| obj(game, *o)).collect();
            format!("Put on the bottom: {}", list.join(", "))
        }
        Action::CommanderToCommandZone { object } => format!("Return {} to the command zone", obj(game, *object)),
        Action::Concede => "Concede".into(),
    }
}

/// The compact text view (§7.2). `you` is labelled YOU; other seats by name and seat.
pub fn render_view(v: &GameView) -> String {
    let mut s = String::new();
    let seat_label = |seat: Seat| -> String {
        if v.you == Some(seat) {
            "YOU".to_string()
        } else {
            format!("{} (seat {})", v.players[seat.index()].name, seat.0)
        }
    };
    let must: Vec<String> = v.must_act.iter().map(|(s, r)| format!("{} ({r:?})", seat_label(*s))).collect();
    writeln!(
        s,
        "TURN {} · {} · Active: {} · Must act: {}",
        v.turn,
        v.phase.label().to_uppercase(),
        seat_label(v.active_player),
        if must.is_empty() { "-".to_string() } else { must.join(", ") }
    )
    .unwrap();
    if let Some(o) = v.outcome {
        writeln!(s, "GAME OVER: {o:?}").unwrap();
    }
    if v.stack.is_empty() {
        writeln!(s, "Stack: (empty)").unwrap();
    } else {
        writeln!(s, "Stack (top last):").unwrap();
        for so in &v.stack {
            writeln!(s, "  {} {} ({})", so.name, so.object, seat_label(so.controller)).unwrap();
        }
    }
    let describe = |id: crate::types::ObjectId| -> String {
        let Some(o) = v.objects.get(&id) else { return id.to_string() };
        let mut d = format!("{} {}", id, o.name);
        if let Some((p, t)) = o.pt {
            write!(d, " {p}/{t}").unwrap();
            if o.damage > 0 {
                write!(d, " ({} dmg)", o.damage).unwrap();
            }
        } else if !o.types.contains(&CardType::Land) {
            write!(d, " {}", o.cost).unwrap();
        }
        let mut flags = Vec::new();
        if o.tapped {
            flags.push("T");
        }
        if o.summoning_sick && o.pt.is_some() {
            flags.push("sick");
        }
        if o.attacking.is_some() {
            flags.push("attacking");
        }
        if !o.blocking.is_empty() {
            flags.push("blocking");
        }
        if !flags.is_empty() {
            write!(d, " ({})", flags.join(", ")).unwrap();
        }
        d
    };
    for p in &v.players {
        let label = seat_label(p.seat);
        if p.eliminated {
            writeln!(s, "\n{label}  (eliminated)").unwrap();
            continue;
        }
        writeln!(
            s,
            "\n{label}  life {}  hand {}  library {}  grave {}{}",
            p.life,
            p.hand.count(),
            p.library.count,
            p.graveyard.len(),
            p.mana_pool.as_ref().map(|m| format!("  pool: {m}")).unwrap_or_default()
        )
        .unwrap();
        if !p.battlefield.is_empty() {
            let list: Vec<String> = p.battlefield.iter().map(|&id| describe(id)).collect();
            writeln!(s, "  battlefield: {}", list.join("  ")).unwrap();
        }
        if let HandView::Yours(hand) = &p.hand {
            let list: Vec<String> = hand
                .iter()
                .map(|&id| {
                    let mut d = describe(id);
                    if v.objects.get(&id).map(|o| o.castable).unwrap_or(false) {
                        d.push_str(" (castable)");
                    }
                    d
                })
                .collect();
            writeln!(s, "  hand: {}", list.join("  ")).unwrap();
        }
    }
    s
}
