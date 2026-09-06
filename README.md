# manaline

Terminal Magic: The Gathering. A rules engine, a text-mode client, and an MCP
server so a human can sit down against an AI agent. The design is in
[`docs/SPEC.md`](docs/SPEC.md).

## Status

**M3 — card IR, spells, abilities, keywords.** Cards are data: each is a
`.ron` file in `crates/cards/data/core` holding a small AST (the card IR)
that the engine interprets, and every committed card must render back to its
Oracle text (a CI test). The engine plays instants and sorceries with targets,
activated and mana abilities, triggered abilities placed in APNAP order,
static lords, auras and equipment, tokens, counters, and the evergreen
keywords: flying, reach, first strike, double strike, deathtouch, lifelink,
trample, vigilance, haste, menace, defender, flash, hexproof, indestructible,
prowess. The core set has 99 cards and the five starter decks use them.

A human plays an AI agent in the terminal:

```
cargo run -- play --deck green --vs claude
```

That starts the game daemon, seats the agent's MCP server on seat 1, opens
the terminal client on seat 0, and shows how to point your agent at the game.
`--vs random` plays the built-in bot; `--vs human` prints a `join` command
for a second terminal.

Not yet: the full ~200-card cube and Scryfall metadata (M4), the ingestion
tool (M5), the deckbuilder (M6).

## Layout

```
crates/cardir   the card IR: schema types, validator, English renderer (§4.1)
crates/engine   pure rules engine: no I/O, no async (§3)
crates/cards    the core card set as IR files, decklists, deck-file parser
crates/protocol the daemon protocol: messages, NDJSON framing, client helpers (§2)
crates/daemon   hosts one game, speaks the protocol over Unix socket and TCP (§5)
crates/tui      the terminal client (§6)
crates/mcp      the MCP server that lets an agent play a seat (§7)
crates/cli      the `manaline` binary
formats/        format definitions as RON (§4.4)
decks/          starter decklists in the standard text format (§4.5)
```

## Try it

```
cargo run -- play --deck green --vs random     # you against the built-in bot
cargo run -- play --deck green --vs claude     # you against an agent (see the hints in the client)
cargo run -- play --deck green --vs human      # prints a join command for another terminal
cargo run -- sim --seats 4 --games 5 --log        # random bots in a pod, every event printed
cargo run -- replay ~/.local/share/manaline/games/<id>.jsonl --log
cargo run -- list formats | decks | cards
```

Advanced pieces `play` is made of: `daemon`, `join`, `tui`, `bot`, `mcp`. Run
any with `--help`.

## Tests

```
cargo test
MANALINE_PROPERTY_SEEDS=40 cargo test --release -p manaline-engine --test property
```

`crates/engine/tests/rules.rs` has one scenario per rule and `tests/cards.rs`
one per card mechanic; `tests/property.rs`
plays random games at 2, 3, and 4 seats, checks invariants after every action
(who must act, zone consistency, life totals, no hidden information in any
view), and replays every finished game from its seed and action log.

## License

GPL-3.0. See [LICENSE](LICENSE).
