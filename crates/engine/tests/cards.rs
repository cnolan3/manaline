//! M3 scenarios: spells, abilities, triggers, keywords in combat, auras,
//! equipment, tokens, counters, choices, and fizzling, driven through the
//! same `legal_actions` / `apply` contract every client uses.

use engine::testing::{advance_until, TestGame};
use engine::{
    ActReason, Action, AttackTarget, DamageTarget, Game, Keyword, ObjectId, PendingChoice, Phase, RulesError, Seat, Target, Zone,
    EQUIP_ABILITY,
};
use std::sync::Arc;

fn db() -> Arc<engine::CardDb> {
    Arc::new(cards::core())
}

/// The core set plus hand-written cards for primitives the cube doesn't use yet.
fn db_with_extras() -> Arc<engine::CardDb> {
    let extras = [
        r#"Card(name: "Test Exile", cost: "{W}", types: [Instant], text: "Exile target creature.", spell: Spell(targets: [Creature], effects: [Exile(target: Target(0))]))"#,
        r#"Card(name: "Test Tap", cost: "{U}", types: [Instant], text: "Tap target creature.", spell: Spell(targets: [Creature], effects: [Tap(target: Target(0))]))"#,
        r#"Card(name: "Test Untap", cost: "{U}", types: [Instant], text: "Untap target creature.", spell: Spell(targets: [Creature], effects: [Untap(target: Target(0))]))"#,
        r#"Card(name: "Test Counters", cost: "{G}", types: [Instant], text: "Put two +1/+1 counters on target creature.", spell: Spell(targets: [Creature], effects: [AddCounters(target: Target(0), kind: Plus1Plus1, count: Const(2))]))"#,
        r#"Card(name: "Test Drain", cost: "{B}", types: [Sorcery], text: "Each opponent loses 2 life. You gain 2 life.", spell: Spell(effects: [LoseLife(player: EachOpponent, amount: Const(2)), GainLife(player: You, amount: Const(2))]))"#,
        r#"Card(name: "Test Conditional", cost: "{G}", types: [Sorcery], text: "If you control a Forest, draw a card.", spell: Spell(effects: [Conditional(if_: Controls(player: You, filter: Subtype("Forest"), at_least: 1), then: Draw(player: You, count: Const(1)))]))"#,
        r#"Card(name: "Test Wings", cost: "{U}", types: [Instant], text: "Target creature gains flying until end of turn.", spell: Spell(targets: [Creature], effects: [GrantKeyword(target: Target(0), keyword: Flying, until: EndOfTurn)]))"#,
        r#"Card(name: "Hexproof Bear", cost: "{1}{G}", types: [Creature], subtypes: ["Bear"], pt: (2, 2), text: "Hexproof", keywords: [Hexproof])"#,
        r#"Card(name: "Stone Bear", cost: "{1}{G}", types: [Creature], subtypes: ["Bear"], pt: (2, 2), text: "Indestructible", keywords: [Indestructible])"#,
        r#"Card(name: "Menace Bear", cost: "{1}{B}", types: [Creature], subtypes: ["Bear"], pt: (2, 2), text: "Menace", keywords: [Menace])"#,
        r#"Card(name: "Flash Bear", cost: "{1}{G}", types: [Creature], subtypes: ["Bear"], pt: (2, 2), text: "Flash", keywords: [Flash])"#,
        r#"Card(name: "Double Bear", cost: "{1}{R}", types: [Creature], subtypes: ["Bear"], pt: (2, 2), text: "Double strike", keywords: [DoubleStrike])"#,
        r#"Card(name: "Test Sac Outlet", cost: "{B}", types: [Creature], subtypes: ["Ghoul"], pt: (1, 1), text: "Sacrifice a creature: You gain 2 life.", activated: [Ability(cost: [Sacrifice(Creature)], effects: [GainLife(player: You, amount: Const(2))])])"#,
        r#"Card(name: "Test Nested Edict", cost: "{1}{B}", types: [Sorcery], text: "Target opponent sacrifices a creature of their choice, then draw a card.", spell: Spell(targets: [Opponent], effects: [Sequence([Sacrifice(player: TargetOpponent(0), filter: Creature, count: Const(1)), Draw(player: You, count: Const(1))])]))"#,
        r#"Card(name: "Test Wheel", cost: "{2}{B}", types: [Sorcery], text: "Each player discards a card. You gain 1 life.", spell: Spell(effects: [Discard(player: EachPlayer, count: Const(1)), GainLife(player: You, amount: Const(1))]))"#,
        r#"Card(name: "Test Reap", cost: "{B}", types: [Sorcery], text: "You may sacrifice a creature. If you do, draw two cards. If you don't, you lose 2 life.", spell: Spell(effects: [May(effect: Sacrifice(player: You, filter: Creature, count: Const(1)), then: [Draw(player: You, count: Const(2))], otherwise: [LoseLife(player: You, amount: Const(2))])]))"#,
        r#"Card(name: "Test Cull", cost: "{W}", types: [Sorcery], text: "Exile up to two creatures you control.", spell: Spell(effects: [Exile(target: Chosen(who: You, filter: And([Creature, ControlledBy(You)]), count: UpTo(2)))]))"#,
        r#"Card(name: "Test Elf Captain", cost: "{1}{G}", types: [Creature], subtypes: ["Elf"], pt: (2, 2), text: "Whenever this creature attacks, if you control another Elf, it gets +2/+2 until end of turn.", triggers: [Trigger(event: ThisAttacks, condition: Controls(player: You, filter: And([Other, Subtype("Elf")]), at_least: 1), effects: [ModifyPt(target: This, power: Const(2), toughness: Const(2), until: EndOfTurn)])])"#,
        r#"Card(name: "Test Brawler", cost: "{1}{R}", types: [Creature], subtypes: ["Bear"], pt: (2, 2), text: "Whenever this creature attacks or blocks, it gets +1/+1 until end of turn.", triggers: [Trigger(event: Any([ThisAttacks, ThisBlocks]), effects: [ModifyPt(target: This, power: Const(1), toughness: Const(1), until: EndOfTurn)])])"#,
        r#"Card(name: "Test Charm", cost: "{G}", types: [Instant], text: "Choose one or both —\n• Target creature gets +2/+2 until end of turn.\n• Draw a card.", spell: Spell(choose: OneOrBoth, modes: [Mode(targets: [Creature], effects: [ModifyPt(target: Target(0), power: Const(2), toughness: Const(2), until: EndOfTurn)]), Mode(effects: [Draw(player: You, count: Const(1))])]))"#,
        r#"Card(name: "Test Volley", cost: "{R}", types: [Instant], text: "Test Volley deals 2 damage to each of up to two target creatures.", spell: Spell(targets: [Targets(UpTo(2), Creature)], effects: [DealDamage(amount: Const(2), to: Target(0))]))"#,
        r#"Card(name: "Test Ward", cost: "{W}", types: [Instant], text: "Target creature gets +2/+2 until your next turn.", spell: Spell(targets: [Creature], effects: [ModifyPt(target: Target(0), power: Const(2), toughness: Const(2), until: UntilYourNextTurn)]))"#,
        r#"Card(name: "Test Rouse", cost: "{G}", types: [Instant], text: "Tap a creature you control, then it gets +2/+2 until end of turn.", spell: Spell(effects: [Sequence([Tap(target: Chosen(who: You, filter: And([Creature, ControlledBy(You)]), count: Exactly(1), bind: "c")), ModifyPt(target: Named("c"), power: Const(2), toughness: Const(2), until: EndOfTurn)])]))"#,
    ];
    let mut all = cards::core_ir();
    for text in extras {
        all.push(cardir::load(text).unwrap_or_else(|e| panic!("{e}")));
    }
    Arc::new(engine::CardDb::from_ir("core", all).unwrap())
}

