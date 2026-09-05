//! Player preferences, saved between games in the platform config directory.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Pass automatically at minor priority moments (where passing is the only
    /// choice, outside your own main phases) after `auto_pass_ms`.
    pub auto_pass: bool,
    pub auto_pass_ms: u64,
    pub verbose_log: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { auto_pass: true, auto_pass_ms: 2000, verbose_log: false }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("manaline").join("settings.json"))
    }

    /// Saved settings, or the defaults if there are none or they are unreadable.
    pub fn load() -> Settings {
        let Some(path) = Settings::path() else { return Settings::default() };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Settings::path() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self).expect("settings serialize"))
    }

    pub fn delay_label(&self) -> String {
        format!("{:.1}s", self.auto_pass_ms as f64 / 1000.0)
    }
}
