//! Analysis: pure functions over a resolved deck and the card database.

use cardir::{CardType, Color, Effect, Ref};
use engine::{CardDb, CardId, Format, Game, GameConfig, PlayerSetup};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub cards: usize,
    pub lands: usize,
    pub creatures: usize,
    pub noncreature_spells: usize,
    /// Mana value → (creatures, other nonland cards).
    pub curve: BTreeMap<u32, (usize, usize)>,
    pub average_mv: f64,
    pub median_mv: f64,
    /// Colour → pips across nonland cards.
    pub pips: BTreeMap<Color, usize>,
    /// Colour → permanents that can produce it (lands and mana creatures).
    pub sources: BTreeMap<Color, usize>,
    /// Card type word → count.
    pub types: BTreeMap<String, usize>,
    /// Cards that destroy, damage, exile, bounce, or counter (a heuristic over the IR).
    pub interaction: usize,
}

impl Stats {
    pub fn compute(deck: &[CardId], db: &CardDb) -> Stats {
        let mut s = Stats {
            cards: deck.len(),
            ..Default::default()
        };
        let mut mvs: Vec<u32> = Vec::new();
        for &id in deck {
            let c = db.get(id);
            for t in &c.types {
                *s.types.entry(t.word().to_string()).or_default() += 1;
            }
            if c.is_land() {
                s.lands += 1;
            } else {
                let mv = c.cost.mana_value();
                mvs.push(mv);
                let slot = s.curve.entry(mv).or_default();
                if c.is_creature() {
                    s.creatures += 1;
                    slot.0 += 1;
                } else {
                    s.noncreature_spells += 1;
                    slot.1 += 1;
                }
                for &p in &c.cost.pips {
                    *s.pips.entry(p).or_default() += 1;
                }
            }
            for color in c.produces() {
                *s.sources.entry(color).or_default() += 1;
            }
            if is_interaction(&c.ir) {
                s.interaction += 1;
            }
        }
        if !mvs.is_empty() {
            s.average_mv = mvs.iter().sum::<u32>() as f64 / mvs.len() as f64;
            mvs.sort();
            let n = mvs.len();
            s.median_mv = if n % 2 == 1 {
                mvs[n / 2] as f64
            } else {
                (mvs[n / 2 - 1] + mvs[n / 2]) as f64 / 2.0
            };
        }
        s
    }

    /// Sources a card with `pips` pips of one colour wants by turn 4 in a
    /// deck of this size, from the usual hypergeometric target (90% to have
    /// them in the first 10 cards on the play).
    pub fn wanted_sources(&self, pips: usize) -> usize {
        let deck = self.cards.max(1);
        (1..=deck)
            .find(|&sources| hypergeometric_at_least(deck, sources, 10, pips) >= 0.9)
            .unwrap_or(deck)
    }
}

fn is_interaction(card: &cardir::Card) -> bool {
    fn effects_of(card: &cardir::Card) -> Vec<&Effect> {
        let mut out = Vec::new();
        if let Some(sp) = &card.spell {
            out.extend(sp.effects.iter());
        }
        for a in &card.activated {
            out.extend(a.effects.iter());
        }
        for t in &card.triggers {
            out.extend(t.effects.iter());
        }
        out
    }
    fn targets_object(r: &Ref) -> bool {
        !matches!(r, Ref::Player(_))
    }
    effects_of(card).into_iter().any(|e| match e {
        Effect::Destroy { target } | Effect::Exile { target } | Effect::ReturnToHand { target } | Effect::CounterSpell { target } => {
            targets_object(target)
        }
        Effect::DealDamage { to, .. } => targets_object(to) || matches!(to, Ref::Target(_)),
        Effect::Sacrifice { .. } => true,
        Effect::ModifyPt {
            power: cardir::Amount::Const(p),
            ..
        } => *p < 0,
        _ => false,
    })
}

/// P(at least `k` successes) drawing `draws` from `n` with `successes` of them.
pub fn hypergeometric_at_least(n: usize, successes: usize, draws: usize, k: usize) -> f64 {
    let draws = draws.min(n);
    let mut p = 0.0;
    for i in k..=draws.min(successes) {
        p += choose(successes, i) * choose(n - successes, draws - i) / choose(n, draws);
    }
    p
}

fn choose(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    let mut r = 1.0;
    for i in 0..k {
        r *= (n - i) as f64 / (i + 1) as f64;
    }
    r
}

/// Opening hands as the engine would deal them under this format's rules.
/// Each is the list of card names, sorted for readability.
pub fn sample_hands(deck: &[CardId], db: &Arc<CardDb>, format: &Format, count: usize, seed: u64) -> Vec<Vec<String>> {
    let mut hands = Vec::new();
    let mut f = format.clone();
    f.players.min = 2;
    f.players.max = f.players.max.max(2);
    f.deck = engine::format_deck_any();
    for i in 0..count {
        let players = (0..2)
            .map(|_| PlayerSetup {
                name: "sample".into(),
                deck: deck.to_vec(),
            })
            .collect();
        let config = GameConfig {
            format: f.clone(),
            players,
            cards: db.clone(),
            starting_player: Some(engine::Seat(0)),
        };
        let Ok(game) = Game::new(config, seed.wrapping_add(i as u64)) else {
            break;
        };
        let mut hand: Vec<String> = game.players[0]
            .hand
            .iter()
            .map(|&o| db.get(game.objects[o].card).name.clone())
            .collect();
        hand.sort();
        hands.push(hand);
    }
    hands
}

/// Text rendering for the CLI and the MCP server.
pub fn render(s: &Stats, format_name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} cards: {} lands, {} creatures, {} other spells   ({format_name})\n",
        s.cards, s.lands, s.creatures, s.noncreature_spells
    ));
    out.push_str(&format!(
        "average mana value {:.2}, median {:.1}, {} interaction\n\n",
        s.average_mv, s.median_mv, s.interaction
    ));
    out.push_str("curve   creatures / other\n");
    let max_mv = s.curve.keys().max().copied().unwrap_or(0);
    for mv in 0..=max_mv {
        let (c, o) = s.curve.get(&mv).copied().unwrap_or((0, 0));
        let bar = format!("{}{}", "#".repeat(c), "+".repeat(o));
        out.push_str(&format!("  {mv:>2}    {bar:<24} {c:>2} / {o:<2}\n"));
    }
    if !s.pips.is_empty() {
        out.push_str("\ncolour   pips  sources  wanted (for a double-pip card by turn 4)\n");
        for (color, pips) in &s.pips {
            let sources = s.sources.get(color).copied().unwrap_or(0);
            let wanted = s.wanted_sources(2);
            let flag = if sources < wanted { "  short" } else { "" };
            out.push_str(&format!("  {:<7} {pips:>4}  {sources:>7}  {wanted:>6}{flag}\n", color.word()));
        }
    }
    if s.lands > 0 {
        out.push_str("\nlands in opening 7: ");
        let parts: Vec<String> = (2..=4)
            .map(|k| format!("≥{k}: {:.0}%", 100.0 * hypergeometric_at_least(s.cards, s.lands, 7, k)))
            .collect();
        out.push_str(&parts.join("  "));
        out.push('\n');
    }
    let _ = CardType::Land;
    out
}
