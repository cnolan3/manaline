//! The property test (§3.8): random games at 2, 3, and 4 seats, invariants
//! checked after every action, and every finished game replayed from its
//! seed and action log.

use engine::bot::RandomBot;
use engine::view::HandView;
use engine::{Format, Game, GameConfig, PlayerSetup, Seat, Zone};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const MAX_ACTIONS: usize = 20_000;

/// Named explicitly rather than taken from the decks directory: a property
/// test must play the same games on every machine, and the decks directory
/// also holds whatever decks the developer is working on.
const TEST_DECKS: [&str; 4] = ["green", "red", "blue", "white"];

fn config(db: &Arc<engine::CardDb>, seats: usize) -> GameConfig {
    let format = if seats == 2 {
        Format::cube()
    } else {
        Format::builtin("free-for-all").unwrap()
    };
    let players = (0..seats)
        .map(|i| {
            let name = TEST_DECKS[i % TEST_DECKS.len()];
            let text = cards::deck_text(name).unwrap_or_else(|| panic!("no {name} deck in {}", cards::decks_dir().display()));
            PlayerSetup {
                name: format!("Bot{i}"),
                deck: cards::parse_decklist(&text, db).unwrap(),
            }
        })
        .collect();
    GameConfig {
        format,
        players,
        cards: db.clone(),
        starting_player: None,
    }
}

struct LifeTracker {
    life: Vec<i32>,
}

impl LifeTracker {
    fn check(&mut self, events: &[engine::Event]) {
        for e in events {
            if let engine::EventBase::LifeChanged { seat, from, to } = e {
                assert_eq!(self.life[seat.index()], *from, "life change does not chain from the previous value");
                self.life[seat.index()] = *to;
            }
        }
    }
}

fn check_invariants(game: &Game, seats: usize) {
    let must = game.must_act();
    if game.is_over().is_some() {
        assert!(must.is_empty(), "must_act must be empty once the game is over");
        for s in 0..seats {
            assert!(game.legal_actions(Seat(s as u8)).is_empty());
        }
    } else {
        assert!(
            !must.is_empty(),
            "must_act is empty while the game is not over (turn {}, {:?})",
            game.turn,
            game.phase
        );
        for s in 0..seats {
            let seat = Seat(s as u8);
            let acts = game.legal_actions(seat);
            if must.contains_key(&seat) {
                assert!(!acts.is_empty(), "{seat} is in must_act but has no legal action");
            } else {
                assert!(acts.is_empty(), "{seat} is not in must_act but has legal actions: {acts:?}");
            }
        }
    }

    // Every object is in exactly one zone list, and its zone field agrees.
    let mut seen: HashMap<engine::ObjectId, Zone> = HashMap::new();
    for (i, p) in game.players.iter().enumerate() {
        let lists = [
            (Zone::Library, &p.library),
            (Zone::Hand, &p.hand),
            (Zone::Graveyard, &p.graveyard),
            (Zone::Exile, &p.exile),
            (Zone::Battlefield, &p.battlefield),
            (Zone::Command, &p.command),
        ];
        for (zone, list) in lists {
            for &id in list {
                assert!(seen.insert(id, zone).is_none(), "{id} appears in two zones");
                let obj = &game.objects[id];
                assert_eq!(obj.zone, zone, "{id} zone field disagrees with its list");
                let expected_owner = match zone {
                    Zone::Battlefield => obj.controller,
                    _ => obj.owner,
                };
                assert_eq!(expected_owner, Seat(i as u8), "{id} is in the wrong player's {zone:?} list");
            }
        }
    }
    for so in &game.stack {
        // Abilities and triggers reference their source, which stays where it is.
        if so.kind == engine::StackKind::Spell {
            assert!(seen.insert(so.object, Zone::Stack).is_none());
            assert_eq!(game.objects[so.object].zone, Zone::Stack);
        }
    }
    for (id, obj) in &game.objects {
        match obj.zone {
            Zone::OutOfGame => {
                assert!(!seen.contains_key(&id));
                let token = game.card_by_id(obj.card).token;
                assert!(
                    token || game.is_eliminated(obj.owner),
                    "{id} left the game but its owner is still playing"
                );
            }
            z => assert_eq!(seen.get(&id), Some(&z), "{id} says it is in {z:?} but no list holds it"),
        }
    }

    // Views never leak hidden information.
    let spectator = game.view_spectator();
    for p in &spectator.players {
        assert!(matches!(p.hand, HandView::Hidden { .. }));
        assert!(p.mana_pool.is_none());
    }
    for (id, ov) in &spectator.objects {
        assert!(ov.zone.is_public(), "spectator view shows {id} in {:?}", ov.zone);
    }
    for s in 0..seats {
        let seat = Seat(s as u8);
        let view = game.view(seat);
        assert_eq!(view.you, Some(seat));
        assert_eq!(view.must_act, must);
        for p in &view.players {
            if p.seat == seat {
                match &p.hand {
                    HandView::Yours(ids) => assert_eq!(ids, &game.players[s].hand),
                    HandView::Hidden { .. } => panic!("your own hand is hidden from you"),
                }
            } else {
                assert!(matches!(p.hand, HandView::Hidden { .. }), "{seat} can see {}'s hand", p.seat);
                assert!(p.mana_pool.is_none());
            }
            assert_eq!(p.library.count as usize, game.players[p.seat.index()].library.len());
        }
        for (id, ov) in &view.objects {
            let obj = &game.objects[*id];
            assert!(
                obj.zone.is_public() || (obj.zone == Zone::Hand && obj.owner == seat),
                "{seat}'s view shows {id} in {:?} owned by {}",
                obj.zone,
                obj.owner
            );
            assert_eq!(ov.zone, obj.zone);
        }
        // The view must serialize (it is what goes over the wire). Checked on
        // a sample of states; it is the slowest invariant.
        if game.state_version() % 25 == 0 {
            let json = serde_json::to_string(&view).unwrap();
            let back: engine::GameView = serde_json::from_str(&json).unwrap();
            assert_eq!(back, view);
        }
    }
}