fn hand_card(game: &Game, seat: Seat, name: &str) -> ObjectId {
    game.players[seat.index()]
        .hand
        .iter()
        .copied()
        .find(|&id| game.object_name(id) == name)
        .unwrap_or_else(|| panic!("{name} not in hand"))
}

fn bf(game: &Game, seat: Seat, name: &str) -> ObjectId {
    game.players[seat.index()]
        .battlefield
        .iter()
        .copied()
        .find(|&id| game.object_name(id) == name)
        .unwrap_or_else(|| panic!("{name} not on battlefield"))
}

fn bfs(game: &Game, seat: Seat, name: &str) -> Vec<ObjectId> {
    game.players[seat.index()]
        .battlefield
        .iter()
        .copied()
        .filter(|&id| game.object_name(id) == name)
        .collect()
}

/// The legal cast of `name` from `seat`'s hand with exactly these targets (first payment).
fn cast_action(game: &Game, seat: Seat, name: &str, targets: &[Target]) -> Action {
    let id = hand_card(game, seat, name);
    game.legal_actions(seat)
        .into_iter()
        .find(|a| matches!(a, Action::CastSpell { object, targets: t, .. } if *object == id && t == targets))
        .unwrap_or_else(|| panic!("no legal cast of {name} with targets {targets:?}"))
}

fn cast(game: &mut Game, seat: Seat, name: &str, targets: &[Target]) {
    let a = cast_action(game, seat, name, targets);
    game.apply(seat, &a).unwrap();
}

/// Everyone passes until the top of the stack resolves (or a choice opens).
fn resolve_top(game: &mut Game) {
    let depth = game.stack.len();
    assert!(depth > 0, "nothing to resolve");
    for _ in 0..8 {
        if game.stack.len() < depth || game.pending.is_some() || game.is_over().is_some() {
            return;
        }
        let seat = game.priority.expect("someone holds priority");
        game.apply(seat, &Action::PassPriority).unwrap();
    }
    panic!("stack did not resolve");
}

fn pass_both(game: &mut Game) {
    for _ in 0..game.turn_order.len() {
        let seat = game.priority.expect("someone has priority");
        game.apply(seat, &Action::PassPriority).unwrap();
    }
}

fn to_attackers(game: &mut Game) {
    advance_until(game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
}

fn attack_with(game: &mut Game, seat: Seat, ids: &[ObjectId], target: Seat) {
    to_attackers(game);
    let attackers = ids.iter().map(|&id| (id, AttackTarget::Player(target))).collect();
    game.apply(seat, &Action::DeclareAttackers { attackers }).unwrap();
    pass_both(game);
}

fn block_with(game: &mut Game, seat: Seat, blocks: &[(ObjectId, ObjectId)]) {
    assert!(
        matches!(game.pending, Some(PendingChoice::DeclareBlockers { .. })),
        "{:?}",
        game.pending
    );
    game.apply(seat, &Action::DeclareBlockers { blocks: blocks.to_vec() }).unwrap();
    pass_both(game);
}

// ----- spells and targeting -----

#[test]
fn lightning_strike_targets_any_target_and_kills() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Mountain")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Lightning Strike")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    let strike = hand_card(&game, Seat(0), "Lightning Strike");
    let casts: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::CastSpell { .. }))
        .collect();
    let targets: Vec<Vec<Target>> = casts
        .iter()
        .map(|a| match a {
            Action::CastSpell { targets, .. } => targets.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert!(targets.contains(&vec![Target::Object(bears)]));
    assert!(targets.contains(&vec![Target::Player(Seat(0))]));
    assert!(targets.contains(&vec![Target::Player(Seat(1))]));
    assert_eq!(casts.len(), 3, "{casts:?}");

    cast(&mut game, Seat(0), "Lightning Strike", &[Target::Object(bears)]);
    assert_eq!(game.objects[strike].zone, Zone::Stack);
    assert_eq!(game.stack.len(), 1);
    resolve_top(&mut game);
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
    assert_eq!(game.objects[strike].zone, Zone::Graveyard);
    assert_eq!(game.priority, Some(Seat(0)));
}

#[test]
fn instants_can_be_cast_on_the_opponents_turn_and_pumps_wear_off() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Grizzly Bears")
        .battlefield(Seat(1), "Forest")
        .hand(Seat(1), "Giant Growth")
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    // Seat 0 passes in its main phase; seat 1 gets priority and may cast the instant.
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    assert_eq!(game.priority, Some(Seat(1)));
    cast(&mut game, Seat(1), "Giant Growth", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(bears), Some((5, 5)));
    advance_until(&mut game, |g| g.turn == 2).unwrap();
    assert_eq!(game.effective_stats(bears), Some((2, 2)), "until end of turn");
}

#[test]
fn sorceries_only_at_sorcery_speed() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Flame Slash")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    assert!(game.legal_actions(Seat(0)).iter().any(|a| matches!(a, Action::CastSpell { .. })));
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::BeginCombat);
    assert!(
        !game.legal_actions(Seat(0)).iter().any(|a| matches!(a, Action::CastSpell { .. })),
        "not in combat"
    );
}

#[test]
fn counterspell_counters_a_creature_spell() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .hand(Seat(0), "Grizzly Bears")
        .battlefield(Seat(1), "Island")
        .battlefield(Seat(1), "Island")
        .hand(Seat(1), "Counterspell")
        .hand(Seat(1), "Negate")
        .build();
    let bears = hand_card(&game, Seat(0), "Grizzly Bears");
    cast(&mut game, Seat(0), "Grizzly Bears", &[]);
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    assert_eq!(game.priority, Some(Seat(1)));
    // Negate can't target a creature spell; Counterspell can.
    let negate = hand_card(&game, Seat(1), "Negate");
    assert!(!game
        .legal_actions(Seat(1))
        .iter()
        .any(|a| matches!(a, Action::CastSpell { object, .. } if *object == negate)));
    cast(&mut game, Seat(1), "Counterspell", &[Target::Object(bears)]);
    assert_eq!(game.stack.len(), 2);
    resolve_top(&mut game);
    assert_eq!(game.objects[bears].zone, Zone::Graveyard, "countered");
    assert!(game.stack.is_empty());
    assert!(game.log.iter().any(|e| matches!(e, engine::EventBase::Countered { .. })));
}

#[test]
fn a_spell_whose_only_target_left_fizzles() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Mountain")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Lightning Strike")
        .battlefield(Seat(1), "Island")
        .battlefield(Seat(1), "Grizzly Bears")
        .hand(Seat(1), "Unsummon")
        .build();
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    cast(&mut game, Seat(0), "Lightning Strike", &[Target::Object(bears)]);
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    cast(&mut game, Seat(1), "Unsummon", &[Target::Object(bears)]);
    resolve_top(&mut game); // Unsummon: bears to hand
    assert_eq!(game.objects[bears].zone, Zone::Hand);
    assert_eq!(game.players[1].life, 20);
    resolve_top(&mut game); // Strike fizzles
    assert_eq!(game.players[1].life, 20);
    assert!(game.stack.is_empty());
}

