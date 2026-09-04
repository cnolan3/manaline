//! The built-in random opponent: picks uniformly from `legal_actions`.

use crate::action::Action;
use crate::error::RulesError;
use crate::game::Game;
use crate::types::Seat;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

pub struct RandomBot {
    rng: ChaCha8Rng,
}

impl RandomBot {
    pub fn new(seed: u64) -> RandomBot {
        RandomBot { rng: ChaCha8Rng::seed_from_u64(seed) }
    }

    /// A uniformly random legal action for `seat`. The bot never concedes
    /// while it has any other option, so its games run to a real result.
    pub fn choose(&mut self, game: &Game, seat: Seat) -> Option<Action> {
        let acts = game.legal_actions(seat);
        let playable: Vec<&Action> = acts.iter().filter(|a| !matches!(a, Action::Concede)).collect();
        let pool: Vec<&Action> = if playable.is_empty() { acts.iter().collect() } else { playable };
        pool.choose(&mut self.rng).map(|a| (*a).clone())
    }
}

/// Drive `game` to completion with one random bot in every seat. Returns the
/// number of actions applied. Stops early after `max_actions`.
pub fn play_random_game(game: &mut Game, bot_seed: u64, max_actions: usize) -> Result<usize, RulesError> {
    let mut bot = RandomBot::new(bot_seed);
    let mut applied = 0;
    while game.is_over().is_none() && applied < max_actions {
        let must = game.must_act();
        let (&seat, _) = must.iter().next().expect("must_act is non-empty while the game is not over");
        let action = bot.choose(game, seat).expect("a seat in must_act has a legal action");
        game.apply(seat, &action)?;
        applied += 1;
    }
    Ok(applied)
}