fn run_game(db: &Arc<engine::CardDb>, seats: usize, seed: u64) -> Game {
    let mut game = Game::new(config(db, seats), seed).unwrap();
    let mut bot = RandomBot::new(seed ^ 0xdead_beef);
    let mut life = LifeTracker {
        life: vec![game.format.starting_life; seats],
    };
    let mut turn_seen = 0;
    check_invariants(&game, seats);
    let mut applied = 0;
    while game.is_over().is_none() {
        assert!(
            applied < MAX_ACTIONS,
            "game did not finish within {MAX_ACTIONS} actions (turn {})",
            game.turn
        );
        let must = game.must_act();
        let (&seat, _) = must.iter().next().unwrap();
        let action = bot.choose(&game, seat).unwrap();
        let before = game.state_version();
        let events = game.apply(seat, &action).unwrap_or_else(|e| panic!("{action:?} for {seat}: {e}"));
        assert_eq!(game.state_version(), before + 1);
        life.check(&events);
        assert!(game.turn >= turn_seen);
        turn_seen = game.turn;
        check_invariants(&game, seats);
        applied += 1;
    }
    for (i, p) in game.players.iter().enumerate() {
        assert_eq!(p.life, life.life[i], "Bot{i}'s life disagrees with the LifeChanged events");
    }
    assert_eq!(game.history().len(), applied);
    game
}

fn replay_matches(db: &Arc<engine::CardDb>, seats: usize, game: &Game) {
    let replayed = Game::replay(config(db, seats), game.seed(), game.history()).unwrap();
    assert_eq!(replayed.log, game.log, "replay produced a different event log");
    assert_eq!(replayed.view_spectator(), game.view_spectator());
    assert_eq!(replayed.is_over(), game.is_over());
}

fn seeds() -> Vec<u64> {
    let n: u64 = std::env::var("MANALINE_PROPERTY_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    (1..=n).collect()
}

#[test]
fn random_games_two_seats() {
    let db = Arc::new(cards::core());
    for seed in seeds() {
        let game = run_game(&db, 2, seed);
        replay_matches(&db, 2, &game);
    }
}

#[test]
fn random_games_three_seats() {
    let db = Arc::new(cards::core());
    for seed in seeds() {
        let game = run_game(&db, 3, seed);
        replay_matches(&db, 3, &game);
    }
}

#[test]
fn random_games_four_seats() {
    let db = Arc::new(cards::core());
    for seed in seeds() {
        let game = run_game(&db, 4, seed);
        replay_matches(&db, 4, &game);
    }
}

#[test]
fn eliminated_seats_stay_out_and_the_rest_keep_playing() {
    // In a pod, at least one game must feature an elimination that does not
    // end the game; check the bookkeeping around it.
    let db = Arc::new(cards::core());
    let mut saw_mid_game_elimination = false;
    for seed in 1..=8u64 {
        let game = run_game(&db, 4, seed);
        let mut alive: BTreeMap<Seat, bool> = (0..4).map(|s| (Seat(s), true)).collect();
        for e in &game.log {
            if let engine::EventBase::Eliminated { seat, .. } = e {
                alive.insert(*seat, false);
                if alive.values().filter(|a| **a).count() >= 2 {
                    saw_mid_game_elimination = true;
                }
            }
        }
        let survivors: Vec<Seat> = alive.iter().filter(|(_, a)| **a).map(|(s, _)| *s).collect();
        assert_eq!(survivors.len(), 1);
        assert_eq!(game.is_over(), Some(engine::Outcome::Winner(survivors[0])));
        assert_eq!(game.turn_order, survivors);
    }
    assert!(saw_mid_game_elimination, "eight four-seat games and never a mid-game elimination?");
}
