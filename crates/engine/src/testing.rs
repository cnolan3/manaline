//! `TestGame`: a builder that puts specific objects on the battlefield and in
//! hands, then hands back a game positioned at the active player's first main
//! phase. Used by the engine's own tests and, from M3, by per-card tests.

use crate::action::Action;
use crate::card::{CardDb, CardId};
use crate::error::RulesError;
use crate::format::{CardPool, Deck, DeckSize, Format, Legality, MulliganRule, PlayerRange};
use crate::game::{Game, GameConfig, GameObject, PendingChoice, PlayerSetup};
use crate::types::{ObjectId, Phase, Seat, Zone};
use std::sync::Arc;

/// A permissive format for scenario tests: no opening hand, no mulligans,
/// any deck size, any card, up to six seats.
pub fn scenario_format(seats: usize) -> Format {
    Format {
        name: "Scenario".into(),
        players: PlayerRange {
            min: 2,
            max: seats.max(2) as u8,
        },
        starting_life: 20,
        starting_hand: 0,
        max_hand_size: 7,
        mulligan: MulliganRule::London { free_first: false },
        deck: Deck {
            size: DeckSize::Min(0),
            singleton: false,
            includes_commander: false,
        },
        rules: Vec::new(),
        legality: Legality {
            pool: CardPool::All,
            banned: Vec::new(),
            allowed: Vec::new(),
        },
    }
}

pub struct TestGame {
    db: Arc<CardDb>,
    seats: usize,
    seed: u64,
    starting_player: Seat,
    /// `None` = the default ten Forests; `Some(vec![])` = deliberately empty.
    libraries: Vec<Option<Vec<CardId>>>,
    battlefield: Vec<(Seat, CardId, bool)>,
    hands: Vec<(Seat, CardId)>,
    life: Vec<(Seat, i32)>,
}

impl TestGame {
    pub fn new(db: Arc<CardDb>, seats: usize) -> TestGame {
        TestGame {
            db,
            seats,
            seed: 1,
            starting_player: Seat(0),
            libraries: vec![None; seats],
            battlefield: Vec::new(),
            hands: Vec::new(),
            life: Vec::new(),
        }
    }

    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    pub fn starting_player(mut self, seat: Seat) -> Self {
        self.starting_player = seat;
        self
    }

    fn card(&self, name: &str) -> CardId {
        self.db
            .lookup(name)
            .unwrap_or_else(|| panic!("no card named {name:?} in the test database"))
    }

    /// Put `name` onto `seat`'s battlefield, untapped and not summoning sick.
    pub fn battlefield(mut self, seat: Seat, name: &str) -> Self {
        let id = self.card(name);
        self.battlefield.push((seat, id, false));
        self
    }

    pub fn battlefield_tapped(mut self, seat: Seat, name: &str) -> Self {
        let id = self.card(name);
        self.battlefield.push((seat, id, true));
        self
    }

    pub fn hand(mut self, seat: Seat, name: &str) -> Self {
        let id = self.card(name);
        self.hands.push((seat, id));
        self
    }

    /// Top of the library is the last name given. An empty list means an empty library.
    pub fn library(mut self, seat: Seat, names: &[&str]) -> Self {
        let ids: Vec<CardId> = names.iter().map(|n| self.card(n)).collect();
        self.libraries[seat.index()] = Some(ids);
        self
    }

    pub fn life(mut self, seat: Seat, life: i32) -> Self {
        self.life.push((seat, life));
        self
    }

    /// Build the game and advance it to the starting player's first main
    /// phase with priority. Libraries default to ten basic Forests so draws
    /// never fail unless a test asks for an empty library.
    pub fn build(self) -> Game {
        let forest = self.db.lookup("Forest");
        let players = (0..self.seats)
            .map(|i| {
                let deck = match &self.libraries[i] {
                    Some(d) => d.clone(),
                    None => forest.map(|f| vec![f; 10]).unwrap_or_default(),
                };
                PlayerSetup {
                    name: format!("P{i}"),
                    deck,
                }
            })
            .collect();
        let config = GameConfig {
            format: scenario_format(self.seats),
            players,
            cards: self.db.clone(),
            starting_player: Some(self.starting_player),
        };
        let mut game = Game::new(config, self.seed).expect("scenario game is legal");
        for (seat, card, tapped) in self.battlefield {
            let id = put_onto_battlefield(&mut game, seat, card);
            game.objects[id].tapped = tapped;
        }
        for (seat, card) in self.hands {
            put_in_hand(&mut game, seat, card);
        }
        for (seat, life) in self.life {
            game.players[seat.index()].life = life;
        }
        advance_to(&mut game, Phase::Main1).expect("can reach main phase");
        game
    }
}

/// Create an object directly on the battlefield under `seat`'s control, not summoning sick.
pub fn put_onto_battlefield(game: &mut Game, seat: Seat, card: CardId) -> ObjectId {
    let id = game.objects.insert_with_key(|id| {
        let mut o = GameObject::new(id, card, seat);
        o.zone = Zone::Battlefield;
        o
    });
    game.players[seat.index()].battlefield.push(id);
    id
}

pub fn put_in_hand(game: &mut Game, seat: Seat, card: CardId) -> ObjectId {
    let id = game.objects.insert_with_key(|id| {
        let mut o = GameObject::new(id, card, seat);
        o.zone = Zone::Hand;
        o
    });
    game.players[seat.index()].hand.push(id);
    id
}

/// Pass priority (declaring no attackers or blockers, and answering other
/// pending choices with their first suggestion) until the game is at the
/// start of `phase` with the active player holding priority.
pub fn advance_to(game: &mut Game, phase: Phase) -> Result<(), RulesError> {
    advance_until(game, |g| {
        g.phase == phase && g.pending.is_none() && g.priority == Some(g.active_player)
    })
}

/// Like `advance_to`, but stops as soon as `done` holds (checked before each
/// default action), so a test can stop at an open pending choice.
pub fn advance_until(game: &mut Game, done: impl Fn(&Game) -> bool) -> Result<(), RulesError> {
    for _ in 0..1000 {
        if game.is_over().is_some() || done(game) {
            return Ok(());
        }
        let (seat, action) = match &game.pending {
            Some(PendingChoice::DeclareAttackers { seat }) => (*seat, Action::DeclareAttackers { attackers: Vec::new() }),
            Some(PendingChoice::DeclareBlockers { seat, .. }) => (*seat, Action::DeclareBlockers { blocks: Vec::new() }),
            Some(p) => {
                let seat = p.seat();
                let first = game
                    .legal_actions(seat)
                    .into_iter()
                    .next()
                    .expect("pending seat has a legal action");
                (seat, first)
            }
            None => (game.priority.expect("someone has priority"), Action::PassPriority),
        };
        game.apply(seat, &action)?;
    }
    panic!("advance_until did not converge (turn {}, {:?})", game.turn, game.phase);
}

/// The seat currently required to act, if any.
pub fn acting_seat(game: &Game) -> Option<Seat> {
    game.must_act().keys().next().copied()
}
