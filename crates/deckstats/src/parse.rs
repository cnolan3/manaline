//! The text deck format:
//!
//! ```text
//! Deck
//! 4 Lightning Strike (FDN) 154
//! 4 Grizzly Bears
//! 20 Mountain
//!
//! Sideboard
//! 2 Shock
//!
//! Commander
//! 1 Elvish Archdruid
//! ```
//!
//! Headers are optional, `//` and `#` start comments, `SB:` prefixes a
//! sideboard line, counts may be written `4`, `4x`, or omitted, and a
//! trailing `(SET) 123` names a printing.

use engine::{CardDb, CardId};
use std::fmt;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Section {
    Main,
    Sideboard,
    Commander,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub count: u32,
    pub name: String,
    pub set: Option<String>,
    pub collector_number: Option<String>,
    /// 1-based line in the file.
    pub line: usize,
    pub section: Section,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decklist {
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

impl Decklist {
    pub fn section(&self, s: Section) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(move |e| e.section == s)
    }

    pub fn main(&self) -> impl Iterator<Item = &Entry> {
        self.section(Section::Main)
    }

    pub fn main_count(&self) -> u32 {
        self.main().map(|e| e.count).sum()
    }

    /// Resolve names against a card database. Unknown names come back with
    /// a suggestion when one is close enough.
    pub fn resolve(&self, db: &CardDb) -> Resolution {
        let mut deck = Vec::new();
        let mut unresolved = Vec::new();
        for e in self.main() {
            match db.lookup(&e.name) {
                Some(id) => deck.extend(std::iter::repeat_n(id, e.count as usize)),
                None => unresolved.push(Unresolved {
                    entry: e.clone(),
                    suggestion: suggest(&e.name, db),
                }),
            }
        }
        Resolution { deck, unresolved }
    }

    /// The canonical text form: main deck by type, then mana value, then name.
    pub fn to_text(&self, db: &CardDb) -> String {
        let mut out = String::new();
        for (section, header) in [
            (Section::Main, "Deck"),
            (Section::Sideboard, "Sideboard"),
            (Section::Commander, "Commander"),
        ] {
            let mut entries: Vec<&Entry> = self.section(section).collect();
            if entries.is_empty() {
                continue;
            }
            entries.sort_by_key(|e| {
                let (rank, mv) = db
                    .lookup(&e.name)
                    .map(|id| {
                        let c = db.get(id);
                        (type_rank(c), c.cost.mana_value())
                    })
                    .unwrap_or((9, 0));
                (rank, mv, e.name.to_lowercase())
            });
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(header);
            out.push('\n');
            for e in entries {
                out.push_str(&format!("{} {}", e.count, e.name));
                if let Some(set) = &e.set {
                    out.push_str(&format!(" ({})", set.to_uppercase()));
                    if let Some(n) = &e.collector_number {
                        out.push(' ');
                        out.push_str(n);
                    }
                }
                out.push('\n');
            }
        }
        out
    }
}

fn type_rank(c: &engine::CardDef) -> u8 {
    use cardir::CardType::*;
    if c.is_land() {
        return 6;
    }
    if c.is_creature() {
        return 0;
    }
    if c.types.contains(&Planeswalker) {
        return 1;
    }
    if c.types.contains(&Instant) {
        return 2;
    }
    if c.types.contains(&Sorcery) {
        return 3;
    }
    if c.types.contains(&Artifact) {
        return 4;
    }
    5
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unresolved {
    pub entry: Entry,
    pub suggestion: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolution {
    pub deck: Vec<CardId>,
    pub unresolved: Vec<Unresolved>,
}

/// Parse deck text. Only structurally broken lines are errors; unknown card
/// names are a resolution concern.
pub fn parse(text: &str) -> Result<Decklist, ParseError> {
    let mut entries = Vec::new();
    let mut section = Section::Main;
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        match line.to_ascii_lowercase().trim_end_matches(':') {
            "deck" | "main" | "maindeck" | "mainboard" => {
                section = Section::Main;
                continue;
            }
            "sideboard" | "side" => {
                section = Section::Sideboard;
                continue;
            }
            "commander" | "commanders" => {
                section = Section::Commander;
                continue;
            }
            "companion" | "about" => {
                section = Section::Sideboard;
                continue;
            }
            _ => {}
        }
        let (line, line_section) = match line.strip_prefix("SB:").or_else(|| line.strip_prefix("SB: ")) {
            Some(rest) => (rest.trim(), Section::Sideboard),
            None => (line, section),
        };
        let (count, rest) = match line.split_once(char::is_whitespace) {
            Some((n, rest)) => match n.trim_end_matches(['x', 'X']).parse::<u32>() {
                Ok(k) => (k, rest.trim()),
                Err(_) => (1, line),
            },
            None => (1, line),
        };
        if count == 0 {
            return Err(ParseError {
                line: lineno,
                message: "a count of 0 makes no sense".into(),
            });
        }
        let (name, set, number) = split_printing(rest);
        if name.is_empty() {
            return Err(ParseError {
                line: lineno,
                message: "no card name".into(),
            });
        }
        entries.push(Entry {
            count,
            name: name.to_string(),
            set,
            collector_number: number,
            line: lineno,
            section: line_section,
        });
    }
    Ok(Decklist { entries })
}

fn strip_comment(line: &str) -> &str {
    let cut = line.find("//").into_iter().chain(line.find('#')).min();
    match cut {
        Some(i) => &line[..i],
        None => line,
    }
}

/// `Lightning Strike (FDN) 154` → name, set, collector number. A trailing
/// `*F*` (foil marker) is dropped too.
fn split_printing(s: &str) -> (&str, Option<String>, Option<String>) {
    let s = s.trim().trim_end_matches("*F*").trim();
    if let Some(open) = s.rfind(" (") {
        if let Some(close) = s[open..].find(')') {
            let set = s[open + 2..open + close].trim();
            let after = s[open + close + 1..].trim();
            if !set.is_empty() && set.len() <= 6 && !set.contains(' ') {
                let number = if after.is_empty() { None } else { Some(after.to_string()) };
                return (s[..open].trim(), Some(set.to_lowercase()), number);
            }
        }
    }
    (s, None, None)
}

/// The closest card name in the database, if it is close enough to be
/// what the writer meant (an edit distance of at most a third of the name).
pub fn suggest(name: &str, db: &CardDb) -> Option<String> {
    let want = name.to_lowercase();
    let mut best: Option<(usize, String)> = None;
    for (_, c) in db.iter() {
        let have = c.name.to_lowercase();
        let d = edit_distance(&want, &have);
        if best.as_ref().map(|(b, _)| d < *b).unwrap_or(true) {
            best = Some((d, c.name.clone()));
        }
    }
    let (d, candidate) = best?;
    if d <= (want.chars().count() / 3).max(1) {
        Some(candidate)
    } else {
        None
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
