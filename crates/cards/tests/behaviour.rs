//! One behaviour check per core card (§4.2: "every cube card is hand-written
//! IR with a hand-written test"). Cards whose behaviour is fully described by
//! their printed stats, keywords, or a static on an aura or equipment get a
//! generic check derived from the IR; everything with a spell, trigger, or
//! activated ability gets a scenario of its own. A card without an entry
//! fails the registry test, so adding a card means adding a check.

use cardir::{Color, Keyword};
use engine::testing::{advance_until, TestGame};
use engine::{Action, AttackTarget, Game, ObjectId, PendingChoice, Seat, Target, Zone, EQUIP_ABILITY};
use std::collections::BTreeMap;
use std::sync::Arc;

const ME: Seat = Seat(0);
const OPP: Seat = Seat(1);

fn db() -> Arc<engine::CardDb> {
    Arc::new(cards::core())
}

/// A two-seat game where I have plenty of every colour of mana and the
/// opponent has a Grizzly Bears, positioned at my first main phase.
fn base() -> TestGame {
    let mut t = TestGame::new(db(), 2);
    for land in ["Plains", "Island", "Swamp", "Mountain", "Forest"] {
        for _ in 0..4 {
            t = t.battlefield(ME, land);
        }
    }
    t.battlefield(OPP, "Grizzly Bears")
}

fn hand(game: &Game, seat: Seat, name: &str) -> ObjectId {
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
        .unwrap_or_else(|| panic!("{name} not on {seat}'s battlefield"))
}

fn count_bf(game: &Game, seat: Seat, name: &str) -> usize {
    game.players[seat.index()]
        .battlefield
        .iter()
        .filter(|&&id| game.object_name(id) == name)
        .count()
}

fn stats(game: &Game, id: ObjectId) -> (i32, i32) {
    game.effective_stats(id).expect("a creature")
}

fn kws(game: &Game, id: ObjectId) -> Vec<Keyword> {
    game.keywords_of(id)
}

/// Answer pending choices with the first suggestion and pass priority until
/// the stack is empty (or a choice that needs a specific answer opens).
fn settle(game: &mut Game) {
    for _ in 0..40 {
        if game.is_over().is_some() {
            return;
        }
        if let Some(p) = &game.pending {
            if matches!(p, PendingChoice::ChooseTargets { .. }) {
                return;
            }
            let seat = p.seat();
            let acts = game.legal_actions(seat);
            let a = acts.iter().find(|a| !matches!(a, Action::Concede)).cloned().expect("a choice");
            game.apply(seat, &a).unwrap();
            continue;
        }
        if game.stack.is_empty() {
            return;
        }
        let seat = game.priority.expect("priority");
        game.apply(seat, &Action::PassPriority).unwrap();
    }
    panic!("stack never emptied");
}

/// Cast `name` from my hand with these targets and let it resolve.
fn cast(game: &mut Game, name: &str, targets: &[Target]) {
    cast_by(game, ME, name, targets);
}

fn cast_by(game: &mut Game, seat: Seat, name: &str, targets: &[Target]) {
    cast_raw(game, seat, name, targets);
    settle(game);
}

/// Cast without settling, for tests that must answer a choice themselves.
fn cast_raw(game: &mut Game, seat: Seat, name: &str, targets: &[Target]) {
    let id = hand(game, seat, name);
    let a = game
        .legal_actions(seat)
        .into_iter()
        .find(|a| matches!(a, Action::CastSpell { object, targets: t, .. } if *object == id && t == targets))
        .unwrap_or_else(|| {
            panic!(
                "no legal cast of {name} with {targets:?}: {:?}",
                game.legal_actions(seat)
                    .iter()
                    .map(|a| engine::text::describe_action(game, a))
                    .collect::<Vec<_>>()
            )
        });
    game.apply(seat, &a).unwrap();
}

/// Cast a spell whose modes and targets are chosen after paying: pay, pick
/// each mode in turn (by index), then each target spec's targets in turn.
fn cast_steps(game: &mut Game, name: &str, modes: &[u8], targets: &[&[Target]]) {
    cast_raw(game, ME, name, &[]);
    for &m in modes {
        assert!(
            matches!(game.pending, Some(PendingChoice::Casting { .. })),
            "expected to be choosing modes, got {:?}",
            game.pending
        );
        game.apply(ME, &Action::ChooseMode { mode: m }).unwrap();
    }
    for t in targets {
        assert!(
            matches!(game.pending, Some(PendingChoice::Casting { .. })),
            "expected to be choosing targets, got {:?}",
            game.pending
        );
        game.apply(ME, &Action::ChooseTargets { targets: t.to_vec() }).unwrap();
    }
    assert_eq!(game.stack.len(), 1, "the spell is on the stack: {:?}", game.pending);
    settle(game);
}

/// Answer a resolving effect's pick ("sacrifice a creature", "a permanent you control").
fn pick(game: &mut Game, seat: Seat, targets: &[Target]) {
    assert!(
        matches!(game.pending, Some(PendingChoice::Choose { .. })),
        "expected a pick, got {:?}",
        game.pending
    );
    let a = game
        .legal_actions(seat)
        .into_iter()
        .find(|a| matches!(a, Action::ChooseTargets { targets: t } if t == targets))
        .expect("that pick is offered");
    game.apply(seat, &a).unwrap();
    settle(game);
}

/// Activate ability `index` of `name` on my battlefield with these targets.
fn activate(game: &mut Game, name: &str, index: u8, targets: &[Target]) {
    let id = bf(game, ME, name);
    let a = game
        .legal_actions(ME)
        .into_iter()
        .find(|a| matches!(a, Action::ActivateAbility { object, ability, targets: t, .. } if *object == id && *ability == index && t == targets))
        .unwrap_or_else(|| panic!("no legal activation of {name} #{index} with {targets:?}"));
    game.apply(ME, &a).unwrap();
    settle(game);
}

/// The target-choice for a trigger that just went looking for one.
fn choose(game: &mut Game, seat: Seat, target: Target) {
    assert!(
        matches!(game.pending, Some(PendingChoice::ChooseTargets { .. })),
        "expected a target choice, got {:?}",
        game.pending
    );
    let a = game
        .legal_actions(seat)
        .into_iter()
        .find(|a| matches!(a, Action::ChooseTargets { targets } if targets.contains(&target)))
        .expect("that target is choosable");
    game.apply(seat, &a).unwrap();
    settle(game);
}

fn attack(game: &mut Game, attackers: &[ObjectId]) {
    advance_until(game, |g| matches!(g.pending, Some(PendingChoice::DeclareAttackers { .. }))).unwrap();
    let attackers = attackers.iter().map(|&id| (id, AttackTarget::Player(OPP))).collect();
    game.apply(ME, &Action::DeclareAttackers { attackers }).unwrap();
}

fn block(game: &mut Game, blocks: Vec<(ObjectId, ObjectId)>) {
    advance_until(game, |g| matches!(g.pending, Some(PendingChoice::DeclareBlockers { .. }))).unwrap();
    game.apply(OPP, &Action::DeclareBlockers { blocks }).unwrap();
}

fn end_of_combat(game: &mut Game) {
    advance_until(game, |g| g.phase == engine::Phase::Main2 || g.is_over().is_some()).unwrap();
}

fn life(game: &Game, seat: Seat) -> i32 {
    game.players[seat.index()].life
}

fn hand_size(game: &Game, seat: Seat) -> usize {
    game.players[seat.index()].hand.len()
}

fn in_graveyard(game: &Game, id: ObjectId) -> bool {
    game.objects[id].zone == Zone::Graveyard
}

fn gy(game: &Game, seat: Seat, name: &str) -> ObjectId {
    game.players[seat.index()]
        .graveyard
        .iter()
        .copied()
        .find(|&id| game.object_name(id) == name)
        .unwrap_or_else(|| panic!("{name} not in {seat}'s graveyard"))
}

/// "Return target <card> from your graveyard to your hand."
fn regrow(name: &str, card: &str) {
    let mut game = base().graveyard(ME, card).hand(ME, name).build();
    let id = gy(&game, ME, card);
    cast(&mut game, name, &[Target::Object(id)]);
    assert_eq!(game.objects[id].zone, Zone::Hand, "{name} returns {card}");
    assert!(game.players[0].hand.contains(&id));
}

/// "Target player mills N cards."
fn mill(name: &str, who: Seat, n: usize) {
    let mut game = base().hand(ME, name).library(who, &["Forest"; 9]).build();
    cast(&mut game, name, &[Target::Player(who)]);
    assert_eq!(game.players[who.index()].graveyard.len(), n, "{name}");
    assert_eq!(game.players[who.index()].library.len(), 9 - n);
}

// ----- generic checks derived from the IR -----

/// A permanent with no spell, trigger, or activated ability beyond mana:
/// cast it (attaching auras and equipment to my bear) and compare the
/// result with what the card says.
fn generic_permanent(name: &str) {
    let db = db();
    let card = db.get(db.lookup(name).unwrap()).ir.clone();
    if card.is_land() {
        return generic_land(name);
    }
    let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, name).build();
    let bear = bf(&game, ME, "Grizzly Bears");
    if card.is_aura() {
        cast(&mut game, name, &[Target::Object(bear)]);
    } else {
        cast(&mut game, name, &[]);
    }
    if card.is_aura() && in_graveyard(&game, bear) {
        // A -N/-N aura killed the 2/2 and went to the graveyard with it.
        assert!(
            card.statics
                .iter()
                .any(|s| matches!(s, cardir::Static::PtBoost { toughness: cardir::Amount::Const(t), .. } if *t <= -2)),
            "{name} unexpectedly killed the bear"
        );
        return;
    }
    let id = bf(&game, ME, name);
    if card.is_equipment() {
        activate(&mut game, name, EQUIP_ABILITY, &[Target::Object(bear)]);
    }
    if let Some((p, t)) = card.pt {
        assert_eq!(stats(&game, id), (p, t), "{name} stats");
        for k in &card.keywords {
            assert!(kws(&game, id).contains(k), "{name} should have {k:?}");
        }
    }
    if card.is_aura() || card.is_equipment() {
        assert_eq!(game.objects[id].attached_to, Some(bear), "{name} attached");
        let (mut p, mut t) = (2, 2);
        let mut expect_kws = Vec::new();
        for s in &card.statics {
            match s {
                cardir::Static::PtBoost {
                    power,
                    toughness,
                    keywords,
                    ..
                } => {
                    if let (cardir::Amount::Const(dp), cardir::Amount::Const(dt)) = (power, toughness) {
                        p += dp;
                        t += dt;
                    }
                    expect_kws.extend(keywords.iter().copied());
                }
                cardir::Static::GrantKeyword { keyword, .. } => expect_kws.push(*keyword),
                cardir::Static::CostReduction { .. } | cardir::Static::AsLongAs { .. } => {}
            }
        }
        if t > 0 {
            assert_eq!(stats(&game, bear), (p, t), "{name} on a 2/2");
            for k in expect_kws {
                assert!(kws(&game, bear).contains(&k), "{name} grants {k:?}");
            }
        } else {
            assert!(in_graveyard(&game, bear), "{name} kills a 2/2");
        }
    }
    // Mana creatures and rocks: once untapped and unsick, they are offered as a source.
    for a in card.mana_abilities() {
        if let Some(cardir::Effect::AddMana { color, .. }) = a.effects.first() {
            let mut game = TestGame::new(db.clone(), 2).battlefield(ME, name).build();
            let id = bf(&game, ME, name);
            game.objects[id].summoning_sick = false;
            let sources = game.mana_sources_with_amounts(ME);
            assert!(
                sources.iter().any(|(s, c, _)| *s == id
                    && color
                        .map(|cc| engine::Mana::Colored(cc) == *c)
                        .unwrap_or(*c == engine::Mana::Colorless)),
                "{name} taps for mana: {sources:?}"
            );
        }
    }
}

