//! One scenario per rule the M0 engine implements: priority passing, land
//! drops, casting and resolving, mana payment, combat, damage assignment,
//! state-based actions, elimination in a pod, mulligans, hidden information,
//! and determinism.

use engine::testing::{acting_seat, advance_to, advance_until, TestGame};
use engine::{
    ActReason, Action, AttackTarget, DamageTarget, Elimination, Format, Game, GameConfig, Outcome, PendingChoice, Phase, PlayerSetup,
    RulesError, Seat, Zone,
};
use std::sync::Arc;

fn db() -> Arc<engine::CardDb> {
    Arc::new(cards::core())
}

fn hand_card(game: &Game, seat: Seat, name: &str) -> engine::ObjectId {
    game.players[seat.index()]
        .hand
        .iter()
        .copied()
        .find(|&id| game.object_name(id) == name)
        .unwrap_or_else(|| panic!("{name} not in {seat}'s hand"))
}

fn battlefield_card(game: &Game, seat: Seat, name: &str) -> engine::ObjectId {
    game.players[seat.index()]
        .battlefield
        .iter()
        .copied()
        .find(|&id| game.object_name(id) == name)
        .unwrap_or_else(|| panic!("{name} not on {seat}'s battlefield"))
}

fn battlefield_cards(game: &Game, seat: Seat, name: &str) -> Vec<engine::ObjectId> {
    game.players[seat.index()]
        .battlefield
        .iter()
        .copied()
        .filter(|&id| game.object_name(id) == name)
        .collect()
}

fn pass_both(game: &mut Game) {
    for _ in 0..game.turn_order.len() {
        let seat = game.priority.expect("someone has priority");
        game.apply(seat, &Action::PassPriority).unwrap();
    }
}

#[test]
fn scenario_starts_in_main_phase_with_priority() {
    let game = TestGame::new(db(), 2).build();
    assert_eq!(game.turn, 1);
    assert_eq!(game.phase, Phase::Main1);
    assert_eq!(game.active_player, Seat(0));
    assert_eq!(game.priority, Some(Seat(0)));
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::Priority));
    assert!(game.legal_actions(Seat(1)).is_empty());
}

#[test]
fn passing_priority_walks_the_turn() {
    let mut game = TestGame::new(db(), 2).build();
    let expected = [
        Phase::BeginCombat,
        Phase::DeclareAttackers,
        Phase::EndCombat, // no attackers: blockers and damage are skipped
        Phase::Main2,
        Phase::End,
    ];
    for phase in expected {
        pass_both(&mut game);
        assert_eq!(game.phase, phase);
        assert_eq!(game.priority, Some(Seat(0)));
    }
    pass_both(&mut game);
    // Cleanup has no priority; the engine moves straight to seat 1's turn.
    assert_eq!(game.turn, 2);
    assert_eq!(game.active_player, Seat(1));
    assert_eq!(game.phase, Phase::Upkeep);
    assert_eq!(game.priority, Some(Seat(1)));
}

#[test]
fn draw_step_is_skipped_only_on_the_first_turn_of_a_two_player_game() {
    let mut two = TestGame::new(db(), 2).build();
    assert_eq!(two.players[0].hand.len(), 0, "two-player starting player skipped their draw");
    advance_to(&mut two, Phase::Main1).unwrap();
    while two.turn < 2 || two.phase != Phase::Main1 {
        pass_both(&mut two);
    }
    assert_eq!(two.players[1].hand.len(), 1, "seat 1 draws on turn 2");

    let three = TestGame::new(db(), 3).build();
    assert_eq!(three.players[0].hand.len(), 1, "in a pod the starting player draws on turn 1");
}

#[test]
fn one_land_per_turn() {
    let mut game = TestGame::new(db(), 2).hand(Seat(0), "Forest").hand(Seat(0), "Forest").build();
    let forest = hand_card(&game, Seat(0), "Forest");
    let acts = game.legal_actions(Seat(0));
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::PlayLand { .. })).count(), 2);
    game.apply(Seat(0), &Action::PlayLand { object: forest }).unwrap();
    assert_eq!(game.objects[forest].zone, Zone::Battlefield);
    assert_eq!(game.priority, Some(Seat(0)), "playing a land keeps priority");
    let acts = game.legal_actions(Seat(0));
    assert!(
        !acts.iter().any(|a| matches!(a, Action::PlayLand { .. })),
        "second land is not offered"
    );
    let other = hand_card(&game, Seat(0), "Forest");
    let err = game.apply(Seat(0), &Action::PlayLand { object: other }).unwrap_err();
    assert!(matches!(err, RulesError::IllegalAction { .. }));
}