#[test]
fn dark_ritual_mana_can_pay_for_a_spell_in_the_same_phase() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Swamp")
        .hand(Seat(0), "Dark Ritual")
        .hand(Seat(0), "Murder")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let murder = hand_card(&game, Seat(0), "Murder");
    assert!(!game
        .legal_actions(Seat(0))
        .iter()
        .any(|a| matches!(a, Action::CastSpell { object, .. } if *object == murder)));
    cast(&mut game, Seat(0), "Dark Ritual", &[]);
    resolve_top(&mut game);
    assert_eq!(game.players[0].mana_pool.total(), 3);
    let cast_murder = game
        .legal_actions(Seat(0))
        .into_iter()
        .find(|a| matches!(a, Action::CastSpell { object, .. } if *object == murder))
        .expect("Murder castable from the pool");
    if let Action::CastSpell { payment, .. } = &cast_murder {
        assert_eq!(payment.from_pool.len(), 3);
        assert!(payment.tap.is_empty());
    }
    game.apply(Seat(0), &cast_murder).unwrap();
    assert_eq!(game.players[0].mana_pool.total(), 0);
}

// ----- effects that need a choice -----

#[test]
fn cruel_edict_asks_the_opponent_which_creature_to_sacrifice() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Swamp")
        .hand(Seat(0), "Cruel Edict")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Hill Giant")
        .build();
    cast(&mut game, Seat(0), "Cruel Edict", &[Target::Player(Seat(1))]);
    resolve_top(&mut game);
    assert!(
        matches!(
            game.pending,
            Some(PendingChoice::Choose {
                seat: Seat(1),
                reason: ActReason::Choice,
                ..
            })
        ),
        "{:?}",
        game.pending
    );
    assert_eq!(game.must_act().get(&Seat(1)), Some(&ActReason::Choice));
    let choices = game.legal_actions(Seat(1));
    assert_eq!(choices.iter().filter(|a| matches!(a, Action::ChooseTargets { .. })).count(), 2);
    let giant = bf(&game, Seat(1), "Hill Giant");
    game.apply(
        Seat(1),
        &Action::ChooseTargets {
            targets: vec![Target::Object(giant)],
        },
    )
    .unwrap();
    assert_eq!(game.objects[giant].zone, Zone::Graveyard);
    assert_eq!(game.players[1].battlefield.len(), 1);
    assert_eq!(game.priority, Some(Seat(0)), "resolution finished; active player gets priority");
    assert!(game.pending.is_none());
}

#[test]
fn mind_rot_lets_the_target_choose_what_to_discard() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Swamp")
        .hand(Seat(0), "Mind Rot")
        .hand(Seat(1), "Forest")
        .hand(Seat(1), "Forest")
        .hand(Seat(1), "Grizzly Bears")
        .build();
    cast(&mut game, Seat(0), "Mind Rot", &[Target::Player(Seat(1))]);
    resolve_top(&mut game);
    assert!(matches!(
        game.pending,
        Some(PendingChoice::Choose {
            seat: Seat(1),
            min: 2,
            max: 2,
            reason: ActReason::Discard,
            ..
        })
    ));
    assert_eq!(game.must_act().get(&Seat(1)), Some(&ActReason::Discard));
    let choices = game.legal_actions(Seat(1));
    assert_eq!(
        choices.iter().filter(|a| matches!(a, Action::ChooseTargets { .. })).count(),
        3,
        "C(3,2)"
    );
    let pick = choices.into_iter().find(|a| matches!(a, Action::ChooseTargets { .. })).unwrap();
    game.apply(Seat(1), &pick).unwrap();
    assert_eq!(game.players[1].hand.len(), 1);
    assert_eq!(game.players[1].graveyard.len(), 2);
}

#[test]
fn a_choice_inside_a_sequence_keeps_the_rest_of_the_sequence() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Swamp")
        .hand(Seat(0), "Test Nested Edict")
        .library(Seat(0), &["Forest"; 3])
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Hill Giant")
        .build();
    let hand_before = game.players[0].hand.len();
    cast(&mut game, Seat(0), "Test Nested Edict", &[Target::Player(Seat(1))]);
    resolve_top(&mut game);
    assert!(matches!(game.pending, Some(PendingChoice::Choose { seat: Seat(1), .. })));
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    game.apply(
        Seat(1),
        &Action::ChooseTargets {
            targets: vec![Target::Object(bears)],
        },
    )
    .unwrap();
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
    assert_eq!(
        game.players[0].hand.len(),
        hand_before - 1 + 1,
        "the draw after the sacrifice still happens"
    );
    assert!(game.pending.is_none());
    assert_eq!(game.priority, Some(Seat(0)));
}

#[test]
fn each_player_discarding_asks_every_seat_in_turn() {
    let mut game = TestGame::new(db_with_extras(), 3)
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Swamp")
        .hand(Seat(0), "Test Wheel")
        .hand(Seat(0), "Forest") // plus the turn-one draw: a pod's starting player draws
        .hand(Seat(1), "Forest")
        .hand(Seat(1), "Grizzly Bears")
        .hand(Seat(2), "Forest")
        .build();
    cast(&mut game, Seat(0), "Test Wheel", &[]);
    resolve_top(&mut game);
    // Seat 0 has two cards left and chooses; seat 1 chooses; seat 2's only card goes on its own.
    for seat in [Seat(0), Seat(1)] {
        assert!(
            matches!(game.pending, Some(PendingChoice::Choose { seat: s, min: 1, reason: ActReason::Discard, .. }) if s == seat),
            "{:?}",
            game.pending
        );
        assert_eq!(game.must_act().get(&seat), Some(&ActReason::Discard));
        let pick = game
            .legal_actions(seat)
            .into_iter()
            .find(|a| matches!(a, Action::ChooseTargets { .. }))
            .unwrap();
        game.apply(seat, &pick).unwrap();
    }
    assert!(game.pending.is_none(), "{:?}", game.pending);
    assert_eq!(game.players[0].hand.len(), 1);
    assert_eq!(game.players[1].hand.len(), 1);
    assert_eq!(game.players[2].hand.len(), 0);
    assert_eq!(game.players[0].life, 21, "the effect after the discards still runs");
    assert_eq!(game.priority, Some(Seat(0)));
}

