//! The query language. Terms are `key:value` or `key<op>value`; bare words
//! match names; `-` negates; `or` alternates; parentheses group.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
pub enum Query {
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    Term(Term),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Cmp {
    /// `:` — includes / at least, depending on the field.
    Includes,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Term {
    /// A bare word or `name:` — substring of the name.
    Name(String),
    /// `t:` — a word of the type line (type, supertype, or subtype).
    Type(String),
    /// `o:` — a word or quoted phrase of the Oracle text (`~` for the card's name).
    Oracle(String),
    /// `c:` — colours, e.g. `r`, `rg`, `c` (colourless), `m` (multicolour).
    Color(Cmp, String),
    /// `ci:` / `id:` — colour identity.
    Identity(Cmp, String),
    /// `mv:` / `cmc:`.
    ManaValue(Cmp, f32),
    Power(Cmp, i32),
    Toughness(Cmp, i32),
    /// `kw:` — has the keyword.
    Keyword(String),
    /// `r:` — rarity.
    Rarity(String),
    /// `s:` / `set:` — set code.
    Set(String),
    /// `f:` — legal in the format.
    Format(String),
    /// `is:implemented`, `is:permanent`, `is:spell`, `is:vanilla`.
    Is(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryError(pub String);

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for QueryError {}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Open,
    Close,
    Neg,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, QueryError> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' => i += 1,
            '(' => {
                out.push(Tok::Open);
                i += 1;
            }
            ')' => {
                out.push(Tok::Close);
                i += 1;
            }
            '-' if matches!(out.last(), None | Some(Tok::Open) | Some(Tok::Neg))
                || chars.get(i.wrapping_sub(1)).is_none_or(|p| p.is_whitespace() || *p == '(') =>
            {
                out.push(Tok::Neg);
                i += 1;
            }
            _ => {
                // A word, which may contain a quoted section: o:"draw a card"
                let mut word = String::new();
                while i < chars.len() {
                    let c = chars[i];
                    if c == '"' {
                        i += 1;
                        while i < chars.len() && chars[i] != '"' {
                            word.push(chars[i]);
                            i += 1;
                        }
                        if i >= chars.len() {
                            return Err(QueryError("unclosed quote".into()));
                        }
                        i += 1;
                    } else if c.is_whitespace() || c == '(' || c == ')' {
                        break;
                    } else {
                        word.push(c);
                        i += 1;
                    }
                }
                out.push(Tok::Word(word));
            }
        }
    }
    Ok(out)
}

pub fn parse(s: &str) -> Result<Query, QueryError> {
    let toks = tokenize(s)?;
    let mut pos = 0;
    let q = parse_or(&toks, &mut pos)?;
    if pos != toks.len() {
        return Err(QueryError("unexpected `)`".into()));
    }
    Ok(q)
}

fn parse_or(toks: &[Tok], pos: &mut usize) -> Result<Query, QueryError> {
    let mut alts = vec![parse_and(toks, pos)?];
    while let Some(Tok::Word(w)) = toks.get(*pos) {
        if w.eq_ignore_ascii_case("or") {
            *pos += 1;
            alts.push(parse_and(toks, pos)?);
        } else {
            break;
        }
    }
    Ok(if alts.len() == 1 { alts.remove(0) } else { Query::Or(alts) })
}

fn parse_and(toks: &[Tok], pos: &mut usize) -> Result<Query, QueryError> {
    let mut parts = Vec::new();
    while let Some(t) = toks.get(*pos) {
        match t {
            Tok::Close => break,
            Tok::Word(w) if w.eq_ignore_ascii_case("or") => break,
            Tok::Word(w) if w.eq_ignore_ascii_case("and") => *pos += 1,
            _ => parts.push(parse_unary(toks, pos)?),
        }
    }
    if parts.is_empty() {
        return Err(QueryError("empty query".into()));
    }
    Ok(if parts.len() == 1 { parts.remove(0) } else { Query::And(parts) })
}

fn parse_unary(toks: &[Tok], pos: &mut usize) -> Result<Query, QueryError> {
    match toks.get(*pos) {
        Some(Tok::Neg) => {
            *pos += 1;
            Ok(Query::Not(Box::new(parse_unary(toks, pos)?)))
        }
        Some(Tok::Open) => {
            *pos += 1;
            let q = parse_or(toks, pos)?;
            match toks.get(*pos) {
                Some(Tok::Close) => {
                    *pos += 1;
                    Ok(q)
                }
                _ => Err(QueryError("missing `)`".into())),
            }
        }
        Some(Tok::Word(w)) => {
            *pos += 1;
            Ok(Query::Term(term(w)?))
        }
        Some(Tok::Close) => Err(QueryError("unexpected `)`".into())),
        None => Err(QueryError("empty query".into())),
    }
}

fn term(word: &str) -> Result<Term, QueryError> {
    // key, operator, value
    let ops = ["<=", ">=", "!=", ":", "=", "<", ">"];
    let mut split: Option<(&str, Cmp, &str)> = None;
    for op in ops {
        if let Some(i) = word.find(op) {
            let key = &word[..i];
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            let cmp = match op {
                ":" => Cmp::Includes,
                "=" => Cmp::Eq,
                "!=" => Cmp::Ne,
                "<" => Cmp::Lt,
                "<=" => Cmp::Le,
                ">" => Cmp::Gt,
                ">=" => Cmp::Ge,
                _ => unreachable!(),
            };
            let value = &word[i + op.len()..];
            if split.as_ref().map(|(k, _, _)| i < k.len()).unwrap_or(true) {
                split = Some((key, cmp, value));
            }
        }
    }
    let Some((key, cmp, value)) = split else {
        return Ok(Term::Name(word.to_lowercase()));
    };
    if value.is_empty() {
        return Err(QueryError(format!("`{key}` needs a value")));
    }
    let v = value.to_lowercase();
    let number = |what: &str| -> Result<f32, QueryError> {
        v.parse::<f32>()
            .map_err(|_| QueryError(format!("`{what}` wants a number, not {value:?}")))
    };
    Ok(match key.to_lowercase().as_str() {
        "name" | "n" => Term::Name(v),
        "t" | "type" => Term::Type(v),
        "o" | "oracle" => Term::Oracle(v),
        "c" | "color" | "colour" => Term::Color(cmp, colors(&v)?),
        "ci" | "id" | "identity" => Term::Identity(cmp, colors(&v)?),
        "mv" | "cmc" | "manavalue" => Term::ManaValue(cmp, number("mv")?),
        "pow" | "power" => Term::Power(cmp, number("pow")? as i32),
        "tou" | "toughness" => Term::Toughness(cmp, number("tou")? as i32),
        "kw" | "keyword" => Term::Keyword(v),
        "r" | "rarity" => Term::Rarity(v),
        "s" | "set" | "e" => Term::Set(v),
        "f" | "format" | "legal" => Term::Format(v),
        "is" => Term::Is(v),
        other => return Err(QueryError(format!("unknown search key `{other}`"))),
    })
}

/// Normalise a colour spec to a string of `wubrg` letters, or `c` / `m`.
fn colors(v: &str) -> Result<String, QueryError> {
    let named = match v {
        "white" => "w",
        "blue" => "u",
        "black" => "b",
        "red" => "r",
        "green" => "g",
        "colorless" | "colourless" | "c" => return Ok("c".into()),
        "multicolor" | "multicolour" | "multi" | "m" => return Ok("m".into()),
        other => other,
    };
    let mut out = String::new();
    for ch in named.chars() {
        if !"wubrg".contains(ch) {
            return Err(QueryError(format!("`{v}` is not a colour (use w, u, b, r, g, c, or m)")));
        }
        if !out.contains(ch) {
            out.push(ch);
        }
    }
    Ok(out)
}
