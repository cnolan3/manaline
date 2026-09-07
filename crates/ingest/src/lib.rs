//! Dev tooling around card IR (§4.3). M4 ships the round-trip check that CI
//! runs over every card; the generation backends come with M5.

use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundTripReport {
    pub total: usize,
    pub exact: usize,
    pub invalid: Vec<(String, String)>,
    pub mismatches: Vec<(String, String, String)>,
    /// Cards whose file disagrees with Scryfall: (name, field, ours, theirs).
    pub oracle_diffs: Vec<(String, String, String, String)>,
    /// Cards the Scryfall data does not know at all.
    pub not_on_scryfall: Vec<String>,
    /// Whether the Scryfall data was consulted.
    pub oracle_checked: bool,
}

impl RoundTripReport {
    pub fn is_clean(&self) -> bool {
        self.invalid.is_empty() && self.mismatches.is_empty() && self.oracle_diffs.is_empty()
    }

    /// Compare every card's text, cost, types, and P/T with the Scryfall data.
    pub fn check_oracle(&mut self, cards: &[cardir::Card], cache: &carddb::Cache) {
        self.oracle_checked = true;
        for c in cards {
            let Some(m) = cache.get(&c.name) else {
                self.not_on_scryfall.push(c.name.clone());
                continue;
            };
            let mut diff = |field: &str, ours: String, theirs: String| {
                if cardir::normalise(&ours, &c.name) != cardir::normalise(&theirs, &c.name) {
                    self.oracle_diffs.push((c.name.clone(), field.to_string(), ours, theirs));
                }
            };
            diff("text", c.text.clone(), m.oracle_text.clone());
            let cost = if c.is_land() && c.cost.is_free() {
                String::new()
            } else {
                c.cost.to_string()
            };
            diff("cost", cost, m.mana_cost.clone());
            let pt_ours = c.pt.map(|(p, t)| format!("{p}/{t}")).unwrap_or_default();
            let pt_theirs = match (&m.power, &m.toughness) {
                (Some(p), Some(t)) => format!("{p}/{t}"),
                _ => String::new(),
            };
            diff("pt", pt_ours, pt_theirs);
            diff("type line", type_line(c), m.type_line.clone());
        }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for (file, err) in &self.invalid {
            out.push_str(&format!("INVALID   {file}\n  {err}\n"));
        }
        for (name, want, got) in &self.mismatches {
            out.push_str(&format!("MISMATCH  {name}\n  oracle:   {want}\n  rendered: {got}\n"));
        }
        for (name, field, ours, theirs) in &self.oracle_diffs {
            out.push_str(&format!("ORACLE    {name} ({field})\n  ours:     {ours}\n  scryfall: {theirs}\n"));
        }
        for name in &self.not_on_scryfall {
            out.push_str(&format!("UNKNOWN   {name} is not in the Scryfall data\n"));
        }
        out.push_str(&format!(
            "{} cards, {} round-trip exactly, {} mismatch, {} invalid",
            self.total,
            self.exact,
            self.mismatches.len(),
            self.invalid.len()
        ));
        if self.oracle_checked {
            out.push_str(&format!(", {} differ from Scryfall", self.oracle_diffs.len()));
        }
        out.push('\n');
        out
    }
}

/// Round-trip every card in a list.
pub fn roundtrip(cards: &[cardir::Card]) -> RoundTripReport {
    let mut r = RoundTripReport {
        total: cards.len(),
        ..Default::default()
    };
    for c in cards {
        match cardir::round_trips(c) {
            Ok(()) => r.exact += 1,
            Err((want, got)) => r.mismatches.push((c.name.clone(), want, got)),
        }
    }
    r
}

/// Round-trip every `.ron` file in a directory (loading and validating each).
pub fn roundtrip_dir(dir: &Path) -> std::io::Result<RoundTripReport> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ron"))
        .collect();
    files.sort();
    let mut cards = Vec::new();
    let mut invalid = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f)?;
        match cardir::load(&text) {
            Ok(c) => cards.push(c),
            Err(e) => invalid.push((f.display().to_string(), e)),
        }
    }
    let mut r = roundtrip(&cards);
    r.total += invalid.len();
    r.invalid = invalid;
    Ok(r)
}

/// Every card that loads from a directory (invalid files skipped).
pub fn load_dir(dir: &Path) -> std::io::Result<Vec<cardir::Card>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ron"))
        .collect();
    files.sort();
    Ok(files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok().and_then(|t| cardir::load(&t).ok()))
        .collect())
}

/// The built-in core set.
pub fn roundtrip_core() -> RoundTripReport {
    roundtrip(&cards::core_ir())
}

/// The type line as Scryfall prints it: "Legendary Creature — Elf Druid".
pub fn type_line(c: &cardir::Card) -> String {
    let mut parts: Vec<String> = c.supertypes.iter().map(|s| capitalize(&format!("{s:?}"))).collect();
    parts.extend(c.types.iter().map(|t| capitalize(t.word())));
    let mut line = parts.join(" ");
    if !c.subtypes.is_empty() {
        line.push_str(" \u{2014} ");
        line.push_str(&c.subtypes.join(" "));
    }
    line
}

fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() {
        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
        None => String::new(),
    }
}
