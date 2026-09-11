//! Value types shared by the IR and the engine: colours, card types,
//! keywords, and mana costs. Mana costs serialize as their Oracle text.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Color {
    White,
    Blue,
    Black,
    Red,
    Green,
}

impl Color {
    pub const ALL: [Color; 5] = [Color::White, Color::Blue, Color::Black, Color::Red, Color::Green];

    pub fn symbol(self) -> char {
        match self {
            Color::White => 'W',
            Color::Blue => 'U',
            Color::Black => 'B',
            Color::Red => 'R',
            Color::Green => 'G',
        }
    }

    pub fn from_symbol(c: char) -> Option<Color> {
        match c.to_ascii_uppercase() {
            'W' => Some(Color::White),
            'U' => Some(Color::Blue),
            'B' => Some(Color::Black),
            'R' => Some(Color::Red),
            'G' => Some(Color::Green),
            _ => None,
        }
    }

    /// The colour word as it appears in Oracle text.
    pub fn word(self) -> &'static str {
        match self {
            Color::White => "white",
            Color::Blue => "blue",
            Color::Black => "black",
            Color::Red => "red",
            Color::Green => "green",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CardType {
    Creature,
    Land,
    Instant,
    Sorcery,
    Artifact,
    Enchantment,
    Planeswalker,
}

impl CardType {
    pub fn is_permanent(self) -> bool {
        !matches!(self, CardType::Instant | CardType::Sorcery)
    }

    pub fn word(self) -> &'static str {
        match self {
            CardType::Creature => "creature",
            CardType::Land => "land",
            CardType::Instant => "instant",
            CardType::Sorcery => "sorcery",
            CardType::Artifact => "artifact",
            CardType::Enchantment => "enchantment",
            CardType::Planeswalker => "planeswalker",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Supertype {
    Basic,
    Legendary,
    /// Snow permanents: no rules weight here, kept so the type line matches.
    Snow,
}

/// The evergreen keywords the engine implements (§4.2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Keyword {
    Flying,
    FirstStrike,
    DoubleStrike,
    Deathtouch,
    Lifelink,
    Trample,
    Vigilance,
    Haste,
    Reach,
    Menace,
    Defender,
    Flash,
    Hexproof,
    Indestructible,
    Prowess,
}

impl Keyword {
    pub const ALL: [Keyword; 15] = [
        Keyword::Flying,
        Keyword::FirstStrike,
        Keyword::DoubleStrike,
        Keyword::Deathtouch,
        Keyword::Lifelink,
        Keyword::Trample,
        Keyword::Vigilance,
        Keyword::Haste,
        Keyword::Reach,
        Keyword::Menace,
        Keyword::Defender,
        Keyword::Flash,
        Keyword::Hexproof,
        Keyword::Indestructible,
        Keyword::Prowess,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Keyword::Flying => "flying",
            Keyword::FirstStrike => "first strike",
            Keyword::DoubleStrike => "double strike",
            Keyword::Deathtouch => "deathtouch",
            Keyword::Lifelink => "lifelink",
            Keyword::Trample => "trample",
            Keyword::Vigilance => "vigilance",
            Keyword::Haste => "haste",
            Keyword::Reach => "reach",
            Keyword::Menace => "menace",
            Keyword::Defender => "defender",
            Keyword::Flash => "flash",
            Keyword::Hexproof => "hexproof",
            Keyword::Indestructible => "indestructible",
            Keyword::Prowess => "prowess",
        }
    }
}

/// A mana cost: generic, coloured pips, and any number of `{X}` symbols
/// (no hybrid or phyrexian mana). Serializes as its Oracle text, e.g.
/// `"{X}{1}{G}"`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ManaCost {
    pub generic: u8,
    pub pips: Vec<Color>,
    /// How many `{X}` symbols: each is paid with the announced value of X.
    pub x: u8,
}

impl ManaCost {
    /// Parse Oracle-style cost text such as `{1}{G}{G}`. The empty string is a free cost.
    pub fn parse(text: &str) -> Result<ManaCost, String> {
        let mut cost = ManaCost::default();
        let mut rest = text.trim();
        while !rest.is_empty() {
            if !rest.starts_with('{') {
                return Err(format!("expected '{{' in mana cost {text:?}"));
            }
            let close = rest.find('}').ok_or_else(|| format!("unterminated mana symbol in {text:?}"))?;
            let sym = &rest[1..close];
            if let Ok(n) = sym.parse::<u8>() {
                cost.generic = cost.generic.saturating_add(n);
            } else if sym == "X" {
                cost.x = cost.x.saturating_add(1);
            } else if sym.len() == 1 {
                let c =
                    Color::from_symbol(sym.chars().next().unwrap()).ok_or_else(|| format!("unknown mana symbol {{{sym}}} in {text:?}"))?;
                cost.pips.push(c);
            } else {
                return Err(format!("unsupported mana symbol {{{sym}}} in {text:?}"));
            }
            rest = &rest[close + 1..];
        }
        cost.pips.sort();
        Ok(cost)
    }

