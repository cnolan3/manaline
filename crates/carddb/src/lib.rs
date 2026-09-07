//! The Scryfall bulk-data cache (§4.5): card metadata that is not behaviour
//! (set, rarity, artist, legalities, Oracle text for cards we have no IR
//! for). Downloaded on first use to the XDG cache directory, refreshed by
//! `manaline cards update`; every consumer can say how old it is. Nothing
//! here is hand-typed and nothing here is needed to play a game.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const BULK_INDEX_URL: &str = "https://api.scryfall.com/bulk-data/oracle-cards";
const USER_AGENT: &str = concat!("manaline/", env!("CARGO_PKG_VERSION"), " (https://github.com/connornolan/manaline)");
const CARDS_FILE: &str = "oracle-cards.json";
const META_FILE: &str = "oracle-cards.meta.json";

/// The fields we keep from a Scryfall card object. Everything else is
/// dropped when the bulk file is trimmed into the cache.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CardMeta {
    pub name: String,
    #[serde(default)]
    pub oracle_id: String,
    #[serde(default)]
    pub set: String,
    #[serde(default)]
    pub set_name: String,
    #[serde(default)]
    pub collector_number: String,
    #[serde(default)]
    pub rarity: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub released_at: String,
    #[serde(default)]
    pub mana_cost: String,
    #[serde(default)]
    pub cmc: f32,
    #[serde(default)]
    pub type_line: String,
    #[serde(default)]
    pub oracle_text: String,
    #[serde(default)]
    pub power: Option<String>,
    #[serde(default)]
    pub toughness: Option<String>,
    #[serde(default)]
    pub colors: Vec<String>,
    #[serde(default)]
    pub color_identity: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Format name → "legal" | "not_legal" | "banned" | "restricted".
    #[serde(default)]
    pub legalities: BTreeMap<String, String>,
    #[serde(default)]
    pub layout: String,
}

impl CardMeta {
    pub fn is_legal(&self, format: &str) -> bool {
        matches!(self.legalities.get(format).map(String::as_str), Some("legal" | "restricted"))
    }

    pub fn is_banned(&self, format: &str) -> bool {
        self.legalities.get(format).map(String::as_str) == Some("banned")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    /// When the bulk file was fetched, seconds since the epoch.
    pub fetched_at: u64,
    /// Scryfall's own timestamp for the bulk file.
    #[serde(default)]
    pub updated_at: String,
    pub cards: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("no cache directory on this system")]
    NoCacheDir,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("download failed: {0}")]
    Http(String),
}

/// The loaded cache: every Oracle card, indexed by lowercase name.
pub struct Cache {
    cards: Vec<CardMeta>,
    by_name: HashMap<String, usize>,
    pub meta: Meta,
    pub dir: PathBuf,
}

/// Where the cache lives: `$MANALINE_CACHE_DIR`, else the platform cache dir + `manaline`.
pub fn cache_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("MANALINE_CACHE_DIR") {
        return Some(PathBuf::from(d));
    }
    dirs::cache_dir().map(|d| d.join("manaline"))
}

impl Cache {
    /// The cache at the default location, or `None` if it has never been downloaded.
    pub fn load() -> Result<Option<Cache>, CacheError> {
        let dir = cache_dir().ok_or(CacheError::NoCacheDir)?;
        Cache::load_from(&dir)
    }

