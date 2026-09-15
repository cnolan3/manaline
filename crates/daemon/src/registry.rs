//! The lobby of a server: game codes, tokens, and the games themselves
//! (§2.2). It is the only state shared between games, it is touched at create
//! and join time and never during play, and every game it hands out is fully
//! independent of every other.
//!
//! `create` is the one internal function that makes a game. `manaline create`
//! reaches it through `create_game`, `manaline join <code>` through
//! `join_game`, and the matchmaker of §2.2.1 will reach the same function with
//! a queue behind it instead of a person.

use crate::game_task::{Context, GameShared, Status};
use crate::lobby::{random_game_code, Lobby, SeatSlot};
use crate::replay::{self, ReplayWriter};
use crate::server::DaemonError;
use engine::Format;
use protocol::{GameId, Role, Token};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

#[derive(Default)]
pub(crate) struct Registry {
    games: HashMap<GameId, Arc<GameShared>>,
    /// Every seat and spectator token of every live game.
    tokens: HashMap<Token, (GameId, Role)>,
}

impl Registry {
    /// Make a game: pick an unused code, issue a token per seat and one for
    /// spectators, and register all of them. The only way a game is born.
    pub(crate) fn create(
        &mut self,
        ctx: &Arc<Context>,
        format_name: &str,
        seats: u8,
        seed: Option<u64>,
        status: watch::Sender<Status>,
    ) -> Result<Arc<GameShared>, DaemonError> {
        let format = Format::builtin(format_name).ok_or_else(|| DaemonError::Setup(format!("unknown format {format_name:?}")))?;
        if !format.allows_player_count(seats as usize) {
            return Err(DaemonError::Setup(format!(
                "{} needs {}–{} players, not {seats}",
                format.name, format.players.min, format.players.max
            )));
        }
        let unsupported = format.unsupported_rules();
        if !unsupported.is_empty() {
            let list: Vec<String> = unsupported.iter().map(ToString::to_string).collect();
            return Err(DaemonError::Setup(list.join("; ")));
        }
        let seed = seed.unwrap_or_else(rand::random);
        let mut lobby = Lobby::new(format_name, format, seats, seed);
        while self.games.contains_key(&lobby.game_id) {
            lobby.game_id = GameId(random_game_code());
        }
        let game = GameShared::new(ctx.clone(), lobby, status, None, None);
        self.insert(game.clone());
        Ok(game)
    }

    fn insert(&mut self, game: Arc<GameShared>) {
        let id = game.game_id.clone();
        for (token, role) in &game.tokens {
            self.tokens.insert(token.clone(), (id.clone(), *role));
        }
        self.games.insert(id, game);
    }

    pub(crate) fn resolve(&self, token: &Token) -> Option<(Arc<GameShared>, Role)> {
        let (id, role) = self.tokens.get(token)?;
        Some((self.games.get(id)?.clone(), *role))
    }

    pub(crate) fn get(&self, id: &GameId) -> Option<Arc<GameShared>> {
        self.games.get(id).cloned()
    }

    /// The one game of a single-game daemon.
    pub(crate) fn only(&self) -> Option<Arc<GameShared>> {
        self.games.values().next().cloned()
    }

    pub(crate) fn len(&self) -> usize {
        self.games.len()
    }

    pub(crate) fn ids(&self) -> Vec<GameId> {
        self.games.keys().cloned().collect()
    }

    pub(crate) fn games(&self) -> Vec<Arc<GameShared>> {
        self.games.values().cloned().collect()
    }

    /// Forget a game and every token that reached it. The `Arc` dies with the
    /// last connection still holding it, which closes its log.
    pub(crate) fn remove(&mut self, id: &GameId) {
        if self.games.remove(id).is_some() {
            self.tokens.retain(|_, (game, _)| game != id);
        }
    }

    /// Put a game recovered from its log back in the maps. Its tokens come
    /// from the log header, so the clients that were playing it reconnect with
    /// the tokens they already hold.
    fn restore(&mut self, game: Arc<GameShared>) {
        self.insert(game);
    }
}

/// Rebuild every unfinished game under `dir` and register it (§2.2, "durability
/// falls out of determinism"). A game is its seed plus its action log, so a
/// crash, a deploy, or a move to another machine costs nothing but the replay.
/// Finished games are left on disk and not reloaded; logs written before the
/// header carried tokens are replayable but not resumable, and are skipped.
pub(crate) fn recover(registry: &mut Registry, ctx: &Arc<Context>, dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();
    let mut n = 0;
    for path in paths {
        match recover_one(ctx, &path) {
            Ok(Some(game)) => {
                info!(game = %game.game_id, "recovered from {}", path.display());
                registry.restore(game);
                n += 1;
            }
            Ok(None) => {}
            Err(e) => warn!("could not recover {}: {e}", path.display()),
        }
    }
    n
}

fn recover_one(ctx: &Arc<Context>, path: &Path) -> Result<Option<Arc<GameShared>>, replay::ReplayError> {
    let (header, game) = replay::rebuild(path, ctx.cards.clone(), None)?;
    if game.is_over().is_some() {
        return Ok(None);
    }
    if header.seat_tokens.len() != header.players.len() {
        warn!("{}: no seat tokens in the header; not resumable", path.display());
        return Ok(None);
    }
    let Some(spectator) = header.spectator_token.clone() else {
        warn!("{}: no spectator token in the header; not resumable", path.display());
        return Ok(None);
    };
    let format = Format::builtin(&header.format).ok_or_else(|| replay::ReplayError::UnknownFormat(header.format.clone()))?;
    let config = replay::config_from_header(&header, ctx.cards.clone())?;
    let seats: Vec<SeatSlot> = header
        .players
        .iter()
        .zip(&header.seat_tokens)
        .zip(config.players.iter())
        .map(|((p, token), setup)| SeatSlot {
            token: Token(token.clone()),
            name: Some(p.name.clone()),
            deck: Some(setup.deck.clone()),
            deck_names: p.deck.clone(),
            ready: true,
            // Everyone who was at this table keeps their seat: the game is
            // under way and `join_game` must not offer any of them.
            claimed: true,
            connections: 0,
            disconnected_since: Some(std::time::Instant::now()),
            idle_warned: false,
        })
        .collect();
    let lobby = Lobby {
        game_id: GameId(header.game_id.clone()),
        format_name: header.format.clone(),
        format,
        seed: header.seed,
        seats,
        spectator_token: Token(spectator),
        started: true,
    };
    let writer = ReplayWriter::open_append(path)?;
    let (status, _) = watch::channel(Status::default());
    Ok(Some(GameShared::new(ctx.clone(), lobby, status, Some(game), Some(writer))))
}
