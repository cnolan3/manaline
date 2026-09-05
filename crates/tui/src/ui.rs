//! Rendering (§6). Everything reads in monochrome; colour reinforces.

use crate::app::{App, LogKind, Mode};
use engine::{ActReason, AttackTarget, CardType, HandView, ObjectId, ObjectView, Outcome, Seat};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

pub const CARD_W: u16 = 11;
pub const CARD_H: u16 = 5;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let outer = Block::default().borders(Borders::ALL).title(header(app));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if app.view.is_none() {
        draw_lobby(f, app, inner);
        draw_overlays(f, app, area);
        return;
    }

    // Optional side column: stack and/or log.
    let (field, side) = if app.show_log || app.show_stack {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(inner);
        (cols[0], Some(cols[1]))
    } else {
        (inner, None)
    };
    if let Some(side) = side {
        draw_side(f, app, side);
    }

    let opponents = app.opponents();
    let collapsed = opponents.len().saturating_sub(1) as u16;
    let hand_height = 2;
    // Two card rows per field (lands, nonlands); boxes when there is room, chips otherwise.
    let fixed = 1 + collapsed + CENTER_H + 1 + hand_height + 1;
    let row_h = if field.height >= fixed + 4 * CARD_H { CARD_H } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1 + collapsed + 2 * row_h), // opponents
            Constraint::Length(CENTER_H),                  // centre strip
            Constraint::Length(2 * row_h + 1),             // me
            Constraint::Length(hand_height),
            Constraint::Length(1),                         // footer
            Constraint::Min(0),
        ])
        .split(field);

    draw_opponents(f, app, chunks[0], &opponents, row_h);
    draw_center(f, app, chunks[1]);
    draw_me(f, app, chunks[2], row_h);
    draw_hand(f, app, chunks[3]);
    draw_footer(f, app, chunks[4]);
    draw_overlays(f, app, area);
}

pub const CENTER_H: u16 = 3;

/// The strip between the two fields: turn, who must act, and the key to press.
fn draw_center(f: &mut Frame, app: &App, area: Rect) {
    let view = app.view.as_ref().unwrap();
    let mine = Some(view.active_player) == app.me;
    let whose = if mine { "your turn".to_string() } else { format!("{}'s turn", app.seat_name(view.active_player)) };
    let lead = format!(" Turn {} · {} · ", view.turn, view.phase.label());
    let title_len = lead.chars().count() + whose.chars().count() + 1;
    let w = area.width as usize;
    let pad = w.saturating_sub(title_len) / 2;
    let whose_style = if mine && view.outcome.is_none() {
        Style::default().fg(Color::Black).bg(Color::Green).bold()
    } else {
        Style::default().dim()
    };
    let rule = Line::from(vec![
        Span::styled("─".repeat(pad), Style::default().dim()),
        Span::styled(lead, Style::default().dim()),
        Span::styled(whose, whose_style),
        Span::styled(" ", Style::default().dim()),
        Span::styled("─".repeat(w.saturating_sub(pad + title_len)), Style::default().dim()),
    ]);
    f.render_widget(Paragraph::new(rule), Rect::new(area.x, area.y, area.width, 1));

    let (text, style) = match view.outcome {
        Some(Outcome::Winner(s)) if Some(s) == app.me => ("YOU WIN".to_string(), Style::default().fg(Color::Green).bold()),
        Some(Outcome::Winner(s)) => (format!("GAME OVER — {} wins", app.seat_name(s)), Style::default().fg(Color::Red).bold()),
        Some(Outcome::Draw) => ("GAME OVER — draw".into(), Style::default().bold()),
        None => match app.my_reason() {
            Some(ActReason::Priority) => ("YOU HAVE PRIORITY  ·  [Space] pass".into(), Style::default().fg(Color::Green).bold()),
            Some(r) => (
                format!("YOU MUST {}  ·  [Enter] open", crate::app::reason_verb(r).to_uppercase()),
                Style::default().fg(Color::Green).bold(),
            ),
            None => {
                let who: Vec<String> = view
                    .must_act
                    .iter()
                    .map(|(s, r)| format!("{} to {}", app.seat_name(*s), crate::app::reason_verb(*r)))
                    .collect();
                let style = if app.waiting_long() { Style::default().fg(Color::Yellow) } else { Style::default().dim() };
                (format!("Waiting on {}…", who.join(", ")), style)
            }
        },
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(text, style))).centered(),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );

    let third = if !view.stack.is_empty() {
        let top = view.stack.last().unwrap();
        let more = if view.stack.len() > 1 { format!(" (+{} below)", view.stack.len() - 1) } else { String::new() };
        Line::from(format!("Stack: {} {} ({}){more}   [s] details", top.name, top.object, app.seat_name(top.controller)))
            .fg(Color::Magenta)
    } else if let Some((msg, _)) = &app.status {
        Line::from(msg.clone()).fg(Color::Yellow)
    } else {
        Line::from("")
    };
    f.render_widget(Paragraph::new(third).centered(), Rect::new(area.x, area.y + 2, area.width, 1));
}

