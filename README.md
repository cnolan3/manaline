# manaline

Terminal Magic: The Gathering. A rules engine, a text-mode client, and an MCP
server so a human can sit down against an AI agent. The design is in
[`docs/SPEC.md`](docs/SPEC.md).

## Status

**M6 — polish and the deckbuilder.** `manaline deck edit` is a three-pane
deckbuilder: Scryfall-style search on the left, the deck grouped by type in
the middle, and the highlighted card (or the deck's curve, colour sources,
and engine-dealt sample hands) on the right. It reads and writes the same
text deck file every deck site exports, saves in canonical order so diffs
stay readable, reloads when the file changes underneath it, and re-checks
legality on every change. It only offers cards the engine can play. The same
editor opens from the lobby with `d`, and saving there resubmits the deck.

Search is the `cardsearch` crate: `t:creature c:g mv<=2 o:"draw a card"
kw:flying -t:elf (bear or wurm)`, over every Oracle card when the Scryfall
cache is present. It powers `manaline cards search`, the editor, and the MCP
`search_cards` tool; `deck_stats` is on the MCP server too, so an agent can
build its own deck.

Also in this milestone: colour themes (`default`, `mono`, `high-contrast`)
chosen in the settings menu or with `--theme`; an 80×24 layout that wraps the
footer and uses spare rows for recent log lines; `manaline replay --step`,
which steps through a recorded game in the client; and an ASCII card render
for `cards show`. The core set is 284 cards with a behaviour test each, and
twelve starter decks ship, including a blue-black graveyard deck: the engine
now mills, returns cards from graveyards to hand or battlefield, activates
abilities from the graveyard, and triggers on other creatures dying.

A human plays an AI agent in the terminal:

```
cargo run -- play --deck rg-stompy --vs claude
```

That starts the game daemon, seats the agent's MCP server on seat 1, opens
the terminal client on seat 0, and shows how to point your agent at the game.
`--vs random` plays the built-in bot; `--vs human` prints a `join` command
for a second terminal.

Not yet: the in-house Oracle-text-to-IR model and its ingestion pipeline (M5,
in a companion research repo), networked play (M8), Commander (M9).

## Layout

```
crates/cardir   the card IR: schema types, validator, English renderer (§4.1)
crates/engine   pure rules engine: no I/O, no async (§3)
crates/cards    the core card set as IR files, starter decklists, per-card behaviour tests
crates/carddb   Scryfall bulk-data cache: metadata and legality (§4.5)
crates/deckstats deck-file parser, `deck check` classification, curve and colour analysis (§4.5)
crates/ingest   card IR tooling: the round-trip and Oracle cross-check CI runs (§4.3)
crates/cardsearch Scryfall-style query language and index (§4.5)
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

cargo run -- deck edit my-deck.txt                  # the deckbuilder (creates the file if needed)
cargo run -- cards search 't:creature c:r mv<=2'    # Scryfall-style search; --all includes unimplemented cards
cargo run -- replay <file.jsonl> --step             # step through a recorded game in the client
cargo run -- play --deck green --vs random --theme mono
cargo run -- mcp --http 127.0.0.1:7454              # card search and deck stats for an agent, no game needed
cargo run -- status                                 # running daemons, MCP servers, stale sockets
cargo run -- daemon stop [--game ID]                # stop daemons gracefully
cargo run -- mcp stop [--http ADDR]                 # stop MCP servers
```

Deckbuilder keys: type to search, `↑↓` and `Enter` to add, `Tab` to the deck
pane, `+`/`-`/`x` to change counts, `t` for stats and sample hands, `s` to
save, `u` to undo, `q` to quit, `?` for the rest. `scripts/screencast.sh`
records a short tour with asciinema.

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