#[test]
fn you_may_asks_and_runs_the_branch_that_was_picked() {
    // Decline: the "if you don't" branch runs.
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Test Reap")
        .library(Seat(0), &["Forest"; 4])
        .build();
    cast(&mut game, Seat(0), "Test Reap", &[]);
    resolve_top(&mut game);
    assert!(
        matches!(game.pending, Some(PendingChoice::ChooseOption { seat: Seat(0), .. })),
        "{:?}",
        game.pending
    );
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::Choice));
    let acts = game.legal_actions(Seat(0));
    let labels: Vec<String> = acts
        .iter()
        .filter(|a| matches!(a, Action::ChooseMode { .. }))
        .map(|a| engine::text::describe_action(&game, a))
        .collect();
    assert_eq!(labels, vec!["Sacrifice a creature".to_string(), "Don't".to_string()]);
    game.apply(Seat(0), &Action::ChooseMode { mode: 1 }).unwrap();
    assert_eq!(game.players[0].life, 18);
    assert_eq!(game.players[0].hand.len(), 0);
    assert_eq!(game.players[0].battlefield.len(), 2, "nothing sacrificed");
    assert!(game.pending.is_none());
    assert_eq!(game.priority, Some(Seat(0)));

    // Accept: the sacrifice is a real choice, and the "if you do" branch follows it.
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Grizzly Bears")
        .battlefield(Seat(0), "Hill Giant")
        .hand(Seat(0), "Test Reap")
        .library(Seat(0), &["Forest"; 4])
        .build();
    let giant = bf(&game, Seat(0), "Hill Giant");
    cast(&mut game, Seat(0), "Test Reap", &[]);
    resolve_top(&mut game);
    game.apply(Seat(0), &Action::ChooseMode { mode: 0 }).unwrap();
    assert!(
        matches!(
            game.pending,
            Some(PendingChoice::Choose {
                seat: Seat(0),
                min: 1,
                max: 1,
                ..
            })
        ),
        "{:?}",
        game.pending
    );
    let text = engine::text::describe_action(
        &game,
        &Action::ChooseTargets {
            targets: vec![Target::Object(giant)],
        },
    );
    assert!(text.starts_with("Sacrifice Hill Giant"), "{text}");
    game.apply(
        Seat(0),
        &Action::ChooseTargets {
            targets: vec![Target::Object(giant)],
        },
    )
    .unwrap();
    assert_eq!(game.objects[giant].zone, Zone::Graveyard);
    assert_eq!(game.players[0].hand.len(), 2, "drew two");
    assert_eq!(game.players[0].life, 20);
}

#[test]
fn up_to_n_offers_every_size_including_none() {
    let mut t = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Plains")
        .hand(Seat(0), "Test Cull");
    for _ in 0..3 {
        t = t.battlefield(Seat(0), "Grizzly Bears");
    }
    let mut game = t.build();
    let bears = bfs(&game, Seat(0), "Grizzly Bears");
    cast(&mut game, Seat(0), "Test Cull", &[]);
    resolve_top(&mut game);
    assert!(
        matches!(game.pending, Some(PendingChoice::Choose { min: 0, max: 2, .. })),
        "{:?}",
        game.pending
    );
    let picks: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::ChooseTargets { .. }))
        .collect();
    assert_eq!(picks.len(), 1 + 3 + 3, "none, each single, each pair: {picks:?}");
    assert_eq!(
        engine::text::describe_action(&game, &Action::ChooseTargets { targets: vec![] }),
        "Exile nothing"
    );
    game.apply(
        Seat(0),
        &Action::ChooseTargets {
            targets: vec![Target::Object(bears[0]), Target::Object(bears[2])],
        },
    )
    .unwrap();
    assert_eq!(game.objects[bears[0]].zone, Zone::Exile);
    assert_eq!(game.objects[bears[1]].zone, Zone::Battlefield);
    assert_eq!(game.objects[bears[2]].zone, Zone::Exile);
}

#[test]
fn a_two_step_cast_pays_first_then_asks_for_modes_and_targets() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Test Charm")
        .library(Seat(0), &["Forest"; 3])
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    let forest = bf(&game, Seat(0), "Forest");
    let charm = hand_card(&game, Seat(0), "Test Charm");
    let casts: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::CastSpell { object, .. } if *object == charm))
        .collect();
    assert_eq!(casts.len(), 1, "one action per payment, no targets yet: {casts:?}");
    game.apply(Seat(0), &casts[0]).unwrap();
    assert!(game.objects[forest].tapped, "mana is paid up front");
    assert_eq!(game.objects[charm].zone, Zone::Hand, "not on the stack until the choices are made");
    assert!(game.stack.is_empty());
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::Choice));
    assert_eq!(game.priority, None);

    // Modes: both offered, plus nothing to stop with until one is chosen.
    let modes: Vec<String> = game
        .legal_actions(Seat(0))
        .iter()
        .filter(|a| matches!(a, Action::ChooseMode { .. }))
        .map(|a| engine::text::describe_action(&game, a))
        .collect();
    assert_eq!(
        modes,
        vec![
            "Target creature gets +2/+2 until end of turn.".to_string(),
            "Draw a card.".to_string()
        ]
    );
    game.apply(Seat(0), &Action::ChooseMode { mode: 1 }).unwrap();
    let modes: Vec<String> = game
        .legal_actions(Seat(0))
        .iter()
        .filter(|a| matches!(a, Action::ChooseMode { .. }))
        .map(|a| engine::text::describe_action(&game, a))
        .collect();
    assert_eq!(
        modes,
        vec![
            "Target creature gets +2/+2 until end of turn.".to_string(),
            "No more modes".to_string()
        ]
    );
    game.apply(Seat(0), &Action::ChooseMode { mode: 0 }).unwrap();
    // Two modes chosen: now the first mode's target.
    let picks: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::ChooseTargets { .. }))
        .collect();
    assert_eq!(
        picks,
        vec![Action::ChooseTargets {
            targets: vec![Target::Object(bears)]
        }]
    );
    assert_eq!(
        engine::text::describe_action(&game, &picks[0]),
        format!("Target Grizzly Bears {bears}")
    );
    game.apply(Seat(0), &picks[0]).unwrap();
    assert_eq!(game.stack.len(), 1);
    assert_eq!(game.stack[0].modes, vec![1, 0]);
    assert_eq!(game.priority, Some(Seat(0)), "the caster gets priority back");
    let view = game.view(Seat(1));
    assert_eq!(view.stack[0].modes, vec![1, 0]);
    assert!(
        view.stack[0].description.contains("Draw a card") && view.stack[0].description.contains("+2/+2"),
        "{}",
        view.stack[0].description
    );
    let before = game.players[0].hand.len();
    resolve_top(&mut game);
    assert_eq!(game.players[0].hand.len(), before + 1, "modes resolve in the order chosen");
    assert_eq!(game.effective_stats(bears), Some((4, 4)));
}

#[test]
fn illegal_targets_are_pruned_one_by_one_and_only_a_total_loss_fizzles() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Test Volley")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Hill Giant")
        .battlefield(Seat(1), "Island")
        .hand(Seat(1), "Unsummon")
        .build();
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    let giant = bf(&game, Seat(1), "Hill Giant");
    cast(&mut game, Seat(0), "Test Volley", &[]);
    game.apply(
        Seat(0),
        &Action::ChooseTargets {
            targets: vec![Target::Object(bears), Target::Object(giant)],
        },
    )
    .unwrap();
    // In response the opponent bounces one target; the other is still hit.
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    cast(&mut game, Seat(1), "Unsummon", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert_eq!(game.objects[bears].zone, Zone::Hand);
    resolve_top(&mut game);
    assert_eq!(game.objects[giant].damage, 2, "the remaining target is still dealt damage");
}

#[test]
fn until_your_next_turn_outlasts_the_opponents_turn() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Plains")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Test Ward")
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    cast(&mut game, Seat(0), "Test Ward", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(bears), Some((4, 4)));
    advance_until(&mut game, |g| g.turn == 2 && g.phase == Phase::Main1).unwrap();
    assert_eq!(game.effective_stats(bears), Some((4, 4)), "still on during the opponent's turn");
    advance_until(&mut game, |g| g.turn == 3).unwrap();
    assert_eq!(game.effective_stats(bears), Some((2, 2)), "gone as my next turn begins");
}