fn generic_land(name: &str) {
    let mut game = TestGame::new(db(), 2).hand(ME, name).build();
    let id = hand(&game, ME, name);
    game.apply(ME, &Action::PlayLand { object: id }).unwrap();
    assert_eq!(game.objects[id].zone, Zone::Battlefield);
    let def = game.card_def(id);
    let colors = def.produces();
    assert!(!colors.is_empty(), "{name} produces mana");
    let sources = game.mana_sources_with_amounts(ME);
    assert!(sources.iter().any(|(s, _, _)| *s == id));
}

// ----- scenario helpers for the common spell shapes -----

/// A spell that damages: on the opponent's bear it dies at 2+, on the opponent it costs life.
fn burn(name: &str, amount: i32, creature_ok: bool, player_ok: bool) {
    if creature_ok {
        let mut game = base().hand(ME, name).build();
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, name, &[Target::Object(bear)]);
        if amount >= 2 {
            assert!(in_graveyard(&game, bear), "{name} kills a 2/2");
        } else {
            assert_eq!(game.objects[bear].damage, amount, "{name} marks damage");
        }
    }
    if player_ok {
        let mut game = base().hand(ME, name).build();
        cast(&mut game, name, &[Target::Player(OPP)]);
        assert_eq!(life(&game, OPP), 20 - amount, "{name} hits face");
    }
}

/// Target creature gets +p/+t (and keywords) until end of turn.
fn pump(name: &str, p: i32, t: i32, keywords: &[Keyword]) {
    let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, name).build();
    let bear = bf(&game, ME, "Grizzly Bears");
    cast(&mut game, name, &[Target::Object(bear)]);
    assert_eq!(stats(&game, bear), (2 + p, 2 + t), "{name}");
    for k in keywords {
        assert!(kws(&game, bear).contains(k), "{name} grants {k:?}");
    }
    advance_until(&mut game, |g| g.turn > 1).unwrap();
    assert_eq!(stats(&game, bear), (2, 2), "{name} wears off");
}

/// Target creature gets -p/-t: the opponent's 2/2 dies when t >= 2.
fn shrink(name: &str, p: i32, t: i32) {
    let mut game = base().battlefield(OPP, "Hill Giant").hand(ME, name).build();
    let giant = bf(&game, OPP, "Hill Giant");
    cast(&mut game, name, &[Target::Object(giant)]);
    if t >= 3 {
        assert!(in_graveyard(&game, giant), "{name} kills a 3/3");
    } else {
        assert_eq!(stats(&game, giant), (3 - p, 3 - t), "{name}");
    }
}

/// Destroy target X: `victim` (on the opponent's side) goes to the graveyard.
fn destroy(name: &str, victim: &str) {
    let mut game = base().battlefield(OPP, victim).hand(ME, name).build();
    let v = bf(&game, OPP, victim);
    cast(&mut game, name, &[Target::Object(v)]);
    assert!(in_graveyard(&game, v), "{name} destroys {victim}");
}

fn destroy_all(name: &str, dies: &[&str], survives: &[&str]) {
    let mut t = base().hand(ME, name);
    for d in dies {
        t = t.battlefield(ME, d);
    }
    for s in survives {
        t = t.battlefield(OPP, s);
    }
    let mut game = t.build();
    let gone: Vec<ObjectId> = dies.iter().map(|d| bf(&game, ME, d)).collect();
    let bear = bf(&game, OPP, "Grizzly Bears");
    let kept: Vec<ObjectId> = survives.iter().map(|s| bf(&game, OPP, s)).collect();
    cast(&mut game, name, &[]);
    for g in gone {
        assert_ne!(game.objects[g].zone, Zone::Battlefield, "{name} removes {}", game.object_name(g));
    }
    assert_ne!(game.objects[bear].zone, Zone::Battlefield, "{name} hits both sides");
    for k in kept {
        assert_eq!(game.objects[k].zone, Zone::Battlefield, "{name} spares {}", game.object_name(k));
    }
}

fn draw_spell(name: &str, n: usize) {
    let mut game = base().hand(ME, name).library(ME, &["Forest"; 6]).build();
    cast(&mut game, name, &[]);
    assert_eq!(hand_size(&game, ME), n, "{name} draws {n}");
}

fn gain_spell(name: &str, n: i32) {
    let mut game = base().hand(ME, name).build();
    cast(&mut game, name, &[]);
    assert_eq!(life(&game, ME), 20 + n, "{name}");
}

fn tokens_spell(name: &str, count: usize, token: &str) {
    let mut game = base().hand(ME, name).build();
    cast(&mut game, name, &[]);
    assert_eq!(count_bf(&game, ME, token), count, "{name} makes {count} {token}");
    let t = bf(&game, ME, token);
    assert!(game.card_def(t).token);
}

fn bounce(name: &str, victim: &str) {
    let mut game = base().battlefield(OPP, victim).hand(ME, name).build();
    let v = bf(&game, OPP, victim);
    cast(&mut game, name, &[Target::Object(v)]);
    assert_eq!(game.objects[v].zone, Zone::Hand, "{name} bounces {victim}");
}

/// A counterspell: the opponent casts `spell` (a creature) and it never lands.
fn counter(name: &str, extra_draw: bool) {
    let mut game = base()
        .battlefield(OPP, "Forest")
        .battlefield(OPP, "Forest")
        .hand(OPP, "Grizzly Bears")
        .hand(ME, name)
        .library(ME, &["Forest"; 4])
        .starting_player(OPP)
        .build();
    let bears = hand(&game, OPP, "Grizzly Bears");
    let a = game
        .legal_actions(OPP)
        .into_iter()
        .find(|a| matches!(a, Action::CastSpell { object, .. } if *object == bears))
        .unwrap();
    game.apply(OPP, &a).unwrap();
    game.apply(OPP, &Action::PassPriority).unwrap();
    let before = hand_size(&game, ME);
    cast(&mut game, name, &[Target::Object(bears)]);
    assert!(in_graveyard(&game, bears), "{name} counters the creature");
    if extra_draw {
        assert_eq!(hand_size(&game, ME), before, "{name} replaces itself");
    }
}

/// A creature with an ETB: cast it and run `check`.
fn etb(name: &str, target: Option<&str>, check: impl Fn(&Game, ObjectId)) {
    let mut t = base()
        .battlefield(ME, "Hill Giant")
        .library(ME, &["Forest"; 6])
        .library(OPP, &["Forest"; 6])
        .hand(ME, name);
    if let Some(v) = target {
        t = t.battlefield(OPP, v);
    }
    let mut game = t.build();
    cast(&mut game, name, &[]);
    if let Some(v) = target {
        let victim = bf(&game, OPP, v);
        choose(&mut game, ME, Target::Object(victim));
    }
    let id = bf(&game, ME, name);
    check(&game, id);
}

/// A creature with a dies trigger: it dies to a Doom Blade from the opponent.
fn dies(name: &str, target: Option<Target>, check: impl Fn(&Game)) {
    let mut game = base()
        .battlefield(ME, name)
        .battlefield(OPP, "Swamp")
        .battlefield(OPP, "Swamp")
        .battlefield(OPP, "Swamp")
        .hand(OPP, "Murder")
        .hand(OPP, "Forest")
        .library(ME, &["Forest"; 6])
        .starting_player(OPP)
        .build();
    let id = bf(&game, ME, name);
    cast_by(&mut game, OPP, "Murder", &[Target::Object(id)]);
    if let Some(t) = target {
        choose(&mut game, ME, t);
    }
    assert_ne!(game.objects[id].zone, Zone::Battlefield);
    check(&game);
}

/// "{cost}: this creature gets +p/+t until end of turn" style abilities.
fn self_pump(name: &str, p: i32, t: i32) {
    let mut game = base().battlefield(ME, name).build();
    let id = bf(&game, ME, name);
    let (bp, bt) = stats(&game, id);
    activate(&mut game, name, 0, &[]);
    assert_eq!(stats(&game, id), (bp + p, bt + t), "{name} pumps");
}

/// "Sacrifice a creature: this creature gets +2/+2 until end of turn."
fn sac_pump(name: &str) {
    let mut game = base().battlefield(ME, name).battlefield(ME, "Grizzly Bears").build();
    let id = bf(&game, ME, name);
    let bear = bf(&game, ME, "Grizzly Bears");
    let (bp, bt) = stats(&game, id);
    let a = game
        .legal_actions(ME)
        .into_iter()
        .find(|a| matches!(a, Action::ActivateAbility { object, payment, .. } if *object == id && payment.sacrifice.contains(&bear)))
        .expect("sacrifice the bear");
    game.apply(ME, &a).unwrap();
    settle(&mut game);
    assert!(in_graveyard(&game, bear));
    assert_eq!(stats(&game, id), (bp + 2, bt + 2), "{name}");
}