#[test]
fn creature_spell_uses_the_stack_and_resolves_summoning_sick() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .hand(Seat(0), "Grizzly Bears")
        .build();
    let bears = hand_card(&game, Seat(0), "Grizzly Bears");
    let casts: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::CastSpell { .. }))
        .collect();
    assert_eq!(casts.len(), 1, "one colour combination pays {{1}}{{G}} with two Forests");
    let events = game.apply(Seat(0), &casts[0]).unwrap();
    assert!(events.iter().any(|e| matches!(e, engine::EventBase::Cast { .. })));
    assert_eq!(game.stack.len(), 1);
    assert_eq!(game.objects[bears].zone, Zone::Stack);
    assert!(game.mana_sources(Seat(0)).is_empty(), "both Forests tapped");
    assert_eq!(game.priority, Some(Seat(0)), "caster gets priority back");

    game.apply(Seat(0), &Action::PassPriority).unwrap();
    assert_eq!(game.priority, Some(Seat(1)));
    assert!(game
        .legal_actions(Seat(1))
        .iter()
        .all(|a| matches!(a, Action::PassPriority | Action::Concede)));
    game.apply(Seat(1), &Action::PassPriority).unwrap();
    assert!(game.stack.is_empty());
    assert_eq!(game.objects[bears].zone, Zone::Battlefield);
    assert!(game.objects[bears].summoning_sick);
    assert_eq!(game.phase, Phase::Main1, "still main phase after resolution");
    assert_eq!(game.priority, Some(Seat(0)));
    assert!(game.attack_candidates(Seat(0)).is_empty(), "summoning sick creatures can't attack");
}

#[test]
fn payments_are_enumerated_by_colour_combination() {
    let game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Centaur Courser")
        .hand(Seat(0), "Craw Wurm")
        .build();
    let bears = hand_card(&game, Seat(0), "Grizzly Bears");
    let courser = hand_card(&game, Seat(0), "Centaur Courser");
    let wurm = hand_card(&game, Seat(0), "Craw Wurm");
    let acts = game.legal_actions(Seat(0));
    let casts_of = |id| {
        acts.iter()
            .filter(|a| matches!(a, Action::CastSpell { object, .. } if *object == id))
            .count()
    };
    assert_eq!(casts_of(bears), 2, "{{1}}{{G}}: tap G+G or G+R");
    assert_eq!(casts_of(courser), 1, "{{2}}{{G}}: all three lands");
    assert_eq!(casts_of(wurm), 0, "{{4}}{{G}}{{G}} is unaffordable");
    let view = game.view(Seat(0));
    assert!(view.objects[&bears].castable);
    assert!(!view.objects[&wurm].castable);
}

#[test]
fn unblocked_attacker_deals_damage_to_the_player() {
    let mut game = TestGame::new(db(), 2).battlefield(Seat(0), "Grizzly Bears").build();
    let bears = battlefield_card(&game, Seat(0), "Grizzly Bears");
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::DeclareAttackers));
    assert_eq!(game.priority, None, "no priority while the declaration is pending");
    let acts = game.legal_actions(Seat(0));
    assert!(acts.contains(&Action::DeclareAttackers {
        attackers: vec![(bears, AttackTarget::Player(Seat(1)))]
    }));
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    assert!(game.objects[bears].tapped, "attacking taps");
    assert_eq!(game.priority, Some(Seat(0)));
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::DeclareBlockers);
    assert_eq!(game.must_act().get(&Seat(1)), Some(&ActReason::DeclareBlockers));
    game.apply(Seat(1), &Action::DeclareBlockers { blocks: vec![] }).unwrap();
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::CombatDamage);
    assert_eq!(game.players[1].life, 18);
    assert_eq!(game.priority, Some(Seat(0)), "priority after damage");
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::EndCombat);
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::Main2);
    assert!(game.objects[bears].attacking.is_none(), "removed from combat");
}