#[test]
fn a_bound_choice_can_be_referred_to_later() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Grizzly Bears")
        .battlefield(Seat(0), "Hill Giant")
        .hand(Seat(0), "Test Rouse")
        .build();
    let giant = bf(&game, Seat(0), "Hill Giant");
    cast(&mut game, Seat(0), "Test Rouse", &[]);
    resolve_top(&mut game);
    game.apply(
        Seat(0),
        &Action::ChooseTargets {
            targets: vec![Target::Object(giant)],
        },
    )
    .unwrap();
    assert!(game.objects[giant].tapped);
    assert_eq!(game.effective_stats(giant), Some((5, 5)), "\"it\" is the tapped creature");
}

// ----- triggers -----

#[test]
fn an_intervening_if_gates_the_trigger() {
    // Without another Elf the trigger never fires.
    let mut game = TestGame::new(db_with_extras(), 2).battlefield(Seat(0), "Test Elf Captain").build();
    let captain = bf(&game, Seat(0), "Test Elf Captain");
    attack_with(&mut game, Seat(0), &[captain], Seat(1));
    assert!(game.stack.is_empty(), "no trigger: {:?}", game.stack);
    assert_eq!(game.effective_stats(captain), Some((2, 2)));

    // With one it fires and resolves.
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Test Elf Captain")
        .battlefield(Seat(0), "Llanowar Elves")
        .build();
    let captain = bf(&game, Seat(0), "Test Elf Captain");
    to_attackers(&mut game);
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(captain, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    assert_eq!(game.stack.len(), 1, "the trigger is on the stack");
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(captain), Some((4, 4)));
}

#[test]
fn an_event_union_fires_on_either_event() {
    // Blocking: the opponent attacks and the brawler blocks.
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Hill Giant")
        .battlefield(Seat(1), "Test Brawler")
        .build();
    let giant = bf(&game, Seat(0), "Hill Giant");
    let brawler = bf(&game, Seat(1), "Test Brawler");
    attack_with(&mut game, Seat(0), &[giant], Seat(1));
    game.apply(
        Seat(1),
        &Action::DeclareBlockers {
            blocks: vec![(brawler, giant)],
        },
    )
    .unwrap();
    assert_eq!(game.stack.len(), 1, "blocking fired it");
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(brawler), Some((3, 3)));
    pass_both(&mut game);
    assert_eq!(game.objects[giant].zone, Zone::Graveyard, "a 3/3 blocker kills the 3/3 attacker");
    assert_eq!(game.objects[brawler].zone, Zone::Graveyard);

    // Attacking fires it too.
    let mut game = TestGame::new(db_with_extras(), 2).battlefield(Seat(0), "Test Brawler").build();
    let brawler = bf(&game, Seat(0), "Test Brawler");
    to_attackers(&mut game);
    game.apply(
        Seat(0),
        &Action::DeclareAttackers {
            attackers: vec![(brawler, AttackTarget::Player(Seat(1)))],
        },
    )
    .unwrap();
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(brawler), Some((3, 3)));
}

#[test]
fn elvish_visionary_draws_on_entering() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .hand(Seat(0), "Elvish Visionary")
        .build();
    let before = game.players[0].hand.len();
    cast(&mut game, Seat(0), "Elvish Visionary", &[]);
    // Both pass: the creature resolves and its trigger goes on the stack.
    pass_both(&mut game);
    assert_eq!(game.stack.len(), 1, "the ETB trigger is on the stack: {:?}", game.stack);
    assert!(matches!(game.stack[0].kind, engine::StackKind::Trigger { .. }));
    assert!(game.log.iter().any(|e| matches!(e, engine::EventBase::Triggered { .. })));
    assert_eq!(game.priority, Some(Seat(0)), "active player may respond to the trigger");
    assert_eq!(game.players[0].hand.len(), before - 1);
    resolve_top(&mut game);
    assert_eq!(game.players[0].hand.len(), before - 1 + 1);
}

#[test]
fn man_o_war_asks_for_a_target_then_bounces() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Island")
        .battlefield(Seat(0), "Island")
        .battlefield(Seat(0), "Island")
        .hand(Seat(0), "Man-o'-War")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    cast(&mut game, Seat(0), "Man-o'-War", &[]);
    resolve_top(&mut game);
    assert!(
        matches!(game.pending, Some(PendingChoice::ChooseTargets { seat: Seat(0), .. })),
        "{:?}",
        game.pending
    );
    assert_eq!(game.must_act().get(&Seat(0)), Some(&ActReason::Choice));
    let choices = game.legal_actions(Seat(0));
    assert_eq!(
        choices.iter().filter(|a| matches!(a, Action::ChooseTargets { .. })).count(),
        2,
        "bears or itself"
    );
    game.apply(
        Seat(0),
        &Action::ChooseTargets {
            targets: vec![Target::Object(bears)],
        },
    )
    .unwrap();
    assert_eq!(game.stack.len(), 1);
    resolve_top(&mut game);
    assert_eq!(game.objects[bears].zone, Zone::Hand);
}

#[test]
fn festering_goblin_dies_trigger_shrinks_a_creature() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Festering Goblin")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Suntail Hawk")
        .build();
    let goblin = bf(&game, Seat(0), "Festering Goblin");
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    let hawk = bf(&game, Seat(1), "Suntail Hawk");
    attack_with(&mut game, Seat(0), &[goblin], Seat(1));
    block_with(&mut game, Seat(1), &[(bears, goblin)]);
    assert_eq!(game.objects[goblin].zone, Zone::Graveyard);
    assert!(
        matches!(game.pending, Some(PendingChoice::ChooseTargets { seat: Seat(0), .. })),
        "{:?}",
        game.pending
    );
    game.apply(
        Seat(0),
        &Action::ChooseTargets {
            targets: vec![Target::Object(hawk)],
        },
    )
    .unwrap();
    resolve_top(&mut game);
    assert_eq!(game.objects[hawk].zone, Zone::Graveyard, "1/1 with -1/-1 dies");
}

#[test]
fn prowess_pumps_on_a_noncreature_spell() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Bloodfire Expert")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Shock")
        .build();
    let expert = bf(&game, Seat(0), "Bloodfire Expert");
    cast(&mut game, Seat(0), "Shock", &[Target::Player(Seat(1))]);
    assert_eq!(game.stack.len(), 2, "prowess trigger above the spell");
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(expert), Some((4, 2)));
}

// ----- activated and mana abilities -----

