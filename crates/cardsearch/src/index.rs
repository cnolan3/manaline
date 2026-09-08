//! The index: one entry per Oracle card, built from the Scryfall cache when
//! present (all cards, with `implemented` marking the engine's) or from the
//! engine's card set alone.

use crate::query::{Cmp, Query, Term};
use engine::CardDb;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub mana_cost: String,
    pub mana_value: f32,
    pub type_line: String,
    /// Lowercased words of the type line.
    type_words: Vec<String>,
    pub oracle_text: String,
    /// Lowercased, with the card's own name replaced by `~`.
    oracle_lower: String,
    /// Letters of `wubrg`.
    pub colors: String,
    pub color_identity: String,
    pub keywords: Vec<String>,
    pub rarity: String,
    pub set: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub legal_in: Vec<String>,
    pub implemented: bool,
}

impl Entry {
    pub fn pt(&self) -> Option<(i32, i32)> {
        Some((self.power?, self.toughness?))
    }

    /// A one-line listing: `Grizzly Bears {1}{G}  Creature — Bear  2/2`.
    pub fn line(&self) -> String {
        let mut s = format!("{} {}", self.name, self.mana_cost).trim_end().to_string();
        s.push_str("  ");
        s.push_str(&self.type_line);
        if let Some((p, t)) = self.pt() {
            s.push_str(&format!("  {p}/{t}"));
        }
        if !self.implemented {
            s.push_str("  (not implemented)");
        }
        s
    }
}

pub struct Index {
    entries: Vec<Entry>,
    /// Whether the entries came from the Scryfall cache (all cards) or only the engine's set.
    pub from_cache: bool,
}