/// Stack and/or log panes in a column beside the field.
fn draw_side(f: &mut Frame, app: &App, area: Rect) {
    let view = app.view.as_ref().unwrap();
    let both = app.show_log && app.show_stack;
    let (stack_area, log_area) = if both {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(8), Constraint::Min(3)])
            .split(area);
        (Some(rows[0]), Some(rows[1]))
    } else if app.show_stack {
        (Some(area), None)
    } else {
        (None, Some(area))
    };
    if let Some(a) = stack_area {
        let block = Block::default().borders(Borders::ALL).title(" STACK  [s] hide ");
        let inner = block.inner(a);
        f.render_widget(block, a);
        let mut lines: Vec<Line> = Vec::new();
        if view.stack.is_empty() {
            lines.push(Line::from("(empty)").dim());
        } else {
            for (i, so) in view.stack.iter().rev().enumerate() {
                lines.push(Line::from(format!("{}. {} {} ({})", i + 1, so.name, so.object, app.seat_name(so.controller))));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }
    if let Some(a) = log_area {
        let block = Block::default().borders(Borders::ALL).title(" LOG  [l] hide  [PgUp/PgDn] scroll ");
        let inner = block.inner(a);
        f.render_widget(block, a);
        draw_log(f, app, inner);
    }
}

fn header(app: &App) -> String {
    let Some(view) = &app.view else {
        return format!(" manaline ── game {} ── lobby ", app.game_id);
    };
    let status = match view.outcome {
        Some(Outcome::Winner(s)) => format!("GAME OVER — {} wins", app.seat_name(s)),
        Some(Outcome::Draw) => "GAME OVER — draw".into(),
        None => match app.my_reason() {
            Some(ActReason::Priority) => "You have priority".into(),
            Some(r) => format!("You must {}", crate::app::reason_verb(r)),
            None => {
                let who: Vec<String> = view
                    .must_act
                    .iter()
                    .map(|(s, r)| format!("{} (seat {}) to {}", app.seat_name(*s), s.0, crate::app::reason_verb(*r)))
                    .collect();
                if who.is_empty() {
                    "…".into()
                } else {
                    format!("Waiting on {}…", who.join(", "))
                }
            }
        },
    };
    format!(
        " manaline ── Turn {} · {} · {} ── active: {} ── game {} ",
        view.turn,
        view.phase.label(),
        status,
        app.seat_name(view.active_player),
        app.game_id
    )
}

fn draw_lobby(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![
        Line::from(format!("Game {} · format {}", app.game_id, app.format_name)).bold(),
        Line::from(""),
    ];
    for s in &app.lobby.seats {
        let name = s.name.clone().unwrap_or_else(|| "(empty)".into());
        let you = if Some(s.seat) == app.me { " (you)" } else { "" };
        let state = match (s.connected, s.deck_ok, s.ready) {
            (false, _, _) => "not connected",
            (true, false, _) => "connected, no deck yet",
            (true, true, false) => "deck submitted, not ready",
            (true, true, true) => "ready",
        };
        lines.push(Line::from(format!("  seat {}  {name}{you}  —  {state}", s.seat.0)));
    }
    lines.push(Line::from(""));
    if !app.lobby.started {
        lines.push(Line::from("Waiting for every seat to be ready…").italic());
    }
    for h in &app.hints {
        lines.push(Line::from(""));
        for l in h.lines() {
            lines.push(Line::from(l.to_string()));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(app.footer()).dim());
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_opponents(f: &mut Frame, app: &App, area: Rect, opponents: &[Seat], row_h: u16) {
    let view = app.view.as_ref().unwrap();
    if opponents.is_empty() {
        return;
    }
    let expanded = opponents[app.expanded_opponent.min(opponents.len() - 1)];
    let mut y = area.y;
    for &seat in opponents {
        let p = view.player(seat);
        let creatures = p.battlefield.iter().filter(|id| view.object(**id).map(|o| o.pt.is_some()).unwrap_or(false)).count();
        let tag = if opponents.len() > 1 { "  [Tab]" } else { "" };
        if seat == expanded {
            let line = Line::from(vec![
                Span::styled(format!("{} (seat {})", p.name, seat.0), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(if p.eliminated {
                    "  ELIMINATED".to_string()
                } else {
                    format!("   ♥ {}   Hand {}   Library {}   Graveyard {}{tag}", p.life, p.hand.count(), p.library.count, p.graveyard.len())
                }),
            ]);
            f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
            y += 1;
            let (lands, others) = split_field(app, &p.battlefield);
            // Mirrored table: the opponent's lands are farthest from the centre.
            draw_row(f, app, Rect::new(area.x, y, area.width, row_h), &lands, None);
            y += row_h;
            draw_row(f, app, Rect::new(area.x, y, area.width, row_h), &others, None);
            y += row_h;
        } else {
            let line = format!(
                "{} (seat {})   ♥ {}  hand {}  lib {}  creatures {}{}",
                p.name,
                seat.0,
                p.life,
                p.hand.count(),
                p.library.count,
                creatures,
                if p.eliminated { "  ELIMINATED" } else { "" }
            );
            f.render_widget(Paragraph::new(line).dim(), Rect::new(area.x, y, area.width, 1));
            y += 1;
        }
    }
}

/// Lands and everything else, each in id order.
fn split_field(app: &App, ids: &[ObjectId]) -> (Vec<ObjectId>, Vec<ObjectId>) {
    let view = app.view.as_ref().unwrap();
    let mut lands = Vec::new();
    let mut others = Vec::new();
    for &id in ids {
        match view.object(id) {
            Some(o) if o.types.contains(&CardType::Land) => lands.push(id),
            Some(_) => others.push(id),
            None => {}
        }
    }
    lands.sort();
    others.sort();
    (lands, others)
}

fn draw_log(f: &mut Frame, app: &App, area: Rect) {
    let height = area.height as usize;
    let total = app.log.len();
    let end = total.saturating_sub(app.log_scroll);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = app.log[start..end]
        .iter()
        .map(|l| {
            let prefix = format!("T{:<3}", l.turn);
            let style = match l.kind {
                LogKind::Chat => Style::default().fg(Color::Cyan),
                LogKind::System => Style::default().fg(Color::Yellow),
                LogKind::Game => Style::default(),
            };
            Line::from(vec![Span::styled(prefix, Style::default().dim()), Span::styled(l.text.clone(), style)])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_me(f: &mut Frame, app: &App, area: Rect, row_h: u16) {
    let view = app.view.as_ref().unwrap();
    let Some(me) = app.me else {
        f.render_widget(Paragraph::new("Spectating").dim(), area);
        return;
    };
    let p = view.player(me);
    let highlight = picker_highlight(app);
    let (lands, others) = split_field(app, &p.battlefield);
    let mut y = area.y;
    draw_row(f, app, Rect::new(area.x, y, area.width, row_h), &others, highlight.as_ref());
    y += row_h;
    draw_row(f, app, Rect::new(area.x, y, area.width, row_h), &lands, highlight.as_ref());
    y += row_h;
    let line = Line::from(vec![
        Span::styled(format!("YOU — {} (seat {})", p.name, me.0), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "   ♥ {}   Library {}   Graveyard {}   Pool: {}",
            p.life,
            p.library.count,
            p.graveyard.len(),
            p.mana_pool.as_ref().map(|m| m.to_string()).unwrap_or_else(|| "-".into())
        )),
    ]);
    f.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
}

/// Which of my permanents an open picker is highlighting: (ids marked, cursor id).
fn picker_highlight(app: &App) -> Option<(Vec<ObjectId>, Option<ObjectId>)> {
    match &app.mode {
        Mode::Attack(p) => Some((
            p.candidates.iter().zip(&p.choice).filter(|(_, c)| c.is_some()).map(|(id, _)| *id).collect(),
            p.candidates.get(p.cursor).copied(),
        )),
        Mode::Block(p) => Some((
            p.blockers.iter().zip(&p.choice).filter(|(_, c)| c.is_some()).map(|(id, _)| *id).collect(),
            p.blockers.get(p.cursor).copied(),
        )),
        _ => None,
    }
}

fn draw_row(
    f: &mut Frame,
    app: &App,
    area: Rect,
    ids: &[ObjectId],
    highlight: Option<&(Vec<ObjectId>, Option<ObjectId>)>,
) {
    let view = app.view.as_ref().unwrap();
    let objects: Vec<&ObjectView> = ids.iter().filter_map(|id| view.object(*id)).collect();
    if area.height < CARD_H {
        draw_chip_row(f, area, &objects, highlight);
        return;
    }
    let per_row = (area.width / CARD_W).max(1) as usize;
    let mut x = area.x;
    for (i, o) in objects.iter().enumerate() {
        if i + 1 == per_row && objects.len() > per_row {
            let more = objects.len() - i;
            f.render_widget(
                Paragraph::new(format!("+{more}\nmore")).dim().block(Block::default().borders(Borders::ALL)),
                Rect::new(x, area.y, CARD_W, CARD_H),
            );
            break;
        }
        let marked = highlight.map(|(m, _)| m.contains(&o.id)).unwrap_or(false);
        let cursor = highlight.and_then(|(_, c)| *c) == Some(o.id);
        draw_card(f, o, Rect::new(x, area.y, CARD_W, CARD_H), marked, cursor);
        x += CARD_W;
    }
}

/// One-line rendering for short terminals: `Forest{G}  Grizzly Bears 2/2 T`.
fn draw_chip_row(f: &mut Frame, area: Rect, objects: &[&ObjectView], highlight: Option<&(Vec<ObjectId>, Option<ObjectId>)>) {
    let mut spans: Vec<Span> = Vec::new();
    for o in objects {
        let mut text = o.name.clone();
        match o.pt {
            Some((p, t)) => text.push_str(&format!(" {p}/{t}")),
            None => {
                for c in &o.produces {
                    text.push_str(&format!("{{{}}}", c.symbol()));
                }
            }
        }
        if o.damage > 0 {
            text.push_str(&format!("({})", o.damage));
        }
        if o.summoning_sick && o.pt.is_some() {
            text.push('★');
        }
        if o.tapped {
            text.push_str(" T");
        }
        let mut style = Style::default();
        if o.tapped {
            style = style.add_modifier(Modifier::DIM);
        }
        if o.attacking.is_some() {
            style = style.fg(Color::Red);
        }
        if !o.blocking.is_empty() {
            style = style.fg(Color::Blue);
        }
        if highlight.map(|(m, _)| m.contains(&o.id)).unwrap_or(false) {
            style = style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
        }
        if highlight.and_then(|(_, c)| *c) == Some(o.id) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        spans.push(Span::styled(format!("[{text}]"), style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn draw_card(f: &mut Frame, o: &ObjectView, area: Rect, marked: bool, cursor: bool) {
    let mut style = Style::default();
    if o.tapped {
        style = style.add_modifier(Modifier::DIM);
    }
    let mut border = Style::default();
    if o.attacking.is_some() {
        border = border.fg(Color::Red);
    }
    if !o.blocking.is_empty() {
        border = border.fg(Color::Blue);
    }
    if marked {
        border = border.fg(Color::Yellow).add_modifier(Modifier::BOLD);
    }
    if cursor {
        border = border.add_modifier(Modifier::REVERSED);
    }
    let width = (CARD_W - 2) as usize;
    let (name1, name2) = wrap_name(&o.name, width);
    let mut stats = match o.pt {
        Some((p, t)) => format!("{p}/{t}"),
        None if o.types.contains(&CardType::Land) => o.produces.iter().map(|c| format!("{{{}}}", c.symbol())).collect(),
        None => String::new(),
    };
    if o.damage > 0 {
        stats.push_str(&format!("({})", o.damage));
    }
    if o.summoning_sick && o.pt.is_some() {
        stats.push('★');
    }
    let tapped = if o.tapped { "T" } else { " " };
    let last = format!("{}{tapped}", fit(&stats, width - 1).chars().chain(std::iter::repeat(' ')).take(width - 1).collect::<String>());
    let lines = vec![Line::from(name1), Line::from(name2), Line::from(last)];
    let block = Block::default().borders(Borders::ALL).border_style(border);
    f.render_widget(Paragraph::new(lines).style(style).block(block), area);
}

/// Split a card name over two lines of `w` characters, breaking at spaces.
fn wrap_name(name: &str, w: usize) -> (String, String) {
    let mut first = String::new();
    let mut rest: Vec<&str> = Vec::new();
    for word in name.split_whitespace() {
        let candidate = if first.is_empty() { word.to_string() } else { format!("{first} {word}") };
        if candidate.chars().count() <= w && rest.is_empty() {
            first = candidate;
        } else {
            rest.push(word);
        }
    }
    if first.is_empty() {
        return (fit(name, w), String::new());
    }
    (first, fit(&rest.join(" "), w))
}

fn fit(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    if s.chars().count() > w && w > 1 {
        out.pop();
        out.push('…');
    }
    out
}

fn draw_hand(f: &mut Frame, app: &App, area: Rect) {
    let view = app.view.as_ref().unwrap();
    let Some(me) = app.me else { return };
    let HandView::Yours(hand) = &view.player(me).hand else { return };
    let mut spans: Vec<Span> = vec![Span::styled("HAND ", Style::default().add_modifier(Modifier::BOLD))];
    if hand.is_empty() {
        spans.push(Span::raw("(empty)").dim());
    }
    for (i, id) in hand.iter().enumerate() {
        let Some(o) = view.object(*id) else { continue };
        let key = if i < 9 { (i + 1).to_string() } else if i == 9 { "0".into() } else { "-".into() };
        let cost = if o.types.contains(&CardType::Land) { String::new() } else { format!(" {}", o.cost) };
        let text = format!("[{key}] {}{cost}", o.name);
        let style = if o.castable { Style::default().add_modifier(Modifier::BOLD) } else { Style::default().dim() };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw("   "));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).wrap(Wrap { trim: true }), area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let text = app.footer();
    let style = if app.waiting_long() && app.my_reason().is_none() && app.outcome().is_none() {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// A popup anchored near the top of the screen, over the opponent's field,
/// so the hand and your own permanents stay visible while deciding.
fn top_anchored(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let max_h = (area.height / 2).saturating_sub(1).max(3);
    let h = height.min(max_h);
    Rect::new(area.x + (area.width - w) / 2, area.y + 2, w, h)
}

fn popup(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, width: u16) {
    let height = lines.len() as u16 + 2;
    let rect = top_anchored(area, width, height);
    f.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(format!(" {title} "));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), rect);
}

fn draw_overlays(f: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        Mode::Normal => {}
        Mode::Menu(menu) => {
            let width = menu.items.iter().map(|i| i.label.chars().count() as u16 + 6).max().unwrap_or(20).max(menu.title.len() as u16 + 4).min(area.width - 4);
            let rect = top_anchored(area, width, menu.items.len() as u16 + 2);
            let height = rect.height;
            f.render_widget(Clear, rect);
            let visible = (height - 2) as usize;
            let start = menu.selected.saturating_sub(visible.saturating_sub(1));
            let items: Vec<ListItem> = menu
                .items
                .iter()
                .enumerate()
                .skip(start)
                .take(visible)
                .map(|(i, item)| {
                    let key = if i < 9 { format!("{}", i + 1) } else { " ".into() };
                    let text = format!("{key} {}", item.label);
                    let style = if i == menu.selected { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                    ListItem::new(text).style(style)
                })
                .collect();
            let block = Block::default().borders(Borders::ALL).title(format!(" {} ", menu.title));
            f.render_widget(List::new(items).block(block), rect);
        }
        Mode::Attack(p) => {
            let mut lines = vec![Line::from("Space toggles; Tab picks whom to attack.").dim()];
            for (i, (id, choice)) in p.candidates.iter().zip(&p.choice).enumerate() {
                let target = match choice {
                    Some(t) => format!("→ {}", app.seat_name(p.targets[*t])),
                    None => "stays home".into(),
                };
                let mark = if choice.is_some() { "[x]" } else { "[ ]" };
                let text = format!("{mark} {} {target}", app.object_label(*id));
                let style = if i == p.cursor { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                lines.push(Line::styled(text, style));
            }
            popup(f, area, "Declare attackers", lines, 50);
        }
        Mode::Block(p) => {
            let mut lines = vec![Line::from("Space toggles; Tab picks which attacker to block.").dim()];
            for (i, (id, choice)) in p.blockers.iter().zip(&p.choice).enumerate() {
                let target = match choice {
                    Some(a) => format!("blocks {}", app.object_label(p.attackers[*a])),
                    None => "doesn't block".into(),
                };
                let mark = if choice.is_some() { "[x]" } else { "[ ]" };
                let text = format!("{mark} {} {target}", app.object_label(*id));
                let style = if i == p.cursor { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                lines.push(Line::styled(text, style));
            }
            lines.push(Line::from(""));
            let attackers: Vec<String> = p.attackers.iter().map(|a| app.object_label(*a)).collect();
            lines.push(Line::from(format!("Attacking you: {}", attackers.join(", "))).dim());
            popup(f, area, "Declare blockers", lines, 56);
        }
        Mode::Damage(p) => {
            let total: i32 = p.amounts.iter().sum();
            let mut lines = vec![Line::from(format!(
                "{} deals {} damage; {total} assigned.",
                app.object_label(p.attacker),
                p.power
            ))];
            for (i, (id, n)) in p.blockers.iter().zip(&p.amounts).enumerate() {
                let text = format!("{:>2} → {}", n, app.object_label(*id));
                let style = if i == p.cursor { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                lines.push(Line::styled(text, style));
            }
            popup(f, area, "Assign combat damage", lines, 50);
        }
        Mode::Chat(text) => {
            let lines = vec![Line::from(format!("> {text}_"))];
            popup(f, area, "Say", lines, 60);
        }
        Mode::Inspect(id) => {
            let mut lines = Vec::new();
            if let Some(o) = app.view.as_ref().and_then(|v| v.object(*id)) {
                let types: Vec<String> = o.types.iter().map(|t| format!("{t:?}")).collect();
                let mut line = types.join(" ");
                if !o.subtypes.is_empty() {
                    line.push_str(" — ");
                    line.push_str(&o.subtypes.join(" "));
                }
                lines.push(Line::from(format!("{} {}", o.name, o.cost)).bold());
                lines.push(Line::from(line));
                if let Some((pw, t)) = o.pt {
                    lines.push(Line::from(format!("{pw}/{t}{}", if o.damage > 0 { format!(" ({} damage marked)", o.damage) } else { String::new() })));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(if o.text.is_empty() { "(no rules text)".to_string() } else { o.text.clone() }));
                lines.push(Line::from(""));
                let mut state = vec![format!("{:?}", o.zone).to_lowercase(), format!("controlled by {}", app.seat_name(o.controller))];
                if o.tapped {
                    state.push("tapped".into());
                }
                if o.summoning_sick && o.pt.is_some() {
                    state.push("summoning sick".into());
                }
                if let Some(AttackTarget::Player(s)) = o.attacking {
                    state.push(format!("attacking {}", app.seat_name(s)));
                }
                if !o.blocking.is_empty() {
                    let b: Vec<String> = o.blocking.iter().map(|a| app.object_label(*a)).collect();
                    state.push(format!("blocking {}", b.join(", ")));
                }
                lines.push(Line::from(state.join(", ")).dim());
            } else {
                lines.push(Line::from(format!("{id} is not visible")));
            }
            let rect = centered(area, 60, lines.len() as u16 + 2);
            f.render_widget(Clear, rect);
            let block = Block::default().borders(Borders::ALL).title(format!(" {id} "));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block), rect);
        }
        Mode::Help => {
            let lines: Vec<Line> = [
                "Space      pass priority (or open the pending decision)",
                "1-9, 0     play or cast the card at that hand position",
                "a / b / d  declare attackers / blockers / assign damage",
                "m          mulligan decision",
                "i          inspect a card",
                "c          chat with the table",
                "Enter      nudge whoever the game is waiting on",
                "Tab        expand the next opponent",
                "l / s      show or hide the log / the stack",
                "PgUp/PgDn  scroll the log",
                "v          toggle verbose log",
                "x          concede",
                "q          quit",
            ]
            .iter()
            .map(|s| Line::from(*s))
            .collect();
            let rect = centered(area, 62, lines.len() as u16 + 2);
            f.render_widget(Clear, rect);
            let block = Block::default().borders(Borders::ALL).title(" Keys ");
            f.render_widget(Paragraph::new(lines).block(block), rect);
        }
        Mode::ConfirmConcede => {
            popup(f, area, "Concede", vec![Line::from("Concede the game? Press y to confirm.")], 44);
        }
    }
}