    pub fn load_from(dir: &Path) -> Result<Option<Cache>, CacheError> {
        let cards_path = dir.join(CARDS_FILE);
        let meta_path = dir.join(META_FILE);
        if !cards_path.exists() || !meta_path.exists() {
            return Ok(None);
        }
        let meta: Meta = serde_json::from_slice(&std::fs::read(&meta_path)?)?;
        let cards: Vec<CardMeta> = serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(&cards_path)?))?;
        Ok(Some(Cache::from_cards(cards, meta, dir.to_path_buf())))
    }

    /// Build a cache from a list of cards (tests, fixtures).
    pub fn from_cards(cards: Vec<CardMeta>, meta: Meta, dir: PathBuf) -> Cache {
        let mut by_name = HashMap::with_capacity(cards.len());
        for (i, c) in cards.iter().enumerate() {
            by_name.entry(c.name.to_lowercase()).or_insert(i);
            // Double-faced and split cards: the front face name also resolves.
            if let Some((front, _)) = c.name.split_once(" // ") {
                by_name.entry(front.to_lowercase()).or_insert(i);
            }
        }
        Cache { cards, by_name, meta, dir }
    }

    /// Download the Scryfall "Oracle Cards" bulk file into `dir`, trim it to
    /// the fields we keep, and return the fresh cache.
    pub fn update(dir: &Path) -> Result<Cache, CacheError> {
        std::fs::create_dir_all(dir)?;
        let index: serde_json::Value = ureq::get(BULK_INDEX_URL)
            .set("User-Agent", USER_AGENT)
            .set("Accept", "application/json")
            .call()
            .map_err(|e| CacheError::Http(e.to_string()))?
            .into_json()?;
        // Scryfall serves either a gzipped JSON Lines file or a plain JSON array.
        let download = index["jsonl_download_uri"]
            .as_str()
            .or_else(|| index["download_uri"].as_str())
            .ok_or_else(|| CacheError::Http("bulk index has no download uri".into()))?
            .to_string();
        let updated_at = index["updated_at"].as_str().unwrap_or_default().to_string();
        let response = ureq::get(&download)
            .set("User-Agent", USER_AGENT)
            .set("Accept", "application/json")
            .call()
            .map_err(|e| CacheError::Http(e.to_string()))?;
        let mut raw = Vec::new();
        response.into_reader().read_to_end(&mut raw)?;
        Cache::install(dir, &raw, updated_at)
    }

    /// Trim a raw bulk file (already downloaded; gzipped or not, JSON Lines
    /// or a JSON array) into the cache.
    pub fn install(dir: &Path, raw: &[u8], updated_at: String) -> Result<Cache, CacheError> {
        std::fs::create_dir_all(dir)?;
        let all = parse_bulk(raw)?;
        // Keep real playable cards: no tokens, emblems, art cards, or the like.
        let cards: Vec<CardMeta> = all
            .into_iter()
            .filter(|c| {
                !matches!(
                    c.layout.as_str(),
                    "token" | "double_faced_token" | "emblem" | "art_series" | "scheme" | "vanguard" | "planar"
                )
            })
            .collect();
        let meta = Meta {
            fetched_at: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            updated_at,
            cards: cards.len(),
        };
        let tmp = dir.join(format!("{CARDS_FILE}.tmp"));
        {
            let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            serde_json::to_writer(&mut f, &cards)?;
            f.flush()?;
        }
        std::fs::rename(&tmp, dir.join(CARDS_FILE))?;
        std::fs::write(dir.join(META_FILE), serde_json::to_vec_pretty(&meta)?)?;
        Ok(Cache::from_cards(cards, meta, dir.to_path_buf()))
    }

    pub fn get(&self, name: &str) -> Option<&CardMeta> {
        self.by_name.get(&name.to_lowercase()).map(|&i| &self.cards[i])
    }

    pub fn contains(&self, name: &str) -> bool {
        self.by_name.contains_key(&name.to_lowercase())
    }

    pub fn iter(&self) -> impl Iterator<Item = &CardMeta> {
        self.cards.iter()
    }

    pub fn len(&self) -> usize {
        self.cards.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// How long ago the bulk file was fetched.
    pub fn age(&self) -> Duration {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Duration::from_secs(now.saturating_sub(self.meta.fetched_at))
    }

    /// "card data as of 2026-08-30 (fetched 3 days ago)"
    pub fn age_text(&self) -> String {
        let days = self.age().as_secs() / 86_400;
        let fetched = match days {
            0 => "fetched today".to_string(),
            1 => "fetched yesterday".to_string(),
            n => format!("fetched {n} days ago"),
        };
        let date = self.meta.updated_at.get(..10).unwrap_or(&self.meta.updated_at);
        if date.is_empty() {
            format!("card data {fetched}")
        } else {
            format!("card data as of {date} ({fetched})")
        }
    }
}

/// Parse a bulk file in any of the shapes Scryfall has used.
pub fn parse_bulk(raw: &[u8]) -> Result<Vec<CardMeta>, CacheError> {
    let mut bytes: Vec<u8>;
    let data: &[u8] = if raw.starts_with(&[0x1f, 0x8b]) {
        bytes = Vec::new();
        flate2::read::GzDecoder::new(raw).read_to_end(&mut bytes)?;
        &bytes
    } else {
        raw
    };
    let first = data.iter().find(|b| !b.is_ascii_whitespace()).copied();
    if first == Some(b'[') {
        return Ok(serde_json::from_slice(data)?);
    }
    let mut cards = Vec::new();
    for line in data.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        cards.push(serde_json::from_str::<CardMeta>(&line)?);
    }
    Ok(cards)
}

impl engine::LegalitySource for Cache {
    fn legal_in(&self, card_name: &str, format: &str) -> Option<bool> {
        self.get(card_name).map(|c| c.is_legal(format))
    }
}

/// A short human summary of the cache's presence, for commands that need it.
pub fn status_line() -> String {
    match Cache::load() {
        Ok(Some(c)) => format!("{} ({} cards, {})", c.age_text(), c.len(), c.dir.display()),
        Ok(None) => "no card data cached yet: run `manaline cards update`".into(),
        Err(e) => format!("card data unavailable: {e}"),
    }
}
