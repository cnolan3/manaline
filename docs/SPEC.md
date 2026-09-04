# manaline — Terminal Magic: The Gathering

*Working title. Rename freely.*

A Magic: The Gathering client that runs entirely in the terminal, renders the table in ASCII/box-drawing characters, and exposes the game to AI agents through an MCP server so a human can sit down against Claude Code, Codex, or any other MCP-capable agent.

---

## 1. Goals and non-goals

**Long-term goals (shape every design decision, even where not built yet)**

- **Networked multiplayer.** Players and agents on different machines join the same game. This is *why* the daemon, TUI, and MCP server are separate processes rather than one binary.
- **Multiple formats.** Commander/EDH, Legacy, Modern, Standard, cube — each with its own player count, starting life, deck-construction rules, and card legality (allowlists and banlists). Commander in particular means 3–4+ players, a command zone, commander damage, and colour identity.
- **Any seat can be a human or an agent.** A 4-player pod with two humans and two different AI agents is the intended end state.
- **Local play feels like a single program.** Solo or against a local agent, `manaline play` starts and wires up everything itself; the player never sees a daemon, a socket, or a token (§2.1). The architecture serves the networked case without taxing the local one.
- **Finding a game is easy either way.** On a server, players can join a specific game by code *or* queue for a random opponent in a format (§2.2). The game itself never knows which.
- **Every card is free to every player.** No collection, no economy. Decks are constrained only by format legality and, in limited formats, by a generated pool (§4.5). Sealed is a planned format; draft follows it.
- **Deck building works in a text editor or in the client, interchangeably.** The deck file is the source of truth; the in-client builder with search and analysis reads and writes the same file (§4.5).

**Goals (initial build)**

- Two-player, 20-life games using a curated ~200-card starter cube, on one machine.
- A headless game daemon that is the single source of truth for game state and rules.
- A TUI client (ratatui) for the human player.
- An MCP server client that lets any MCP-capable agent play as the second player.
- The AI learns it must act two ways: a blocking `wait_for_turn` tool, and a printed nudge in the human's TUI as fallback.
- Deterministic, replayable games (seeded RNG + action log) so bugs are reproducible and games are reviewable.
- Every card in the cube fully expressed in a small, validated card IR (§4). The game never parses Oracle text at runtime.
- A dev-time ingestion tool that drafts IR from Oracle text so new sets are a review pass, not a typing exercise (§4.3).

**Not in the initial build — but nothing below may preclude them**

- Remote connections to the daemon (TCP + auth). The protocol is transport-agnostic from day one; only the Unix-socket transport ships first.
- 3+ player games, Commander rules, and the format registry beyond `cube` and a plain `two-player` format. The engine is N-player from day one (§3.1); the *rules* for those formats come later.
- Limited/draft.

**Non-goals**

- Full Comprehensive Rules coverage. No layers system beyond what the cube needs, no replacement-effect ordering, no copiable values, no split/DFC/adventure cards — until a format or an `ingest` report makes one worth it.
- The daemon spawning or driving an agent process. Users bring their own agent and their own conversation window.
- Accounts or ratings. A hosted lobby server with a matchmaking queue is planned (§2.2), but it's the same binary anyone can run, and identity on it is a game code and a seat token until there's a reason for more.

**The design rule that follows from all this:** nothing in `engine`, `protocol`, or `daemon` may assume two players, a fixed 20 life, or a single machine. Where the initial build only *exercises* two players, the types still say N.

---

## 2. Architecture

One daemon per game, any number of seat clients, one protocol.

```
   machine A                              machine B                    machine C
┌───────────────────┐   ┌───────────────────┐   ┌──────────────────┐  ┌──────────────────┐
│ manaline-tui      │   │ manaline-mcp      │   │ manaline-tui     │  │ manaline-mcp     │
│ seat 0 (Connor)   │   │ seat 1 ◀── Claude │   │ seat 2 (friend)  │  │ seat 3 ◀── Codex │
└─────────┬─────────┘   └─────────┬─────────┘   └────────┬─────────┘  └────────┬─────────┘
          │ unix socket           │ unix socket          │ TCP                 │ TCP
          └───────────┬───────────┘                      │                     │
                      ▼                                  │                     │
            ┌───────────────────────┐ ◀──────────────────┴─────────────────────┘
            │  manaline-daemon      │
            │  rules engine, state, │        ── daemon protocol: newline-delimited JSON,
            │  replay log, lobby    │           identical over every transport ──
            └───────────────────────┘
```

**Why a separate daemon.** The TUI and the MCP server are just clients that submit actions and subscribe to state; neither owns the rules. The daemon doesn't know or care whether a seat is a human, an agent, local, or remote — it sees a socket and a seat token. That single property is what makes human-vs-agent on one laptop, four humans across the internet, and agent-vs-agent benchmark runs the *same program*.

**Transports.** The protocol is bytes-in, bytes-out; transports are interchangeable:

