//! Drawing the deckbuilder: search | deck | detail-or-stats.

use crate::editor::{Editor, Focus, Row};
use deckstats::CardStatus;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

pub fn draw(f: &mut Frame, ed: &Editor) {
    let area = f.area();
    let title = match &ed.path {
        Some(p) => format!(" manaline deck edit — {}{} ", p.display(), if ed.dirty { " *" } else { "" }),
        None => format!(" manaline deck edit{} ", if ed.dirty { " *" } else { "" }),
    };
    let outer = Block::default().borders(Borders::ALL).title(title);
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)])
        .split(inner);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(34), Constraint::Percentage(34)])
        .split(rows[0]);

    draw_search(f, ed, cols[0]);
    draw_deck(f, ed, cols[1]);
    if ed.show_stats {
        draw_stats(f, ed, cols[2]);
    } else {
        draw_detail(f, ed, cols[2]);
    }

    // Status line: legality (or a transient message) and the footer.
    let status = match &ed.status {
        Some(s) => Line::from(s.clone()).fg(ed.theme.warn),
        None if ed.report.is_legal() => Line::from(ed.legality_line()).fg(ed.theme.good),
        None => Line::from(ed.legality_line()).fg(ed.theme.danger),
    };
    f.render_widget(Paragraph::new(status), rows[1]);
    f.render_widget(Paragraph::new(Line::from(ed.footer()).dim()), rows[2]);

    if ed.show_help {
        draw_help(f, area);
    }
}

fn pane(title: &str, focused: bool, theme: crate::theme::Theme) -> Block<'static> {
    let style = if focused {
        Style::default().fg(theme.focus)
    } else {
        Style::default().dim()
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(format!(" {title} "))
}

fn draw_search(f: &mut Frame, ed: &Editor, area: Rect) {
    let focused = ed.focus == Focus::Search;
    let block = pane("Search", focused, ed.theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let cursor = if focused { "_" } else { "" };
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().bold()),
        Span::raw(format!("{}{cursor}", ed.query)),
    ]);
    f.render_widget(Paragraph::new(prompt), Rect::new(inner.x, inner.y, inner.width, 1));
    let hint = if ed.query.is_empty() {
        format!("{} playable cards · try t:creature c:g mv<=2", ed.results.len())
    } else {
        format!("{} match", ed.results.len())
    };
    f.render_widget(
        Paragraph::new(Line::from(hint).dim()),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    let visible = list.height as usize;
    if visible == 0 {
        return;
    }
    let first = ed
        .results_cursor
        .saturating_sub(visible.saturating_sub(1))
        .min(ed.results.len().saturating_sub(visible));
    let mut lines = Vec::new();
    for (i, e) in ed.results.iter().enumerate().skip(first).take(visible) {
        let in_deck = ed
            .main
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(&e.name))
            .map(|(_, c)| *c)
            .unwrap_or(0);
        let mark = if in_deck > 0 { format!("{in_deck:>2}×") } else { "   ".into() };
        let pt = e.pt().map(|(p, t)| format!(" {p}/{t}")).unwrap_or_default();
        let text = fit(&format!("{mark} {} {}{pt}", e.name, e.mana_cost), list.width as usize);
        let style = if i == ed.results_cursor && focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else if i == ed.results_cursor {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::styled(text, style));
    }
    f.render_widget(Paragraph::new(lines), list);
}