#[test]
fn prodigal_pyromancer_pings_and_respects_summoning_sickness() {
    let mut game = TestGame::new(db(), 2).battlefield(Seat(0), "Prodigal Pyromancer").build();
    let pinger = bf(&game, Seat(0), "Prodigal Pyromancer");
    let acts: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::ActivateAbility { object, .. } if *object == pinger))
        .collect();
    assert_eq!(acts.len(), 3, "bears? no: itself, and both players: {acts:?}");
    let ping = acts
        .iter()
        .find(|a| matches!(a, Action::ActivateAbility { targets, .. } if targets == &vec![Target::Player(Seat(1))]))
        .unwrap()
        .clone();
    game.apply(Seat(0), &ping).unwrap();
    assert!(game.objects[pinger].tapped);
    resolve_top(&mut game);
    assert_eq!(game.players[1].life, 19);

    let mut sick = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Mountain")
        .battlefield(Seat(0), "Mountain")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Prodigal Pyromancer")
        .build();
    cast(&mut sick, Seat(0), "Prodigal Pyromancer", &[]);
    resolve_top(&mut sick);
    assert!(
        !sick
            .legal_actions(Seat(0))
            .iter()
            .any(|a| matches!(a, Action::ActivateAbility { .. })),
        "summoning sick: no {{T}}"
    );
}

#[test]
fn elves_make_mana_and_the_archdruid_scales_with_elves() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Llanowar Elves")
        .battlefield(Seat(0), "Elvish Mystic")
        .battlefield(Seat(0), "Elvish Archdruid")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .hand(Seat(0), "Craw Wurm")
        .build();
    let elves = bf(&game, Seat(0), "Llanowar Elves");
    assert_eq!(game.effective_stats(elves), Some((2, 2)), "lord");
    let sources = game.mana_sources_with_amounts(Seat(0));
    let druid = bf(&game, Seat(0), "Elvish Archdruid");
    assert_eq!(
        sources.iter().find(|(id, _, _)| *id == druid).map(|(_, _, n)| *n),
        Some(3),
        "three Elves"
    );
    let wurm = hand_card(&game, Seat(0), "Craw Wurm");
    assert!(
        game.legal_actions(Seat(0))
            .iter()
            .any(|a| matches!(a, Action::CastSpell { object, .. } if *object == wurm)),
        "{{4}}{{G}}{{G}} from 3 Forests + Elves"
    );
    cast(&mut game, Seat(0), "Craw Wurm", &[]);
    assert!(game.objects[druid].tapped || game.objects[elves].tapped);
}

#[test]
fn sacrifice_costs_enumerate_each_candidate() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Test Sac Outlet")
        .battlefield(Seat(0), "Grizzly Bears")
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    let acts: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::ActivateAbility { .. }))
        .collect();
    assert_eq!(acts.len(), 2, "sacrifice itself or the bears: {acts:?}");
    let sac_bears = acts
        .iter()
        .find(|a| matches!(a, Action::ActivateAbility { payment, .. } if payment.sacrifice == vec![bears]))
        .unwrap()
        .clone();
    game.apply(Seat(0), &sac_bears).unwrap();
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
    resolve_top(&mut game);
    assert_eq!(game.players[0].life, 22);
}

// ----- statics, auras, equipment, tokens -----

#[test]
fn goblin_chieftain_grants_haste_to_other_goblins() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Goblin Chieftain")
        .battlefield(Seat(0), "Mountain")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Goblin Piker")
        .build();
    cast(&mut game, Seat(0), "Goblin Piker", &[]);
    resolve_top(&mut game);
    let piker = bf(&game, Seat(0), "Goblin Piker");
    assert!(game.objects[piker].summoning_sick);
    assert!(game.has_keyword(piker, Keyword::Haste));
    assert_eq!(game.effective_stats(piker), Some((3, 2)));
    assert!(game.attack_candidates(Seat(0)).contains(&piker), "haste lets it attack");
}

#[test]
fn auras_attach_and_fall_off_when_the_creature_dies() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Plains")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Holy Strength")
        .battlefield(Seat(1), "Mountain")
        .battlefield(Seat(1), "Mountain")
        .hand(Seat(1), "Lightning Strike")
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    let aura = hand_card(&game, Seat(0), "Holy Strength");
    cast(&mut game, Seat(0), "Holy Strength", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert_eq!(game.objects[aura].attached_to, Some(bears));
    assert_eq!(game.effective_stats(bears), Some((3, 4)));
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    cast(&mut game, Seat(1), "Lightning Strike", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert_eq!(game.objects[bears].zone, Zone::Battlefield, "3 damage on a 3/4");
    assert_eq!(game.objects[bears].damage, 3);
}

#[test]
fn equipment_equips_at_sorcery_speed_and_survives_its_bearer() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Plains")
        .battlefield(Seat(0), "Plains")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Bonesplitter")
        .battlefield(Seat(1), "Hill Giant")
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    cast(&mut game, Seat(0), "Bonesplitter", &[]);
    resolve_top(&mut game);
    let axe = bf(&game, Seat(0), "Bonesplitter");
    let equip = game
        .legal_actions(Seat(0))
        .into_iter()
        .find(|a| matches!(a, Action::ActivateAbility { object, ability, .. } if *object == axe && *ability == EQUIP_ABILITY))
        .expect("equip is offered");
    game.apply(Seat(0), &equip).unwrap();
    resolve_top(&mut game);
    assert_eq!(game.objects[axe].attached_to, Some(bears));
    assert_eq!(game.effective_stats(bears), Some((4, 2)));
    // Attack into the giant; the bears die, the axe stays.
    let giant = bf(&game, Seat(1), "Hill Giant");
    attack_with(&mut game, Seat(0), &[bears], Seat(1));
    block_with(&mut game, Seat(1), &[(giant, bears)]);
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
    assert_eq!(game.objects[giant].zone, Zone::Graveyard, "4 power kills a 3/3");
    assert_eq!(game.objects[axe].zone, Zone::Battlefield);
    assert_eq!(game.objects[axe].attached_to, None);
}

#[test]
fn raise_the_alarm_makes_tokens_that_cease_to_exist_when_they_die() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Plains")
        .battlefield(Seat(0), "Plains")
        .hand(Seat(0), "Raise the Alarm")
        .battlefield(Seat(1), "Mountain")
        .hand(Seat(1), "Shock")
        .build();
    cast(&mut game, Seat(0), "Raise the Alarm", &[]);
    resolve_top(&mut game);
    let soldiers = bfs(&game, Seat(0), "Soldier");
    assert_eq!(soldiers.len(), 2);
    assert_eq!(game.effective_stats(soldiers[0]), Some((1, 1)));
    assert!(game.view(Seat(1)).objects[&soldiers[0]].token);
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    cast(&mut game, Seat(1), "Shock", &[Target::Object(soldiers[0])]);
    resolve_top(&mut game);
    assert_eq!(game.objects[soldiers[0]].zone, Zone::OutOfGame);
    let graveyard_names: Vec<&str> = game.players[0].graveyard.iter().map(|&id| game.object_name(id)).collect();
    assert_eq!(graveyard_names, vec!["Raise the Alarm"], "the token never lands in the graveyard");
}

#[test]
fn counters_change_stats_and_annihilate() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Test Counters")
        .build();
    let bears = bf(&game, Seat(0), "Grizzly Bears");
    cast(&mut game, Seat(0), "Test Counters", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert_eq!(game.effective_stats(bears), Some((4, 4)));
    assert_eq!(game.view(Seat(0)).objects[&bears].counters, 2);
    game.objects[bears].counters.minus1 = 1;
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    assert_eq!((game.objects[bears].counters.plus1, game.objects[bears].counters.minus1), (1, 0));
}

// ----- keywords in combat -----