#[test]
fn single_block_needs_no_assignment_and_lethal_damage_destroys() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Hill Giant")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let giant = battlefield_card(&game, Seat(0), "Hill Giant");
    let bears = battlefield_card(&game, Seat(1), "Grizzly Bears");
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(giant, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    pass_both(&mut game);
    game.apply(
        Seat(1),
        &Action::DeclareBlockers {
            blocks: vec![(bears, giant)],
        },
    )
    .unwrap();
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::CombatDamage);
    assert!(game.pending.is_none(), "one blocker: no assignment choice");
    assert_eq!(game.objects[bears].zone, Zone::Graveyard, "2/2 took 3");
    assert_eq!(game.objects[giant].zone, Zone::Battlefield);
    assert_eq!(game.objects[giant].damage, 2);
    assert_eq!(game.players[1].life, 20, "blocked: no damage to the player");
    // Damage wears off in cleanup.
    advance_until(&mut game, |g| g.turn == 2).unwrap();
    assert_eq!(game.objects[giant].damage, 0);
}

#[test]
fn multiple_blockers_require_a_damage_assignment_validated_by_rule() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Hill Giant")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let giant = battlefield_card(&game, Seat(0), "Hill Giant");
    let bears = battlefield_cards(&game, Seat(1), "Grizzly Bears");
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(giant, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    pass_both(&mut game);
    game.apply(
        Seat(1),
        &Action::DeclareBlockers {
            blocks: vec![(bears[0], giant), (bears[1], giant)],
        },
    )
    .unwrap();
    pass_both(&mut game);

    assert_eq!(game.phase, Phase::CombatDamage);
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::AssignDamage));
    assert_eq!(game.priority, None, "no priority between the assignment and the damage");
    let suggestions = game.legal_actions(Seat(0));
    assert!(suggestions.iter().all(|a| a.is_division() || matches!(a, Action::Concede)));
    assert!(suggestions.contains(&Action::AssignCombatDamage {
        attacker: giant,
        assignments: vec![(DamageTarget::Object(bears[0]), 3)],
    }));

    // Wrong total, unknown recipient, and the player are rejected.
    let bad_sum = Action::AssignCombatDamage {
        attacker: giant,
        assignments: vec![(DamageTarget::Object(bears[0]), 2)],
    };
    assert!(matches!(game.apply(Seat(0), &bad_sum), Err(RulesError::IllegalAction { .. })));
    let to_player = Action::AssignCombatDamage {
        attacker: giant,
        assignments: vec![(DamageTarget::Player(Seat(1)), 3)],
    };
    assert!(matches!(game.apply(Seat(0), &to_player), Err(RulesError::IllegalAction { .. })));

    // A split that was never listed is accepted because it satisfies the rule.
    let custom = Action::AssignCombatDamage {
        attacker: giant,
        assignments: vec![(DamageTarget::Object(bears[0]), 1), (DamageTarget::Object(bears[1]), 2)],
    };
    assert!(!suggestions.contains(&custom));
    game.apply(Seat(0), &custom).unwrap();
    assert_eq!(game.objects[bears[0]].zone, Zone::Battlefield);
    assert_eq!(game.objects[bears[0]].damage, 1);
    assert_eq!(game.objects[bears[1]].zone, Zone::Graveyard);
    assert_eq!(game.objects[giant].zone, Zone::Graveyard, "3/3 took 4 from two bears");
    assert_eq!(game.priority, Some(Seat(0)));
}

#[test]
fn zero_life_eliminates_and_ends_a_two_player_game() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Grizzly Bears")
        .life(Seat(1), 2)
        .build();
    let bears = battlefield_card(&game, Seat(0), "Grizzly Bears");
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    pass_both(&mut game);
    game.apply(Seat(1), &Action::DeclareBlockers { blocks: vec![] }).unwrap();
    let events = pass_both_collect(&mut game);
    assert_eq!(game.players[1].life, 0);
    assert_eq!(game.players[1].eliminated, Some(Elimination::LifeZero));
    assert_eq!(game.is_over(), Some(Outcome::Winner(Seat(0))));
    assert!(events.iter().any(|e| matches!(e, engine::EventBase::GameOver { .. })));
    assert!(game.must_act().is_empty());
    assert!(matches!(
        game.apply(Seat(0), &Action::PassPriority),
        Err(RulesError::GameOver { .. })
    ));
}

fn pass_both_collect(game: &mut Game) -> Vec<engine::Event> {
    let mut all = Vec::new();
    for _ in 0..game.turn_order.len() {
        let Some(seat) = game.priority else { break };
        all.extend(game.apply(seat, &Action::PassPriority).unwrap());
    }
    all
}