    pub fn mana_value(&self) -> u32 {
        self.generic as u32 + self.pips.len() as u32
    }

    pub fn pips_of(&self, color: Color) -> u8 {
        self.pips.iter().filter(|&&c| c == color).count() as u8
    }

    pub fn is_free(&self) -> bool {
        self.generic == 0 && self.pips.is_empty() && self.x == 0
    }

    /// Whether the cost has an X to announce.
    pub fn has_x(&self) -> bool {
        self.x > 0
    }

    /// The cost with X announced as `value`: every `{X}` becomes that much generic mana.
    pub fn with_x(&self, value: u32) -> ManaCost {
        ManaCost {
            generic: (self.generic as u32 + self.x as u32 * value).min(u8::MAX as u32) as u8,
            pips: self.pips.clone(),
            x: 0,
        }
    }

    /// The colours in the cost, each once, in WUBRG order.
    pub fn colors(&self) -> Vec<Color> {
        let mut cs = self.pips.clone();
        cs.sort();
        cs.dedup();
        cs
    }
}

impl fmt::Display for ManaCost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for _ in 0..self.x {
            f.write_str("{X}")?;
        }
        if self.generic > 0 || (self.pips.is_empty() && self.x == 0) {
            write!(f, "{{{}}}", self.generic)?;
        }
        for c in &self.pips {
            write!(f, "{{{}}}", c.symbol())?;
        }
        Ok(())
    }
}

impl Serialize for ManaCost {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.is_free() {
            s.serialize_str("")
        } else {
            s.serialize_str(&self.to_string())
        }
    }
}

impl<'de> Deserialize<'de> for ManaCost {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<ManaCost, D::Error> {
        let text = String::deserialize(d)?;
        ManaCost::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ManaCost {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ManaCost".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A mana cost in Oracle notation, e.g. \"{1}{G}{G}\" or \"{X}{R}\"; empty for no cost.",
            "pattern": "^(\\{[0-9XWUBRG]\\})*$"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_costs() {
        let c = ManaCost::parse("{1}{G}{G}").unwrap();
        assert_eq!(c.generic, 1);
        assert_eq!(c.pips, vec![Color::Green, Color::Green]);
        assert_eq!(c.mana_value(), 3);
        assert_eq!(c.to_string(), "{1}{G}{G}");
        assert!(ManaCost::parse("").unwrap().is_free());
        assert_eq!(ManaCost::parse("{0}").unwrap().to_string(), "{0}");
        let x = ManaCost::parse("{X}{X}{1}{R}").unwrap();
        assert_eq!((x.x, x.generic, x.mana_value()), (2, 1, 2));
        assert_eq!(x.to_string(), "{X}{X}{1}{R}");
        assert_eq!(x.with_x(3).to_string(), "{7}{R}");
        assert!(!x.is_free() && x.has_x());
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"{1}{G}{G}\"");
        let back: ManaCost = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }
}