#[test]
fn flying_needs_flying_or_reach_to_block() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Wind Drake")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Wall of Vines")
        .build();
    let drake = bf(&game, Seat(0), "Wind Drake");
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    let wall = bf(&game, Seat(1), "Wall of Vines");
    attack_with(&mut game, Seat(0), &[drake], Seat(1));
    let err = game
        .apply(
            Seat(1),
            &Action::DeclareBlockers {
                blocks: vec![(bears, drake)],
            },
        )
        .unwrap_err();
    assert!(matches!(err, RulesError::IllegalAction { .. }));
    assert!(!game.legal_actions(Seat(1)).contains(&Action::DeclareBlockers {
        blocks: vec![(bears, drake)]
    }));
    block_with(&mut game, Seat(1), &[(wall, drake)]);
    assert_eq!(game.objects[drake].damage, 0, "a 0/3 wall deals nothing");
    assert_eq!(game.objects[wall].damage, 2);
    assert!(!game.attack_candidates(Seat(1)).contains(&wall), "defender");
}

#[test]
fn first_strike_kills_before_taking_damage() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Youthful Knight")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let knight = bf(&game, Seat(0), "Youthful Knight");
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    attack_with(&mut game, Seat(0), &[knight], Seat(1));
    block_with(&mut game, Seat(1), &[(bears, knight)]);
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
    assert_eq!(game.objects[knight].zone, Zone::Battlefield);
    assert_eq!(game.objects[knight].damage, 0, "the bears never dealt damage");
    assert_eq!(game.phase, Phase::CombatDamage);
    // Priority after first-strike damage, then the regular round passes with no one left.
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::CombatDamage);
    pass_both(&mut game);
    assert_eq!(game.phase, Phase::EndCombat);
}

#[test]
fn double_strike_hits_in_both_rounds() {
    let mut game = TestGame::new(db_with_extras(), 2).battlefield(Seat(0), "Double Bear").build();
    let bear = bf(&game, Seat(0), "Double Bear");
    attack_with(&mut game, Seat(0), &[bear], Seat(1));
    game.apply(Seat(1), &Action::DeclareBlockers { blocks: vec![] }).unwrap();
    pass_both(&mut game);
    assert_eq!(game.players[1].life, 18, "first strike round");
    pass_both(&mut game);
    assert_eq!(game.players[1].life, 16, "regular round");
}

#[test]
fn deathtouch_lifelink_and_indestructible() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Typhoid Rats")
        .battlefield(Seat(0), "Child of Night")
        .battlefield(Seat(1), "Hill Giant")
        .battlefield(Seat(1), "Stone Bear")
        .build();
    let rats = bf(&game, Seat(0), "Typhoid Rats");
    let child = bf(&game, Seat(0), "Child of Night");
    let giant = bf(&game, Seat(1), "Hill Giant");
    let stone = bf(&game, Seat(1), "Stone Bear");
    attack_with(&mut game, Seat(0), &[rats, child], Seat(1));
    block_with(&mut game, Seat(1), &[(giant, rats), (stone, child)]);
    assert_eq!(game.objects[giant].zone, Zone::Graveyard, "deathtouch");
    assert_eq!(game.objects[rats].zone, Zone::Graveyard);
    assert_eq!(game.objects[stone].zone, Zone::Battlefield, "indestructible survives lethal damage");
    assert_eq!(game.objects[stone].damage, 2);
    assert_eq!(game.players[0].life, 22, "lifelink on a blocked creature still gains");
    assert_eq!(game.objects[child].zone, Zone::Graveyard);
}

#[test]
fn trample_assigns_the_rest_to_the_player() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Colossal Dreadmaw")
        .battlefield(Seat(1), "Grizzly Bears")
        .build();
    let maw = bf(&game, Seat(0), "Colossal Dreadmaw");
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    attack_with(&mut game, Seat(0), &[maw], Seat(1));
    block_with(&mut game, Seat(1), &[(bears, maw)]);
    assert!(
        matches!(game.pending, Some(PendingChoice::AssignDamage { seat: Seat(0), .. })),
        "trample with a blocker is a choice"
    );
    let suggestions = game.legal_actions(Seat(0));
    let lethal_then_player = Action::AssignCombatDamage {
        attacker: maw,
        assignments: vec![(DamageTarget::Object(bears), 2), (DamageTarget::Player(Seat(1)), 4)],
    };
    assert!(
        suggestions.iter().any(|a| a.canonical() == lethal_then_player.canonical()),
        "{suggestions:?}"
    );
    let greedy = Action::AssignCombatDamage {
        attacker: maw,
        assignments: vec![(DamageTarget::Object(bears), 1), (DamageTarget::Player(Seat(1)), 5)],
    };
    assert!(
        matches!(game.apply(Seat(0), &greedy), Err(RulesError::IllegalAction { .. })),
        "lethal first"
    );
    game.apply(Seat(0), &lethal_then_player).unwrap();
    assert_eq!(game.players[1].life, 16);
    assert_eq!(game.objects[bears].zone, Zone::Graveyard);
}

#[test]
fn vigilance_haste_menace_and_flash() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Serra Angel")
        .battlefield(Seat(0), "Menace Bear")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Raging Goblin")
        .battlefield(Seat(1), "Grizzly Bears")
        .battlefield(Seat(1), "Hill Giant")
        .battlefield(Seat(1), "Forest")
        .battlefield(Seat(1), "Forest")
        .hand(Seat(1), "Flash Bear")
        .build();
    cast(&mut game, Seat(0), "Raging Goblin", &[]);
    resolve_top(&mut game);
    let goblin = bf(&game, Seat(0), "Raging Goblin");
    let angel = bf(&game, Seat(0), "Serra Angel");
    let menace = bf(&game, Seat(0), "Menace Bear");
    assert!(game.attack_candidates(Seat(0)).contains(&goblin), "haste");
    attack_with(&mut game, Seat(0), &[angel, menace, goblin], Seat(1));
    assert!(!game.objects[angel].tapped, "vigilance");
    assert!(game.objects[goblin].tapped);
    // Flash: seat 1 can cast a creature during combat, before blocks.
    // (Blockers are pending for seat 1; priority passed already, so test flash on their priority instead.)
    let bears = bf(&game, Seat(1), "Grizzly Bears");
    let giant = bf(&game, Seat(1), "Hill Giant");
    let one_block = Action::DeclareBlockers {
        blocks: vec![(bears, menace)],
    };
    assert!(
        matches!(game.apply(Seat(1), &one_block), Err(RulesError::IllegalAction { .. })),
        "menace"
    );
    block_with(&mut game, Seat(1), &[(bears, menace), (giant, menace)]);
    assert!(matches!(game.pending, Some(PendingChoice::AssignDamage { .. })));
    let split = game
        .legal_actions(Seat(0))
        .into_iter()
        .find(|a| matches!(a, Action::AssignCombatDamage { .. }))
        .unwrap();
    game.apply(Seat(0), &split).unwrap();
    assert_eq!(game.objects[menace].zone, Zone::Graveyard);
    // Now in the combat damage step with priority: flash creature is castable at instant speed.
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    assert_eq!(game.priority, Some(Seat(1)));
    let flash = hand_card(&game, Seat(1), "Flash Bear");
    assert!(
        game.legal_actions(Seat(1))
            .iter()
            .any(|a| matches!(a, Action::CastSpell { object, .. } if *object == flash)),
        "flash"
    );
}