- **Unix domain socket** in the platform runtime directory (`$XDG_RUNTIME_DIR/manaline/` on Linux; macOS has no `XDG_RUNTIME_DIR`, so fall back to `$TMPDIR/manaline-<uid>/`, mode 0700 — use the `dirs` crate's `runtime_dir()` with that fallback) — default for local clients. Ships in the initial build.
- **TCP** on a host-chosen port — for remote seats. Same messages, plus seat-token auth is mandatory rather than implied by filesystem permissions. Ships when networked play is built; until then, `ssh -L` / Tailscale over the TCP listener is the documented workaround and costs nothing to support.
- **WebSocket over TLS** — the transport for lobby servers (§2.2), chosen because it passes every home firewall on port 443 without anyone opening a port. Same messages, one frame per line.

**Lobby and seats.** A game is created with a format and a seat count, and hands out one token per seat plus an unlimited spectator token. Seats are indices, `Seat(u8)`, never named "player 1/2" in any type.

| Client → daemon                              | Daemon → client                                         |
|----------------------------------------------|---------------------------------------------------------|
| `create_game { format, seats, seed? }`       | `game_created { game_id, seat_tokens: [..], spectator_token }` |
| `hello { token, protocol_version }`          | `welcome { seat \| spectator, game_id, format, state, protocol_version }` |
| `get_pool`                                   | `pool { cards: [..] }` — limited formats only, seat-private (§4.4) |
| `set_deck { decklist, commander? }`          | `deck_ok` / `deck_rejected { violations: [..] }` (§4.4, §4.5) |
| `ready`                                      | `event { kind: "game_started" }` once all seats ready   |
| `get_state`                                  | `state { ... }` (seat-filtered)                         |
| `get_legal_actions`                          | `legal_actions { actions: [...], state_version }`       |
| `act { action_id \| action, state_version }` | `ack { applied, events, new_state }` / `error`          |
| `subscribe`                                  | `event { ... }` stream (push)                           |
| `chat { text, to?: Seat }`                   | `event { kind: "chat", from, ... }`                     |
| `queue { format, seats? }` / `cancel_queue`  | `event { kind: "matched", game_id, token }` (§2.2.1, future) |

**Versioning and errors.** `protocol_version` is a single integer, bumped on any incompatible change; the daemon refuses a `hello` with a version it doesn't speak and says which versions it does. Remote clients will outlive daemon releases, so this exists from M1 even though it's `1` for a long time. Every failure is one shape:

```json
{ "type": "error", "code": "illegal_action", "message": "…human-readable…", "retryable": false, "state_version": 812 }
```

Codes are a closed enum (`bad_token`, `unsupported_version`, `illegal_action`, `stale_state_version`, `not_your_turn_to_act`, `deck_rejected`, `game_over`, `internal`); `message` is for humans and agents, `code` is for programs. `stale_state_version` and `not_your_turn_to_act` are `retryable: true` — the right response is to fetch state and try again.

The MCP server and the TUI are thin: they translate their own protocol into these messages. A seat that disconnects keeps its seat; reconnecting with the same token resumes. Long-term, a seat with no connection for N minutes can be auto-passed or conceded per format config; the initial build just waits.

### 2.1 Local play is one command

The multi-process architecture exists for the networked case. **A person playing alone, or against a local agent, must never see it.** `manaline play` is the whole user experience:

```
$ manaline play --deck decks/rw.txt --vs claude
```

Behind that one command, `play` does all of the following and shows none of it:

1. Spawns `manaline-daemon` as a child process on a fresh Unix socket (a random path in the platform runtime directory per §2's transport rules; no `--tcp` listener unless asked), with a `--parent-pid` so the daemon exits if `play` dies.
2. Creates the game, takes seat 0's token for itself, and hands seat 1's token to whichever opponent was requested: the built-in random bot (spawned in-process), or the MCP server (spawned as a child, bound to seat 1).
3. Submits the human's deck, waits for the opponent seat to be ready, and starts the game.
4. Opens the TUI in the current terminal, connected as seat 0.
5. For `--vs claude` (or `codex`, or the generic `--vs mcp`), prints the one thing the human *does* need to know, in the TUI's log pane: how to point their agent at the game — the streamable-HTTP URL or the stdio command, plus a copy-pasteable JSON snippet for the agent's config, and the suggested opening line to say to the agent.
6. On quit: tears down the MCP server and the daemon, writes the replay file, and prints its path.

There is no separate "start the server" step, no game id to type, no token to copy — all of that is internal to `play`. The only process the human is consciously aware of besides the terminal they're typing in is their agent, which by design runs in a window they own. Crash and edge-case behaviour follows the same principle: if the daemon dies, `play` reports "the game crashed" and offers the replay path; it never surfaces socket paths or child-process errors as the primary message.

`--vs human` on the same machine (two terminals) is the one local case that's *slightly* visible: `play` prints a second command for the other terminal to run. That's still one line, and it's exactly the command `join` will accept over the network later.

The individual `daemon` / `tui` / `mcp` subcommands remain for scripting, debugging, and networked play. They're documented as advanced; `play` is the front door.

### 2.2 Networked play: servers, not relays *(post-initial-build)*

Two kinds of remote player need serving: power users who'll happily run their own server, and casual users who won't open a port on their home router. Both are served by the same design.

**The daemon runs on a server, never on a player's machine, whenever strangers are involved.** The alternative — a hosted relay that pumps bytes between players while one player's machine runs the daemon — was considered and rejected on one decisive point: the daemon holds every hidden zone in the game. Whoever runs it can read every opponent's hand and library with a debugger. Between friends that's fine, and the direct `host`/`join` path (§8) stays for them. For anything resembling a public lobby, the only fix is to move the daemon off the player, which is the server model anyway. The relay model also inherits host-disconnect-ends-the-game, host latency as everyone's floor, and eventually host migration — and a relay is about as much code as a lobby server with worse properties.

**One server binary, three deployment tiers.**

```
tier 0   manaline host / join         direct TCP between friends; Tailscale or ssh -L for NAT.
                                      No infrastructure. Ships with M8.
tier 1   manaline server               a lobby + many game daemons in one process. Power users
                                      run it on a VPS or homelab. Ships with M8.
tier 2   the hosted instance           the same `manaline server`, run by the project, and the
                                      default `--server` for every client. Ships when tier 1 is
                                      stable and there's a reason to.
```

There is deliberately no separate "hosted lobby" codebase. Every bug a power user hits on their own server is one the hosted instance would hit too, and vice versa.

**`manaline server` is the daemon, pluralised.** It holds many games in one tokio runtime — one `Mutex<Game>` and one watch channel per game, exactly the §5 structure — behind a lobby that maps a short game code to a game. A game between actions is a few hundred kilobytes and no CPU, so a single small instance serves thousands of concurrent games. Spawning daemons as separate processes or onto other machines is an optimisation for a scale problem that doesn't exist yet; the lobby's interface (`create_game` → code + tokens; `hello { token }` → seat) doesn't change if it ever does.

```
manaline server
├── transports        WS/TLS listener, TCP listener → framed NDJSON connections
├── lobby             the only shared state; small and mostly idle
│   ├── codes         game code → game id            (join a specific game)
│   ├── queues        format → waiting seats          (matchmaking, §2.2.1)
│   └── tokens        seat/spectator token → (game id, seat)
└── games             game id → GameTask, fully independent of one another
    └── GameTask      Mutex<Game> + watch channel + durable action log + connected clients
```

Games are independent by construction: each `GameTask` owns its state, its log, and its client set, and knows nothing about any other game. A game is created by a code or by the matchmaker, runs until `is_over()` or an idle timeout on an abandoned game, then its entry is dropped and its log closed. New games start and old ones end at any moment without coordination, because there is nothing to coordinate — the lobby is touched at create and join time and never during play.

#### 2.2.1 Matchmaking *(future goal — designed for now, built later)*

Two ways into a game on a server, and the game can't tell them apart:

- **By code.** `create` returns a short code; you send it to the people you want to play with; they `join <code>`. This is the lobby in its entirety and ships with tier 1. It covers every "play with a specific person" case with no identity at all.
- **By queue.** `manaline queue --format commander --seats 4` registers a waiting seat with the server's matchmaker. When the queue for that format holds enough compatible seats, the server does *exactly what `create_game` does* — constructs a game, issues each queued player a seat token — and pushes `matched { game_id, token }` to each of them. From that message on, the flow is identical to joining by code.

The matchmaker is a small structure in the lobby: per format, a list of waiting seats with their queue time and (later) any matching criteria. Its interface to the rest of the server is one function — `try_match(format) -> Option<Vec<WaitingSeat>>` — and its only side effect is a `create_game` call. Design decisions elsewhere that exist specifically so this drops in cleanly:

- **Game creation is one internal function** used by `create` and, later, by the matchmaker. Nothing in game setup assumes the players know each other or chose each other.
- **Seat tokens are issued by the lobby, not by the creator.** The player who ran `create` has no special standing over other seats; the matchmaker issuing tokens to strangers is the same operation.
- **Decks are submitted per seat after joining** (`set_deck`), not at creation, so a queued player's deck is validated against the format the same way a code-joined player's is, and a rejected deck sends them back to the queue rather than breaking the game for everyone.
- **`Format` carries player count**, so "queue for commander" already knows it needs four seats, and the queue is keyed by format rather than by a separate mode concept.
- **The protocol has room for unsolicited server → client messages** (`event { … }` is already push), so `matched` is another event, not a new channel.

What matchmaking will need that the lobby doesn't have yet, in the order it's likely to matter: a widening rule (wait 60 s for an exact match, then relax constraints), a cancel message, a "ready check" so a match doesn't start with someone who walked away, and — only if it becomes a problem — some notion of identity to keep the same two people from being re-paired, or to attach a rating. None of those change the game protocol.

**Transport: WebSocket over TLS on 443.** Every home firewall, hotel network, and corporate proxy passes it; no one opens a port; certificates come from Let's Encrypt like any website. The daemon protocol is newline-delimited JSON, so one WebSocket text frame is one message and nothing above the transport changes. It also leaves the door open to a browser client that speaks the same protocol. The plain TCP listener from M1 stays as the tier-0 transport.

**Clients don't care which tier they're on.** `manaline join <code> --server wss://play.example` and `manaline join 192.168.1.10:7454 --token …` reach the same code path in the TUI and in the MCP server; an agent on a laptop plays in a hosted four-seat pod by connecting its local `manaline mcp` to the server with a seat token, no differently from a local game.

**Durability falls out of determinism.** A game is its seed plus its action log (§3.6). The server appends each accepted action to durable storage before acking it, so a crash, a deploy, or a migration to another instance reconstructs every in-flight game by replay. Reconnecting clients present their seat token and resume. This is the operational property the relay model can never have: the host's machine going away takes the log with it.

**Auth stays minimal until it can't.** A game code (six characters, unguessable enough for a lobby that expires games) and per-seat tokens. No accounts, no identity, no ratings. If matchmaking or persistent identity ever matters, that's an additive layer on the lobby, and the game protocol doesn't learn about it.

**What the server must never do,** restated for the network case: send one seat's hidden zones to another seat's connection, in any form, including "the client will ignore it". Seat-filtered views are the whole defence, and the test in §10 (diff the bytes on a remote connection against the spectator view) is the guard.

### 2.3 Cargo workspace

```
manaline/
├── Cargo.toml                 # workspace
├── crates/
│   ├── engine/                # pure rules engine, no I/O, no async
│   ├── cardir/                # card IR: schema types, validator, English renderer
│   ├── cards/                 # the cube: IR files + loader + per-card tests
│   ├── carddb/                # Scryfall bulk cache, card metadata, legality lookup
│   ├── cardsearch/            # Scryfall-style query language + index over carddb (§4.5)
│   ├── deckstats/             # deck parsing (text format) + analysis functions (§4.5)
│   ├── protocol/              # shared serde types for daemon protocol + state views
│   ├── daemon/                # tokio server: hosts a Game, speaks protocol
│   ├── tui/                   # ratatui client
│   ├── mcp/                   # rmcp server: MCP tools ↔ daemon protocol
│   ├── ingest/                # dev tool: Oracle text → IR (§4.3)
│   └── cli/                   # `manaline` binary: subcommands wrap the above
└── decks/                     # starter decklists, standard text deck format (§4.5)
```

`engine`, `cardir`, and `cards` must compile without tokio or any networking dependency. That constraint is what keeps them testable in plain `#[test]`s and fast to fuzz. `ingest` is a dev-time tool and is the only crate allowed to call out to an LLM API.

**Key dependencies**

| Crate         | Purpose                                                                 |
|---------------|-------------------------------------------------------------------------|
| `ratatui` + `crossterm` | TUI rendering and input                                        |
| `tokio`       | daemon and MCP server runtime                                            |
| `rmcp`        | official Rust MCP SDK; `transport-streamable-http-server` and `transport-io` (stdio) features |
| `serde` / `serde_json` | protocol and state serialization                               |
| `rand` + `rand_chacha` | seeded, reproducible shuffles                                  |
| `thiserror`   | engine error types                                                      |
| `clap`        | CLI                                                                     |
| `tracing`     | structured logs (daemon writes the action log here too)                 |
| `ron`         | card IR on disk (human-readable, comments, enums without quoting)       |
| `schemars`    | JSON Schema from the IR types, fed to the ingestion model                |
| `reqwest`     | `ingest` only — LLM API calls                                           |
| `tokio-tungstenite` + `rustls` | WebSocket/TLS transport for `server` and remote clients (M8) |

---

## 3. Rules engine (`crates/engine`)

The engine is a pure state machine. Everything else in the project is a consequence of this interface:

```rust
pub struct Game { /* full state, every player's hidden info */ }

pub struct GameConfig {
    pub format: Format,                   // §4.4 — starting life, player count, commander rules, …
    pub players: Vec<PlayerSetup>,        // one per seat; decks already validated against the format
}

impl Game {
    pub fn new(config: GameConfig, seed: u64) -> Game;
    pub fn legal_actions(&self, seat: Seat) -> Vec<Action>;
    pub fn apply(&mut self, seat: Seat, action: &Action) -> Result<Vec<Event>, RulesError>;
    pub fn view(&self, seat: Seat) -> GameView;          // hidden info removed
    pub fn view_spectator(&self) -> GameView;            // all hidden info removed
    pub fn is_over(&self) -> Option<Outcome>;            // Outcome::Winner(Seat) | Draw
}
```

`legal_actions` is the load-bearing function. The TUI renders it as a menu, the MCP server hands it to the agent as a numbered list, and tests assert on it. If an action isn't in the list, `apply` rejects it. The agent can never make an illegal move, which means the agent never needs to know the rules — only to choose well.

**One carve-out, stated once.** Actions whose legal space is a *numeric division* — today only `AssignCombatDamage`, later anything of the form "divide N among targets" — are validated by rule rather than by list membership: `apply` checks that the amounts sum to the attacker's power, every recipient is a legal recipient, and trample's lethal-first constraint holds. The entries `legal_actions` enumerates for these are *suggestions* (the common splits), and any assignment satisfying the rule is accepted whether or not it was listed. The daemon's validation step (§5) applies the same test: list membership for every other action, rule check for divisions. `Action::is_division()` marks which kind an action is so neither layer has to special-case by name.

**"Who must act" is not "who has priority."** The defending player declares blockers without holding priority; a seat answers a resolution-time choice ("sacrifice a creature") without holding priority; cleanup discard and mulligans are decided by seats that hold nothing. So the engine exposes a second function, and everything downstream keys on it rather than on priority:

```rust
pub enum ActReason { Priority, DeclareAttackers, DeclareBlockers, AssignDamage,
                     Mulligan, BottomCards, Discard, Choice }

impl Game {
    /// Seats whose `legal_actions` is non-empty right now, and why. Empty iff the game is over.
    pub fn must_act(&self) -> BTreeMap<Seat, ActReason>;
}
```

The reason is computed by the engine once and travels with the seat everywhere: the daemon's watch channel carries `(state_version, must_act: BTreeMap<Seat, ActReason>, game_over)`, `GameView.must_act` is the same map, the MCP tool blocks on "my seat is a key in `must_act`" and returns its reason verbatim, the TUI's header says "waiting on seat 2 to declare blockers" from the same map, and the property-test invariant is *"`must_act` is non-empty while the game is not over."* Priority is still tracked — it decides who may cast spells — but it's an input to `legal_actions`, not the thing clients wait on.

**The engine never returns in a state where nobody can act.** `apply` runs turn-based actions, state-based actions, trigger placement, and phase advancement in a loop until either some seat has a legal action or `is_over()` is `Some`. Untap and cleanup, which have no priority, are therefore never observable from outside except as events. This is what makes the `must_act` invariant hold.

### 3.1 State model

```rust
pub struct Game {
    pub format: Format,
    pub turn: u32,
    pub turn_order: Vec<Seat>,        // seats still in the game, in APNAP order from the starting player
    pub active_player: Seat,
    pub phase: Phase,                 // Untap, Upkeep, Draw, Main1, BeginCombat, DeclareAttackers,
                                      // DeclareBlockers, CombatDamage, EndCombat, Main2, End, Cleanup
    pub priority: Option<Seat>,       // None during untap/cleanup
    pub passed_in_succession: u8,     // == turn_order.len() with empty stack → advance phase
    pub stack: Vec<StackObject>,
    pub players: Vec<PlayerState>,    // indexed by Seat; eliminated players stay (for their objects' owner refs)
    pub objects: SlotMap<ObjectId, GameObject>,  // every card/token/ability, zone-agnostic
    pub pending: Option<PendingChoice>,          // resolution-time choices, damage assignment, bottoming, etc.
    pub rng: ChaCha8Rng,
    pub log: Vec<Event>,
}

pub struct PlayerState {
    pub life: i32,                    // starts at format.starting_life
    pub eliminated: Option<Elimination>,   // Some(reason) once they've lost
    pub library: Vec<ObjectId>,
    pub hand: Vec<ObjectId>,
    pub graveyard: Vec<ObjectId>,
    pub exile: Vec<ObjectId>,
    pub battlefield: Vec<ObjectId>,
    pub command: Vec<ObjectId>,       // command zone; empty outside commander formats
    pub commander_damage: HashMap<ObjectId, i32>,  // damage taken from each commander (rule 903.10a)
    pub commander_casts: HashMap<ObjectId, u8>,    // for commander tax
    pub mana_pool: ManaPool,
    pub lands_played_this_turn: u8,
    pub poison: u8,
}
```

**N-player rules that are cheap to get right on day one and painful to retrofit:**

- `Seat` is an index, `players` is a `Vec`, and every "the other player" in the engine is written as `self.opponents_of(seat)` — an iterator — never `1 - seat`.
- Priority and trigger ordering follow APNAP over `turn_order`, which already generalises to N.
- **Elimination** (rule 800.4a): when a player loses, they leave `turn_order`, all objects they own leave the game, and any spells/abilities they control on the stack cease to exist. `is_over()` is "one seat left in `turn_order`" (or zero → draw). The two-player case is just N=2.
- **Attack targets** are `AttackTarget::Player(Seat) | Planeswalker(ObjectId)`, so declaring attackers in a pod is choosing per attacker — the same `DeclareAttackers` action, longer list.
- **State-based actions** iterate over all players (0 life, poison ≥ 10, empty-library draw, commander damage ≥ 21 from one commander).
- **Hidden-information views** are per-seat, and `view(seat)` never leaks another seat's hand or library order. Spectators get `view_spectator()`.
- **Starting life, hand size, mulligan rule, and win conditions** come from `Format`, never from constants.

```rust
pub struct GameObject {
    pub id: ObjectId,
    pub card: CardId,                 // index into the card database
    pub owner: Seat,
    pub controller: Seat,
    pub zone: Zone,
    pub tapped: bool,
    pub summoning_sick: bool,
    pub damage: i32,
    pub counters: Counters,           // +1/+1, -1/-1
    pub attached_to: Option<ObjectId>,
    pub attacking: Option<AttackTarget>,
    pub blocking: Vec<ObjectId>,
    pub modifiers: Vec<Modifier>,     // "until end of turn" effects, see §3.4
}
```

### 3.2 Actions

The full action vocabulary for v1. Deliberately small.

```rust
pub enum Action {
    PassPriority,
    PlayLand { object: ObjectId },
    CastSpell { object: ObjectId, targets: Vec<Target>, payment: ManaPayment },
    ActivateAbility { object: ObjectId, ability: u8, targets: Vec<Target>, payment: ManaPayment },
    DeclareAttackers { attackers: Vec<(ObjectId, AttackTarget)> },
    DeclareBlockers { blocks: Vec<(ObjectId /*blocker*/, ObjectId /*attacker*/)> },
    AssignCombatDamage { attacker: ObjectId, assignments: Vec<(DamageTarget, i32)> },  // only when a choice exists
    ChooseTargets { targets: Vec<Target> },         // answering a PendingChoice
    ChooseMode { mode: u8 },
    Discard { objects: Vec<ObjectId> },             // cleanup step hand-size
    Mulligan { keep: bool },
    BottomCards { objects: Vec<ObjectId> },         // London mulligan: after keeping, put N on the bottom
    CastCommander { object: ObjectId, targets: Vec<Target>, payment: ManaPayment },  // from command zone, with tax
    CommanderToCommandZone { object: ObjectId },     // answering the "return it?" choice on zone change
    Concede,
}
```

Two details worth getting right up front:

**Mana payment is explicit.** `CastSpell` carries a `ManaPayment` naming which permanents to tap and which pool mana to spend. For the TUI this is auto-filled by a solver when unambiguous and prompted otherwise. For the agent, `legal_actions` enumerates one `CastSpell` per distinct legal payment when the cast is actually possible, and the state view shows `castable: true/false` per card. This avoids the agent trying to cast things it can't afford.

**Targets are chosen at cast time.** `legal_actions` enumerates `CastSpell` with each legal target combination for spells with ≤ 2 targets, which covers the entire v1 cube. This is simpler for agents than a two-step "cast, then choose" flow. `PendingChoice` is reserved for choices that happen on resolution (e.g., "choose a creature to sacrifice"), for mulligan bottoming, and for combat damage assignment.

**Combat damage assignment follows current rules, not the pre-Foundations ones.** The Foundations rules update (November 2024) removed "damage assignment order": an attacker blocked by several creatures divides its damage among them however its controller likes, with only trample still requiring lethal damage to every blocker before any goes to the player. So there is no `OrderBlockers`. Instead: when an attacker has exactly one blocker and no trample, the engine assigns damage automatically. Otherwise the attacker's controller gets `AssignCombatDamage` in `legal_actions` and the engine enumerates a small set of sensible splits (all to each blocker; lethal to each in the declared order then remainder to the next / to the player) as suggestions, and any other split that satisfies the rule is accepted under the division carve-out at the top of §3. The default the TUI offers on `Enter` is "lethal to each blocker in the order they were declared, remainder to the player if trample".

**When it's asked matters.** Assignment is a turn-based action at the *start of the combat damage step* (rule 510.1), after players have had priority in the declare blockers step with blocks known — a pump spell or removal cast after blocks changes what "lethal" means, and the attacker assigns with that information. So `AssignCombatDamage` is a pending choice raised on entering `CombatDamage`, answered by the attacking player for each attacker that needs it, with **no priority between the choice and the damage**. If any creature has first or double strike, this happens twice: once for the first-strike damage round and again for the regular round (only creatures that deal damage in that round are asked about, and blockers killed in the first round are gone by the second).

**London mulligan is two actions.** `Mulligan { keep: false }` shuffles and redraws seven; `Mulligan { keep: true }` after N mulligans puts the seat into a pending `BottomCards` choice for exactly N cards. Mulligans are decided in turn order starting with the starting player (rule 103.5), so `must_act` holds one seat at a time.

### 3.3 Turn structure and priority

Standard Comprehensive Rules turn loop, implemented as a state machine on `Phase`:

1. On entering a step, run turn-based actions (untap, draw, combat damage, cleanup discard) and put triggers on the stack.
2. Give priority to the active player.
3. On `PassPriority`: priority moves to the next seat in `turn_order`. If the stack is non-empty and every seat has passed in succession, resolve the top object and return to step 2. If the stack is empty and every seat has passed, advance to the next step.
4. Casting a spell or activating an ability resets `passed_in_succession` and gives priority back to the actor.

Combat is the hairiest part. Sequence: `BeginCombat` → `DeclareAttackers` (active player acts, then priority) → `DeclareBlockers` (each defending player declares, then priority) → `CombatDamage` (damage assignment choices as a turn-based action, then damage, then priority; a first-strike round precedes the regular round if any creature has first or double strike, each with its own assignment) → `EndCombat`. Skip `DeclareBlockers`/`CombatDamage` if no attackers.

**Multiplayer blockers are sequential, not simultaneous.** In a pod where several players were attacked, defending players declare blockers one at a time in APNAP order (rule 802.3), each seeing the previous declarations. This is decided now, at N=2, because it fixes the state machine: `DeclareBlockers` is a sub-loop over `turn_order` filtered to attacked seats, and `must_act` holds exactly one seat throughout. A simultaneous model would need a different `PendingChoice` shape and a reveal step, and nothing in the rules asks for it.

### 3.4 Continuous effects — the deliberately minimal version

The full layers system is out of scope. The cube is chosen so that only these forms of continuous effect exist:

- **Static P/T or keyword buffs from the object itself** (e.g., a lord: "other Elves you control get +1/+1") — computed on read by `Game::effective_stats(id)`, which walks the battlefield for applicable statics. Recomputed every time it's asked, never cached. Correct by construction, and fast enough at two-player cube scale.
- **"Until end of turn" modifiers** from spells and abilities — stored as `Modifier { kind, expires: Expiry::EndOfTurn }` on the object and cleared in cleanup.
- **Auras and Equipment** granting P/T or keywords — treated as statics on the attached object, resolved through the same `effective_stats` walk.

Order of application inside `effective_stats`: base → copy (none in v1) → control (none) → text-changing (none) → type-changing (none) → P/T setting → P/T modifying (statics, modifiers, counters) → P/T switching (none). That's layer 7 and nothing else, which is honest about what the cube needs.

### 3.5 Triggered abilities

Triggers are collected when an `Event` is emitted, not polled. Each card's IR `triggers` list (§4.1) is matched against emitted events by `engine::interp`. After each turn-based action or resolution, the engine drains triggered abilities onto the stack in APNAP order, prompting for targets via `PendingChoice` where needed. Supported trigger events in v1: enters the battlefield, dies, attacks, deals combat damage to a player, beginning of upkeep/end step, becomes tapped/untapped (for a couple of cards).

### 3.6 Determinism and replay

`Game::new(config, seed)` plus the ordered list of `(Seat, Action)` pairs fully determines a game. The daemon writes both to `~/.local/share/manaline/games/<game-id>.jsonl` as it goes. `manaline replay <file>` reconstructs the game and can step through it in the TUI. This is also the test format: a failing game becomes a regression test by copying the file into `crates/engine/tests/replays/`.

### 3.7 Views and filtered events

Hidden information never leaves the engine except through two types that are *structurally* unable to carry it. Nothing filters `Game` at the protocol layer; the engine produces safe types and the daemon forwards them.

```rust
/// What one seat is allowed to know. Produced only by Game::view(seat) / view_spectator().
pub struct GameView {
    pub you: Option<Seat>,                       // None for spectators
    pub turn, active_player, phase, priority, state_version,
    pub must_act: BTreeMap<Seat, ActReason>,
    pub stack: Vec<StackObjectView>,             // spells on the stack are public
    pub players: Vec<PlayerView>,
    pub objects: HashMap<ObjectId, ObjectView>,  // only objects in public zones, plus your own hand
}

pub struct PlayerView {
    pub seat: Seat, pub name: String, pub life: i32, pub eliminated: bool,
    pub hand: HandView,                          // Yours(Vec<ObjectId>) | Hidden { count: u8 }
    pub library: LibraryView,                    // always Hidden { count } — even yours
    pub graveyard: Vec<ObjectId>, pub exile: Vec<ObjectId>, pub battlefield: Vec<ObjectId>,
    pub command: Vec<ObjectId>, pub commander_damage: HashMap<ObjectId, i32>,
    pub mana_pool: Option<ManaPool>,             // Some only for `you`
    pub pool: Option<Vec<CardId>>,               // limited pool, Some only for `you`
}

/// Events as seen from one seat. Produced by Game::apply → Vec<Event>, then Event::view(seat).
pub enum EventView {
    Drew { seat: Seat, cards: DrawnCards },      // Yours(Vec<ObjectId>) | Hidden { count: u8 }
    Discarded { seat, objects: Vec<ObjectId> },  // public once it happens
    Shuffled { seat },
    MulliganTaken { seat, to: u8 },
    Cast { seat, object, targets }, Resolved { object },
    Damage { source, to: DamageTarget, amount }, LifeChanged { seat, from, to },
    ZoneChange { object, from: Zone, to: Zone }, // library→hand is *never* emitted as ZoneChange; it's Drew
    Attacked, Blocked, PhaseChanged, PriorityPassed, Eliminated, GameOver, Chat, …
}
```

The rules that make this hold: an object gets an `ObjectView` entry only when it's in a public zone or in *your* hand; a card drawn by another seat is `Drew { cards: Hidden { count: 1 } }` and nothing else; library order is never viewable by anyone, including its owner; scry/surveil-style effects (later) surface only to the acting seat. Chat is the one non-zone piece of private information in the stream: a `Chat` event with a `to` seat is delivered only to the sender and recipient, and spectators never receive private chat at all. The byte-diff test in §10 compares a remote seat's wire traffic against `view_spectator()` plus that seat's own hand and confirms there's nothing else in it.

### 3.8 Testing strategy

- Unit tests per rule in `engine` (priority passing, combat damage assignment, state-based actions, mana payment).
- One integration test per card in `cards`, using a `TestGame` builder that puts specific objects on the battlefield and asserts on the result of casting/activating. Plus one round-trip test per card: `render(load(card)) == normalise(card.text)` (§4.3).
- One interpreter test per IR primitive in `engine::interp`, independent of any real card.
- Replay regression tests as above.
- A property test: play N random games choosing uniformly from `legal_actions`; assert invariants after every action (life totals consistent with damage events, no object in two zones, `must_act()` non-empty while the game is not over and every seat in it has ≥ 1 legal action, stack empties within bounded passes, `view(seat)` for every seat contains no hidden zone contents of any other seat).

The property test is the one most worth writing early. It catches an enormous class of engine bugs without needing a single hand-written scenario.

---

## 4. Cards: the IR (`crates/cardir`) and the cube (`crates/cards`)

Cards are **data, not code**. Each card is a file in `crates/cards/data/<set>/<name>.ron` holding a small AST — the *card IR* — that the engine interprets. **One file per Oracle name, ever.** Oracle text belongs to a card name, not a printing, so a reprint is not a new file; the `<set>` directory records only where the card was first ingested, and the loader rejects a duplicate name anywhere in the tree. Printing-specific data (art, collector number, set symbol) lives in `carddb`, never in IR. The engine implements a fixed vocabulary of primitives (effects, costs, targets, triggers, statics); a card is a composition of those primitives and nothing else.

This is a deliberate trade against the "each card is a Rust function" approach. Closures are more expressive, but an AST is schema-checkable, diffable, loadable without a compile cycle, renderable back to English, and — the reason this decision matters — a tractable target for a model to generate (§4.3). Anything a card needs that the IR can't express is, by definition, a new engine primitive, and adding primitives is the one place hand-written Rust is expected.

### 4.1 The IR

```ron
// crates/cards/data/core/lightning_strike.ron
Card(
    name: "Lightning Strike",
    cost: "{1}{R}",
    types: [Instant],
    text: "Lightning Strike deals 3 damage to any target.",   // Oracle, for display + round-trip
    spell: Spell(
        targets: [Any],
        effects: [DealDamage(amount: Const(3), to: Target(0))],
    ),
)

// crates/cards/data/core/elvish_archdruid.ron
Card(
    name: "Elvish Archdruid",
    cost: "{1}{G}{G}",
    types: [Creature], subtypes: [Elf, Druid], pt: (2, 2),
    text: "Other Elf creatures you control get +1/+1.\n{T}: Add {G} for each Elf you control.",
    statics: [
        PtBoost(
            filter: And([Other, Creature, Subtype(Elf), ControlledBy(You)]),
            power: Const(1), toughness: Const(1),
        ),
    ],
    activated: [
        Ability(
            cost: [Tap],
            effects: [AddMana(color: Green, amount: Count(And([Subtype(Elf), ControlledBy(You)])))],
        ),
    ],
)
```

The IR is defined once as Rust types in `cardir` and everything else derives from them:

```rust
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Card {
    pub name: String,
    pub cost: ManaCost,
    pub types: Vec<CardType>,
    #[serde(default)] pub subtypes: Vec<Subtype>,
    pub pt: Option<(i32, i32)>,
    pub text: String,
    #[serde(default)] pub keywords: Vec<Keyword>,
    pub spell: Option<Spell>,             // instants & sorceries
    #[serde(default)] pub statics: Vec<Static>,
    #[serde(default)] pub triggers: Vec<Trigger>,
    #[serde(default)] pub activated: Vec<Ability>,
}

pub enum Effect {
    DealDamage { amount: Amount, to: Ref },
    Destroy { target: Ref }, Exile { target: Ref },
    Draw { player: PlayerRef, count: Amount },
    Discard { player: PlayerRef, count: Amount, random: bool },
    GainLife { player: PlayerRef, amount: Amount }, LoseLife { .. },
    ModifyPt { target: Ref, power: Amount, toughness: Amount, until: Duration },
    GrantKeyword { target: Ref, keyword: Keyword, until: Duration },
    CreateToken { spec: TokenSpec, count: Amount },
    AddCounters { target: Ref, kind: CounterKind, count: Amount },
    AddMana { color: Color, amount: Amount },
    Tap { target: Ref }, Untap { target: Ref },
    ReturnToHand { target: Ref },
    CounterSpell { target: Ref },
    Sacrifice { player: PlayerRef, filter: Filter, count: Amount },
    Sequence(Vec<Effect>),                          // "do A, then B"
    Conditional { if_: Condition, then: Box<Effect>, else_: Option<Box<Effect>> },
}

pub enum Amount { Const(i32), Count(Filter), LifeOf(PlayerRef), PowerOf(Ref), X }
pub enum Ref { Target(u8), This, Triggering, Each(Filter), Player(PlayerRef), .. }
pub enum PlayerRef { You, TargetOpponent(u8), EachOpponent, EachPlayer, Triggering, Owner(Box<Ref>) }
pub enum Filter { Creature, Land, Any, Other, Subtype(Subtype), ControlledBy(PlayerRef),
                  Tapped, Attacking, Blocking, PowerAtLeast(i32), And(Vec<Filter>), Or(Vec<Filter>), Not(Box<Filter>) }
pub enum Trigger { Etb { effects }, Dies { effects }, Attacks { effects }, CombatDamageToPlayer { effects },
                   Upkeep { whose: PlayerRef, effects }, EndStep { .. }, BecomesTapped { .. } }
pub enum Static { PtBoost { filter, power, toughness }, GrantKeyword { filter, keyword }, CostReduction { .. } }
pub enum Cost { Mana(ManaCost), Tap, SacrificeThis, Sacrifice(Filter), PayLife(i32), Discard(i32) }
```

**There is no `Opponent` singular.** Oracle text is already written for N players — "each opponent", "target opponent", "target player" — and the IR mirrors it exactly. A card that says "each opponent loses 2 life" is `LoseLife { player: EachOpponent, .. }` and works unchanged in a two-player game and a four-player pod. This is the one place multiplayer costs nothing if you're disciplined from the first card and costs a full cube rewrite if you're not. The renderer (§4.3) enforces it: there's no template for a bare "opponent".

The exact enum set is the v1 cube's vocabulary (§4.2); it grows only when a card needs it. Three derived artefacts fall out of these types for free:

- **A JSON Schema** via `schemars`, handed to the ingestion model as the contract for what it may emit.
- **A validator** (`cardir::validate`) that checks what the schema can't: target indices in range, `X` only on cards with `{X}` in cost, `Triggering` only inside a trigger, colour of `AddMana` consistent with colour identity, P/T present iff Creature.
- **An English renderer** (`cardir::render`) that turns IR back into templated Oracle-style text. This is the correctness oracle for ingestion (§4.3) and the fallback display text for any card that has no Scryfall entry (custom cards).

**Presentation will leak into the IR; here's the boundary.** Exact-match rendering means the IR must eventually distinguish things that are semantically identical but phrased differently — "it" vs. "that creature", clause order inside `Sequence`, "you may" placement. Two rules, decided now: (1) anything that changes *game behaviour* is a real field (so `Any` vs. `CreatureOrPlayer` are different filters — "any target" includes planeswalkers and battles — not a rendering choice); (2) anything that doesn't lives in one optional `hints: RenderHints` field on the node, `#[serde(default)]`, ignored by `engine::interp`, ignored by IR equality in tests, and never required — a node with no hints renders with the most common phrasing. If a round-trip fails only on a hint-level difference, the ingestion report classes it as "near", not "failed". The hint set is expected to stay under a dozen variants; if it grows past that, the renderer is doing the IR's job and the design should be revisited.

The engine consumes the IR through one interpreter module, `engine::interp`, which maps each `Effect` variant to engine operations. Adding a primitive means: add an enum variant, add an interpreter arm, add a renderer arm, add one test. That's the whole ceremony.

Card metadata that isn't behaviour (Scryfall id, set, rarity, artist, legalities) comes from `carddb`'s cached Scryfall bulk data (§4.5) and is joined by name at load time; it's never hand-typed. The IR file carries only what the engine needs plus the Oracle text for round-trip.

### 4.2 The starter cube

~200 cards, two colours deep in each of five colours plus a small artifact/land slice, sized so ten 40-card or four 60-card decks are possible. Selection criteria, in priority order:

1. Effect is one of: damage, destroy, exile, draw, discard, gain life, lose life, P/T modification, keyword grant, token creation, counter placement, mana production, tap/untap, return-to-hand, counter target spell.
2. Zero replacement effects, no copies, no control-change, no "as enters" choices, no X costs, no alternate costs, no additional costs beyond tap/sacrifice.
3. Keywords limited to: flying, first strike, double strike, deathtouch, lifelink, trample, vigilance, haste, reach, menace, defender, flash, hexproof, indestructible, prowess.
4. Prefer cards with real printings so Scryfall metadata exists.

`decks/` ships with five or six pre-built two-colour decks so a first game is `manaline play --deck decks/rg-aggro.txt` and nothing else.

Every cube card is hand-written IR with a hand-written test. That's ~200 labeled (Oracle text → IR) pairs, which is what makes §4.3 possible.

### 4.3 Card ingestion tool (`crates/ingest`)

A dev-time tool — never part of the game binary — that turns Oracle text into candidate IR so that adding a set is a review pass rather than a typing exercise. Its job is not to be right every time; its job is to be right *often* and to be *loud* when it can't be.

**Why this is tractable.** Oracle text is a controlled language. Wizards writes it to templates ("When ~ enters, …", "Target creature gets +N/+N until end of turn", "{T}: Add {G}"), the vocabulary is a few hundred phrases, and the same template recurs across hundreds of cards. Parsing it is not the hard problem. The hard problem is that a new card often needs a primitive the engine doesn't have, and no model can conjure an implementation for a missing feature. So the tool's contract is: **parse → map onto existing primitives → flag what's unsupported**.

**Pipeline**

```
Scryfall bulk JSON ──▶ select cards ──▶ ┌────────────────────────┐
                                        │  1. Generate            │  LLM (few-shot + JSON Schema)
                                        │     Oracle → IR         │  or fine-tuned model (§4.3.3)
                                        └───────────┬────────────┘
                                                    ▼
                                        ┌────────────────────────┐
                                        │  2. Validate            │  schema + cardir::validate
                                        │     retry ≤ 3 w/ errors │
                                        └───────────┬────────────┘
                                                    ▼
                                        ┌────────────────────────┐
                                        │  3. Round-trip          │  cardir::render(ir) ≈ oracle?
                                        │     normalise, diff     │
                                        └───────────┬────────────┘
                                                    ▼
                                        ┌────────────────────────┐
                                        │  4. Propose tests       │  LLM → TestGame scenario per effect
                                        │     compile + run       │
                                        └───────────┬────────────┘
                                                    ▼
                          report.md + data/<set>/*.ron + tests/<set>/*.rs   (human reviews, commits)
```

**Step 1 — Generate.** The default backend is a hosted LLM called with: the IR JSON Schema, ~20 few-shot pairs selected by embedding similarity to the input card's text (nearest neighbours from the existing cube — a card with an ETB trigger gets ETB examples), the list of supported keywords and primitives, and the instruction to emit `Unsupported { reason }` in place of any effect it cannot express with the given vocabulary. Structured-output / JSON mode where the API offers it. Backends are a trait so the fine-tuned model in §4.3.3 slots in.

**Step 2 — Validate.** Deserialize into `cardir::Card`, run `cardir::validate`. On failure, feed the error messages back and regenerate, up to three times. Most failures are target indices and `Amount` shapes, which models fix reliably when told.

**Step 3 — Round-trip.** Render the IR back to English with `cardir::render`, normalise both sides (lowercase, strip reminder text in parentheses, collapse whitespace, replace the card's own name with `~`), and compare. Exact match is *strong* evidence of correctness precisely because Oracle text is templated: if the renderer emits "Target creature gets +3/+3 until end of turn." and Scryfall says the same thing, the IR almost certainly encodes the right effect. Near-misses are diffed and shown in the report. This check is the single most valuable component of the tool and costs nothing per card.

**Step 4 — Propose tests.** For each effect on the card, the model proposes a `TestGame` scenario in the same builder used by hand-written tests ("battlefield: 2/2 bear (opp); cast this targeting it; assert it's in the graveyard"). Generated tests are compiled and run; failures land in the report. Generated tests are proposals for a human to skim, not proof — but a passing generated test plus an exact round-trip is a very high bar.

**Output.** One `.ron` per card, one test file per set, and a `report.md`:

```
INGEST  set=FDN  cards=281

  ✔ 203  exact round-trip, tests pass                 → data/fdn/*.ron
  ~  31  near round-trip (diff shown), tests pass      → review
  ✘  12  validation failed after 3 attempts            → review
  ⊘  35  unsupported primitives:
         17  ReplacementEffect          (e.g. "If ~ would die, exile it instead")
          9  ControlChange              (e.g. "Gain control of target creature")
          5  AsEntersChoice             (e.g. "As ~ enters, choose a color")
          4  Copy
```

The `⊘` section is a prioritised engine roadmap: implement `ReplacementEffect` and seventeen cards unlock at once.

#### 4.3.1 CLI

```
manaline ingest set <CODE> [--backend api|local] [--only <name>…] [--out crates/cards/data/<set>]
manaline ingest card "<Oracle text>"                 # one-off, prints IR + round-trip diff
manaline ingest roundtrip crates/cards/data/**       # regression: every committed card must round-trip
manaline ingest eval  --gold crates/cards/data/core  # score a backend against hand-written IR
```

`ingest roundtrip` runs in CI. A card whose hand-written IR doesn't render back to its Oracle text is either a renderer gap or a mis-encoded card; both are worth knowing.

#### 4.3.2 Training data

Three sources, in order of trust:

1. **The cube.** ~200 hand-written, tested pairs. Gold standard; also the eval set (hold out ~40).
2. **Ingestion output that passed review.** Every set added through the tool grows the corpus. Cards that needed a human fix are the most valuable examples of all and are tagged as such.
3. **Forge card scripts.** Forge encodes ~30k cards in a text DSL that is structurally very close to this IR. A mechanical Forge-script → IR converter for the primitives this engine supports yields thousands of real pairs and, as a side effect, an inventory of which Forge primitives have no equivalent here. Forge is GPL-3; use its scripts as training data for an internal dev tool, don't redistribute converted files.

#### 4.3.3 Fine-tuning path

The few-shot API backend is the baseline and is expected to be good enough for a long time. Fine-tuning is worth doing when one of three things is true: API cost per set becomes annoying, you want offline/local ingestion, or you want a measurable ML project with a hard correctness metric. The recipe:

- **Task.** Seq2seq, Oracle text (plus name, cost, types, P/T as a header) → IR serialised as compact JSON. Input ≤ 256 tokens, output ≤ 512.
- **Model.** Start with a ~1–3B instruct model + LoRA (fits on a single consumer GPU or a cheap cloud instance); CodeT5-class encoder-decoders are the alternative if you want something smaller and are willing to give up the few-shot fallback.
- **Data.** §4.3.2 sources, deduplicated by normalised Oracle text, stratified so no template dominates (there are a *lot* of vanilla creatures). Augment by re-templating: swap numbers, colours, subtypes and keywords in existing pairs — valid because the IR transforms identically.
- **Metric.** Not BLEU. Three hard metrics from the pipeline itself: schema-valid rate, exact round-trip rate, and generated-test pass rate. Report all three against the held-out cube cards and against the few-shot baseline. Beating the baseline on round-trip rate at a fraction of the cost is the success criterion.
- **Serving.** `--backend local` loads the model with `candle` or calls a local `llama.cpp`/`vLLM` endpoint; either way it's behind the same backend trait.

#### 4.3.4 What the tool will never do

- Run inside the game. The engine only ever loads committed, reviewed `.ron` files.
- Invent primitives. Anything outside the enum set is `Unsupported`, full stop.
- Commit on its own. A human reads the report and runs `git add`.

### 4.4 Formats (`crates/engine/src/format.rs`, `formats/*.ron`)

A format is data, loaded by the daemon at `create_game` and carried in `Game`. It answers three questions: how is the game set up, what are the extra rules, and which cards are allowed.

```ron
// formats/commander.ron
Format(
    name: "Commander",
    players: (min: 2, max: 6),
    starting_life: 40,
    starting_hand: 7,
    mulligan: London(free_first: true),      // house-rule toggle
    deck: Deck(size: Exact(100), singleton: true, includes_commander: true),
    rules: [Commander, CommanderDamage(21), ColorIdentity],
    legality: Legality(
        pool: Scryfall(format: "commander"),  // uses Scryfall's per-card `legalities.commander`
        banned: [],                           // additions on top of the pool
        allowed: [],                          // exceptions on top of the pool
    ),
)

// formats/cube.ron
Format(
    name: "Starter Cube",
    players: (min: 2, max: 2),
    starting_life: 20,
    starting_hand: 7,
    mulligan: London(free_first: false),
    deck: Deck(size: Range(40, 60), singleton: false, includes_commander: false),
    rules: [],
    legality: Legality(pool: Builtin("core"), banned: [], allowed: []),
)
```

```rust
pub struct Format {
    pub name: String,
    pub players: (u8, u8),
    pub starting_life: i32,
    pub starting_hand: u8,
    pub mulligan: MulliganRule,
    pub deck: DeckRules,
    pub rules: Vec<FormatRule>,        // Commander, CommanderDamage(n), ColorIdentity, Poison(n), …
    pub legality: Legality,
}

pub enum CardPool {
    Builtin(String),                   // a named card set compiled into the binary (`include_dir!`); "core" is the cube
    Dir(PathBuf),                      // a directory of IR files, relative to the manaline data dir — for custom/community sets
    Scryfall { format: String },       // Scryfall `legalities[format] == "legal"`, joined at load
    Sets(Vec<String>),                 // set codes
    All,
}
pub struct Legality { pub pool: CardPool, pub banned: Vec<String>, pub allowed: Vec<String> }
```

**Legality is checked at three points:** `manaline deck check`, `set_deck` in the lobby (rejects with a list of violations), and — belt and braces — `Game::new`, which refuses to construct an illegal game. A card is playable in a format iff it's in the pool (or `allowed`), not in `banned`, **and** the engine has an IR file for it. That last clause is the honest one: "Legacy" in this engine means "the Legacy-legal subset of cards we've implemented", and `deck check` says so, naming the cards it can't play.

**`FormatRule` is where the engine grows a format-specific rule.** Each variant is a hook: `Commander` adds the command zone, `CastCommander`, the tax, and the zone-change replacement; `CommanderDamage(21)` adds one state-based action; `ColorIdentity` adds one deck-check constraint. Legacy and Modern add nothing — they're purely a pool. The engine's default behaviour is two-player 20-life, and every rule is opt-in through this list, so a format can't accidentally inherit another format's quirks.

Two formats ship in the initial build: `cube` and `two-player` (same as cube with `pool: All`, so any implemented card is playable). `commander.ron`, `legacy.ron`, `modern.ron`, `standard.ron` are written at the same time as data — they're tiny — but their `FormatRule`s aren't implemented until the Commander milestone.

**Limited formats** add one more field. A format is *constructed* (build from the whole pool) or *limited* (build from a pool generated for you at game time):

```ron
// formats/sealed.ron
Format(
    name: "Sealed",
    players: (min: 2, max: 8),
    starting_life: 20,
    deck: Deck(size: Min(40), singleton: false, includes_commander: false),
    limited: Some(Limited(
        kind: Sealed(packs: 6),
        set: FromArgs,                   // `--set FDN` at create time
        basics: Unlimited,
        build_time: Some(1800),          // seconds before the lobby auto-readies you
    )),
    legality: Legality(pool: Set(FromArgs), banned: [], allowed: []),
    …
)
```

For a limited format the lobby generates each seat's pool from the game seed and the set's booster configuration (Scryfall's per-set data plus a rarity-slot approximation of collation; exact print-run collation is out of scope), sends it to that seat only, and `set_deck` is validated as *pool + basics ⊇ deck* on top of the usual checks. The pool is hidden information like a hand: other seats never see it. Draft is the same shape with a pick protocol on top (pack contents pushed as events, `pick { card }` messages, passing per the format) and is deferred behind sealed.

### 4.5 Deck building

**Every card is available to every player.** There is no collection, no economy, no unlocking, and never will be. The only constraints on what goes in a deck are the format's legality (§4.4), the format's limited pool if it has one, and whether the engine has an IR file for the card. This is stated as a design invariant so nothing downstream ever grows a "cards you own" concept.

**The deck file is the source of truth.** A deck is a plain-text file in the format every major tool already exports and imports — Arena, MTGO, Moxfield, Archidekt, Scryfall:

```
Deck
4 Lightning Strike (FDN) 154
4 Grizzly Bears
20 Mountain
…

Sideboard
2 Shock

Commander
1 Elvish Archdruid
```

Parsing is lenient: the `Deck` header is optional, set code and collector number are optional and used only for art/printing preference, `SB:` prefixes and `//` comments are accepted, and names are matched case-insensitively with a fuzzy fallback that *asks* rather than guesses. Decks live wherever the user wants; `~/.local/share/manaline/decks/` is the default and `manaline deck` commands take paths. JSON is not a second format — the text format is the interchange format the ecosystem already agreed on, and inventing another one would only make pasting from Moxfield harder.

**Two ways to build, one result.** Both produce that file, so they're interchangeable at any moment — edit in `$EDITOR`, open in the TUI to check the curve, edit in `$EDITOR` again. The TUI watches the file and reloads on external change; it writes back only on an explicit save, in canonical order (by type, then mana value, then name) so diffs stay readable.

1. **Text editor.** `manaline deck check <file>` validates against a format and prints violations (illegal, banned, not-yet-implemented, over the singleton limit, outside colour identity, outside your sealed pool) with the reason per card. `manaline deck new <name> --format <f>` writes a commented skeleton.
2. **In-client deckbuilder.** `manaline deck edit <file>` (also reachable from the lobby, and automatically at the start of a limited game) opens a three-pane TUI: search results, the deck list, and a card detail / analysis pane. Adding and removing is single-key; the list shows the deck's legality status live.

**Search** (`crates/cardsearch`) is a query language deliberately close to Scryfall's, since players already know it: `t:creature c:r mv<=2 o:"draw a card" is:implemented`. The index is built once from the cached Scryfall bulk data (all ~30k Oracle cards, not only implemented ones — seeing that a card exists but isn't implemented yet is useful, and `is:implemented` / `f:<format>` narrow it). The same crate serves `manaline cards search` on the command line and the MCP `search_cards` tool.

**Analysis** (`crates/deckstats`) is pure functions over a parsed deck plus the card database, rendered in the deckbuilder's third pane and by `manaline deck stats <file>`:

- Mana curve by type; average and median mana value.
- Colour requirements vs. sources: pip counts per colour against land and mana-producer counts, with the usual "you want ~N sources for a card with CC by turn 4" guidance computed hypergeometrically, not looked up.
- Type and subtype breakdown; creature count; interaction count (a heuristic over IR: `Destroy`, `DealDamage` to creatures, `CounterSpell`, …).
- Sample opening hands — drawn by the actual engine with the format's mulligan rule, so what you see is what a game would deal.
- Probability of having N lands / a given colour / a specific card by turn T.
- Legality summary: format, banned, unimplemented, identity, pool.

Because analysis reads the IR, not just card metadata, it can answer engine-aware questions ("how many of my cards can kill a 4-toughness creature?") that external tools can't.

**The deckbuilder is offline.** `deck edit`, `deck check`, and `deck stats` need no daemon and no network: they read the deck file, the local card cache, and the `engine` crate linked in-process (for sample hands and mulligans). The builder takes a *constraint source* — a `Format` plus an optional `Pool` — and doesn't care where they came from. In constructed play the format is a file. In sealed play the pool is fetched once from the lobby (`get_pool`, seat-private) and cached locally for that game; the finished deck goes back through `set_deck`; everything in between is the same offline builder. Never let the builder call the daemon for something convenient like legality — legality is a pure function of format, pool, and card database, and stays that way.

**Agents build decks too.** In a constructed game an agent's operator hands it a deck file, but in a sealed game the agent seat has a pool and must build from it. So deck building is exposed through the MCP server as well as the TUI: `get_pool`, `search_cards`, `deck_stats`, `submit_deck`. The rules primer (§7.3) gets a short "building a limited deck" section. This is also why search and analysis are crates rather than TUI code — three consumers, one implementation.

**Card data.** Scryfall bulk data ("Oracle Cards", ~40 MB) is downloaded on first use to the XDG cache directory and refreshed with `manaline cards update`; every command that needs it says how old the cache is. Deck files reference cards by name, so they're unaffected by cache age; legality and search are what go stale.

---

## 5. Daemon (`crates/daemon`)

`manaline-daemon` owns exactly one `Game` per process. Responsibilities:

- Run the lobby: create the game from a format, issue seat and spectator tokens, validate decks against the format, start when all seats are ready.
- Accept `format.players.max` seat connections plus any number of spectators, over whichever transports are enabled (`--socket`, `--tcp <addr>`).
- Validate `act` messages: correct seat, `state_version` current, action present in `legal_actions` (or, for division actions, passes the rule check — §3's carve-out), then `Game::apply`.
- Broadcast resulting `Event`s to subscribers, seat-filtered (a player doesn't see what another player drew; spectators see no hidden information).
- Persist `(seed, actions)` to the replay log after every accepted action.
- Publish `must_act` (and why each seat must act) on the watch channel so clients can block on it (see §7).
- Chat relay: `chat` messages from either seat are broadcast as events. This is how the in-game "AI has been prompted" nudge and any table talk flow.

Concurrency is intentionally boring: a single `tokio::sync::Mutex<Game>` and a `tokio::sync::watch` channel carrying `(state_version, must_act: BTreeMap<Seat, ActReason>, game_over: bool)`. `wait_for_turn` is literally `watch::Receiver::wait_for(|s| s.must_act.contains_key(&my_seat) || s.game_over)`, returning the reason from the map. This design already handles N seats; it's per-game, not per-player, and the daemon never needs to know how many clients are waiting.

**Remote seats** differ from local ones in exactly two ways: the token is the only authentication (no filesystem permissions to lean on), and disconnects are expected rather than fatal. Both are handled in the transport layer, not the game loop.

---

## 6. TUI (`crates/tui`)

ratatui, full-screen, 100×32 minimum, degrades gracefully to 80×24 by collapsing the log pane.

```
┌ manaline ── Turn 6 · Main 1 · You have priority ────────────────────── Seed 8a3f ┐
│ OPPONENT (Claude)  ♥ 14   Hand 4   Library 41   Graveyard 3                       │
│ ┌─────────┐┌─────────┐┌─────────┐┌─────────┐┌─────────┐                           │
│ │Forest   ││Forest   ││Mountain ││Llanowar ││Grizzly  │                           │
│ │        T││         ││         ││Elves    ││Bears    │                           │
│ │  {G}    ││  {G}    ││  {R}    ││ 1/1  T  ││ 2/2     │                           │
│ └─────────┘└─────────┘└─────────┘└─────────┘└─────────┘                           │
├─ STACK ────────────────────────────────────┬─ LOG ────────────────────────────────┤
│ 1. Lightning Strike → Grizzly Bears (opp)  │ T6  You cast Lightning Strike        │
│    (you)                                   │ T6  Claude: "Hm, fair enough."       │
│                                            │ T5  Claude attacks with Grizzly Bears│
│                                            │ T5  You take 2 (16 → 14)             │
├─ YOU ──────────────────────────────────────┴──────────────────────────────────────┤
│ ┌─────────┐┌─────────┐┌─────────┐┌─────────┐┌───────────┐                        │
│ │Mountain ││Mountain ││Plains   ││Boros    ││Kor Sky-   │                        │
│ │        T││        T││         ││Elite    ││fisher     │                        │
│ │  {R}    ││  {R}    ││  {W}    ││ 3/3     ││ 2/3 ✈     │                        │
│ └─────────┘└─────────┘└─────────┘└─────────┘└───────────┘                        │
│ YOU  ♥ 14   Pool: {R}                                                             │
├─ HAND ────────────────────────────────────────────────────────────────────────────┤
│ [1] Shock {R}   [2] Boros Charm {R}{W}   [3] Plains   [4] Ajani's Pridemate {1}{W}│
├───────────────────────────────────────────────────────────────────────────────────┤
│ [Space] pass priority  [1-9] play/cast  [a] attack  [i] inspect  [c] chat  [?] help│
└───────────────────────────────────────────────────────────────────────────────────┘
```

Design rules:

- **Every legal action has a key.** The footer is generated from `legal_actions`, so it's never wrong.
- **Cards are 11×4 boxes** with name (truncated), an indicator line (T for tapped, ★ for summoning sick, counters), and a stats line (P/T for creatures, mana symbol for lands, ✈ flying, ⚔ first strike, etc.). `i` on a card opens a popup with full Oracle text.
- **Colour is optional.** Everything reads in monochrome; colour reinforces (red border for attacking, dim for tapped, yellow highlight for selected/targetable).
- **The log pane is also the chat pane.** Agent chat arrives as events and interleaves with game events, so the human sees "Claude: 'Hm, fair enough'" right where it happened.
- **Mulligan, targeting, blocker assignment** are modal overlays; escape cancels.
- **The layout is a list of opponent panes, not "the opponent pane."** The initial build renders one; with N−1 opponents the top region becomes a row of collapsed panes (name, life, hand/library counts, creature count, commander damage you've taken from them) and `Tab` expands one at a time. Attack declaration and "target opponent" prompts pick a seat from that row. Building the two-player layout as the N=2 case of this costs an afternoon now and saves a rewrite later.

The waiting-on-agent nudge lives in the header line and reads from `must_act`, never from priority: when another seat must act, the header reads `Waiting on Claude (seat 1) to declare blockers…` — the reason is the same `reason` value `wait_for_turn` returns — and, if no action arrives within a configurable timeout (default 30 s), the footer switches to `Seat 1 must declare blockers — nudge your agent, or [Enter] to re-notify`. Pressing Enter does two things, neither of which touches the watch channel (a seat that must act has already been released from `wait_for_turn`; re-publishing the same value wakes nobody): it sends a `must_act` event directly on that seat's connection — which helps a client that is connected but idle, and is what the MCP server surfaces if the agent is mid-`get_state` — and it appends a chat event `"[system] Claude, you need to declare blockers."` so an agent that only looks at the log sees it there. The chat line is the part that reliably does the work; the direct event is a courtesy.

---

## 7. MCP server (`crates/mcp`)

Built on `rmcp`. Runs as `manaline mcp --game <id> --seat 2` and speaks **both** transports:

- **Streamable HTTP** on `127.0.0.1:7454` (default) for clients that prefer URL config.
- **stdio** when invoked with `--stdio`, for clients that only launch processes. In this mode the MCP process is a thin proxy; the daemon is still where state lives.

### 7.1 Tools

| Tool                 | Input                                  | Returns                                                                                          |
|----------------------|----------------------------------------|--------------------------------------------------------------------------------------------------|
| `get_game_state`     | —                                      | Seat-filtered `GameView` as structured JSON **and** a compact text rendering (§7.2)              |
| `get_legal_actions`  | —                                      | Numbered list of legal actions with human-readable descriptions; empty list if your seat is not in `must_act` |
| `take_action`        | `{ action_id: u32 }` or `{ action: Action }` | Result: events that occurred, new state, and — if your seat is still in `must_act` — the next legal actions and the reason |
| `wait_for_turn`      | `{ timeout_seconds?: u32 }` (default 300) | Blocks until this seat **must act** or the game ends; returns state + legal actions and says *why* — the engine's `ActReason` for this seat, verbatim: `"priority" \| "declare_attackers" \| "declare_blockers" \| "assign_damage" \| "mulligan" \| "bottom_cards" \| "discard" \| "choice"`. Returns `{ timed_out: true }` on timeout so the agent can loop |
| `get_card`           | `{ name: string }` or `{ object_id }`, `{ include_ir?: bool }` | Full Oracle text, types, P/T, plus current effective stats and modifiers if on the battlefield. With `include_ir`, the card's IR too — useful for agents that want the unambiguous semantics rather than the prose |
| `get_log`            | `{ since_turn?: u32 }`                 | Event log including chat, seat-filtered                                                          |
| `say`                | `{ text: string }`                     | Posts chat to the table; appears in the human's log pane                                        |
| `concede`            | —                                      | Ends the game                                                                                    |
| `search_cards`       | `{ query: string, limit?: u32 }`       | Scryfall-style search over the card database (§4.5); `is:implemented` and `f:<format>` filters |
| `get_pool`           | —                                      | This seat's limited pool, if the format has one                                                  |
| `deck_stats`         | `{ decklist: string }`                 | Curve, colour sources, legality violations — same analysis the TUI shows                        |
| `submit_deck`        | `{ decklist: string, commander?: string }` | Validates against format (and pool) and readies the seat; returns violations on failure     |

`take_action` accepting either an `action_id` (from the last `get_legal_actions`) or a full `Action` matters: ids are convenient but go stale if state changes between calls; the full form is unambiguous. Ids are validated against a `state_version` and rejected with a helpful error if stale.

### 7.2 Text rendering for models

Models reason better over a compact, consistent text view than over raw JSON, so every state-returning tool includes both. Example:

```
TURN 6 · MAIN 1 · Active: opponent · Priority: YOU
Stack: (empty)

OPPONENT  life 14  hand 4  library 41  grave 3
  battlefield:
    #12 Mountain (T)  #13 Mountain (T)  #14 Plains  #15 Boros Elite 3/3  #16 Kor Skyfisher 2/3 flying

YOU  life 14  hand 3  library 40  grave 2  pool: -
  battlefield:
    #21 Forest (T)  #22 Forest  #23 Mountain  #24 Llanowar Elves 1/1 (T)  #25 Grizzly Bears 2/2
  hand:
    #31 Giant Growth {G} (castable)  #32 Shock {R} (castable)  #33 Forest (land, already played this turn)

LEGAL ACTIONS
  1. Pass priority
  2. Cast Giant Growth {G} → #25 Grizzly Bears (tap #22)
  3. Cast Giant Growth {G} → #24 Llanowar Elves (tap #22)
  4. Cast Shock {R} → #15 Boros Elite (tap #23)
  5. Cast Shock {R} → #16 Kor Skyfisher (tap #23)
  6. Cast Shock {R} → opponent (tap #23)
```

Object ids are stable for the life of the game and appear everywhere so the agent can refer to things unambiguously. In a multiplayer game the rendering lists every seat by name and seat number, marks eliminated players, and shows commander damage where the format has it; the agent's own section is always labelled `YOU`.

### 7.3 Resources and prompts

- Resource `manaline://rules-primer` — a ~1,500-word plain-English summary of turn structure, priority, combat, and the stack, written for an agent that has never played. Agents that read it first play noticeably better.
- Resource `manaline://cube` — the full card list with Oracle text.
- Prompt `play-a-game` — the recommended agent loop, as an MCP prompt so any client can pull it: *read the primer; loop { wait_for_turn; think; take_action until you pass; }; use `say` to talk to your opponent.*

### 7.4 The agent loop, end to end

1. Human runs `manaline play --deck decks/rw.txt --vs claude --opp-deck decks/ug.txt`. Per §2.1 this silently starts the daemon, opens the TUI in seat 0, starts the MCP server bound to seat 1, and prints the MCP connection snippet (URL, stdio command, config JSON) in the TUI's log pane. Nothing else is required of the human.
2. Human adds the server to Claude Code / Codex in another window and says something like "you're playing Magic against me, pull the `play-a-game` prompt and go."
3. Agent calls `wait_for_turn`. Mulligan decision arrives first.
4. Game proceeds. Whenever the agent must act — priority, blockers, a choice — the blocked `wait_for_turn` returns with state and legal actions. The agent acts, possibly several times (cast, then pass), then calls `wait_for_turn` again.
5. If the agent's client kills long tool calls (some do at 60 s), `wait_for_turn` returns `{ timed_out: true }` and the agent just calls it again. Meanwhile the human sees `Waiting on Claude…` in the TUI and can press Enter to post a system chat nudge, which the agent sees the next time it looks.
6. Human and agent can `say` / `[c]hat` at any time. The human is also free to talk to the agent directly in the agent's own window — that conversation is outside the game and the game doesn't know about it.

---

## 8. CLI (`crates/cli`)

```
manaline play      --deck <file> [--vs mcp|human|random] [--opp-deck <file>] [--format <name>] [--seed <u64>]
manaline host      --format <name> --seats <n> [--tcp <addr>]     # tier 0: prints one join token per seat
manaline join      <host:port> --token <t> --deck <file>          # tier 0: direct to a friend's daemon
manaline join      <code> [--server wss://…] --deck <file>        # tier 1/2: via a lobby server
manaline create    --format <name> --seats <n> [--server wss://…] # tier 1/2: prints the game code
manaline queue     --format <name> [--seats <n>] [--server wss://…] --deck <file>   # matchmaking (future)
manaline server    --listen <addr> [--tls-cert … --tls-key …] [--state-dir …]   # tier 1: run a lobby
manaline daemon    --game <id> [--socket <path>] [--tcp <addr>]
manaline tui       --game <id> --seat <n> | --token <t>
manaline mcp       --game <id> --seat <n> | --token <t> [--http <addr> | --stdio]
manaline replay    <file.jsonl> [--step]
manaline cards     search <query> | show <card> | update       # card database (§4.5)
manaline deck      new <name> --format <f> | check <file> [--format <f>] | stats <file> | edit <file>
manaline ingest    set | card | roundtrip | eval     # dev tool, see §4.3.1
```

`play` is the only command most people ever run; it orchestrates the others as child processes and tears them down together (§2.1). `--vs` accepts `random`, `human`, `mcp`, and named presets (`claude`, `codex`, …) that are just `mcp` plus a config snippet tailored to that client. `host` / `join` are the networked-play entry points and are stubs until that milestone — but the daemon flags they wrap exist from M1.

---

## 9. Milestones

Each milestone ends with something you can run.

**M0 — Engine skeleton (1–2 weeks)**
`Format` struct and `cube.ron`. N-player state (`Vec<PlayerState>`, `turn_order`, elimination). Turn/phase state machine, `must_act` and the auto-advance loop, priority passing, lands, vanilla creatures, combat with no keywords — including `AssignCombatDamage` enumeration at the start of the combat damage step, because random bots will multi-block on turn three — state-based actions (lethal damage, 0 life). `--vs random` opponent that picks uniformly from `legal_actions`. Property test in place **and run at N=2, 3, and 4 seats** — three random bots in a pod is the cheapest multiplayer test you'll ever write. *Runnable: random bots finish a game of Forests and Grizzly Bears without panicking, at any seat count.*

**M1 — Daemon + TUI (1–2 weeks)**
Protocol crate, daemon with lobby, seat tokens, `--socket` and `--tcp` listeners, replay log. TUI rendering battlefield/hand/stack/log with keyboard actions generated from `legal_actions`; opponent region built as a list of panes. *Runnable: a human beats the random bot in the terminal. Also runnable, quietly: two TUIs on one machine via `--tcp 127.0.0.1`, which is networked play in everything but the WAN.*

**M2 — MCP (1 week)**
`rmcp` server with all tools in §7.1, both transports, the rules primer resource, the `play-a-game` prompt, the Enter-to-nudge fallback. *Runnable: a human plays Claude Code. This is the demo.*

**M3 — IR, spells, abilities, keywords (2–3 weeks)**
`cardir` types, validator, and renderer. `engine::interp`. Stack with instants/sorceries, targeting, mana payment solver, triggered abilities, evergreen keywords, "until end of turn" modifiers, auras and equipment. First ~60 cards as `.ron` with round-trip tests. The IR vocabulary settles here — expect to reshape the enums a few times before it does, and don't start M5 until it has.

**M4 — The cube and deck files (2–3 weeks, parallelisable)**
Remaining ~140 cards, one test each. `carddb` with the Scryfall bulk cache and `cards update`. `deckstats` parser for the text deck format, `deck check` with per-card reasons, `deck new`. Six starter decks. `ingest roundtrip` in CI.

**M5 — Ingestion tool, few-shot backend (1–2 weeks)**
`ingest` crate: API backend, embedding-selected few-shot examples, validate/retry loop, round-trip diff, report. `ingest eval` scored against the held-out cube cards. *Runnable: point it at a real set and get a report with a supported/unsupported split.* First real use: whichever set you most want to play with.

**M6 — Polish and the deckbuilder**
Mulligans, 80×24 layout, colour themes, replay stepping in the TUI, `cards show` with ASCII card render, README with a screencast. `cardsearch` query language and index; `deck stats`; the three-pane `deck edit` TUI reading and writing the same file, with live legality and sample hands drawn by the engine. `search_cards` / `deck_stats` on the MCP server.

**M7 — Fine-tuned ingestion backend (optional, 2–4 weeks)**
Forge-script → IR converter for supported primitives, dataset assembly and augmentation, LoRA fine-tune, `--backend local`, eval against the M5 baseline on schema-valid / round-trip / test-pass rates.

**After the initial build — the long-term goals, in the order they're likely to be pulled in:**

**M8 — Networked play.** Tier 0: `host` / `join`, token auth on TCP, reconnect handling, idle-seat policy. Tier 1: `manaline server` — the lobby, many games per process, WebSocket-over-TLS listener, durable action log with replay-on-restart, `create` / `join <code>`. The protocol and daemon were built for this from M1, so this milestone is mostly the lobby, the WS transport, hardening, and a lot of testing over a bad connection. *Runnable: a friend joins your game from their house — directly, or through a server you run on a VPS.* Tier 2 (the hosted instance) is an ops decision, not a code milestone.

**M9 — Commander.** `FormatRule::Commander`, `CommanderDamage`, `ColorIdentity`; command zone, tax, zone-change choice; `deck check` for 100-singleton and identity; TUI pod layout with commander damage; the `Scryfall` card pool so banlists come from Scryfall's legality data. Then run `ingest set` on a few Commander staples and let the report tell you which engine primitives a real Commander pod needs. *Runnable: a four-seat pod, any mix of humans and agents.*

**M10 — Constructed formats.** `legacy.ron`, `modern.ron`, `standard.ron` are data; the work is card coverage, driven by `ingest` reports per format. Replacement effects and control change are almost certainly the first two primitives these force.

**M10.5 — Sealed.** `Limited` in `Format`, pool generation from seed and set booster config, seat-private `get_pool`, `set_deck` validated against the pool, the deck-building phase in the lobby with a timer, `deck edit` in pool mode, `get_pool` / `submit_deck` on the MCP server so agents can build. Draft afterwards, as a pick protocol on the same pool machinery.

**M11 — Matchmaking.** The queue in §2.2.1: `queue` / `cancel_queue`, per-format waiting lists, `try_match`, the `matched` event, a widening rule and a ready check. Small, because the lobby was shaped for it from M8. *Runnable: `manaline queue --format cube` finds you a stranger.*

Also on the list: agent-vs-agent mode (N MCP seats, no TUI — the benchmark harness), spectator TUI, a community format for contributed card sets built on the IR.

---

## 10. Open questions

- **Hidden information leakage through `legal_actions`.** Enumerating castable spells reveals nothing an opponent shouldn't know since each seat only gets its own list, but the *count* of legal actions in an event could leak hand contents. Don't include it in broadcast events.
- **Mana payment enumeration blow-up.** With many lands of overlapping colours, "one `CastSpell` per distinct payment" gets long — and it multiplies against target enumeration, so a two-target spell with five legal targets and three payment shapes is 60 actions. Plan: enumerate by *colour combination tapped*, not by specific permanents, and let the engine pick concrete permanents (preferring lands over mana creatures, basic over non-basic); cap the product and fall back to a two-step "choose targets, then payment" pending choice above the cap. Revisit if agents seem confused.
- **Timeout defaults for `wait_for_turn`.** 300 s is a guess. Instrument what clients actually tolerate.
- **Should `say` be rate-limited?** Probably not for v1; a chatty agent is a feature.
- **IR expressiveness vs. simplicity.** `Conditional` and `Sequence` are already a small programming language. The temptation will be to keep adding combinators until the IR is a general-purpose interpreter, at which point it's harder to generate and harder to render. Rule of thumb: add a *named* primitive (`ReturnToHand`) over a *composed* one (`Move { from: Battlefield, to: Hand }`) whenever the named version matches a recurring Oracle template. Named primitives render deterministically; composed ones don't.
- **Renderer fidelity.** Round-trip only works if the renderer produces Wizards' exact phrasing, including the fiddly bits ("enters" vs. "enters the battlefield" post-2024, "any target", "that creature" vs. "it"). Budget for the renderer to be as much work as the validator. Test it against the cube from day one.
- **Multiplayer combat.** Attacking a specific player is already in the action model, but "attacks each opponent" effects, goad, and the multiplayer variant of "defending player" need care in the interpreter. Scope these when Commander is actually built, not before.
- **Multiplayer priority and agents.** In a four-seat pod, three of the seats may be agents waiting on `wait_for_turn`. That's fine for the daemon, but the human's "nudge" key needs to nudge the seat(s) in `must_act`, and the header needs to say which seat that is. Small, but easy to get wrong.
- **Legality data freshness.** Scryfall's `legalities` change with every banlist update. Cache the bulk file with its date and surface it in `deck check` output ("legality as of 2026-08-30") so nobody's confused by a stale ban.
- **Hidden information over the network.** Seat-filtered views are the only defence — the daemon must never send another seat's hidden zones over the wire, even to a client that could "just ignore" them. Worth a test that diffs the raw bytes on a remote connection against the spectator view.
- **Interpretation overhead.** Interpreting an AST per effect is slower than a closure. At two-player cube scale it will not matter; if a profile ever says otherwise, compile IR to closures once at load time — the IR stays the source of truth either way.

---

## Sources

- [modelcontextprotocol/rust-sdk (rmcp)](https://github.com/modelcontextprotocol/rust-sdk) — official Rust MCP SDK, streamable HTTP and stdio transports
- [rmcp on crates.io](https://crates.io/crates/rmcp)
- [Build an MCP server in Rust with rmcp](https://dev.to/gde/build-an-mcp-server-in-rust-with-rmcp-a-walk-through-4cif)