#[test]
fn a_pod_continues_after_an_elimination() {
    let mut game = TestGame::new(db(), 3)
        .battlefield(Seat(0), "Grizzly Bears")
        .life(Seat(1), 2)
        .build();
    let bears = battlefield_card(&game, Seat(0), "Grizzly Bears");
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    let acts = game.legal_actions(Seat(0));
    assert!(acts.contains(&Action::DeclareAttackers {
        attackers: vec![(bears, AttackTarget::Player(Seat(2)))]
    }));
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    pass_both(&mut game);
    assert_eq!(acting_seat(&game), Some(Seat(1)), "only the attacked seat declares blockers");
    game.apply(Seat(1), &Action::DeclareBlockers { blocks: vec![] }).unwrap();
    pass_both(&mut game);
    assert_eq!(game.players[1].eliminated, Some(Elimination::LifeZero));
    assert_eq!(game.is_over(), None);
    assert_eq!(game.turn_order, vec![Seat(0), Seat(2)]);
    assert!(game.players[1].battlefield.is_empty() && game.players[1].library.is_empty());
    assert_eq!(game.opponents_of(Seat(0)).collect::<Vec<_>>(), vec![Seat(2)]);
    assert!(game.view(Seat(2)).players[1].eliminated);
    // Priority passes over the empty seat and the next turn belongs to seat 2.
    advance_until(&mut game, |g| g.turn == 2).unwrap();
    assert_eq!(game.active_player, Seat(2));
}

#[test]
fn blockers_are_declared_sequentially_in_apnap_order() {
    let mut game = TestGame::new(db(), 3)
        .battlefield(Seat(0), "Grizzly Bears")
        .battlefield(Seat(0), "Hill Giant")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(2), "Grizzly Bears")
        .build();
    let bears = battlefield_card(&game, Seat(0), "Grizzly Bears");
    let giant = battlefield_card(&game, Seat(0), "Hill Giant");
    let b1 = battlefield_card(&game, Seat(1), "Grizzly Bears");
    let b2 = battlefield_card(&game, Seat(2), "Grizzly Bears");
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(bears, AttackTarget::Player(Seat(2))), (giant, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    pass_both(&mut game);
    assert_eq!(
        game.pending,
        Some(PendingChoice::DeclareBlockers {
            seat: Seat(1),
            remaining: vec![Seat(2)]
        })
    );
    // Seat 1 may only block what attacks seat 1.
    let wrong = Action::DeclareBlockers { blocks: vec![(b1, bears)] };
    assert!(matches!(game.apply(Seat(1), &wrong), Err(RulesError::IllegalAction { .. })));
    assert!(matches!(
        game.apply(Seat(2), &Action::DeclareBlockers { blocks: vec![] }),
        Err(RulesError::NotYourTurnToAct { .. })
    ));
    game.apply(Seat(1), &Action::DeclareBlockers { blocks: vec![(b1, giant)] }).unwrap();
    assert_eq!(
        game.pending,
        Some(PendingChoice::DeclareBlockers {
            seat: Seat(2),
            remaining: vec![]
        })
    );
    game.apply(Seat(2), &Action::DeclareBlockers { blocks: vec![(b2, bears)] }).unwrap();
    assert_eq!(game.priority, Some(Seat(0)));
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::CombatDamage);
    assert_eq!(game.objects[b1].zone, Zone::Graveyard);
    assert_eq!(game.objects[b2].zone, Zone::Graveyard);
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
    assert_eq!(game.objects[giant].damage, 2);
}

#[test]
fn drawing_from_an_empty_library_loses() {
    let mut game = TestGame::new(db(), 2).library(Seat(1), &[]).build();
    advance_until(&mut game, |g| g.is_over().is_some() || g.turn == 3).unwrap();
    assert_eq!(game.players[1].eliminated, Some(Elimination::DrewFromEmptyLibrary));
    assert_eq!(game.is_over(), Some(Outcome::Winner(Seat(0))));
}

#[test]
fn active_player_conceding_ends_their_turn() {
    let mut game = TestGame::new(db(), 3).build();
    assert!(game.legal_actions(Seat(0)).contains(&Action::Concede));
    game.apply(Seat(0), &Action::Concede).unwrap();
    assert_eq!(game.players[0].eliminated, Some(Elimination::Conceded));
    assert_eq!(game.is_over(), None);
    assert_eq!(game.turn, 2);
    assert_eq!(game.active_player, Seat(1));
    assert_eq!(game.turn_order, vec![Seat(1), Seat(2)]);
}