#[test]
fn hexproof_blocks_opponents_targets_but_not_your_own() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Mountain")
        .battlefield(Seat(0), "Mountain")
        .hand(Seat(0), "Lightning Strike")
        .battlefield(Seat(1), "Hexproof Bear")
        .battlefield(Seat(1), "Forest")
        .hand(Seat(1), "Giant Growth")
        .build();
    let bear = bf(&game, Seat(1), "Hexproof Bear");
    let strikes: Vec<Action> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter(|a| matches!(a, Action::CastSpell { .. }))
        .collect();
    assert!(!strikes
        .iter()
        .any(|a| matches!(a, Action::CastSpell { targets, .. } if targets.contains(&Target::Object(bear)))));
    assert_eq!(strikes.len(), 2, "only the two players");
    game.apply(Seat(0), &Action::PassPriority).unwrap();
    assert!(
        game.legal_actions(Seat(1))
            .iter()
            .any(|a| matches!(a, Action::CastSpell { targets, .. } if targets.contains(&Target::Object(bear)))),
        "its controller can target it"
    );
}

// ----- interpreter primitives not yet used by a cube card -----

#[test]
fn exile_tap_untap_drain_conditional_and_granted_flying() {
    let mut game = TestGame::new(db_with_extras(), 2)
        .battlefield(Seat(0), "Plains")
        .battlefield(Seat(0), "Island")
        .battlefield(Seat(0), "Island")
        .battlefield(Seat(0), "Island")
        .battlefield(Seat(0), "Swamp")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Grizzly Bears")
        .hand(Seat(0), "Test Exile")
        .hand(Seat(0), "Test Tap")
        .hand(Seat(0), "Test Untap")
        .hand(Seat(0), "Test Drain")
        .hand(Seat(0), "Test Conditional")
        .hand(Seat(0), "Test Wings")
        .battlefield(Seat(1), "Hill Giant")
        .build();
    let giant = bf(&game, Seat(1), "Hill Giant");
    let bears = bf(&game, Seat(0), "Grizzly Bears");

    cast(&mut game, Seat(0), "Test Tap", &[Target::Object(giant)]);
    resolve_top(&mut game);
    assert!(game.objects[giant].tapped);
    cast(&mut game, Seat(0), "Test Untap", &[Target::Object(giant)]);
    resolve_top(&mut game);
    assert!(!game.objects[giant].tapped);

    cast(&mut game, Seat(0), "Test Wings", &[Target::Object(bears)]);
    resolve_top(&mut game);
    assert!(game.has_keyword(bears, Keyword::Flying));
    assert!(game.view(Seat(1)).objects[&bears].keywords.contains(&Keyword::Flying));

    cast(&mut game, Seat(0), "Test Drain", &[]);
    resolve_top(&mut game);
    assert_eq!(game.players[1].life, 18);
    assert_eq!(game.players[0].life, 22);

    let hand_before = game.players[0].hand.len();
    cast(&mut game, Seat(0), "Test Conditional", &[]);
    resolve_top(&mut game);
    assert_eq!(game.players[0].hand.len(), hand_before, "cast one, drew one");

    cast(&mut game, Seat(0), "Test Exile", &[Target::Object(giant)]);
    resolve_top(&mut game);
    assert_eq!(game.objects[giant].zone, Zone::Exile);
}

#[test]
fn views_describe_the_stack_and_abilities() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Prodigal Pyromancer")
        .battlefield(Seat(0), "Furnace Whelp")
        .battlefield(Seat(0), "Mountain")
        .build();
    let view = game.view(Seat(0));
    let pinger = bf(&game, Seat(0), "Prodigal Pyromancer");
    assert_eq!(
        view.objects[&pinger].abilities,
        vec!["{T}: ~ deals 1 damage to any target.".to_string()]
    );
    let ping = game
        .legal_actions(Seat(0))
        .into_iter()
        .find(|a| matches!(a, Action::ActivateAbility { object, targets, .. } if *object == pinger && targets == &vec![Target::Player(Seat(1))]))
        .unwrap();
    let text = engine::text::describe_action(&game, &ping);
    assert!(text.contains("deals 1 damage") && text.contains("P1"), "{text}");
    game.apply(Seat(0), &ping).unwrap();
    let view = game.view(Seat(1));
    assert_eq!(view.stack[0].kind, "ability");
    assert!(view.stack[0].description.contains("deals 1 damage"));
    let rendered = engine::text::render_view(&view);
    assert!(rendered.contains("[ability:"), "{rendered}");
    assert!(rendered.contains("flying"), "keywords in the text view: {rendered}");
}

#[test]
fn big_mana_producers_are_offered_and_custom_payments_are_accepted() {
    let mut game = TestGame::new(db(), 2)
        .battlefield(Seat(0), "Elvish Archdruid")
        .battlefield(Seat(0), "Llanowar Elves")
        .battlefield(Seat(0), "Llanowar Elves")
        .battlefield(Seat(0), "Elvish Mystic")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .battlefield(Seat(0), "Forest")
        .hand(Seat(0), "Overrun")
        .hand(Seat(0), "Giant Growth")
        .build();
    let druid = bf(&game, Seat(0), "Elvish Archdruid");
    let forests = bfs(&game, Seat(0), "Forest");
    let overrun = hand_card(&game, Seat(0), "Overrun");
    let payments: Vec<engine::ManaPayment> = game
        .legal_actions(Seat(0))
        .into_iter()
        .filter_map(|a| match a {
            Action::CastSpell { object, payment, .. } if object == overrun => Some(payment),
            _ => None,
        })
        .collect();
    // Lands first: three Forests plus the Archdruid (one creature, not two Elves).
    assert!(
        payments
            .iter()
            .any(|p| p.tap.len() == 4 && p.tap.contains(&druid) && forests.iter().all(|f| p.tap.contains(f))),
        "{payments:?}"
    );
    // Producers first: the Archdruid (4 mana) plus one Forest.
    assert!(payments.iter().any(|p| p.tap.len() == 2 && p.tap.contains(&druid)), "{payments:?}");

    // A hand-built payment that covers the cost is accepted even though it was never listed:
    // the Archdruid and one specific Forest, leaving the other two up for Giant Growth.
    let custom = Action::CastSpell {
        object: overrun,
        targets: vec![],
        payment: engine::ManaPayment {
            tap: vec![druid, forests[2]],
            ..Default::default()
        },
    };
    game.apply(Seat(0), &custom).unwrap();
    assert!(game.objects[druid].tapped);
    assert!(game.objects[forests[2]].tapped);
    assert!(!game.objects[forests[0]].tapped && !game.objects[forests[1]].tapped);
    resolve_top(&mut game);
    let growth = hand_card(&game, Seat(0), "Giant Growth");
    assert!(
        game.legal_actions(Seat(0))
            .iter()
            .any(|a| matches!(a, Action::CastSpell { object, .. } if *object == growth)),
        "Forests left up"
    );

    // One that does not cover the cost is refused.
    let short = Action::CastSpell {
        object: growth,
        targets: vec![Target::Object(druid)],
        payment: engine::ManaPayment {
            tap: vec![druid],
            ..Default::default()
        },
    };
    assert!(
        matches!(game.apply(Seat(0), &short), Err(RulesError::IllegalAction { .. })),
        "tapped source"
    );
}