fn letters(cs: &[String]) -> String {
    let mut out = String::new();
    for c in "WUBRG".chars() {
        if cs.iter().any(|x| x.eq_ignore_ascii_case(&c.to_string())) {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

fn parse_stat(s: &Option<String>) -> Option<i32> {
    s.as_ref()?.trim_start_matches('+').parse().ok()
}

impl Index {
    /// Every Oracle card in the cache, marking the engine's as implemented.
    pub fn from_cache(cache: &carddb::Cache, db: &CardDb) -> Index {
        let mut entries: Vec<Entry> = cache
            .iter()
            .filter(|m| !m.type_line.is_empty() && !m.digital && !m.name.starts_with("A-"))
            .map(|m| {
                Entry::new(
                    m.name.clone(),
                    m.mana_cost.clone(),
                    m.cmc,
                    m.type_line.clone(),
                    m.oracle_text.clone(),
                    letters(&m.colors),
                    letters(&m.color_identity),
                    m.keywords.iter().map(|k| k.to_lowercase()).collect(),
                    m.rarity.clone(),
                    m.set.clone(),
                    parse_stat(&m.power),
                    parse_stat(&m.toughness),
                    m.legalities
                        .iter()
                        .filter(|(_, v)| v.as_str() == "legal")
                        .map(|(k, _)| k.clone())
                        .collect(),
                    db.lookup(&m.name).is_some(),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Index { entries, from_cache: true }
    }

    /// Only the engine's cards (no cache): rarity, set, and legality are unknown.
    pub fn from_db(db: &CardDb) -> Index {
        let mut entries: Vec<Entry> = db
            .iter()
            .map(|(_, c)| {
                let mut parts: Vec<String> = c.supertypes.iter().map(|s| format!("{s:?}")).collect();
                parts.extend(c.types.iter().map(|t| capitalize(t.word())));
                let mut type_line = parts.join(" ");
                if !c.subtypes.is_empty() {
                    type_line.push_str(" \u{2014} ");
                    type_line.push_str(&c.subtypes.join(" "));
                }
                let colors: String = c.colors.iter().map(|c| c.symbol().to_ascii_lowercase()).collect();
                Entry::new(
                    c.name.clone(),
                    if c.is_land() { String::new() } else { c.cost.to_string() },
                    c.cost.mana_value() as f32,
                    type_line,
                    c.text.clone(),
                    colors.clone(),
                    colors,
                    c.keywords.iter().map(|k| k.word().to_lowercase()).collect(),
                    String::new(),
                    c.set.clone(),
                    c.pt.map(|(p, _)| p),
                    c.pt.map(|(_, t)| t),
                    Vec::new(),
                    true,
                )
            })
            .collect();
        entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Index {
            entries,
            from_cache: false,
        }
    }

    /// The cache if it is loadable, else the engine's set.
    pub fn load(db: &CardDb) -> Index {
        match carddb::Cache::load() {
            Ok(Some(c)) => Index::from_cache(&c, db),
            _ => Index::from_db(db),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name))
    }

    /// Matching entries, implemented cards first, then by name. `limit` 0 means all.
    pub fn search(&self, q: &Query, limit: usize) -> Vec<&Entry> {
        let mut hits: Vec<&Entry> = self.entries.iter().filter(|e| e.matches(q)).collect();
        hits.sort_by_key(|e| (!e.implemented, e.name.to_lowercase()));
        if limit > 0 {
            hits.truncate(limit);
        }
        hits
    }

    /// Parse and search in one go.
    pub fn query(&self, text: &str, limit: usize) -> Result<Vec<&Entry>, crate::QueryError> {
        Ok(self.search(&crate::parse(text)?, limit))
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

impl Entry {
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: String,
        mana_cost: String,
        mana_value: f32,
        type_line: String,
        oracle_text: String,
        colors: String,
        color_identity: String,
        keywords: Vec<String>,
        rarity: String,
        set: String,
        power: Option<i32>,
        toughness: Option<i32>,
        legal_in: Vec<String>,
        implemented: bool,
    ) -> Entry {
        let type_words = type_line
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(String::from)
            .collect();
        let oracle_lower = oracle_text.replace(&name, "~").to_lowercase();
        Entry {
            name,
            mana_cost,
            mana_value,
            type_line,
            type_words,
            oracle_text,
            oracle_lower,
            colors,
            color_identity,
            keywords,
            rarity,
            set,
            power,
            toughness,
            legal_in,
            implemented,
        }
    }

    pub fn matches(&self, q: &Query) -> bool {
        match q {
            Query::And(qs) => qs.iter().all(|q| self.matches(q)),
            Query::Or(qs) => qs.iter().any(|q| self.matches(q)),
            Query::Not(q) => !self.matches(q),
            Query::Term(t) => self.term(t),
        }
    }

    fn term(&self, t: &Term) -> bool {
        match t {
            Term::Name(w) => self.name.to_lowercase().contains(w),
            Term::Type(w) => self.type_words.iter().any(|t| t == w),
            Term::Oracle(w) => self.oracle_lower.contains(w),
            Term::Color(cmp, want) => color_cmp(&self.colors, *cmp, want),
            Term::Identity(cmp, want) => color_cmp(&self.color_identity, *cmp, want),
            Term::ManaValue(cmp, n) => num_cmp(self.mana_value, *cmp, *n),
            Term::Power(cmp, n) => self.power.map(|p| num_cmp(p as f32, *cmp, *n as f32)).unwrap_or(false),
            Term::Toughness(cmp, n) => self.toughness.map(|p| num_cmp(p as f32, *cmp, *n as f32)).unwrap_or(false),
            Term::Keyword(w) => self.keywords.iter().any(|k| k == w),
            Term::Rarity(w) => self.rarity == *w || self.rarity.starts_with(w.as_str()),
            Term::Set(w) => self.set == *w,
            Term::Format(w) => self.legal_in.iter().any(|f| f == w),
            Term::Is(w) => match w.as_str() {
                "implemented" => self.implemented,
                "permanent" => !self.type_words.iter().any(|t| t == "instant" || t == "sorcery"),
                "spell" => !self.type_words.iter().any(|t| t == "land"),
                "vanilla" => self.oracle_text.is_empty() && self.type_words.iter().any(|t| t == "creature"),
                _ => false,
            },
        }
    }
}

fn num_cmp(have: f32, cmp: Cmp, want: f32) -> bool {
    match cmp {
        Cmp::Includes | Cmp::Eq => have == want,
        Cmp::Ne => have != want,
        Cmp::Lt => have < want,
        Cmp::Le => have <= want,
        Cmp::Gt => have > want,
        Cmp::Ge => have >= want,
    }
}

/// Scryfall's colour comparisons: `:` and `>=` mean "at least these",
/// `=` exactly these, `<=` no others, `<` strictly fewer, `>` strictly more.
fn color_cmp(have: &str, cmp: Cmp, want: &str) -> bool {
    match want {
        "c" => return matches!(cmp, Cmp::Includes | Cmp::Eq) == have.is_empty(),
        "m" => return matches!(cmp, Cmp::Includes | Cmp::Eq) == (have.len() >= 2),
        _ => {}
    }
    let superset = want.chars().all(|c| have.contains(c));
    let subset = have.chars().all(|c| want.contains(c));
    match cmp {
        Cmp::Includes | Cmp::Ge => superset,
        Cmp::Eq => superset && subset,
        Cmp::Ne => !(superset && subset),
        Cmp::Le => subset,
        Cmp::Lt => subset && have.len() < want.len(),
        Cmp::Gt => superset && have.len() > want.len(),
    }
}

/// Group results by a key, preserving order: for the deck editor's type buckets.
pub fn group_by_type(entries: &[&Entry]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in entries {
        out.entry(e.type_line.clone()).or_default().push(e.name.clone());
    }
    out
}
