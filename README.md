# manaline

Terminal Magic: The Gathering. A rules engine, a text-mode client, and an MCP
server so a human can sit down against an AI agent. The design is in
[`docs/SPEC.md`](docs/SPEC.md).

## Status

**M4 — the cube and deck files.** The core set is 265 cards, every one a
`.ron` file in `crates/cards/data/core` whose identity fields come from
Scryfall and whose behaviour is hand-written IR. Two checks run in CI over
all of them: each card renders back to its Oracle text, and each card has a
behaviour test (a scenario per spell, trigger, and ability; a check derived
from the IR for stats, keywords, auras, equipment, and mana producers).
Eleven starter decks ship: five mono-colour and six two-colour.

Deck files are the standard text format every deck site exports. They are
checked offline with per-card reasons (unknown, not implemented, banned, not
legal, singleton), analysed for curve, colour sources, and sample hands, and
validated again by the daemon and the engine. `manaline cards update` caches
Scryfall's Oracle Cards bulk data for metadata, legality, and the
"is this a real card the engine can't play yet" distinction; every command
that uses it says how old it is.

A human plays an AI agent in the terminal:

```
cargo run -- play --deck rg-stompy --vs claude
```

That starts the game daemon, seats the agent's MCP server on seat 1, opens
the terminal client on seat 0, and shows how to point your agent at the game.
`--vs random` plays the built-in bot; `--vs human` prints a `join` command
for a second terminal.

Not yet: the ingestion tool that drafts IR from Oracle text (M5), the
in-client deckbuilder and card search (M6).

## Layout

```
crates/cardir   the card IR: schema types, validator, English renderer (§4.1)
crates/engine   pure rules engine: no I/O, no async (§3)
crates/cards    the core card set as IR files, starter decklists, per-card behaviour tests
crates/carddb   Scryfall bulk-data cache: metadata and legality (§4.5)
crates/deckstats deck-file parser, `deck check` classification, curve and colour analysis (§4.5)
crates/ingest   card IR tooling: the round-trip and Oracle cross-check CI runs (§4.3)
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

cargo run -- deck check my-deck.txt --format cube   # per-card reasons; exit 1 if not legal
cargo run -- deck stats rg-stompy                   # curve, colour sources, sample hands
cargo run -- deck new my-deck --format cube         # writes a commented skeleton
cargo run -- cards update                           # fetch Scryfall's Oracle Cards into the cache
cargo run -- cards show "Elvish Archdruid"          # rules text, printing, legality
cargo run -- ingest roundtrip                       # every card renders back to its Oracle text
```

Deck files: one `N Card Name` per line, optional `Deck` / `Sideboard` headers,
`//` comments, `(SET) 123` printing suffixes tolerated. Built-in deck names
work anywhere a file path does.

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
