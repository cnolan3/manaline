//! The rules primer resource: a plain-English summary for an agent that has
//! never played, plus the recommended play loop.

pub const RULES_PRIMER: &str = r#"# How to play Magic: The Gathering at this table

You are one seat at a table of two or more players. Everyone starts with a
life total (20 in the starter cube) and a deck of cards. You lose when your
life reaches 0, when you must draw from an empty library, or when you concede.
The last player left wins.

## What you are looking at

`get_game_state` shows every player's life, hand size, library size and
graveyard, the permanents on each battlefield, and your own hand. Objects have
stable ids like `#12`; use them when you refer to a card. Your own section is
always labelled YOU.

## The turn

Each turn belongs to one player (the active player) and runs through phases
in a fixed order: untap, upkeep, draw, first main phase, combat (begin combat,
declare attackers, declare blockers, combat damage, end combat), second main
phase, end step, cleanup. You may play one land per turn, during one of your
main phases. Creature spells are also cast during your main phases when
nothing is on the stack.

## Priority and passing

Within a phase, players take turns holding priority. When you hold priority
you may cast a spell, play a land, or pass. When every player passes in a row
with nothing on the stack, the game moves to the next phase. Passing does not
skip your whole turn: you will get priority again in the next phase.
`wait_for_turn` passes for you whenever passing is your only option, so you
are woken only when there is something you could actually do.

## Mana

Lands tap for mana. To cast a spell you tap lands whose colours match the
cost: `{1}{G}` needs one green mana and one mana of any colour. Tapped lands
untap at the start of your next turn. `get_legal_actions` only lists spells you
can actually pay for, with the payment already worked out, so you never need
to compute mana yourself. It lists up to two payments per colour combination:
lands first, and mana creatures first (a creature that taps for several mana,
like Elvish Archdruid, counts for all of it). If you would rather tap a
different set of sources, pass the action with `payment.tap` edited to any of
your untapped sources that covers the cost; `act` accepts it as long as it pays.

## Combat

During your declare attackers step you choose which untapped creatures attack
and whom they attack (in a pod, each attacker can target a different opponent).
Attacking taps the creature. Creatures that came under your control this turn
cannot attack (summoning sickness) unless they have haste; the state marks
these "sick" or "sick but hasty". The defending player then assigns blockers:
each of their untapped creatures may block one attacker, and several may block
the same attacker. Unblocked attackers deal damage equal to their power to the
player. Blocked attackers and their blockers deal damage to each other at the
same time. A creature with damage at least equal to its toughness dies. Damage
wears off at the end of the turn. If one of your attackers is blocked by more
than one creature, you will be asked how to divide its damage.

## The stack

Spells go on the stack when cast and resolve only after every player passes.
Instants and abilities can be added in response. In the starter cube almost
everything is a creature, so in practice: cast a creature, pass, your opponent
passes, the creature enters the battlefield. It can attack from your next turn.

## Opening hand and mulligans

You draw seven cards. You may keep, or mulligan: shuffle, draw seven again,
and after you finally keep put one card on the bottom of your library for each
mulligan taken. A hand with two to five lands is usually a keep.

## Sound basic strategy

Play a land every turn you can. Spend your mana every turn on the biggest
creatures you can cast. Attack when your attackers survive the likely blocks
or when the opponent has no good blocks; keep creatures home when they would
die for nothing. Block when the block kills an attacker and your blocker
survives, or when you must to stay alive. Count your opponent's life: if your
attackers add up to lethal and they cannot block enough of them, attack with
everything.

## The loop

1. Call `wait_for_turn`. It returns when you have a real decision to make,
   saying why, with the state and the numbered legal actions. Moments where
   you could only pass are passed for you meanwhile. If it times out, the
   opponent is still thinking: call it again at once. The game only ends
   when a reply says `game_over`; until then, keep looping without stopping
   to ask anyone.
2. Read the state. Decide.
3. Call `take_action` with the id of the action you chose and the
   `state_version` of the list it came from. Ids are only meaningful for
   that version: if the game has moved on, the call is refused and you
   fetch a fresh list instead of accidentally doing something else. The
   reply tells you whether you still must act (for example you cast a
   creature and still hold priority) and lists the next legal actions.
4. Repeat step 3 until it is no longer your turn to act, then go back to 1.

## Building a deck

Between games, `manaline mcp --http 127.0.0.1:7454` serves these tools with no
game attached, for deckbuilding.

`search_cards` finds cards with Scryfall-style queries (`t:creature c:g mv<=2`,
`o:"draw a card"`, `kw:flying`), returning only cards this engine can play
unless you ask otherwise. `deck_stats` analyses a decklist you write (one
`N Card Name` per line): curve, colour sources against pips, and legality in
the table's format. `save_deck` writes it to a file the human can play or open
in the deckbuilder; `submit_deck` sends it into a game's lobby. A 40-card deck wants about 17
lands; a 60-card deck about 24.

Use `say` to talk to the other players; it is a friendly table. You may
`concede` at any point if the game is clearly lost, but play it out while
you have outs.
"#;
