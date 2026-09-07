//! The replay log (§3.6): a header line, then one line per accepted action.
//! Seed plus actions fully determines the game.

use engine::{Action, CardDb, Format, Game, GameConfig, PlayerSetup, Seat};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayPlayer {
    pub name: String,
    /// Card names in deck order.
    pub deck: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayHeader {
    pub game_id: String,
    pub format: String,
    pub seed: u64,
    pub players: Vec<ReplayPlayer>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayLine {
    Header(ReplayHeader),
    Action { seat: Seat, action: Action },
}

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {line}: {error}")]
    Malformed { line: usize, error: String },
    #[error("replay does not start with a header")]
    NoHeader,
    #[error("unknown format {0:?}")]
    UnknownFormat(String),
    #[error("unknown card {0:?}")]
    UnknownCard(String),
    #[error(transparent)]
    Rules(#[from] engine::RulesError),
}

pub struct ReplayWriter {
    path: PathBuf,
    file: std::fs::File,
}

impl ReplayWriter {
    pub fn create(path: &Path, header: &ReplayHeader) -> Result<ReplayWriter, ReplayError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::File::create(path)?;
        write_line(&mut file, &ReplayLine::Header(header.clone()))?;
        Ok(ReplayWriter {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Append one accepted action. Flushed before returning, so an ack never
    /// precedes its log line reaching the OS.
    pub fn append(&mut self, seat: Seat, action: &Action) -> Result<(), ReplayError> {
        write_line(
            &mut self.file,
            &ReplayLine::Action {
                seat,
                action: action.clone(),
            },
        )
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn write_line(file: &mut std::fs::File, line: &ReplayLine) -> Result<(), ReplayError> {
    let mut bytes = serde_json::to_vec(line).expect("replay line serializes");
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.flush()?;
    Ok(())
}

pub fn read(path: &Path) -> Result<(ReplayHeader, Vec<(Seat, Action)>), ReplayError> {
    let file = std::fs::File::open(path)?;
    let mut header = None;
    let mut actions = Vec::new();
    for (i, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed: ReplayLine = serde_json::from_str(&line).map_err(|e| ReplayError::Malformed {
            line: i + 1,
            error: e.to_string(),
        })?;
        match parsed {
            ReplayLine::Header(h) => header = Some(h),
            ReplayLine::Action { seat, action } => actions.push((seat, action)),
        }
    }
    Ok((header.ok_or(ReplayError::NoHeader)?, actions))
}

/// The game config a header describes.
pub fn config_from_header(header: &ReplayHeader, cards: Arc<CardDb>) -> Result<GameConfig, ReplayError> {
    let format = Format::builtin(&header.format).ok_or_else(|| ReplayError::UnknownFormat(header.format.clone()))?;
    let mut players = Vec::new();
    for p in &header.players {
        let deck = p
            .deck
            .iter()
            .map(|n| cards.lookup(n).ok_or_else(|| ReplayError::UnknownCard(n.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        players.push(PlayerSetup {
            name: p.name.clone(),
            deck,
        });
    }
    Ok(GameConfig {
        format,
        players,
        cards,
        starting_player: None,
    })
}

/// Reconstruct a game from its log, applying `up_to` actions (all if `None`).
pub fn rebuild(path: &Path, cards: Arc<CardDb>, up_to: Option<usize>) -> Result<(ReplayHeader, Game), ReplayError> {
    let (header, actions) = read(path)?;
    let config = config_from_header(&header, cards)?;
    let n = up_to.unwrap_or(actions.len()).min(actions.len());
    let game = Game::replay(config, header.seed, &actions[..n])?;
    Ok((header, game))
}
