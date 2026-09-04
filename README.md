# manaline

Terminal Magic: The Gathering. A rules engine, a text-mode client, and an MCP
server so a human can sit down against an AI agent. The design is in
[`docs/SPEC.md`](docs/SPEC.md).

## Status

**M0 — engine skeleton.** The pure rules engine plays complete games of basic
lands and vanilla creatures at any seat count: turn structure, priority,
land drops, casting and resolving creature spells with explicit mana payment,
combat with sequential multiplayer blockers and post-Foundations damage
assignment, state-based actions, elimination, London mulligans, cleanup
discard, seat-filtered views, and seed-plus-action-log replay. Random bots
finish games at 2, 3, 4, and 6 seats without panicking.

Not yet: daemon and protocol (M1), TUI (M1), MCP server (M2), the card IR and
any non-vanilla card (M3), the cube (M4).

## Layout

```
crates/engine   pure rules engine: no I/O, no async (docs/SPEC.md §3)
crates/cards    the card set (a vanilla table until M3), decklists, deck-file parser
crates/cli      the `manaline` binary
formats/        format definitions as RON (§4.4)
decks/          starter decklists in the standard text format (§4.5)
```

## Try it

```
cargo run -- sim --seats 2 --games 10            # random bots, cube format
cargo run -- sim --seats 4 --games 5 --log       # four-seat pod, every event printed
cargo run -- sim --seats 2 --seed 3 --actions --board
cargo run -- list formats | decks | cards
```

`sim` takes `--deck <file-or-builtin-name>` per seat and `--format <name-or-path>`.

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
