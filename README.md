# manaline

Terminal Magic: The Gathering. A rules engine, a text-mode client, and an MCP
server so a human can sit down against an AI agent. The design is in
[`docs/SPEC.md`](docs/SPEC.md).

## Status

**M2 — MCP server.** A human plays an AI agent in the terminal:

```
cargo run -- play --deck m0-green --vs claude
```

That starts the game daemon, seats the agent's MCP server on seat 1, opens
the terminal client on seat 0, and shows the one thing you need: how to point
your agent at the game (an HTTP URL, a stdio command, and a config snippet).
Tell the agent to pull the `play-a-game` prompt and go. `--vs codex` and
`--vs mcp` do the same with different hints; `--vs random` plays the built-in
bot; `--vs human` prints a `join` command for a second terminal.

What exists: the pure rules engine (M0), the daemon and protocol with seat
tokens over Unix sockets and TCP, replay logs, the ratatui client (M1), and
the MCP server with `get_game_state`, `get_legal_actions`, `take_action`,
`wait_for_turn`, `get_card`, `get_log`, `say`, `concede`, `submit_deck`, the
rules primer and card list resources, and the `play-a-game` prompt, over
streamable HTTP and stdio (M2). Cards are still the vanilla starter table;
the card IR and the real cube are M3 and M4.

## Layout

```
crates/engine   pure rules engine: no I/O, no async (docs/SPEC.md §3)
crates/cards    the card set (a vanilla table until M3), decklists, deck-file parser
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
cargo run -- play --deck m0-green --vs random     # you against the built-in bot
cargo run -- play --deck m0-green --vs claude     # you against an agent (see the hints in the client)
cargo run -- play --deck m0-green --vs human      # prints a join command for another terminal
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

`crates/engine/tests/rules.rs` has one scenario per rule; `tests/property.rs`
plays random games at 2, 3, and 4 seats, checks invariants after every action
(who must act, zone consistency, life totals, no hidden information in any
view), and replays every finished game from its seed and action log.

## License

GPL-3.0. See [LICENSE](LICENSE).
