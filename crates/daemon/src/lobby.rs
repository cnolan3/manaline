//! The lobby: one game's format, seats, tokens, decks, and readiness.

use engine::{CardId, Format, Seat};
use protocol::{GameId, LobbyView, Role, SeatStatus, Token};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct SeatSlot {
    pub token: Token,
    pub name: Option<String>,
    pub deck: Option<Vec<CardId>>,
    /// The decklist as submitted, for the replay header.
    pub deck_names: Vec<String>,
    pub ready: bool,
    pub connections: u32,
    /// When this seat last dropped to no connections at all, which is what the
    /// idle and abandonment clocks measure (§5, M8). A seat that has never
    /// connected has been away since the lobby was created.
    pub disconnected_since: Option<Instant>,
    /// One idle warning per absence, cleared when the seat comes back.
    pub idle_warned: bool,
}

#[derive(Clone, Debug)]
pub struct Lobby {
    pub game_id: GameId,
    pub format_name: String,
    pub format: Format,
    pub seed: u64,
    pub seats: Vec<SeatSlot>,
    pub spectator_token: Token,
    pub started: bool,
}

impl Lobby {
    pub fn new(format_name: &str, format: Format, seats: u8, seed: u64) -> Lobby {
        Lobby {
            game_id: GameId(random_game_code()),
            format_name: format_name.to_string(),
            format,
            seed,
            seats: (0..seats)
                .map(|_| SeatSlot {
                    token: random_token(),
                    name: None,
                    deck: None,
                    deck_names: Vec::new(),
                    ready: false,
                    connections: 0,
                    disconnected_since: Some(Instant::now()),
                    idle_warned: false,
                })
                .collect(),
            spectator_token: random_token(),
            started: false,
        }
    }

    /// Count a connection for `seat`: the idle clock stops and rearms.
    pub fn connect(&mut self, seat: Seat) {
        let slot = &mut self.seats[seat.index()];
        slot.connections += 1;
        slot.disconnected_since = None;
        slot.idle_warned = false;
    }

    /// Drop one connection; the idle clock starts when the last one goes.
    pub fn disconnect(&mut self, seat: Seat) {
        let slot = &mut self.seats[seat.index()];
        slot.connections = slot.connections.saturating_sub(1);
        if slot.connections == 0 && slot.disconnected_since.is_none() {
            slot.disconnected_since = Some(Instant::now());
        }
    }

    pub fn resolve(&self, token: &Token) -> Option<Role> {
        if *token == self.spectator_token {
            return Some(Role::Spectator);
        }
        self.seats.iter().position(|s| s.token == *token).map(|i| Role::Seat(Seat(i as u8)))
    }

    pub fn seat_tokens(&self) -> Vec<Token> {
        self.seats.iter().map(|s| s.token.clone()).collect()
    }

    pub fn all_ready(&self) -> bool {
        self.seats.iter().all(|s| s.ready && s.deck.is_some())
    }

    pub fn seat_name(&self, seat: Seat) -> String {
        self.seats[seat.index()].name.clone().unwrap_or_else(|| format!("Seat {}", seat.0))
    }

    pub fn view(&self) -> LobbyView {
        LobbyView {
            seats: self
                .seats
                .iter()
                .enumerate()
                .map(|(i, s)| SeatStatus {
                    seat: Seat(i as u8),
                    name: s.name.clone(),
                    connected: s.connections > 0,
                    deck_ok: s.deck.is_some(),
                    ready: s.ready,
                })
                .collect(),
            started: self.started,
        }
    }
}

fn random_token() -> Token {
    let s: String = thread_rng().sample_iter(&Alphanumeric).take(24).map(char::from).collect();
    Token(s)
}

/// Six characters from an alphabet without look-alikes (no 0/O, 1/I/L).
fn random_game_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = thread_rng();
    (0..6).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}