#[test]
fn any_seat_may_concede_at_any_time() {
    let mut game = TestGame::new(db(), 3).build();
    assert_eq!(game.priority, Some(Seat(0)));
    assert!(
        !game.legal_actions(Seat(2)).contains(&Action::Concede),
        "not listed when it isn't their turn"
    );
    game.apply(Seat(2), &Action::Concede).unwrap();
    assert_eq!(game.players[2].eliminated, Some(Elimination::Conceded));
    assert_eq!(game.turn_order, vec![Seat(0), Seat(1)]);
    assert!(matches!(
        game.apply(Seat(2), &Action::Concede),
        Err(RulesError::IllegalAction { .. })
    ));
    assert_eq!(game.priority, Some(Seat(0)), "the turn continues");
}

#[test]
fn cleanup_discards_down_to_hand_size() {
    let mut t = TestGame::new(db(), 2);
    for _ in 0..9 {
        t = t.hand(Seat(0), "Forest");
    }
    let mut game = t.build();
    // Play nothing; walk to cleanup.
    advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::Discard { .. }))).unwrap();
    assert_eq!(game.phase, Phase::Cleanup);
    assert_eq!(game.pending, Some(PendingChoice::Discard { seat: Seat(0), count: 2 }));
    let acts = game.legal_actions(Seat(0));
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::Discard { .. })).count(), 36, "C(9,2)");
    let choice = acts.into_iter().find(|a| matches!(a, Action::Discard { .. })).unwrap();
    game.apply(Seat(0), &choice).unwrap();
    assert_eq!(game.players[0].hand.len(), 7);
    assert_eq!(game.players[0].graveyard.len(), 2);
    assert_eq!(game.turn, 2);
}

fn cube_game(starting: Seat) -> Game {
    let db = db();
    let green = cards::parse_decklist(&cards::deck_text("green").unwrap(), &db).unwrap();
    let red = cards::parse_decklist(&cards::deck_text("red").unwrap(), &db).unwrap();
    let config = GameConfig {
        format: Format::cube(),
        players: vec![
            PlayerSetup {
                name: "Connor".into(),
                deck: green,
            },
            PlayerSetup {
                name: "Claude".into(),
                deck: red,
            },
        ],
        cards: db,
        starting_player: Some(starting),
    };
    Game::new(config, 42).unwrap()
}

#[test]
fn london_mulligan_bottoms_one_card_per_mulligan() {
    let mut game = cube_game(Seat(1));
    assert_eq!(game.turn, 0);
    assert_eq!(
        game.must_act().get(&Seat(1)),
        Some(&ActReason::Mulligan),
        "starting player decides first"
    );
    assert!(game.legal_actions(Seat(0)).is_empty());
    game.apply(Seat(1), &Action::Mulligan { keep: false }).unwrap();
    assert_eq!(game.players[1].hand.len(), 7, "London: redraw seven");
    assert_eq!(game.must_act().get(&Seat(1)), Some(&ActReason::Mulligan));
    game.apply(Seat(1), &Action::Mulligan { keep: true }).unwrap();
    assert_eq!(game.pending, Some(PendingChoice::BottomCards { seat: Seat(1), count: 1 }));
    let acts = game.legal_actions(Seat(1));
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::BottomCards { .. })).count(), 7);
    let bottom = acts.into_iter().find(|a| matches!(a, Action::BottomCards { .. })).unwrap();
    game.apply(Seat(1), &bottom).unwrap();
    assert_eq!(game.players[1].hand.len(), 6);
    assert_eq!(game.players[1].library.len(), 34);
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::Mulligan));
    game.apply(Seat(0), &Action::Mulligan { keep: true }).unwrap();
    assert_eq!(game.turn, 1);
    assert_eq!(game.active_player, Seat(1));
    assert_eq!(game.phase, Phase::Upkeep);
}

#[test]
fn views_hide_other_hands_and_all_libraries() {
    let game = cube_game(Seat(0));
    let v0 = game.view(Seat(0));
    let v1 = game.view(Seat(1));
    let spec = game.view_spectator();
    assert_eq!(v0.player(Seat(0)).hand, engine::HandView::Yours(game.players[0].hand.clone()));
    assert_eq!(v0.player(Seat(1)).hand, engine::HandView::Hidden { count: 7 });
    assert_eq!(v1.player(Seat(0)).hand, engine::HandView::Hidden { count: 7 });
    for id in &game.players[1].hand {
        assert!(!v0.objects.contains_key(id));
        assert!(v1.objects.contains_key(id));
        assert!(!spec.objects.contains_key(id));
    }
    assert_eq!(v0.player(Seat(0)).library.count, 33);
    assert!(spec.players.iter().all(|p| matches!(p.hand, engine::HandView::Hidden { count: 7 })));
    // Draw events reveal cards only to the drawing seat.
    let drew = game
        .log
        .iter()
        .find(|e| matches!(e, engine::EventBase::Drew { seat: Seat(1), .. }))
        .unwrap();
    match drew.view(Some(Seat(0))).unwrap() {
        engine::EventBase::Drew {
            cards: engine::DrawnCards::Hidden { count },
            ..
        } => assert_eq!(count, 7),
        other => panic!("{other:?}"),
    }
    match drew.view(Some(Seat(1))).unwrap() {
        engine::EventBase::Drew {
            cards: engine::DrawnCards::Yours(ids),
            ..
        } => assert_eq!(ids.len(), 7),
        other => panic!("{other:?}"),
    }
    let private = engine::Event::Chat {
        from: Seat(0),
        to: Some(Seat(1)),
        text: "gg".into(),
    };
    assert!(private.view(None).is_none());
    assert!(private.view(Some(Seat(1))).is_some());
}

