# Next steps

These are the next features/changes that should be implemented.

## Agent picks its own deck

- `play --vs claude --opp-deck agent` starts the agent's seat without a deck.
  The lobby waits, and the agent is told to choose one from the existing list
  of decks.
- New `list_decks` tool: every deck reachable by name with card count,
  colours, legality, and where it lives, plus any decks agents saved.
- New `get_deck` tool: a deck's full text plus the `deck_stats` analysis.
- `submit_deck` accepts a deck `name` as well as a decklist, so a listed deck
  can be played as-is.

## `save_deck` tool clarification

- `save_deck` tool is only available while helping a human build a deck in
  the deck editor.
- Unknown card names are refused with suggestions; files are written in
  canonical order.

## The editor announces its open file

- `deck edit` writes a marker under the runtime directory while open, with
  its process id, the file's path, and the format, and removes it on exit.
  Dead editors' markers are cleaned up.
- The standalone MCP server reads it: `save_deck` and `get_deck` default to
  that file, and `list_decks` and the server's opening instructions name it.
  The editor's reload message now says when its file does not exist yet.

## Tests and docs

- MCP tests cover the deckless seat submitting by name, listing and reading
  decks, the open-editor default, and the error cases.
- The primer describes the deck-choice flow, and the README lists
  `--opp-deck agent`.
