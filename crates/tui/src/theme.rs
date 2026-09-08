//! Colour themes (§6: colour reinforces, monochrome still reads). A theme
//! is a handful of semantic colours; `mono` maps them all to the terminal
//! default so only bold, dim, and reverse carry meaning.

use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub name: &'static str,
    /// Good news, your turn, legal.
    pub good: Color,
    /// Attention: countdowns, status lines, selections.
    pub warn: Color,
    /// Bad news: attackers, illegal, game over against you.
    pub danger: Color,
    /// Blockers, informational lines.
    pub info: Color,
    /// The stack.
    pub stack: Color,
    /// Chat and focused pane borders.
    pub focus: Color,
    /// Text drawn on a `good` background.
    pub on_good: Color,
}

pub const NAMES: [&str; 3] = ["default", "mono", "high-contrast"];

impl Theme {
    pub fn named(name: &str) -> Option<Theme> {
        Some(match name {
            "default" => Theme {
                name: "default",
                good: Color::Green,
                warn: Color::Yellow,
                danger: Color::Red,
                info: Color::Blue,
                stack: Color::Magenta,
                focus: Color::Cyan,
                on_good: Color::White,
            },
            "mono" => Theme {
                name: "mono",
                good: Color::Reset,
                warn: Color::Reset,
                danger: Color::Reset,
                info: Color::Reset,
                stack: Color::Reset,
                focus: Color::Reset,
                on_good: Color::Reset,
            },
            "high-contrast" => Theme {
                name: "high-contrast",
                good: Color::LightGreen,
                warn: Color::LightYellow,
                danger: Color::LightRed,
                info: Color::LightBlue,
                stack: Color::LightMagenta,
                focus: Color::LightCyan,
                on_good: Color::Black,
            },
            _ => return None,
        })
    }

    pub fn next(name: &str) -> &'static str {
        let i = NAMES.iter().position(|n| *n == name).unwrap_or(0);
        NAMES[(i + 1) % NAMES.len()]
    }

    pub fn previous(name: &str) -> &'static str {
        let i = NAMES.iter().position(|n| *n == name).unwrap_or(0);
        NAMES[(i + NAMES.len() - 1) % NAMES.len()]
    }
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::named("default").unwrap()
    }
}