#[test]
fn illegal_and_out_of_turn_actions_are_rejected() {
    let mut game = TestGame::new(db(), 2).hand(Seat(1), "Forest").build();
    let forest = hand_card(&game, Seat(1), "Forest");
    let version = game.state_version();
    assert!(matches!(
        game.apply(Seat(1), &Action::PlayLand { object: forest }),
        Err(RulesError::NotYourTurnToAct { seat: Seat(1) })
    ));
    assert!(matches!(
        game.apply(Seat(0), &Action::PlayLand { object: forest }),
        Err(RulesError::IllegalAction { .. })
    ));
    assert!(matches!(
        game.apply(Seat(0), &Action::DeclareAttackers { attackers: vec![] }),
        Err(RulesError::IllegalAction { .. })
    ));
    assert_eq!(game.state_version(), version, "rejected actions do not change state");
}

#[test]
fn illegal_decks_and_player_counts_are_refused() {
    let db = db();
    let green = cards::parse_decklist(&cards::deck_text("green").unwrap(), &db).unwrap();
    let short = green[..30].to_vec();
    let players = |a: Vec<engine::CardId>, b: Vec<engine::CardId>| {
        vec![PlayerSetup { name: "a".into(), deck: a }, PlayerSetup { name: "b".into(), deck: b }]
    };
    let err = Game::new(
        GameConfig {
            format: Format::cube(),
            players: players(short, green.clone()),
            cards: db.clone(),
            starting_player: None,
        },
        1,
    )
    .unwrap_err();
    assert!(matches!(err, RulesError::Setup { .. }), "{err}");
    let mut three = players(green.clone(), green.clone());
    three.push(PlayerSetup {
        name: "c".into(),
        deck: green.clone(),
    });
    let err = Game::new(
        GameConfig {
            format: Format::cube(),
            players: three,
            cards: db.clone(),
            starting_player: None,
        },
        1,
    )
    .unwrap_err();
    assert!(err.to_string().contains("players"), "{err}");
    let err = Game::new(
        GameConfig {
            format: Format::builtin("commander").unwrap(),
            players: players(green.clone(), green),
            cards: db,
            starting_player: None,
        },
        1,
    )
    .unwrap_err();
    assert!(err.to_string().contains("not implemented"), "{err}");
}

#[test]
fn same_seed_and_actions_give_the_same_game() {
    let mut a = cube_game(Seat(0));
    engine::bot::play_random_game(&mut a, 9, 5000).unwrap();
    assert!(a.is_over().is_some());
    let b = cube_game(Seat(0));
    let b = Game::replay(
        GameConfig {
            format: b.format.clone(),
            players: vec![
                PlayerSetup {
                    name: "Connor".into(),
                    deck: deck_of(&a, Seat(0)),
                },
                PlayerSetup {
                    name: "Claude".into(),
                    deck: deck_of(&a, Seat(1)),
                },
            ],
            cards: Arc::new(cards::core()),
            starting_player: Some(Seat(0)),
        },
        a.seed(),
        a.history(),
    )
    .unwrap();
    assert_eq!(a.log, b.log);
    assert_eq!(a.view_spectator(), b.view_spectator());
}

/// The card ids of `seat`'s original deck, in the order `Game::new` received them.
fn deck_of(game: &Game, seat: Seat) -> Vec<engine::CardId> {
    let mut objs: Vec<_> = game.objects.iter().filter(|(_, o)| o.owner == seat).collect();
    objs.sort_by_key(|(id, _)| *id);
    objs.into_iter().map(|(_, o)| o.card).collect()
}
