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
`search_cards` tool; `deck_stats` and the `editor_*` tools are on the MCP server too, so an agent
can help build a deck through the open editor.

Also in this milestone: colour themes (`default`, `mono`, `high-contrast`)
chosen in the settings menu or with `--theme`; an 80×24 layout that wraps the
footer and uses spare rows for recent log lines; `manaline replay --step`,
which steps through a recorded game in the client; and an ASCII card render
for `cards show`. The core set is 328 cards with a behaviour test each, and
twelve starter decks ship, including a blue-black graveyard deck: the engine
now mills, returns cards from graveyards to hand or battlefield, activates
abilities from the graveyard, and triggers on other creatures dying.

A human plays an AI agent in the terminal:

```
cargo run -- play --deck rg-stompy --vs claude
```

That starts the game daemon, opens the terminal client on seat 0, and
publishes the table so your agent can find it: nobody starts or stops an MCP
server by hand. `--vs random` plays the built-in bot; `--vs human` prints a
`join` command for a second terminal.

`--seats` seats every chair at once, one word per seat in seat order — `me`,
`human`, `random`, `claude`, `codex`, or `mcp`:

```
cargo run -- play --deck green --seats me,claude,random --format free-for-all
cargo run -- play --seats random,claude --format free-for-all   # no `me`: nothing opens the TUI
```

With no `me` seat `play` stays in the foreground, keeps the table published,
and prints one line per thing worth knowing (who joined, who is ready, whose
turn and decision it is, the outcome); `--watch` opens the terminal client as
a spectator instead. Ctrl-C ends the table either way.

Not yet: the in-house Oracle-text-to-IR model and its ingestion pipeline (M5,
in a companion research repo), networked play (M8), Commander (M9).

## Pointing an agent at manaline

Register the MCP server with your agent once, and never start one again:

```
claude mcp add manaline -- manaline mcp --stdio       # Claude Code
codex mcp add manaline -- manaline mcp --stdio        # Codex
```

```json
{"mcpServers": {"manaline": {"command": "manaline", "args": ["mcp", "--stdio"]}}}
```

`play` publishes the table as a marker in the runtime directory
(`$XDG_RUNTIME_DIR/manaline/games`, or `$MANALINE_RUNTIME_DIR` when set). The
agent's session finds the newest published table the first time it calls a game
tool and claims the next free agent seat, so the seating is first come, first
served; with two agent seats you need two separate agent sessions, one each. An
agent seat with no deck assigned picks one itself with `list_decks` and
`submit_deck`. `manaline status` shows every published table, its seats, and
which process holds each agent seat. The marker goes away when `play` exits.

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
cargo run -- play --seats random,claude --format free-for-all   # a table you only watch
cargo run -- sim --seats 4 --games 5 --log        # random bots in a pod, every event printed
cargo run -- replay ~/.local/share/manaline/games/<id>.jsonl --log
cargo run -- list formats | decks | cards

cargo run -- deck check my-deck.txt --format cube   # per-card reasons; exit 1 if not legal
cargo run -- deck stats rg-stompy                   # curve, colour sources, sample hands
cargo run -- deck new my-deck --format cube         # a new deck in your decks directory
cargo run -- cards update                           # fetch Scryfall's Oracle Cards into the cache
cargo run -- cards show "Elvish Archdruid"          # rules text, printing, legality
cargo run -- ingest roundtrip                       # every card renders back to its Oracle text

cargo run -- deck edit my-deck                      # the deckbuilder, by name or file path
cargo run -- deck edit green                        # tweak a shipped deck: saves your own copy
cargo run -- cards search 't:creature c:r mv<=2'    # Scryfall-style search; --all includes unimplemented cards
cargo run -- replay <file.jsonl> --step             # step through a recorded game in the client
cargo run -- play --deck green --vs random --theme mono
cargo run -- play --deck green --vs claude --opp-deck agent   # the agent picks one of the existing decks
cargo run -- play --deck green --seats me,claude,codex --seat-deck 2=wu-fliers --format free-for-all
cargo run -- mcp --stdio                            # the MCP server an agent launches; finds the published table
cargo run -- mcp --http 127.0.0.1:0                 # the same over streamable HTTP, for a client that wants a URL
cargo run -- status                                 # published tables and their seats, daemons, stale sockets
cargo run -- daemon stop [--game ID]                # stop daemons gracefully
cargo run -- mcp stop [--http ADDR]                 # stop MCP servers
```

Deckbuilder keys: type to search, `↑↓` and `Enter` to add, `Tab` to the deck
pane, `+`/`-`/`x` to change counts, `t` for stats and sample hands, `s` to
save, `u` to undo, `q` to quit, `?` for the rest. `scripts/screencast.sh`
records a short tour with asciinema.

An agent can sit with you at the deckbuilder. `manaline deck edit <file>`
announces the file it has open in the runtime directory, and your agent's own
MCP session finds it from there; its `editor_*` tools add and remove cards,
pull up stats, undo, and save through the editor you already have open. Every
change the agent makes is highlighted in place, so you see it arrive and can
take it back with `u`. The MCP server never writes deck files itself; only the
editor does, when you or `editor_save` saves.

Deck files: one `N Card Name` per line, optional `Deck` / `Sideboard` headers,
`//` comments, `(SET) 123` printing suffixes tolerated. A deck name works
anywhere a file path does. Names are looked up at run time, first match wins,
in your own decks (`~/.local/share/manaline/decks`, where saved decks go),
then the decks a package installed (`/usr/share/manaline/decks`, or wherever
`MANALINE_DATA_DIR` pointed when the package was built), then this repo's
`decks/` when running from a checkout. A file in any of them is usable as
`--deck <name>` straight away and shows up in `list decks`; your copy of a
shipped deck shadows it. The deckbuilder writes to your own directory: `deck
new` creates there, and editing a deck by name — from the command line or
with `d` in the lobby — saves your own copy, so a shipped deck is never
changed where it was installed. An existing file path always means that
file, edited in place. `$MANALINE_DECKS_DIR` replaces the whole search path
with one directory.

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