/// "{T}: this deals 1 damage to any target."
fn pinger(name: &str) {
    let mut game = base().battlefield(ME, name).build();
    activate(&mut game, name, 0, &[Target::Player(OPP)]);
    assert_eq!(life(&game, OPP), 19, "{name} pings");
    assert!(game.objects[bf(&game, ME, name)].tapped);
}

/// An aura that only matters through an activated ability on it.
fn aura_ability(name: &str, check: impl Fn(&Game, ObjectId)) {
    let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, name).build();
    let bear = bf(&game, ME, "Grizzly Bears");
    cast(&mut game, name, &[Target::Object(bear)]);
    activate(&mut game, name, 0, &[]);
    check(&game, bear);
}

// ----- the registry -----

type Check = Box<dyn Fn()>;

fn registry() -> BTreeMap<&'static str, Check> {
    let mut r: BTreeMap<&'static str, Check> = BTreeMap::new();
    macro_rules! generic {
        ($($n:literal),* $(,)?) => { $( r.insert($n, Box::new(|| generic_permanent($n))); )* };
    }
    macro_rules! check {
        ($n:literal, $body:expr) => {
            r.insert($n, Box::new($body));
        };
    }

    // Stats, keywords, statics on auras and equipment, lands, and mana producers.
    generic!(
        "Plains",
        "Island",
        "Swamp",
        "Mountain",
        "Forest",
        // white
        "Alaborn Trooper",
        "Devoted Hero",
        "Eager Cadet",
        "Glory Seeker",
        "Oreskos Swiftclaw",
        "Pearled Unicorn",
        "Regal Unicorn",
        "Savannah Lions",
        "Serra Angel",
        "Suntail Hawk",
        "Youthful Knight",
        "Holy Strength",
        "Serra's Embrace",
        "Seraph of Dawn",
        "Benalish Knight",
        "Ajani's Sunstriker",
        "Concordia Pegasus",
        "Mesa Unicorn",
        "Elite Vanguard",
        "Fencing Ace",
        "Skyhunter Skirmisher",
        "Bishop's Soldier",
        "Pillarfield Ox",
        "Aven Skirmisher",
        // blue
        "Air Elemental",
        "Coral Merfolk",
        "Fugitive Wizard",
        "Horned Turtle",
        "Merfolk of the Pearl Trident",
        "Phantom Monster",
        "Vodalian Soldiers",
        "Wind Drake",
        "Mahamoti Djinn",
        "Snapping Drake",
        "Serra Sphinx",
        "Nimbus of the Isles",
        "Amphin Cutthroat",
        "Faerie Invaders",
        "Wall of Air",
        "Storm Crow",
        "Kraken Hatchling",
        // black
        "Child of Night",
        "Muck Rats",
        "Scathe Zombies",
        "Typhoid Rats",
        "Undead Minotaur",
        "Vampire Nighthawk",
        "Walking Corpse",
        "Zombie Goliath",
        "Deathgaze Cockatrice",
        "Dead Weight",
        "Weakness",
        "Unholy Strength",
        "Bogstomper",
        // red
        "Balduvian Barbarians",
        "Bloodfire Expert",
        "Borderland Minotaur",
        "Canyon Minotaur",
        "Fire Elemental",
        "Goblin Piker",
        "Gray Ogre",
        "Hill Giant",
        "Raging Goblin",
        "Thundering Giant",
        "Goblin Roughrider",
        "Volcanic Dragon",
        "Lightning Elemental",
        "Lightning Talons",
        "Goblin War Paint",
        "Skyraker Giant",
        // green
        "Alpine Grizzly",
        "Centaur Courser",
        "Colossal Dreadmaw",
        "Craw Wurm",
        "Elvish Warrior",
        "Garruk's Companion",
        "Grizzled Outrider",
        "Grizzly Bears",
        "Rumbling Baloth",
        "Runeclaw Bear",
        "Terrain Elemental",
        "Wall of Vines",
        "Llanowar Elves",
        "Elvish Mystic",
        "Deadly Recluse",
        "Giant Spider",
        "Stampeding Rhino",
        "Yavimaya Wurm",
        "Spined Wurm",
        "Kalonian Tusker",
        "Leatherback Baloth",
        "Fyndhorn Elves",
        "Boreal Druid",
        "Druid of the Cowl",
        "Vastwood Gorger",
        "Enormous Baloth",
        "Duskdale Wurm",
        "Silverback Ape",
        "Nessian Courser",
        "Ambush Viper",
        "Tajuru Pathwarden",
        // artifacts
        "Bonesplitter",
        "Vulshok Morningstar",
        "Leonin Scimitar",
        "Short Sword",
        "Vulshok Battlegear",
        "Loxodon Warhammer",
        "Trusty Machete",
        "Darksteel Axe",
        "Neurok Hoversail",
        "Accorder's Shield",
        "Ornithopter",
        "Phyrexian Hulk",
        "Alpha Myr",
        "Iron Myr",
        "Copper Myr",
        "Gold Myr",
        "Silver Myr",
        "Leaden Myr",
        "Steel Wall",
        "Wall of Spears",
    );

    // ----- white -----
    check!("Angelic Blessing", || pump("Angelic Blessing", 3, 3, &[Keyword::Flying]));
    check!("Chaplain's Blessing", || gain_spell("Chaplain's Blessing", 5));
    check!("Divine Verdict", || {
        let mut game = base()
            .battlefield(ME, "Grizzly Bears")
            .hand(OPP, "Divine Verdict")
            .battlefield(OPP, "Plains")
            .battlefield(OPP, "Plains")
            .battlefield(OPP, "Plains")
            .battlefield(OPP, "Plains")
            .build();
        let bear = bf(&game, ME, "Grizzly Bears");
        attack(&mut game, &[bear]);
        // The opponent kills the attacker at instant speed.
        advance_until(&mut game, |g| g.priority == Some(OPP)).unwrap();
        cast_by(&mut game, OPP, "Divine Verdict", &[Target::Object(bear)]);
        assert!(in_graveyard(&game, bear));
    });
    check!("Inspired Charge", || {
        let mut game = base()
            .battlefield(ME, "Grizzly Bears")
            .battlefield(ME, "Hill Giant")
            .hand(ME, "Inspired Charge")
            .build();
        let bear = bf(&game, ME, "Grizzly Bears");
        let giant = bf(&game, ME, "Hill Giant");
        let opp_bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Inspired Charge", &[]);
        assert_eq!(stats(&game, bear), (4, 3));
        assert_eq!(stats(&game, giant), (5, 4));
        assert_eq!(stats(&game, opp_bear), (2, 2), "only mine");
    });
    check!("Raise the Alarm", || tokens_spell("Raise the Alarm", 2, "Soldier"));
    check!("Revitalize", || {
        let mut game = base().hand(ME, "Revitalize").library(ME, &["Forest"; 3]).build();
        cast(&mut game, "Revitalize", &[]);
        assert_eq!(life(&game, ME), 23);
        assert_eq!(hand_size(&game, ME), 1);
    });
    check!("Smite the Monstrous", || destroy("Smite the Monstrous", "Craw Wurm"));
    check!("Benalish Marshal", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").battlefield(ME, "Benalish Marshal").build();
        assert_eq!(stats(&game, bf(&game, ME, "Grizzly Bears")), (3, 3));
        assert_eq!(stats(&game, bf(&game, ME, "Benalish Marshal")), (3, 3), "not itself");
        assert_eq!(stats(&game, bf(&game, OPP, "Grizzly Bears")), (2, 2));
        let _ = &mut game;
    });
    check!("Angel of Mercy", || etb("Angel of Mercy", None, |g, _| assert_eq!(life(g, ME), 23)));
    check!("Righteousness", || {
        let mut game = base()
            .battlefield(ME, "Hill Giant")
            .hand(OPP, "Righteousness")
            .battlefield(OPP, "Plains")
            .build();
        let giant = bf(&game, ME, "Hill Giant");
        let bear = bf(&game, OPP, "Grizzly Bears");
        attack(&mut game, &[giant]);
        block(&mut game, vec![(bear, giant)]);
        advance_until(&mut game, |g| g.priority == Some(OPP)).unwrap();
        cast_by(&mut game, OPP, "Righteousness", &[Target::Object(bear)]);
        assert_eq!(stats(&game, bear), (9, 9));
        end_of_combat(&mut game);
        assert!(in_graveyard(&game, giant), "the blocker wins");
    });
    check!("Sunlance", || {
        burn("Sunlance", 3, true, false);
        let game = base().battlefield(OPP, "Savannah Lions").hand(ME, "Sunlance").build();
        let lions = bf(&game, OPP, "Savannah Lions");
        assert!(
            !game
                .legal_actions(ME)
                .iter()
                .any(|a| matches!(a, Action::CastSpell { targets, .. } if targets.contains(&Target::Object(lions)))),
            "not white creatures"
        );
    });
    check!("Solemn Offering", || {
        let mut game = base().battlefield(OPP, "Bonesplitter").hand(ME, "Solemn Offering").build();
        let axe = bf(&game, OPP, "Bonesplitter");
        cast(&mut game, "Solemn Offering", &[Target::Object(axe)]);
        assert!(in_graveyard(&game, axe));
        assert_eq!(life(&game, ME), 24);
    });
    check!("Demystify", || destroy("Demystify", "Fervor"));
    check!("Disenchant", || destroy("Disenchant", "Bonesplitter"));
    check!("Kill Shot", || {
        let mut game = base()
            .battlefield(ME, "Hill Giant")
            .hand(OPP, "Kill Shot")
            .battlefield(OPP, "Plains")
            .battlefield(OPP, "Plains")
            .battlefield(OPP, "Plains")
            .build();
        let giant = bf(&game, ME, "Hill Giant");
        attack(&mut game, &[giant]);
        advance_until(&mut game, |g| g.priority == Some(OPP)).unwrap();
        cast_by(&mut game, OPP, "Kill Shot", &[Target::Object(giant)]);
        assert!(in_graveyard(&game, giant));
    });
    check!("Celestial Flare", || {
        let mut game = base()
            .battlefield(ME, "Hill Giant")
            .battlefield(ME, "Grizzly Bears")
            .hand(OPP, "Celestial Flare")
            .battlefield(OPP, "Plains")
            .battlefield(OPP, "Plains")
            .build();
        let giant = bf(&game, ME, "Hill Giant");
        attack(&mut game, &[giant]);
        advance_until(&mut game, |g| g.priority == Some(OPP)).unwrap();
        cast_by(&mut game, OPP, "Celestial Flare", &[Target::Player(ME)]);
        assert!(in_graveyard(&game, giant), "the only attacker is sacrificed");
        assert_eq!(game.objects[bf(&game, ME, "Grizzly Bears")].zone, Zone::Battlefield);
    });
    check!("Glorious Charge", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Glorious Charge").build();
        cast(&mut game, "Glorious Charge", &[]);
        assert_eq!(stats(&game, bf(&game, ME, "Grizzly Bears")), (3, 3));
        assert_eq!(stats(&game, bf(&game, OPP, "Grizzly Bears")), (2, 2));
    });
    check!("Lone Missionary", || etb("Lone Missionary", None, |g, _| assert_eq!(
        life(g, ME),
        24
    )));
    check!("Attended Knight", || etb("Attended Knight", None, |g, _| assert_eq!(
        count_bf(g, ME, "Soldier"),
        1
    )));
    check!("Wall of Omens", || etb("Wall of Omens", None, |g, _| assert_eq!(
        hand_size(g, ME),
        1
    )));
    check!("Charging Griffin", || {
        let mut game = base().battlefield(ME, "Charging Griffin").build();
        let g = bf(&game, ME, "Charging Griffin");
        attack(&mut game, &[g]);
        settle(&mut game);
        assert_eq!(stats(&game, g), (3, 3));
    });
    check!("Soul Warden", || {
        let mut game = base()
            .hand(ME, "Soul Warden")
            .hand(ME, "Grizzly Bears")
            .battlefield(OPP, "Forest")
            .battlefield(OPP, "Forest")
            .hand(OPP, "Grizzly Bears")
            .build();
        cast(&mut game, "Soul Warden", &[]);
        assert_eq!(life(&game, ME), 20, "not itself");
        cast(&mut game, "Grizzly Bears", &[]);
        assert_eq!(life(&game, ME), 21);
        // An opponent's creature counts too.
        advance_until(&mut game, |g| g.active_player == OPP && g.phase == engine::Phase::Main1).unwrap();
        cast_by(&mut game, OPP, "Grizzly Bears", &[]);
        assert_eq!(life(&game, ME), 22);
    });
    check!("Ajani's Pridemate", || {
        let mut game = base().battlefield(ME, "Ajani's Pridemate").hand(ME, "Chaplain's Blessing").build();
        let cat = bf(&game, ME, "Ajani's Pridemate");
        cast(&mut game, "Chaplain's Blessing", &[]);
        assert_eq!(life(&game, ME), 25);
        assert_eq!(stats(&game, cat), (3, 3), "one counter per life gain event");
    });
    check!("Flickerwisp", || {
        let mut game = base().hand(ME, "Flickerwisp").build();
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Flickerwisp", &[]);
        choose(&mut game, ME, Target::Object(bear));
        assert_eq!(game.objects[bear].zone, Zone::Exile);
        advance_until(&mut game, |g| g.turn == 2).unwrap();
        assert_eq!(game.objects[bear].zone, Zone::Battlefield, "back at the end step");
        assert_eq!(game.objects[bear].controller, OPP, "under its owner's control");
        assert!(game.players[1].battlefield.contains(&bear));
    });
    check!("Tandem Tactics", || {
        let mut game = base()
            .battlefield(ME, "Grizzly Bears")
            .battlefield(ME, "Hill Giant")
            .hand(ME, "Tandem Tactics")
            .build();
        let bear = bf(&game, ME, "Grizzly Bears");
        let giant = bf(&game, ME, "Hill Giant");
        cast_steps(&mut game, "Tandem Tactics", &[], &[&[Target::Object(bear), Target::Object(giant)]]);
        assert_eq!(stats(&game, bear), (3, 4));
        assert_eq!(stats(&game, giant), (4, 5));
        assert_eq!(life(&game, ME), 22);
        // Zero targets is a legal cast too, and the life still comes.
        let mut game = base().hand(ME, "Tandem Tactics").build();
        cast_steps(&mut game, "Tandem Tactics", &[], &[&[]]);
        assert_eq!(life(&game, ME), 22);
    });
    check!("Selesnya Charm", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Selesnya Charm").build();
        let bear = bf(&game, ME, "Grizzly Bears");
        cast_steps(&mut game, "Selesnya Charm", &[0], &[&[Target::Object(bear)]]);
        assert_eq!(stats(&game, bear), (4, 4));
        assert!(kws(&game, bear).contains(&Keyword::Trample));
        let mut game = base().battlefield(OPP, "Craw Wurm").hand(ME, "Selesnya Charm").build();
        let wurm = bf(&game, OPP, "Craw Wurm");
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast_raw(&mut game, ME, "Selesnya Charm", &[]);
        let mode_texts: Vec<String> = game
            .legal_actions(ME)
            .iter()
            .filter(|a| matches!(a, Action::ChooseMode { .. }))
            .map(|a| engine::text::describe_action(&game, a))
            .collect();
        assert_eq!(mode_texts.len(), 3, "{mode_texts:?}");
        assert!(mode_texts[1].starts_with("Exile target creature with power 5"), "{mode_texts:?}");
        game.apply(ME, &Action::ChooseMode { mode: 1 }).unwrap();
        let picks: Vec<Action> = game
            .legal_actions(ME)
            .into_iter()
            .filter(|a| matches!(a, Action::ChooseTargets { .. }))
            .collect();
        assert_eq!(
            picks,
            vec![Action::ChooseTargets {
                targets: vec![Target::Object(wurm)]
            }],
            "only the 6/4 qualifies"
        );
        game.apply(ME, &picks[0]).unwrap();
        settle(&mut game);
        assert_eq!(game.objects[wurm].zone, Zone::Exile);
        assert_eq!(game.objects[bear].zone, Zone::Battlefield);
        let mut game = base().hand(ME, "Selesnya Charm").build();
        cast_steps(&mut game, "Selesnya Charm", &[2], &[]);
        let knight = bf(&game, ME, "Knight");
        assert_eq!(stats(&game, knight), (2, 2));
        assert!(kws(&game, knight).contains(&Keyword::Vigilance));
    });
    check!("Loam Lion", || {
        let game = base().battlefield(ME, "Loam Lion").build();
        assert_eq!(stats(&game, bf(&game, ME, "Loam Lion")), (2, 3), "four Forests");
        let game = TestGame::new(db(), 2).battlefield(ME, "Loam Lion").build();
        assert_eq!(stats(&game, bf(&game, ME, "Loam Lion")), (1, 1));
    });
    check!("Ballynock Cohort", || {
        let game = base().battlefield(ME, "Ballynock Cohort").build();
        assert_eq!(stats(&game, bf(&game, ME, "Ballynock Cohort")), (2, 2), "alone");
        let game = base().battlefield(ME, "Ballynock Cohort").battlefield(ME, "Savannah Lions").build();
        assert_eq!(stats(&game, bf(&game, ME, "Ballynock Cohort")), (3, 3));
        let game = base().battlefield(ME, "Ballynock Cohort").battlefield(ME, "Grizzly Bears").build();
        assert_eq!(
            stats(&game, bf(&game, ME, "Ballynock Cohort")),
            (2, 2),
            "a green creature doesn't count"
        );
    });
    check!("Serra Ascendant", || {
        let game = base().battlefield(ME, "Serra Ascendant").build();
        let monk = bf(&game, ME, "Serra Ascendant");
        assert_eq!(stats(&game, monk), (1, 1));
        assert!(!kws(&game, monk).contains(&Keyword::Flying));
        let game = base().battlefield(ME, "Serra Ascendant").life(ME, 30).build();
        let monk = bf(&game, ME, "Serra Ascendant");
        assert_eq!(stats(&game, monk), (6, 6));
        assert!(kws(&game, monk).contains(&Keyword::Flying));
        assert!(kws(&game, monk).contains(&Keyword::Lifelink));
    });
    check!("Kor Skyfisher", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Kor Skyfisher").build();
        let bear = bf(&game, ME, "Grizzly Bears");
        cast_raw(&mut game, ME, "Kor Skyfisher", &[]);
        advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::Choose { .. }))).unwrap();
        let options = game
            .legal_actions(ME)
            .iter()
            .filter(|a| matches!(a, Action::ChooseTargets { .. }))
            .count();
        assert_eq!(options, 22, "twenty lands, the bear, and itself");
        pick(&mut game, ME, &[Target::Object(bear)]);
        assert_eq!(game.objects[bear].zone, Zone::Hand);
        let fisher = bf(&game, ME, "Kor Skyfisher");
        assert!(kws(&game, fisher).contains(&Keyword::Flying));
    });
    check!("Whitemane Lion", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Whitemane Lion").build();
        let bear = bf(&game, ME, "Grizzly Bears");
        let lion = hand(&game, ME, "Whitemane Lion");
        cast_raw(&mut game, ME, "Whitemane Lion", &[]);
        advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::Choose { .. }))).unwrap();
        pick(&mut game, ME, &[Target::Object(lion)]);
        assert_eq!(game.objects[lion].zone, Zone::Hand, "it can return itself");
        assert_eq!(game.objects[bear].zone, Zone::Battlefield);
        // Alone, it has to return itself: no question asked.
        let mut game = base().hand(ME, "Whitemane Lion").build();
        let lion = hand(&game, ME, "Whitemane Lion");
        cast(&mut game, "Whitemane Lion", &[]);
        assert_eq!(game.objects[lion].zone, Zone::Hand);
    });
    check!("Angel of the Dawn", || etb("Angel of the Dawn", None, |g, _| {
        let giant = bf(g, ME, "Hill Giant");
        assert_eq!(stats(g, giant), (4, 4));
        assert!(kws(g, giant).contains(&Keyword::Vigilance));
    }));
    check!("Day of Judgment", || destroy_all(
        "Day of Judgment",
        &["Hill Giant"],
        &["Bonesplitter"]
    ));
    check!("Final Judgment", || {
        let mut game = base().battlefield(ME, "Hill Giant").hand(ME, "Final Judgment").build();
        let giant = bf(&game, ME, "Hill Giant");
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Final Judgment", &[]);
        assert_eq!(game.objects[giant].zone, Zone::Exile);
        assert_eq!(game.objects[bear].zone, Zone::Exile);
    });

    // ----- blue -----
    check!("Cancel", || counter("Cancel", false));
    check!("Counterspell", || counter("Counterspell", false));
    check!("Essence Scatter", || counter("Essence Scatter", false));
    check!("Negate", || {
        let game = base().hand(ME, "Negate").build();
        // Nothing to counter: no legal cast (creature spells are not noncreature spells).
        assert!(!game
            .legal_actions(ME)
            .iter()
            .any(|a| matches!(a, Action::CastSpell { object, .. } if *object == hand(&game, ME, "Negate"))));
    });
    check!("Crippling Chill", || {
        let mut game = base().hand(ME, "Crippling Chill").library(ME, &["Forest"; 3]).build();
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Crippling Chill", &[Target::Object(bear)]);
        assert!(game.objects[bear].tapped);
        assert_eq!(hand_size(&game, ME), 1);
        // The opponent's untap step leaves it tapped once; the one after untaps it.
        advance_until(&mut game, |g| g.turn == 2 && g.phase == engine::Phase::Main1).unwrap();
        assert!(game.objects[bear].tapped, "held through the opponent's untap step");
        advance_until(&mut game, |g| g.turn == 4 && g.phase == engine::Phase::Main1).unwrap();
        assert!(!game.objects[bear].tapped);
    });
    check!("Frost Breath", || {
        let mut game = base().battlefield(OPP, "Hill Giant").hand(ME, "Frost Breath").build();
        let bear = bf(&game, OPP, "Grizzly Bears");
        let giant = bf(&game, OPP, "Hill Giant");
        cast_steps(&mut game, "Frost Breath", &[], &[&[Target::Object(bear), Target::Object(giant)]]);
        assert!(game.objects[bear].tapped && game.objects[giant].tapped);
        advance_until(&mut game, |g| g.turn == 2 && g.phase == engine::Phase::Main1).unwrap();
        assert!(game.objects[bear].tapped && game.objects[giant].tapped);
    });
    check!("Divination", || draw_spell("Divination", 2));
    check!("Unsummon", || bounce("Unsummon", "Hill Giant"));
    check!("Man-o'-War", || etb("Man-o'-War", Some("Hill Giant"), |g, _| assert_eq!(
        hand_size(g, OPP),
        1
    )));
    check!("Aether Adept", || etb("Aether Adept", Some("Hill Giant"), |g, _| assert_eq!(
        hand_size(g, OPP),
        1
    )));
    check!("Cloudkin Seer", || etb("Cloudkin Seer", None, |g, _| assert_eq!(
        hand_size(g, ME),
        1
    )));
    check!("Exclude", || counter("Exclude", true));
    check!("Remove Soul", || counter("Remove Soul", false));
    check!("Sift", || {
        let mut game = base().hand(ME, "Sift").library(ME, &["Forest"; 5]).build();
        cast(&mut game, "Sift", &[]);
        assert_eq!(hand_size(&game, ME), 2, "three drawn, one discarded");
        assert_eq!(game.players[0].graveyard.len(), 2, "Sift and the discard");
    });
    check!("Disperse", || bounce("Disperse", "Bonesplitter"));
    check!("Boomerang", || bounce("Boomerang", "Forest"));
    check!("Merfolk Looter", || {
        let mut game = base()
            .battlefield(ME, "Merfolk Looter")
            .library(ME, &["Forest"; 3])
            .hand(ME, "Island")
            .build();
        activate(&mut game, "Merfolk Looter", 0, &[]);
        assert_eq!(hand_size(&game, ME), 1);
        assert_eq!(game.players[0].graveyard.len(), 1);
    });
    check!("Prodigal Sorcerer", || pinger("Prodigal Sorcerer"));
    check!("Inspiration", || {
        let mut game = base().hand(ME, "Inspiration").library(OPP, &["Forest"; 3]).build();
        cast(&mut game, "Inspiration", &[Target::Player(OPP)]);
        assert_eq!(hand_size(&game, OPP), 2);
    });
    check!("Jace's Ingenuity", || draw_spell("Jace's Ingenuity", 3));
    check!("Concentrate", || draw_spell("Concentrate", 3));
    check!("Warden of Evos Isle", || {
        let game = TestGame::new(db(), 2)
            .battlefield(ME, "Warden of Evos Isle")
            .battlefield(ME, "Island")
            .battlefield(ME, "Island")
            .battlefield(ME, "Island")
            .hand(ME, "Wind Drake")
            .hand(ME, "Coral Merfolk")
            .build();
        let drake = hand(&game, ME, "Wind Drake");
        assert_eq!(game.cast_cost(ME, drake).to_string(), "{1}{U}", "flyer costs one less");
        assert_eq!(
            game.cast_cost(ME, hand(&game, ME, "Coral Merfolk")).to_string(),
            "{1}{U}",
            "non-flyer unchanged"
        );
        let mut game = game;
        cast(&mut game, "Wind Drake", &[]);
        assert_eq!(
            game.players[0].battlefield.iter().filter(|&&id| game.objects[id].tapped).count(),
            2,
            "paid two"
        );
    });
    check!("Scroll Thief", || {
        let mut game = base().battlefield(ME, "Scroll Thief").library(ME, &["Forest"; 3]).build();
        let thief = bf(&game, ME, "Scroll Thief");
        attack(&mut game, &[thief]);
        block(&mut game, vec![]);
        end_of_combat(&mut game);
        assert_eq!(life(&game, OPP), 19);
        assert_eq!(hand_size(&game, ME), 1);
    });

    // ----- black -----
    check!("Assassinate", || {
        let mut game = base().battlefield_tapped(OPP, "Hill Giant").hand(ME, "Assassinate").build();
        let giant = bf(&game, OPP, "Hill Giant");
        cast(&mut game, "Assassinate", &[Target::Object(giant)]);
        assert!(in_graveyard(&game, giant));
    });
    check!("Cruel Edict", || {
        let mut game = base().hand(ME, "Cruel Edict").build();
        cast(&mut game, "Cruel Edict", &[Target::Player(OPP)]);
        assert_eq!(game.players[1].battlefield.len(), 0);
    });
    check!("Dark Ritual", || {
        let mut game = TestGame::new(db(), 2).battlefield(ME, "Swamp").hand(ME, "Dark Ritual").build();
        cast(&mut game, "Dark Ritual", &[]);
        assert_eq!(game.players[0].mana_pool.total(), 3);
    });
    check!("Doom Blade", || destroy("Doom Blade", "Hill Giant"));
    check!("Murder", || destroy("Murder", "Hill Giant"));
    check!("Mind Rot", || {
        let mut game = base()
            .hand(ME, "Mind Rot")
            .hand(OPP, "Forest")
            .hand(OPP, "Forest")
            .hand(OPP, "Forest")
            .build();
        cast(&mut game, "Mind Rot", &[Target::Player(OPP)]);
        assert_eq!(hand_size(&game, OPP), 1);
    });
    check!("Festering Goblin", || {
        let mut game = base()
            .battlefield(ME, "Festering Goblin")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .hand(OPP, "Murder")
            .starting_player(OPP)
            .build();
        let goblin = bf(&game, ME, "Festering Goblin");
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast_by(&mut game, OPP, "Murder", &[Target::Object(goblin)]);
        choose(&mut game, ME, Target::Object(bear));
        assert_eq!(stats(&game, bear), (1, 1));
    });
    check!("Liliana's Specter", || etb("Liliana's Specter", None, |g, _| assert_eq!(
        hand_size(g, OPP),
        0
    )));
    check!("Ravenous Rats", || {
        let mut game = base().hand(ME, "Ravenous Rats").hand(OPP, "Forest").build();
        cast(&mut game, "Ravenous Rats", &[]);
        choose(&mut game, ME, Target::Player(OPP));
        assert_eq!(hand_size(&game, OPP), 0);
    });
    check!("Go for the Throat", || {
        destroy("Go for the Throat", "Hill Giant");
        let game = base().battlefield(OPP, "Alpha Myr").hand(ME, "Go for the Throat").build();
        let myr = bf(&game, OPP, "Alpha Myr");
        assert!(
            !game
                .legal_actions(ME)
                .iter()
                .any(|a| matches!(a, Action::CastSpell { targets, .. } if targets.contains(&Target::Object(myr)))),
            "not artifact creatures"
        );
    });
    check!("Last Gasp", || shrink("Last Gasp", 3, 3));
    check!("Disfigure", || shrink("Disfigure", 2, 2));
    check!("Grasp of Darkness", || shrink("Grasp of Darkness", 4, 4));
    check!("Vicious Hunger", || {
        let mut game = base().hand(ME, "Vicious Hunger").build();
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Vicious Hunger", &[Target::Object(bear)]);
        assert!(in_graveyard(&game, bear));
        assert_eq!(life(&game, ME), 22);
    });
    check!("Deathmark", || {
        destroy("Deathmark", "Centaur Courser");
        destroy("Deathmark", "Savannah Lions");
        let game = base().battlefield(OPP, "Hill Giant").hand(ME, "Deathmark").build();
        let giant = bf(&game, OPP, "Hill Giant");
        assert!(
            !game
                .legal_actions(ME)
                .iter()
                .any(|a| matches!(a, Action::CastSpell { targets, .. } if targets.contains(&Target::Object(giant)))),
            "red creatures are safe"
        );
    });
    check!("Cackling Fiend", || etb("Cackling Fiend", None, |g, _| assert_eq!(
        hand_size(g, OPP),
        0
    )));
    check!("Nightwing Shade", || self_pump("Nightwing Shade", 1, 1));
    check!("Looming Shade", || self_pump("Looming Shade", 1, 1));
    check!("Frozen Shade", || self_pump("Frozen Shade", 1, 1));
    check!("Vampire Aristocrat", || sac_pump("Vampire Aristocrat"));
    check!("Bloodthrone Vampire", || sac_pump("Bloodthrone Vampire"));
    check!("Nantuko Husk", || sac_pump("Nantuko Husk"));
    check!("Necrogen Scudder", || etb("Necrogen Scudder", None, |g, _| assert_eq!(
        life(g, ME),
        17
    )));
    check!("Black Cat", || {
        let mut game = base()
            .battlefield(ME, "Black Cat")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .hand(OPP, "Murder")
            .hand(OPP, "Forest")
            .starting_player(OPP)
            .build();
        let cat = bf(&game, ME, "Black Cat");
        cast_by(&mut game, OPP, "Murder", &[Target::Object(cat)]);
        choose(&mut game, ME, Target::Player(OPP));
        assert_eq!(hand_size(&game, OPP), 0, "the Forest is discarded at random");
    });
    check!("Fleshbag Marauder", || {
        let mut game = base()
            .battlefield(ME, "Hill Giant")
            .battlefield(OPP, "Hill Giant")
            .hand(ME, "Fleshbag Marauder")
            .build();
        cast_raw(&mut game, ME, "Fleshbag Marauder", &[]);
        advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::Choose { .. }))).unwrap();
        // The active player chooses first (APNAP), then the opponent.
        assert!(matches!(game.pending, Some(PendingChoice::Choose { seat: ME, .. })));
        let marauder = bf(&game, ME, "Fleshbag Marauder");
        pick(&mut game, ME, &[Target::Object(marauder)]);
        assert!(in_graveyard(&game, marauder));
        assert_eq!(count_bf(&game, ME, "Hill Giant"), 1);
        assert_eq!(game.players[1].battlefield.len(), 1, "the opponent lost one of two");
    });
    check!("Burglar Rat", || {
        let mut game = base().hand(ME, "Burglar Rat").hand(OPP, "Forest").hand(OPP, "Forest").build();
        cast_raw(&mut game, ME, "Burglar Rat", &[]);
        advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::Choose { .. }))).unwrap();
        assert_eq!(game.must_act().get(&OPP), Some(&engine::ActReason::Discard));
        let forest = game.players[1].hand[0];
        pick(&mut game, OPP, &[Target::Object(forest)]);
        assert_eq!(hand_size(&game, OPP), 1);
        assert!(in_graveyard(&game, forest));
    });
    check!("Liliana's Caress", || {
        let mut game = base()
            .battlefield(ME, "Liliana's Caress")
            .hand(ME, "Mind Rot")
            .hand(OPP, "Forest")
            .hand(OPP, "Forest")
            .build();
        cast(&mut game, "Mind Rot", &[Target::Player(OPP)]);
        assert_eq!(hand_size(&game, OPP), 0);
        assert_eq!(life(&game, OPP), 16, "2 life per card discarded");
    });
    check!("Vampire Envoy", || {
        let mut game = base().battlefield(ME, "Vampire Envoy").build();
        let envoy = bf(&game, ME, "Vampire Envoy");
        attack(&mut game, &[envoy]);
        settle(&mut game);
        assert_eq!(life(&game, ME), 21, "attacking taps it");
    });

    // ----- red -----
    check!("Lightning Bolt", || burn("Lightning Bolt", 3, true, true));
    check!("Shock", || burn("Shock", 2, true, true));
    check!("Lightning Strike", || burn("Lightning Strike", 3, true, true));
    check!("Volcanic Hammer", || burn("Volcanic Hammer", 3, true, true));
    check!("Flame Slash", || burn("Flame Slash", 4, true, false));
    check!("Searing Spear", || burn("Searing Spear", 3, true, true));
    check!("Fire Ambush", || burn("Fire Ambush", 3, true, true));
    check!("Trumpet Blast", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Trumpet Blast").build();
        let bear = bf(&game, ME, "Grizzly Bears");
        attack(&mut game, &[bear]);
        settle(&mut game);
        cast(&mut game, "Trumpet Blast", &[]);
        assert_eq!(stats(&game, bear), (4, 2));
        assert_eq!(stats(&game, bf(&game, OPP, "Grizzly Bears")), (2, 2), "only attackers");
    });
    check!("Kiln Fiend", || {
        let mut game = base()
            .battlefield(ME, "Kiln Fiend")
            .hand(ME, "Shock")
            .hand(ME, "Grizzly Bears")
            .build();
        let fiend = bf(&game, ME, "Kiln Fiend");
        cast(&mut game, "Grizzly Bears", &[]);
        assert_eq!(stats(&game, fiend), (1, 2), "creature spells don't count");
        cast(&mut game, "Shock", &[Target::Player(OPP)]);
        assert_eq!(stats(&game, fiend), (4, 2));
    });
    check!("Falter", || {
        let mut game = base()
            .battlefield(ME, "Grizzly Bears")
            .battlefield(OPP, "Wind Drake")
            .hand(ME, "Falter")
            .build();
        let bear = bf(&game, ME, "Grizzly Bears");
        let drake = bf(&game, OPP, "Wind Drake");
        cast(&mut game, "Falter", &[]);
        attack(&mut game, &[bear]);
        advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::DeclareBlockers { .. }))).unwrap();
        assert_eq!(game.block_candidates(OPP), vec![drake], "only the flyer may block");
    });
    check!("Dual Shot", || {
        let mut game = base().battlefield(OPP, "Hill Giant").hand(ME, "Dual Shot").build();
        let bear = bf(&game, OPP, "Grizzly Bears");
        let giant = bf(&game, OPP, "Hill Giant");
        cast_raw(&mut game, ME, "Dual Shot", &[]);
        let picks = game
            .legal_actions(ME)
            .iter()
            .filter(|a| matches!(a, Action::ChooseTargets { .. }))
            .count();
        assert_eq!(picks, 1 + 2 + 1, "none, each, both");
        game.apply(
            ME,
            &Action::ChooseTargets {
                targets: vec![Target::Object(bear), Target::Object(giant)],
            },
        )
        .unwrap();
        settle(&mut game);
        assert_eq!(game.objects[bear].damage, 1);
        assert_eq!(game.objects[giant].damage, 1);
    });
    check!("Kolaghan's Command", || {
        let mut game = base()
            .graveyard(ME, "Hill Giant")
            .battlefield(OPP, "Bonesplitter")
            .hand(OPP, "Forest")
            .hand(ME, "Kolaghan's Command")
            .build();
        let axe = bf(&game, OPP, "Bonesplitter");
        let bear = bf(&game, OPP, "Grizzly Bears");
        // Destroy the axe and shock the bear: two modes, each with its own target.
        cast_steps(
            &mut game,
            "Kolaghan's Command",
            &[2, 3],
            &[&[Target::Object(axe)], &[Target::Object(bear)]],
        );
        assert!(in_graveyard(&game, axe));
        assert!(in_graveyard(&game, bear));
        assert_eq!(hand_size(&game, OPP), 1, "the discard mode was not chosen");
        // Discard and regrow, in that order.
        let mut game = base()
            .graveyard(ME, "Hill Giant")
            .hand(OPP, "Forest")
            .hand(ME, "Kolaghan's Command")
            .build();
        let giant = gy(&game, ME, "Hill Giant");
        cast_steps(
            &mut game,
            "Kolaghan's Command",
            &[0, 1],
            &[&[Target::Player(OPP)], &[Target::Object(giant)]],
        );
        assert_eq!(hand_size(&game, OPP), 0);
        assert_eq!(game.objects[giant].zone, Zone::Hand);
    });
    check!("Kird Ape", || {
        let game = base().battlefield(ME, "Kird Ape").build();
        assert_eq!(stats(&game, bf(&game, ME, "Kird Ape")), (2, 3));
        let mut game = TestGame::new(db(), 2).battlefield(ME, "Kird Ape").hand(ME, "Forest").build();
        let ape = bf(&game, ME, "Kird Ape");
        assert_eq!(stats(&game, ape), (1, 1));
        let forest = hand(&game, ME, "Forest");
        game.apply(ME, &Action::PlayLand { object: forest }).unwrap();
        assert_eq!(stats(&game, ape), (2, 3), "the condition is checked live");
    });
    check!("Prodigal Pyromancer", || pinger("Prodigal Pyromancer"));
    check!("Goblin Chieftain", || {
        let game = base().battlefield(ME, "Goblin Chieftain").battlefield(ME, "Goblin Piker").build();
        let piker = bf(&game, ME, "Goblin Piker");
        assert_eq!(stats(&game, piker), (3, 2));
        assert!(kws(&game, piker).contains(&Keyword::Haste));
        assert_eq!(stats(&game, bf(&game, ME, "Goblin Chieftain")), (2, 2));
    });
    check!("Furnace Whelp", || self_pump("Furnace Whelp", 1, 0));
    check!("Fiery Hellhound", || self_pump("Fiery Hellhound", 1, 0));
    check!("Shivan Dragon", || self_pump("Shivan Dragon", 1, 0));
    check!("Dragon Hatchling", || self_pump("Dragon Hatchling", 1, 0));
    check!("Wall of Fire", || self_pump("Wall of Fire", 1, 0));
    check!("Krenko's Command", || tokens_spell("Krenko's Command", 2, "Goblin"));
    check!("Dragon Fodder", || tokens_spell("Dragon Fodder", 2, "Goblin"));
    check!("Hordeling Outburst", || tokens_spell("Hordeling Outburst", 3, "Goblin"));
    check!("Goblin Instigator", || etb("Goblin Instigator", None, |g, _| assert_eq!(
        count_bf(g, ME, "Goblin"),
        1
    )));
    check!("Mogg Fanatic", || {
        let mut game = base().battlefield(ME, "Mogg Fanatic").build();
        let fanatic = bf(&game, ME, "Mogg Fanatic");
        activate(&mut game, "Mogg Fanatic", 0, &[Target::Player(OPP)]);
        assert_eq!(life(&game, OPP), 19);
        assert!(in_graveyard(&game, fanatic));
    });
    check!("Firebreathing", || aura_ability("Firebreathing", |g, bear| assert_eq!(
        stats(g, bear),
        (3, 2)
    )));
    check!("Brute Force", || pump("Brute Force", 3, 3, &[]));
    check!("Pyroclasm", || {
        let mut game = base()
            .battlefield(ME, "Hill Giant")
            .battlefield(ME, "Grizzly Bears")
            .hand(ME, "Pyroclasm")
            .build();
        let giant = bf(&game, ME, "Hill Giant");
        let mine = bf(&game, ME, "Grizzly Bears");
        let theirs = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Pyroclasm", &[]);
        assert!(in_graveyard(&game, mine) && in_graveyard(&game, theirs));
        assert_eq!(game.objects[giant].damage, 2);
    });
    check!("Vulshok Sorcerer", || {
        let mut game = base().hand(ME, "Vulshok Sorcerer").build();
        cast(&mut game, "Vulshok Sorcerer", &[]);
        activate(&mut game, "Vulshok Sorcerer", 0, &[Target::Player(OPP)]);
        assert_eq!(life(&game, OPP), 19, "haste lets it tap the turn it arrives");
    });
    check!("Goblin Balloon Brigade", || {
        let mut game = base().battlefield(ME, "Goblin Balloon Brigade").build();
        let id = bf(&game, ME, "Goblin Balloon Brigade");
        activate(&mut game, "Goblin Balloon Brigade", 0, &[]);
        assert!(kws(&game, id).contains(&Keyword::Flying));
    });
    check!("Torch Fiend", || {
        let mut game = base().battlefield(ME, "Torch Fiend").battlefield(OPP, "Bonesplitter").build();
        let axe = bf(&game, OPP, "Bonesplitter");
        let fiend = bf(&game, ME, "Torch Fiend");
        activate(&mut game, "Torch Fiend", 0, &[Target::Object(axe)]);
        assert!(in_graveyard(&game, axe) && in_graveyard(&game, fiend));
    });
    check!("Shatter", || destroy("Shatter", "Bonesplitter"));
    check!("Demolish", || destroy("Demolish", "Forest"));
    check!("Stone Rain", || destroy("Stone Rain", "Forest"));
    check!("Uncaged Fury", || pump("Uncaged Fury", 1, 1, &[Keyword::DoubleStrike]));
    check!("Fervor", || {
        let mut game = base().battlefield(ME, "Fervor").hand(ME, "Grizzly Bears").build();
        cast(&mut game, "Grizzly Bears", &[]);
        let bear = bf(&game, ME, "Grizzly Bears");
        assert!(kws(&game, bear).contains(&Keyword::Haste));
        attack(&mut game, &[bear]);
        assert!(game.objects[bear].attacking.is_some());
    });
    check!("Sparkmage Apprentice", || etb(
        "Sparkmage Apprentice",
        Some("Hill Giant"),
        |g, _| assert_eq!(g.objects[bf(g, OPP, "Hill Giant")].damage, 1)
    ));
    check!("Bogardan Firefiend", || {
        let mut game = base()
            .battlefield(ME, "Bogardan Firefiend")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .hand(OPP, "Murder")
            .starting_player(OPP)
            .build();
        let fiend = bf(&game, ME, "Bogardan Firefiend");
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast_by(&mut game, OPP, "Murder", &[Target::Object(fiend)]);
        choose(&mut game, ME, Target::Object(bear));
        assert!(in_graveyard(&game, bear), "2 damage from the graveyard");
    });
    check!("Flametongue Kavu", || etb("Flametongue Kavu", Some("Hill Giant"), |g, _| assert!(
        g.players[1].graveyard.len() == 1
    )));
    check!("Crimson Mage", || {
        let mut game = base().battlefield(ME, "Crimson Mage").hand(ME, "Grizzly Bears").build();
        cast(&mut game, "Grizzly Bears", &[]);
        let bear = bf(&game, ME, "Grizzly Bears");
        activate(&mut game, "Crimson Mage", 0, &[Target::Object(bear)]);
        assert!(kws(&game, bear).contains(&Keyword::Haste));
    });
    check!("Goblin Bombardment", || {
        let mut game = base().battlefield(ME, "Goblin Bombardment").battlefield(ME, "Goblin Piker").build();
        let piker = bf(&game, ME, "Goblin Piker");
        let a = game
            .legal_actions(ME)
            .into_iter()
            .find(|a| matches!(a, Action::ActivateAbility { targets, payment, .. } if targets == &[Target::Player(OPP)] && payment.sacrifice.contains(&piker)))
            .expect("sacrifice the piker at the opponent");
        game.apply(ME, &a).unwrap();
        settle(&mut game);
        assert_eq!(life(&game, OPP), 19);
        assert!(in_graveyard(&game, piker));
    });
    check!("Goblin Warchief", || {
        let game = TestGame::new(db(), 2)
            .battlefield(ME, "Goblin Warchief")
            .battlefield(ME, "Mountain")
            .hand(ME, "Goblin Piker")
            .hand(ME, "Goblin Chieftain")
            .build();
        assert_eq!(game.cast_cost(ME, hand(&game, ME, "Goblin Piker")).to_string(), "{R}");
        assert_eq!(game.cast_cost(ME, hand(&game, ME, "Goblin Chieftain")).to_string(), "{R}{R}");
        let mut game = game;
        cast(&mut game, "Goblin Piker", &[]);
        let piker = bf(&game, ME, "Goblin Piker");
        assert!(kws(&game, piker).contains(&Keyword::Haste));
    });

    // ----- green -----
    check!("Wild Nacatl", || {
        let game = base().battlefield(ME, "Wild Nacatl").build();
        assert_eq!(stats(&game, bf(&game, ME, "Wild Nacatl")), (3, 3), "a Mountain and a Plains");
        let game = TestGame::new(db(), 2)
            .battlefield(ME, "Wild Nacatl")
            .battlefield(ME, "Mountain")
            .build();
        assert_eq!(stats(&game, bf(&game, ME, "Wild Nacatl")), (2, 2));
    });
    check!("Giant Growth", || pump("Giant Growth", 3, 3, &[]));
    check!("Titanic Growth", || pump("Titanic Growth", 4, 4, &[]));
    check!("Naturalize", || destroy("Naturalize", "Bonesplitter"));
    check!("Plummet", || destroy("Plummet", "Wind Drake"));
    check!("Overrun", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Overrun").build();
        cast(&mut game, "Overrun", &[]);
        let bear = bf(&game, ME, "Grizzly Bears");
        assert_eq!(stats(&game, bear), (5, 5));
        assert!(kws(&game, bear).contains(&Keyword::Trample));
    });
    check!("Elvish Visionary", || etb("Elvish Visionary", None, |g, _| assert_eq!(
        hand_size(g, ME),
        1
    )));
    check!("Elvish Archdruid", || {
        let game = base().battlefield(ME, "Elvish Archdruid").battlefield(ME, "Llanowar Elves").build();
        assert_eq!(stats(&game, bf(&game, ME, "Llanowar Elves")), (2, 2));
        let druid = bf(&game, ME, "Elvish Archdruid");
        assert!(
            game.mana_sources_with_amounts(ME)
                .iter()
                .any(|(s, c, n)| *s == druid && *c == engine::Mana::Colored(Color::Green) && *n == 2),
            "two Elves, two mana"
        );
    });
    check!("Harmonize", || draw_spell("Harmonize", 3));
    check!("Might of Oaks", || pump("Might of Oaks", 7, 7, &[]));
    check!("Ranger's Guile", || pump("Ranger's Guile", 1, 1, &[Keyword::Hexproof]));
    check!("Predator's Strike", || pump("Predator's Strike", 3, 3, &[Keyword::Trample]));
    check!("Larger Than Life", || pump("Larger Than Life", 4, 4, &[Keyword::Trample]));
    check!("Serpent's Gift", || {
        let mut game = base().battlefield(ME, "Grizzly Bears").hand(ME, "Serpent's Gift").build();
        let bear = bf(&game, ME, "Grizzly Bears");
        cast(&mut game, "Serpent's Gift", &[Target::Object(bear)]);
        assert!(kws(&game, bear).contains(&Keyword::Deathtouch));
    });
    check!("Tranquility", || {
        let mut game = base()
            .battlefield(ME, "Fervor")
            .battlefield(OPP, "Goblin Bombardment")
            .hand(ME, "Tranquility")
            .build();
        let mine = bf(&game, ME, "Fervor");
        let theirs = bf(&game, OPP, "Goblin Bombardment");
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Tranquility", &[]);
        assert!(in_graveyard(&game, mine) && in_graveyard(&game, theirs));
        assert_eq!(game.objects[bear].zone, Zone::Battlefield);
    });
    check!("Creeping Corrosion", || {
        let mut game = base()
            .battlefield(ME, "Bonesplitter")
            .battlefield(OPP, "Alpha Myr")
            .hand(ME, "Creeping Corrosion")
            .build();
        let axe = bf(&game, ME, "Bonesplitter");
        let myr = bf(&game, OPP, "Alpha Myr");
        let bear = bf(&game, OPP, "Grizzly Bears");
        cast(&mut game, "Creeping Corrosion", &[]);
        assert!(in_graveyard(&game, axe) && in_graveyard(&game, myr));
        assert_eq!(game.objects[bear].zone, Zone::Battlefield);
    });
    check!("Imperious Perfect", || {
        let mut game = base()
            .battlefield(ME, "Imperious Perfect")
            .battlefield(ME, "Llanowar Elves")
            .build();
        assert_eq!(stats(&game, bf(&game, ME, "Llanowar Elves")), (2, 2));
        assert_eq!(stats(&game, bf(&game, ME, "Imperious Perfect")), (2, 2));
        activate(&mut game, "Imperious Perfect", 0, &[]);
        let token = bf(&game, ME, "Elf Warrior");
        assert_eq!(stats(&game, token), (2, 2), "the token is an Elf too");
    });
    check!("Elvish Herder", || {
        let mut game = base().battlefield(ME, "Elvish Herder").battlefield(ME, "Grizzly Bears").build();
        let bear = bf(&game, ME, "Grizzly Bears");
        activate(&mut game, "Elvish Herder", 0, &[Target::Object(bear)]);
        assert!(kws(&game, bear).contains(&Keyword::Trample));
    });
    check!("Wall of Blossoms", || etb("Wall of Blossoms", None, |g, _| assert_eq!(
        hand_size(g, ME),
        1
    )));
    check!("Carven Caryatid", || etb("Carven Caryatid", None, |g, _| assert_eq!(
        hand_size(g, ME),
        1
    )));
    check!("Bramble Creeper", || {
        let mut game = base().battlefield(ME, "Bramble Creeper").build();
        let c = bf(&game, ME, "Bramble Creeper");
        attack(&mut game, &[c]);
        settle(&mut game);
        assert_eq!(stats(&game, c), (5, 3));
    });
    check!("Pelakka Wurm", || {
        etb("Pelakka Wurm", None, |g, _| assert_eq!(life(g, ME), 27));
        dies("Pelakka Wurm", None, |g| assert_eq!(hand_size(g, ME), 1));
    });
    check!("Kavu Climber", || etb("Kavu Climber", None, |g, _| assert_eq!(hand_size(g, ME), 1)));
    check!("Bramblecrush", || destroy("Bramblecrush", "Forest"));
    check!("Briarpack Alpha", || etb("Briarpack Alpha", Some("Hill Giant"), |g, _| assert_eq!(
        stats(g, bf(g, OPP, "Hill Giant")),
        (5, 5)
    )));
    check!("Yeva's Forcemage", || etb(
        "Yeva's Forcemage",
        Some("Hill Giant"),
        |g, _| assert_eq!(stats(g, bf(g, OPP, "Hill Giant")), (5, 5))
    ));

    // ----- graveyard -----
    check!("Raise Dead", || regrow("Raise Dead", "Grizzly Bears"));
    check!("Disentomb", || regrow("Disentomb", "Grizzly Bears"));
    check!("Wildwood Rebirth", || regrow("Wildwood Rebirth", "Grizzly Bears"));
    check!("Regrowth", || regrow("Regrowth", "Giant Growth"));
    check!("Nature's Spiral", || {
        regrow("Nature's Spiral", "Bonesplitter");
        let game = base().graveyard(ME, "Shock").hand(ME, "Nature's Spiral").build();
        assert!(
            !game
                .legal_actions(ME)
                .iter()
                .any(|a| matches!(a, Action::CastSpell { targets, .. } if !targets.is_empty())),
            "an instant is not a permanent card"
        );
    });
    check!("Zombify", || {
        let mut game = base().graveyard(ME, "Hill Giant").hand(ME, "Zombify").build();
        let giant = gy(&game, ME, "Hill Giant");
        cast(&mut game, "Zombify", &[Target::Object(giant)]);
        assert_eq!(game.objects[giant].zone, Zone::Battlefield);
        assert_eq!(game.objects[giant].controller, ME);
        assert!(game.objects[giant].summoning_sick);
    });
    check!("Tome Scour", || mill("Tome Scour", OPP, 5));
    check!("Mind Sculpt", || mill("Mind Sculpt", OPP, 7));
    check!("Thought Scour", || {
        let mut game = base().hand(ME, "Thought Scour").library(ME, &["Forest"; 6]).build();
        cast(&mut game, "Thought Scour", &[Target::Player(ME)]);
        assert_eq!(game.players[0].graveyard.len(), 3, "two milled plus the spell");
        assert_eq!(hand_size(&game, ME), 1);
        assert_eq!(game.players[0].library.len(), 3);
    });
    check!("Mental Note", || {
        let mut game = base().hand(ME, "Mental Note").library(ME, &["Forest"; 6]).build();
        cast(&mut game, "Mental Note", &[]);
        assert_eq!(game.players[0].graveyard.len(), 3);
        assert_eq!(hand_size(&game, ME), 1);
    });
    check!("Stitcher's Supplier", || {
        etb("Stitcher's Supplier", None, |g, _| assert_eq!(g.players[0].graveyard.len(), 3));
        dies("Stitcher's Supplier", None, |g| {
            assert_eq!(g.players[0].graveyard.len(), 4, "three milled plus itself")
        });
    });
    check!("Crow of Dark Tidings", || {
        etb("Crow of Dark Tidings", None, |g, _| assert_eq!(g.players[0].graveyard.len(), 2));
        dies("Crow of Dark Tidings", None, |g| assert_eq!(g.players[0].graveyard.len(), 3));
    });
    check!("Doomed Dissenter", || dies("Doomed Dissenter", None, |g| assert_eq!(
        stats(g, bf(g, ME, "Zombie")),
        (2, 2)
    )));
    check!("Doomed Traveler", || dies("Doomed Traveler", None, |g| {
        let spirit = bf(g, ME, "Spirit");
        assert_eq!(stats(g, spirit), (1, 1));
        assert!(kws(g, spirit).contains(&Keyword::Flying));
    }));
    check!("Gravedigger", || {
        let mut game = base().graveyard(ME, "Hill Giant").hand(ME, "Gravedigger").build();
        let giant = gy(&game, ME, "Hill Giant");
        cast(&mut game, "Gravedigger", &[]);
        choose(&mut game, ME, Target::Object(giant)); // settle takes "do it"
        assert_eq!(game.objects[giant].zone, Zone::Hand);
        // Declining leaves the card where it is.
        let mut game = base().graveyard(ME, "Hill Giant").hand(ME, "Gravedigger").build();
        let giant = gy(&game, ME, "Hill Giant");
        cast(&mut game, "Gravedigger", &[]);
        let a = game
            .legal_actions(ME)
            .into_iter()
            .find(|a| matches!(a, Action::ChooseTargets { .. }))
            .unwrap();
        game.apply(ME, &a).unwrap();
        advance_until(&mut game, |g| matches!(g.pending, Some(PendingChoice::ChooseOption { .. }))).unwrap();
        game.apply(ME, &Action::ChooseMode { mode: 1 }).unwrap();
        assert!(in_graveyard(&game, giant));
    });
    check!("Lord of the Undead", || {
        let mut game = base()
            .battlefield(ME, "Lord of the Undead")
            .battlefield(ME, "Walking Corpse")
            .battlefield(OPP, "Walking Corpse")
            .graveyard(ME, "Scathe Zombies")
            .build();
        assert_eq!(stats(&game, bf(&game, ME, "Walking Corpse")), (3, 3));
        assert_eq!(
            stats(&game, bf(&game, OPP, "Walking Corpse")),
            (3, 3),
            "every other Zombie, not just yours"
        );
        assert_eq!(stats(&game, bf(&game, ME, "Lord of the Undead")), (2, 2));
        let zombies = gy(&game, ME, "Scathe Zombies");
        activate(&mut game, "Lord of the Undead", 0, &[Target::Object(zombies)]);
        assert_eq!(game.objects[zombies].zone, Zone::Hand);
    });
    check!("Diregraf Captain", || {
        let mut game = base()
            .battlefield(ME, "Diregraf Captain")
            .battlefield(ME, "Walking Corpse")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .hand(OPP, "Murder")
            .starting_player(OPP)
            .build();
        let corpse = bf(&game, ME, "Walking Corpse");
        assert_eq!(stats(&game, corpse), (3, 3));
        cast_by(&mut game, OPP, "Murder", &[Target::Object(corpse)]);
        choose(&mut game, ME, Target::Player(OPP));
        assert_eq!(life(&game, OPP), 19);
    });
    check!("Vindictive Vampire", || {
        let mut game = base()
            .battlefield(ME, "Vindictive Vampire")
            .battlefield(ME, "Grizzly Bears")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .battlefield(OPP, "Swamp")
            .hand(OPP, "Murder")
            .starting_player(OPP)
            .build();
        let bear = bf(&game, ME, "Grizzly Bears");
        cast_by(&mut game, OPP, "Murder", &[Target::Object(bear)]);
        assert_eq!(life(&game, OPP), 19);
        assert_eq!(life(&game, ME), 21);
    });
    check!("Blood Bairn", || sac_pump("Blood Bairn"));
    check!("Sanitarium Skeleton", || {
        let mut game = base().graveyard(ME, "Sanitarium Skeleton").build();
        let skeleton = gy(&game, ME, "Sanitarium Skeleton");
        let a = game
            .legal_actions(ME)
            .into_iter()
            .find(|a| matches!(a, Action::ActivateAbility { object, .. } if *object == skeleton))
            .expect("activated from the graveyard");
        game.apply(ME, &a).unwrap();
        settle(&mut game);
        assert_eq!(game.objects[skeleton].zone, Zone::Hand);
        cast(&mut game, "Sanitarium Skeleton", &[]);
        assert_eq!(game.objects[skeleton].zone, Zone::Battlefield);
        assert!(
            !game
                .legal_actions(ME)
                .iter()
                .any(|a| matches!(a, Action::ActivateAbility { object, .. } if *object == skeleton)),
            "not from the battlefield"
        );
    });

    // ----- artifacts -----
    check!("Mind Stone", || {
        let mut game = TestGame::new(db(), 2)
            .battlefield(ME, "Mind Stone")
            .battlefield(ME, "Forest")
            .library(ME, &["Forest"; 3])
            .build();
        let stone = bf(&game, ME, "Mind Stone");
        assert!(
            game.mana_sources_with_amounts(ME).iter().any(|(s, _, _)| *s == stone),
            "taps for colourless"
        );
        activate(&mut game, "Mind Stone", 1, &[]);
        assert!(in_graveyard(&game, stone));
        assert_eq!(hand_size(&game, ME), 1);
    });
    check!("Fountain of Youth", || {
        let mut game = base().battlefield(ME, "Fountain of Youth").build();
        activate(&mut game, "Fountain of Youth", 0, &[]);
        assert_eq!(life(&game, ME), 21);
    });
    check!("Rod of Ruin", || {
        let mut game = base().battlefield(ME, "Rod of Ruin").build();
        activate(&mut game, "Rod of Ruin", 0, &[Target::Player(OPP)]);
        assert_eq!(life(&game, OPP), 19);
    });
    check!("Bottle Gnomes", || {
        let mut game = base().battlefield(ME, "Bottle Gnomes").build();
        let gnomes = bf(&game, ME, "Bottle Gnomes");
        activate(&mut game, "Bottle Gnomes", 0, &[]);
        assert_eq!(life(&game, ME), 23);
        assert!(in_graveyard(&game, gnomes));
    });
    check!("Perilous Myr", || dies("Perilous Myr", Some(Target::Player(OPP)), |g| assert_eq!(
        life(g, OPP),
        18
    )));
    check!("Myr Sire", || dies("Myr Sire", None, |g| assert_eq!(
        count_bf(g, ME, "Phyrexian Myr"),
        1
    )));
    check!("Skyscanner", || etb("Skyscanner", None, |g, _| assert_eq!(hand_size(g, ME), 1)));

    r
}

#[test]
fn every_core_card_has_a_behaviour_check() {
    let db = db();
    let r = registry();
    let missing: Vec<&str> = db.iter().map(|(_, c)| c.name.as_str()).filter(|n| !r.contains_key(n)).collect();
    assert!(missing.is_empty(), "cards without a behaviour check: {missing:?}");
    let extra: Vec<&&str> = r.keys().filter(|n| db.lookup(n).is_none()).collect();
    assert!(extra.is_empty(), "checks for cards that do not exist: {extra:?}");
}

#[test]
fn every_behaviour_check_passes() {
    let r = registry();
    let mut failures = Vec::new();
    for (name, check) in &r {
        if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(check)) {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            failures.push(format!("{name}: {msg}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} card check(s) failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