fn draw_deck(f: &mut Frame, ed: &Editor, area: Rect) {
    let focused = ed.focus == Focus::Deck;
    let block = pane(&format!("Deck · {} cards", ed.card_count()), focused, ed.theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = ed.rows();
    let mut lines: Vec<Line> = Vec::new();
    let mut card_index = 0usize;
    let mut cursor_line = 0usize;
    for r in &rows {
        match r {
            Row::Header(title, n) => lines.push(Line::from(format!("{title} ({n})")).bold().underlined()),
            Row::Card { name, count } => {
                let cost = ed.card_cost(name);
                let mut text = format!("{count:>2} {name} {cost}");
                let mut style = Style::default();
                match ed.status_of(name) {
                    Some(CardStatus::Ok) | None => {}
                    Some(s) => {
                        text.push_str(&format!("  ← {s}"));
                        style = style.fg(ed.theme.danger);
                    }
                }
                if ed.agent_marked(name) {
                    text.push_str("  ◆ agent");
                    style = style.fg(ed.theme.warn).add_modifier(Modifier::BOLD);
                }
                if card_index == ed.deck_cursor {
                    cursor_line = lines.len();
                    style = if focused {
                        style.add_modifier(Modifier::REVERSED)
                    } else {
                        style.add_modifier(Modifier::BOLD)
                    };
                }
                lines.push(Line::styled(fit(&text, inner.width as usize), style));
                card_index += 1;
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("empty — search on the left and press Enter to add cards").dim());
    }
    let visible = inner.height as usize;
    let first = cursor_line
        .saturating_sub(visible.saturating_sub(1))
        .min(lines.len().saturating_sub(visible));
    let shown: Vec<Line> = lines.into_iter().skip(first).take(visible).collect();
    f.render_widget(Paragraph::new(shown), inner);
}

fn draw_detail(f: &mut Frame, ed: &Editor, area: Rect) {
    let block = pane("Card", false, ed.theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(e) = ed.detail_card() else {
        f.render_widget(Paragraph::new(Line::from("select a card").dim()), inner);
        return;
    };
    let mut lines = crate::cardbox::render(&crate::cardbox::CardFace::from_entry(&e), inner.width.min(40));
    lines.push(String::new());
    if !e.legal_in.is_empty() {
        let mut legal = e.legal_in.clone();
        legal.retain(|f| ["standard", "pioneer", "modern", "legacy", "vintage", "commander", "pauper"].contains(&f.as_str()));
        if !legal.is_empty() {
            lines.push(format!("legal: {}", legal.join(", ")));
        }
    }
    if let Some(s) = ed.status_of(&e.name) {
        if *s != CardStatus::Ok {
            lines.push(format!("in this deck: {s}"));
        }
    }
    let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

fn draw_stats(f: &mut Frame, ed: &Editor, area: Rect) {
    let block = pane("Stats", false, ed.theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut text = deckstats::stats::render(&ed.stats, &ed.format.name);
    if !ed.hands.is_empty() {
        text.push_str("\nsample opening hands ([h] for new ones):\n");
        for h in &ed.hands {
            text.push_str("  ");
            text.push_str(&h.join(", "));
            text.push('\n');
        }
    }
    let lines: Vec<Line> = text.lines().map(|l| Line::from(l.to_string())).collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let lines: Vec<Line> = [
        "Search pane: type a query (Scryfall syntax: t:creature c:g mv<=2 o:\"draw a card\" kw:flying).",
        "  ↑↓ move · Enter add one · Esc clear · Tab to the deck pane",
        "Deck pane: ↑↓/jk move · + or Enter add · - remove one · x remove all · / back to search",
        "  s save · u undo · t stats pane · h new sample hands · r reload from disk · q quit",
        "Anywhere: Ctrl-S save · Ctrl-Z undo · Ctrl-T stats · Ctrl-H hands · Ctrl-Q quit · F1 this help",
        "",
        "Only cards the engine can play are offered. The file is written in canonical order",
        "(by type, then mana value, then name) so diffs stay readable.",
        "",
        "press any key",
    ]
    .iter()
    .map(|s| Line::from(*s))
    .collect();
    let w = 88.min(area.width.saturating_sub(2));
    let h = lines.len() as u16 + 2;
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h.min(area.height),
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Keys ")),
        rect,
    );
}

fn fit(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    if s.chars().count() > w && w > 1 {
        out.pop();
        out.push('…');
    }
    out
}
