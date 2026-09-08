//! An ASCII card: name and cost, type line, rules text, stats. Shared by
//! `cards show`, the deckbuilder's detail pane, and the inspect popup.

/// What a card face shows; built from an engine card, a search entry, or a view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardFace {
    pub name: String,
    pub cost: String,
    pub type_line: String,
    pub text: String,
    pub pt: Option<(i32, i32)>,
    /// Bottom-left corner: set and rarity, or a state line.
    pub footer: String,
}

impl CardFace {
    pub fn from_entry(e: &cardsearch::Entry) -> CardFace {
        let footer = match (e.set.is_empty(), e.rarity.is_empty()) {
            (false, false) => format!("{} · {}", e.set.to_uppercase(), e.rarity),
            (false, true) => e.set.to_uppercase(),
            _ => String::new(),
        };
        CardFace {
            name: e.name.clone(),
            cost: e.mana_cost.clone(),
            type_line: e.type_line.clone(),
            text: e.oracle_text.clone(),
            pt: e.pt(),
            footer,
        }
    }

    pub fn from_def(c: &engine::CardDef) -> CardFace {
        let mut parts: Vec<String> = c.supertypes.iter().map(|s| format!("{s:?}")).collect();
        parts.extend(c.types.iter().map(|t| capitalize(t.word())));
        let mut type_line = parts.join(" ");
        if !c.subtypes.is_empty() {
            type_line.push_str(" \u{2014} ");
            type_line.push_str(&c.subtypes.join(" "));
        }
        CardFace {
            name: c.name.clone(),
            cost: if c.is_land() { String::new() } else { c.cost.to_string() },
            type_line,
            text: c.text.clone(),
            pt: c.pt,
            footer: String::new(),
        }
    }
}

/// Render as lines of exactly `width` columns (bordered).
pub fn render(face: &CardFace, width: u16) -> Vec<String> {
    let width = width.max(24) as usize;
    let inner = width - 4; // "│ " and " │"
    let mut body: Vec<String> = Vec::new();
    // Name … cost, on one line when they fit.
    let name_cost_gap = inner.saturating_sub(face.name.chars().count() + face.cost.chars().count());
    if face.cost.is_empty() {
        body.extend(wrap(&face.name, inner));
    } else if name_cost_gap >= 1 {
        body.push(format!("{}{}{}", face.name, " ".repeat(name_cost_gap), face.cost));
    } else {
        body.extend(wrap(&face.name, inner));
        body.push(format!("{:>inner$}", face.cost));
    }
    body.extend(wrap(&face.type_line, inner));
    body.push("─".repeat(inner));
    if face.text.is_empty() {
        body.push(String::new());
    } else {
        for para in face.text.split('\n') {
            body.extend(wrap(para, inner));
        }
    }
    body.push(String::new());
    let pt = face.pt.map(|(p, t)| format!("{p}/{t}")).unwrap_or_default();
    let foot_gap = inner.saturating_sub(face.footer.chars().count() + pt.chars().count());
    body.push(format!(
        "{}{}{}",
        fit(&face.footer, inner.saturating_sub(pt.chars().count() + 1)),
        " ".repeat(foot_gap.max(1).min(inner)),
        pt
    ));

    let mut out = Vec::with_capacity(body.len() + 2);
    out.push(format!("┌{}┐", "─".repeat(width - 2)));
    for line in body {
        let line = fit(&line, inner);
        let pad = inner - line.chars().count();
        out.push(format!("│ {line}{} │", " ".repeat(pad)));
    }
    out.push(format!("└{}┘", "─".repeat(width - 2)));
    out
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let candidate = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if candidate.chars().count() <= width {
            cur = candidate;
        } else {
            if !cur.is_empty() {
                lines.push(cur);
            }
            cur = fit(word, width);
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

fn fit(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    if s.chars().count() > w && w > 1 {
        out.pop();
        out.push('…');
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
