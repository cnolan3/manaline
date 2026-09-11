//! The MCP server: tools, the play-a-game prompt, and the two resources (§7).

use crate::primer::RULES_PRIMER;
use crate::render::{outcome_text, reason_text, render_legal, render_state};
use crate::session::{describe_client_error, Session, Wait};
use engine::{Action, GameView, ObjectId, Outcome};
use protocol::{ClientError, LegalAction};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CacheScope, CallToolResult, ContentBlock, ErrorData, Implementation, ListResourcesResult, PaginatedRequestParams, PromptMessage,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{prompt, prompt_handler, prompt_router, tool, tool_handler, tool_router, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

pub const PRIMER_URI: &str = "manaline://rules-primer";
pub const CUBE_URI: &str = "manaline://cube";
/// Short enough to come back before any MCP client gives up on the call.
pub const DEFAULT_WAIT_SECS: u64 = 45;

/// What every MCP session in this process shares: the card database, the
/// format, the search index, and the runtime directory games and
/// deckbuilders announce themselves in. Seats are *not* in here — one
/// session, one seat (§7).
#[derive(Clone)]
pub struct Shared {
    pub cards: Arc<engine::CardDb>,
    pub format: engine::Format,
    index: std::sync::OnceLock<Arc<cardsearch::Index>>,
    pub runtime: protocol::endpoint::Runtime,
    /// The seat handed to this process on the command line (`manaline mcp
    /// --connect --token`): every session on it plays that one seat instead
    /// of claiming its own from the runtime directory.
    fixed_seat: Option<Arc<Session>>,
}

/// The seat one MCP session holds: the daemon connection, and the claim in
/// the runtime directory that keeps other agents out of it. Dropping it
/// releases the claim.
struct Seated {
    session: Arc<Session>,
    /// The claim this seat was taken with; `None` for a seat given on the
    /// command line, which no other agent could take anyway.
    claim: Option<protocol::endpoint::SeatClaim>,
}

/// The handler rmcp serves. One of these per MCP session: over stdio that is
/// one per process, over streamable HTTP one per client session, each with
/// its own seat at the table over the one set of shared card data.
pub struct McpServer {
    shared: Arc<Shared>,
    /// This session's seat, or `None` while it is serving card data only.
    seat: std::sync::RwLock<Option<Seated>>,
    /// Held while seating, so two tool calls racing in one session cannot
    /// claim two seats.
    seating: tokio::sync::Mutex<()>,
}

impl Clone for McpServer {
    /// A clone is a *new session* over the same shared state: it never shares
    /// this session's seat, so a cloned handler cannot act for it.
    fn clone(&self) -> McpServer {
        self.new_session()
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TakeActionParams {
    /// The id of an action from the last `get_legal_actions` or `wait_for_turn` result.
    #[serde(default)]
    pub action_id: Option<u32>,
    /// A full action object instead of an id (the `action` field of a legal action entry).
    #[serde(default)]
    pub action: Option<serde_json::Value>,
    /// The `state_version` the action list came from. If the game has moved on, the action is
    /// refused instead of being applied to a different situation. Recommended with `action_id`.
    #[serde(default)]
    pub state_version: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaitParams {
    /// Give up after this many seconds and return `{ "timed_out": true }`. Default 45.
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
    /// Pass priority for you whenever passing (or conceding) is your only option, and keep
    /// waiting, so you are only woken when there is a real decision. Default true.
    #[serde(default)]
    pub auto_pass: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetCardParams {
    /// A card name, e.g. "Grizzly Bears".
    #[serde(default)]
    pub name: Option<String>,
    /// An object id from the state, e.g. 12 for #12.
    #[serde(default)]
    pub object_id: Option<u32>,
    /// Include the card's engine definition as well as the prose.
    #[serde(default)]
    pub include_ir: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetLogParams {
    /// Only lines from this turn onwards.
    #[serde(default)]
    pub since_turn: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SayParams {
    /// What to say. (`message` is accepted as an alias.)
    #[serde(alias = "message")]
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchParams {
    /// Scryfall-style query, e.g. `t:creature c:g mv<=2 o:"draw a card" kw:flying`.
    pub query: String,
    /// Most results to return. Default 20.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Include cards the engine cannot play yet. Default false: only playable cards.
    #[serde(default)]
    pub include_unimplemented: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DeckStatsParams {
    /// A decklist in the standard text format: one `N Card Name` per line.
    pub decklist: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SubmitDeckParams {
    /// The `name` of a deck from `list_decks`, to play it as-is. Pass this or `decklist`.
    #[serde(default)]
    pub name: Option<String>,
    /// A decklist in the standard text format: one `N Card Name` per line.
    #[serde(default)]
    pub decklist: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetDeckParams {
    /// A deck from `list_decks` or a file path.
    pub name: String,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct SitDownParams {
    /// The seat number to take. Omit to take the next free agent seat.
    #[serde(default)]
    pub seat: Option<u8>,
}

/// A deck reachable by name (any of the deck directories) or by path.
struct DeckSource {
    name: String,
    /// The directory it was found in, for the listing.
    origin: String,
    text: String,
}

/// Every deck reachable by name, in `cards::deck_dirs` order (first match wins).
fn available_decks() -> Vec<DeckSource> {
    let mut out = Vec::new();
    for name in cards::deck_names() {
        let Some(path) = cards::deck_path(&name) else { continue };
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let origin = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
        out.push(DeckSource { name, origin, text });
    }
    out
}

/// Resolve a deck by name or, failing that, as a file path.
fn find_deck(name: &str) -> Option<DeckSource> {
    let name = name.trim();
    if let Some(path) = cards::deck_path(name) {
        let text = std::fs::read_to_string(&path).ok()?;
        let origin = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
        return Some(DeckSource {
            name: name.trim_end_matches(".txt").to_string(),
            origin,
            text,
        });
    }
    let text = std::fs::read_to_string(name).ok()?;
    Some(DeckSource {
        name: name.to_string(),
        origin: "file".into(),
        text,
    })
}

/// The file the human has open in the deckbuilder, if exactly one editor is running.
fn open_editor_in(runtime: &protocol::endpoint::Runtime) -> Result<Option<protocol::endpoint::EditorSession>, String> {
    let mut live = runtime.live_editors();
    match live.len() {
        0 => Ok(None),
        1 => Ok(live.pop()),
        _ => Err(format!(
            "several deckbuilders are open ({}); name the file with `path`",
            live.iter().map(|e| e.path.display().to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// The agent seats a game publishes, as "1, 2" (for an error message).
fn agent_seats(game: &protocol::endpoint::GameMarker) -> String {
    let seats: Vec<String> = game
        .seats
        .iter()
        .filter(|s| s.kind == protocol::endpoint::SeatKind::Agent)
        .map(|s| s.seat.to_string())
        .collect();
    if seats.is_empty() {
        "none".into()
    } else {
        seats.join(", ")
    }
}

/// Tools that drive the human's open deckbuilder; hidden from a seat in a game.
const DECKBUILDING_ONLY_TOOLS: &[&str] = &[
    "editor_status",
    "editor_deck",
    "editor_add_card",
    "editor_remove_card",
    "editor_set_count",
    "editor_replace_deck",
    "editor_undo",
    "editor_stats",
    "editor_save",
];

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EditorCardParams {
    /// The card's name.
    pub name: String,
    /// How many copies. Default 1.
    #[serde(default)]
    pub count: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EditorRemoveParams {
    /// The card's name.
    pub name: String,
    /// How many copies to remove. Default 1.
    #[serde(default)]
    pub count: Option<u32>,
    /// Remove every copy.
    #[serde(default)]
    pub all: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EditorSetCountParams {
    /// The card's name.
    pub name: String,
    /// The exact number of copies; 0 removes it.
    pub count: u32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EditorReplaceParams {
    /// The whole main deck, in the standard text format: one `N Card Name` per line.
    pub decklist: String,
}

fn text_and_json(text: String, json: serde_json::Value) -> CallToolResult {
    let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
    r.structured_content = Some(json);
    r
}

/// Priority with nothing to do: every legal action is a pass or a concession.
fn superseded(auto_passed: u32) -> CallToolResult {
    text_and_json(
        "This wait was superseded by a newer call and did nothing further.".to_string(),
        serde_json::json!({ "superseded": true, "auto_passed": auto_passed }),
    )
}

fn nothing_to_do(legal: &[LegalAction]) -> bool {
    !legal.is_empty() && legal.iter().all(|l| matches!(l.action, Action::PassPriority | Action::Concede))
}

/// The reply for game tools when no game is published to sit down at.
fn no_game() -> CallToolResult {
    tool_error(
        "No game is connected: no game is published for agents right now, so this session is serving card data only \
         (search_cards, deck_stats, get_card, list_decks, and the resources). Ask the human to start a game with \
         `manaline play --vs claude`; your next game tool call (or `sit_down`) will take a seat at it.",
    )
}

fn status_text(st: &protocol::editor::EditorStatus) -> String {
    format!(
        "Deckbuilder on {} ({} format): {}{}{}",
        st.path.display(),
        st.format,
        st.legality,
        if st.dirty { " · unsaved changes" } else { "" },
        st.last_agent_action
            .as_ref()
            .map(|a| format!(" · last agent action: {a}"))
            .unwrap_or_default()
    )
}

/// Deck-level and per-card problems from a check report, as one list of strings.
fn problems_of(report: &deckstats::CheckReport) -> Vec<String> {
    report
        .deck
        .iter()
        .map(ToString::to_string)
        .chain(
            report
                .lines
                .iter()
                .filter(|l| l.status != deckstats::CardStatus::Ok)
                .map(|l| format!("{}: {}", l.name, l.status)),
        )
        .collect()
}

fn tool_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

impl McpServer {
    /// A server already holding a seat: `manaline mcp --connect --token`.
    /// Every session it serves plays that seat.
    pub fn new(session: Arc<Session>) -> McpServer {
        let shared = Shared {
            cards: Arc::new(session.cards.clone()),
            format: session.format.clone(),
            index: std::sync::OnceLock::new(),
            runtime: protocol::endpoint::Runtime::default(),
            fixed_seat: Some(session),
        };
        McpServer::from_shared(Arc::new(shared))
    }

    /// A server with no seat: card data, search, and deck analysis, plus a
    /// seat at whatever game is published when a game tool is first called.
    pub fn standalone(format: engine::Format) -> McpServer {
        let shared = Shared {
            cards: Arc::new(cards::core()),
            format,
            index: std::sync::OnceLock::new(),
            runtime: protocol::endpoint::Runtime::default(),
            fixed_seat: None,
        };
        McpServer::from_shared(Arc::new(shared))
    }

    fn from_shared(shared: Arc<Shared>) -> McpServer {
        let seat = shared.fixed_seat.clone().map(|session| Seated { session, claim: None });
        McpServer {
            shared,
            seat: std::sync::RwLock::new(seat),
            seating: tokio::sync::Mutex::new(()),
        }
    }

    /// A handler for one new MCP session: the same cards, index and runtime
    /// directory, its own seat. `serve_http` makes one per client session so
    /// several agents can play each other through one process.
    pub fn new_session(&self) -> McpServer {
        McpServer::from_shared(self.shared.clone())
    }

    /// Look for games and deckbuilders under this runtime directory instead
    /// of the user's.
    pub fn with_runtime(mut self, runtime: protocol::endpoint::Runtime) -> McpServer {
        Arc::make_mut(&mut self.shared).runtime = runtime;
        self
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    pub fn cards(&self) -> &Arc<engine::CardDb> {
        &self.shared.cards
    }

    pub fn format(&self) -> &engine::Format {
        &self.shared.format
    }

    /// Where games and deckbuilders announce themselves.
    pub fn runtime(&self) -> &protocol::endpoint::Runtime {
        &self.shared.runtime
    }

    /// The seat this session plays, if it has sat down.
    pub fn session(&self) -> Option<Arc<Session>> {
        self.seat.read().unwrap().as_ref().map(|s| s.session.clone())
    }

    /// The seat number this session holds, if any.
    pub fn seat(&self) -> Option<engine::Seat> {
        self.session().map(|s| s.me)
    }

    /// "card data only" or "game <id>, seat <n>", for this session.
    pub fn mode(&self) -> String {
        match self.session() {
            Some(s) => format!("game {}, seat {}", s.game_id, s.me.0),
            None => "card data only".into(),
        }
    }

    /// This session's seat, sitting down at the published game if it has none.
    /// Every game tool goes through here, so an agent that simply starts
    /// playing ends up at the table without being told where it is.
    async fn ensure_seated(&self) -> Result<Arc<Session>, CallToolResult> {
        if let Some(session) = self.session() {
            return Ok(session);
        }
        self.take_seat(None).await
    }

    /// The published seat this session is sitting in, as the marker described
    /// it (`None` for a seat given on the command line).
    fn claimed_slot(&self) -> Option<protocol::endpoint::SeatSlot> {
        let seat = self.seat.read().unwrap();
        seat.as_ref()?.claim.as_ref().map(|c| c.slot.clone())
    }

    /// Claim a seat at the newest published game and connect to it. The
    /// session keeps the claim until it `leave`s, so the final state is still
    /// readable after the game ends.
    async fn take_seat(&self, want: Option<u8>) -> Result<Arc<Session>, CallToolResult> {
        let _seating = self.seating.lock().await;
        if let Some(session) = self.session() {
            // Another call in this session sat down while we waited for the lock.
            return Ok(session);
        }
        let runtime = self.runtime();
        let Some(game) = runtime.newest_game() else {
            return Err(no_game());
        };
        let claim = match runtime.claim_seat(&game, want) {
            Ok(Some(c)) => c,
            Ok(None) => {
                let taken: Vec<String> = game.claims().iter().map(|(s, pid)| format!("seat {s} (pid {pid})")).collect();
                return Err(tool_error(match want {
                    Some(n) => format!(
                        "seat {n} at {} is not a free agent seat (agent seats: {}; claimed: {}). \
                         Call sit_down with no seat to take the next free one.",
                        game.game_id,
                        agent_seats(&game),
                        if taken.is_empty() { "none".into() } else { taken.join(", ") }
                    ),
                    None => format!(
                        "every agent seat at {} is taken ({}); ask the human to start a game with an agent seat.",
                        game.game_id,
                        if taken.is_empty() {
                            "the game has no agent seats".into()
                        } else {
                            taken.join(", ")
                        }
                    ),
                }));
            }
            Err(e) => return Err(tool_error(format!("could not claim a seat at {}: {e}", game.game_id))),
        };
        let Some(endpoint) = game.endpoint() else {
            return Err(tool_error(format!("game {} publishes no address to connect to", game.game_id)));
        };
        let Some(token) = claim.slot.token.clone() else {
            return Err(tool_error(format!(
                "seat {} at {} publishes no token, so it cannot be played by an agent",
                claim.seat(),
                game.game_id
            )));
        };
        let decklist = claim.slot.deck.as_deref().and_then(|d| match find_deck(d) {
            Some(found) => Some(found.text),
            None => {
                tracing::warn!("seat {} names deck {d:?}, which could not be read", claim.seat());
                None
            }
        });
        let config = crate::SessionConfig {
            endpoint,
            token,
            name: claim.slot.name.clone(),
            decklist,
        };
        let session = match Session::connect(config).await {
            Ok(s) => s,
            Err(e) => {
                claim.release();
                return Err(tool_error(format!("could not sit down at {}: {e:#}", game.game_id)));
            }
        };
        *self.seat.write().unwrap() = Some(Seated {
            session: session.clone(),
            claim: Some(claim),
        });
        Ok(session)
    }

    /// Give up this session's seat: the claim goes back so another agent may
    /// take it, and the daemon connection is dropped.
    fn give_up_seat(&self) -> Option<Arc<Session>> {
        let seated = self.seat.write().unwrap().take()?;
        if let Some(claim) = seated.claim {
            claim.release();
        }
        Some(seated.session)
    }

    pub fn card_index(&self) -> Arc<cardsearch::Index> {
        self.shared
            .index
            .get_or_init(|| Arc::new(cardsearch::Index::load(self.cards())))
            .clone()
    }

    /// Card count, colour letters, legality, and problems for a decklist.
    fn summarise(&self, text: &str) -> (u32, String, bool, Vec<String>) {
        let Ok(list) = deckstats::parse(text) else {
            return (0, String::new(), false, vec!["unreadable decklist".into()]);
        };
        let res = list.resolve(self.cards());
        let stats = deckstats::Stats::compute(&res.deck, self.cards());
        let report = deckstats::check::check(&list, self.format(), self.cards(), None);
        let colours: String = stats.pips.keys().map(|c| c.symbol()).collect();
        (list.main_count(), colours, report.is_legal(), problems_of(&report))
    }

    /// The `deck_stats` reply for a decklist.
    fn deck_stats_result(&self, decklist: &str) -> CallToolResult {
        let list = match deckstats::parse(decklist) {
            Ok(l) => l,
            Err(e) => return tool_error(format!("could not read the decklist: {e}")),
        };
        let db = self.cards().clone();
        let res = list.resolve(&db);
        let stats = deckstats::Stats::compute(&res.deck, &db);
        let report = deckstats::check::check(&list, self.format(), &db, None);
        let mut text = deckstats::stats::render(&stats, &self.format().name);
        if report.is_legal() {
            text.push_str("\nlegal in this format\n");
        } else {
            text.push_str("\nproblems:\n");
            for v in &report.deck {
                text.push_str(&format!("  - {v}\n"));
            }
            for l in report.lines.iter().filter(|l| l.status != deckstats::CardStatus::Ok) {
                text.push_str(&format!("  - {} {}: {}\n", l.count, l.name, l.status));
            }
        }
        let problems = problems_of(&report);
        text_and_json(
            text,
            serde_json::json!({
                "cards": stats.cards, "lands": stats.lands, "creatures": stats.creatures, "other_spells": stats.noncreature_spells,
                "average_mana_value": stats.average_mv, "median_mana_value": stats.median_mv, "interaction": stats.interaction,
                "curve": stats.curve.iter().map(|(mv, (c, o))| serde_json::json!({"mana_value": mv, "creatures": c, "other": o})).collect::<Vec<_>>(),
                "pips": stats.pips.iter().map(|(c, n)| (c.word().to_string(), n)).collect::<std::collections::BTreeMap<_, _>>(),
                "sources": stats.sources.iter().map(|(c, n)| (c.word().to_string(), n)).collect::<std::collections::BTreeMap<_, _>>(),
                "legal": report.is_legal(), "problems": problems,
            }),
        )
    }

    /// Send one request to the human's open deckbuilder and turn the reply into a tool result.
    async fn editor(&self, req: protocol::editor::EditorRequest) -> CallToolResult {
        if self.session().is_some() {
            return tool_error("the deckbuilder tools are only available when helping a human (no game connected)");
        }
        let editor = match open_editor_in(self.runtime()) {
            Ok(Some(e)) => e,
            Ok(None) => return tool_error("no deckbuilder is open; ask the human to run `manaline deck edit <file>` and try again"),
            Err(e) => return tool_error(e),
        };
        let Some(socket) = editor.socket.clone() else {
            return tool_error(format!(
                "the deckbuilder on {} is not accepting requests; ask the human to reopen it",
                editor.path.display()
            ));
        };
        use protocol::editor::EditorReply;
        match protocol::editor::request(&socket, &req).await {
            Ok(EditorReply::Status(st)) => text_and_json(status_text(&st), serde_json::to_value(&st).unwrap_or_default()),
            Ok(EditorReply::Deck(d)) => {
                let mut text = status_text(&d.status);
                text.push('\n');
                for g in &d.groups {
                    let _ = writeln!(text, "{} ({})", g.title, g.count);
                    for c in &g.cards {
                        let problem = if c.problem.is_empty() {
                            String::new()
                        } else {
                            format!("  ← {}", c.problem)
                        };
                        let _ = writeln!(text, "  {:>2} {} {}{problem}", c.count, c.name, c.cost);
                    }
                }
                text_and_json(text, serde_json::to_value(&d).unwrap_or_default())
            }
            Ok(EditorReply::Changed { message, status }) => text_and_json(
                format!("{message}\n{}", status_text(&status)),
                serde_json::json!({ "message": message, "status": status }),
            ),
            Ok(EditorReply::Stats { text, json, status }) => text_and_json(
                format!("{}\n{text}", status_text(&status)),
                serde_json::json!({ "stats": json, "status": status }),
            ),
            Ok(EditorReply::Saved { path, status }) => text_and_json(
                format!("Saved {}.\n{}", path.display(), status_text(&status)),
                serde_json::json!({ "path": path, "status": status }),
            ),
            Ok(EditorReply::Error { message }) => tool_error(message),
            Err(e) => tool_error(format!("could not reach the deckbuilder: {e}")),
        }
    }

    fn state_result(&self, session: &Session, view: &GameView, legal: &[LegalAction], extra: Option<serde_json::Value>) -> CallToolResult {
        let text = render_state(session, view, legal);
        let mut json = serde_json::json!({
            "state": view,
            "legal_actions": legal,
            "your_seat": session.me,
            "must_act": view.must_act.get(&session.me).map(|r| serde_json::to_value(r).unwrap()),
        });
        if let Some(extra) = extra {
            if let (Some(obj), Some(e)) = (json.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        text_and_json(text, json)
    }

    fn not_started(&self, session: &Session) -> CallToolResult {
        let lobby = session.lobby();
        let seats: Vec<String> = lobby
            .seats
            .iter()
            .map(|s| {
                format!(
                    "seat {}: {}{}{}",
                    s.seat.0,
                    s.name.clone().unwrap_or_else(|| "(empty)".into()),
                    if s.deck_ok { ", deck ok" } else { ", no deck" },
                    if s.ready { ", ready" } else { "" }
                )
            })
            .collect();
        tool_error(format!(
            "The game has not started yet. Lobby: {}. {}",
            seats.join("; "),
            if lobby.seats.get(session.me.index()).map(|s| !s.deck_ok).unwrap_or(false) {
                "Submit a deck with submit_deck."
            } else {
                "Call wait_for_turn to wait for it to start."
            }
        ))
    }
}

#[tool_router]
impl McpServer {
    #[tool(
        name = "get_game_state",
        description = "The current game from your seat: every player's life and zones, the battlefield, your hand, whose turn it is to act, and your legal actions if it is yours. Returns a text rendering and structured JSON."
    )]
    pub async fn get_game_state(&self) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        session.begin_call();
        session.refresh().await;
        let Some(view) = session.view() else {
            return Ok(self.not_started(&session));
        };
        let legal = if view.must_act.contains_key(&session.me) {
            session.legal_actions().await.map(|(l, _, _)| l).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(self.state_result(&session, &view, &legal, None))
    }

    #[tool(
        name = "get_legal_actions",
        description = "The numbered list of actions you may take right now, with descriptions. Empty if it is not your turn to act. Pass an id to take_action."
    )]
    pub async fn get_legal_actions(&self) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        session.begin_call();
        if !session.started() {
            session.refresh().await;
            if !session.started() {
                return Ok(self.not_started(&session));
            }
        }
        match session.legal_actions().await {
            Ok((legal, version, reason)) => {
                let text = if legal.is_empty() {
                    "It is not your turn to act. Call wait_for_turn.".to_string()
                } else {
                    format!(
                        "Your turn to act: {}.\n{}",
                        reason.map(reason_text).unwrap_or("act"),
                        render_legal(&legal)
                    )
                };
                Ok(text_and_json(
                    text,
                    serde_json::json!({ "actions": legal, "state_version": version, "reason": reason }),
                ))
            }
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }

    #[tool(
        name = "take_action",
        description = "Take one of your legal actions, by id from the last list (pass the list's state_version too, so a stale id is refused rather than applied to a different situation), or pass a full `action` object. For a cast or activation you may edit `payment.tap` to any set of your untapped mana sources that covers the cost (e.g. tap a big mana creature instead of lands); the listed payments are just the common choices. The reply says whether you still must act and lists the next legal actions if so."
    )]
    pub async fn take_action(&self, Parameters(p): Parameters<TakeActionParams>) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        session.begin_call();
        let (action, version) = match (p.action_id, p.action) {
            (Some(id), None) => match session.action_by_id(id, p.state_version) {
                Ok(x) => x,
                Err(e) => return Ok(tool_error(e.to_string())),
            },
            (None, Some(value)) => match serde_json::from_value::<Action>(value) {
                Ok(a) => (a, p.state_version.unwrap_or_else(|| session.current_version())),
                Err(e) => return Ok(tool_error(format!("that is not a valid action object: {e}"))),
            },
            _ => return Ok(tool_error("pass exactly one of action_id or action")),
        };
        match session.act(action.clone(), version).await {
            Ok((events, view, legal)) => {
                let names = |id: ObjectId| format!("{} {id}", session.name_of(id));
                let seats = |s: engine::Seat| session.seat_name(s);
                let happened: Vec<String> = events
                    .iter()
                    .filter(|e| !matches!(e, engine::EventBase::PriorityPassed { .. } | engine::EventBase::Tapped { .. }))
                    .map(|e| engine::text::describe_event_view(e, &names, &seats))
                    .collect();
                let mut text = String::new();
                if !happened.is_empty() {
                    text.push_str("WHAT HAPPENED\n");
                    for h in &happened {
                        text.push_str("  ");
                        text.push_str(h);
                        text.push('\n');
                    }
                    text.push('\n');
                }
                text.push_str(&render_state(&session, &view, &legal));
                let json = serde_json::json!({
                    "applied": action,
                    "events": events,
                    "state": view,
                    "state_version": view.state_version,
                    "legal_actions": legal,
                    "still_your_turn": view.must_act.contains_key(&session.me),
                });
                Ok(text_and_json(text, json))
            }
            Err(ClientError::Protocol(e)) => {
                let hint = if e.retryable { " Fetch the state again and retry." } else { "" };
                Ok(tool_error(format!("{}{hint}", e.message)))
            }
            Err(e) => Ok(tool_error(e.to_string())),
        }
    }

    #[tool(
        name = "wait_for_turn",
        description = "Block until you have a real decision to make (a spell or ability you can afford, a land drop, attackers, blockers, a mulligan, a choice) or the game ends. Priority moments where passing is your only option are passed for you while you wait (set auto_pass=false to be woken at every one). Returns the state, its state_version, why you must act, and your legal actions; `auto_passed` counts the passes made for you. A { \"timed_out\": true } reply means the opponent is still thinking: the game is NOT over and you must call wait_for_turn again straight away, without stopping or asking anyone."
    )]
    pub async fn wait_for_turn(&self, Parameters(p): Parameters<WaitParams>) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        let generation = session.begin_call();
        let secs = p.timeout_seconds.map(u64::from).unwrap_or(DEFAULT_WAIT_SECS).max(1);
        let auto_pass = p.auto_pass.unwrap_or(true);
        if !session.started() {
            session.refresh().await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        let mut auto_passed = 0u32;
        loop {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match session.wait_for_turn(left, generation).await {
                Wait::TimedOut => break,
                Wait::Superseded => return Ok(superseded(auto_passed)),
                Wait::Ready(view) => {
                    if let Some(o) = view.outcome {
                        let text = format!(
                            "The game is over: {}.\n\n{}",
                            outcome_text(&session, o),
                            render_state(&session, &view, &[])
                        );
                        return Ok(text_and_json(
                            text,
                            serde_json::json!({ "game_over": true, "outcome": o, "state": view, "auto_passed": auto_passed }),
                        ));
                    }
                    let Ok((legal, version, reason)) = session.legal_actions().await else {
                        session.refresh().await;
                        continue;
                    };
                    if session.superseded(generation) {
                        return Ok(superseded(auto_passed));
                    }
                    if legal.is_empty() || version != view.state_version {
                        // The view and the list disagree (the game moved between them): resync.
                        session.refresh().await;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    if auto_pass && nothing_to_do(&legal) {
                        // Only pass and concede: pass on the agent's behalf and keep waiting.
                        match session.act(Action::PassPriority, version).await {
                            Ok(_) => auto_passed += 1,
                            Err(ClientError::Protocol(e))
                                if matches!(
                                    e.code,
                                    protocol::ErrorCode::StaleStateVersion | protocol::ErrorCode::NotYourTurnToAct
                                ) => {}
                            Err(e) => return Ok(tool_error(format!("auto-pass failed: {}", describe_client_error(e)))),
                        }
                        continue;
                    }
                    let extra = serde_json::json!({ "reason": reason, "timed_out": false, "auto_passed": auto_passed, "state_version": view.state_version });
                    return Ok(self.state_result(&session, &view, &legal, Some(extra)));
                }
            }
        }
        Ok(text_and_json(
            format!(
                "Still waiting after {secs}s: the game is in progress and the opponent is thinking ({auto_passed} priority passes made for you). \
                 Call wait_for_turn again now. Do not stop, do not end your turn, and do not ask for confirmation: keep waiting until it returns your legal actions or says the game is over."
            ),
            serde_json::json!({ "timed_out": true, "game_over": false, "auto_passed": auto_passed }),
        ))
    }

    #[tool(
        name = "get_card",
        description = "Look up a card by name or by object id: its cost, types, power/toughness, and rules text, plus its current state if it is on the battlefield."
    )]
    pub async fn get_card(&self, Parameters(p): Parameters<GetCardParams>) -> Result<CallToolResult, ErrorData> {
        let view = self.session().and_then(|s| s.view());
        if let Some(id) = p.object_id {
            let id = ObjectId(id);
            let Some(o) = view.as_ref().and_then(|v| v.object(id).cloned()) else {
                return Ok(tool_error(format!("{id} is not visible to you (or does not exist)")));
            };
            let def = self.cards().lookup(&o.name).map(|c| self.cards().get(c).clone());
            let mut text = card_text(&o.name, &o.cost.to_string(), &o.types, &o.subtypes, o.pt, &o.text);
            let mut state = vec![
                format!("{:?}", o.zone).to_lowercase(),
                format!(
                    "controlled by {}",
                    self.session().map(|s| s.seat_name(o.controller)).unwrap_or_default()
                ),
            ];
            if o.tapped {
                state.push("tapped".into());
            }
            if o.summoning_sick && o.pt.is_some() {
                state.push(if o.keywords.contains(&engine::Keyword::Haste) {
                    "summoning sick, but has haste so it can attack".into()
                } else {
                    "summoning sick".into()
                });
            }
            if o.damage > 0 {
                state.push(format!("{} damage marked", o.damage));
            }
            if o.attacking.is_some() {
                state.push("attacking".into());
            }
            if !o.blocking.is_empty() {
                state.push(format!(
                    "blocking {}",
                    o.blocking.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", ")
                ));
            }
            text.push_str(&format!("\nState: {}", state.join(", ")));
            let mut json = serde_json::json!({ "object": o });
            if p.include_ir.unwrap_or(false) {
                json["definition"] = serde_json::to_value(&def).unwrap_or_default();
            }
            return Ok(text_and_json(text, json));
        }
        let Some(name) = p.name else {
            return Ok(tool_error("pass name or object_id"));
        };
        let Some(id) = self.cards().lookup(&name) else {
            return Ok(tool_error(format!("no card named {name:?} in this game's card set")));
        };
        let def = self.cards().get(id);
        let text = card_text(&def.name, &def.cost.to_string(), &def.types, &def.subtypes, def.pt, &def.text);
        let mut json = serde_json::json!({ "card": { "name": def.name, "cost": def.cost.to_string(), "types": def.types, "subtypes": def.subtypes, "pt": def.pt, "text": def.text } });
        if p.include_ir.unwrap_or(false) {
            json["definition"] = serde_json::to_value(def).unwrap_or_default();
        }
        Ok(text_and_json(text, json))
    }

    #[tool(
        name = "search_cards",
        description = "Search the card database with Scryfall-style syntax: t: types, c: colours (c:rg, c:c colourless, c:m multicolour), ci: identity, mv/pow/tou with comparisons, o: Oracle text (quote phrases), kw: keywords, r: rarity, s: set, f: format legality, is:implemented; `-` negates, `or` alternates, parentheses group. Only cards the engine can play are returned unless include_unimplemented is set."
    )]
    pub async fn search_cards(&self, Parameters(p): Parameters<SearchParams>) -> Result<CallToolResult, ErrorData> {
        if let Some(session) = self.session() {
            session.begin_call();
        }
        let index = self.card_index();
        let limit = p.limit.unwrap_or(20).clamp(1, 200) as usize;
        let query = if p.include_unimplemented.unwrap_or(false) || !index.from_cache {
            p.query.clone()
        } else {
            format!("({}) is:implemented", p.query)
        };
        let hits = match index.query(&query, limit) {
            Ok(h) => h,
            Err(e) => return Ok(tool_error(format!("bad query: {e}"))),
        };
        let text = if hits.is_empty() {
            "no cards match".to_string()
        } else {
            hits.iter().map(|e| e.line()).collect::<Vec<_>>().join("\n")
        };
        let json: Vec<serde_json::Value> = hits
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name, "mana_cost": e.mana_cost, "mana_value": e.mana_value, "type_line": e.type_line,
                    "text": e.oracle_text, "power": e.power, "toughness": e.toughness, "colors": e.colors,
                    "keywords": e.keywords, "implemented": e.implemented,
                })
            })
            .collect();
        Ok(text_and_json(text, serde_json::json!({ "cards": json, "count": hits.len() })))
    }

    #[tool(
        name = "editor_status",
        description = "Whether the human has a deckbuilder open, which file and format, the card count, unsaved changes, and the legality line. All editor_* tools act on that open deckbuilder: the human sees every change, can undo it, and the file only changes on save."
    )]
    pub async fn editor_status(&self) -> Result<CallToolResult, ErrorData> {
        Ok(self.editor(protocol::editor::EditorRequest::Status).await)
    }

    #[tool(
        name = "editor_deck",
        description = "The deck as the open deckbuilder holds it right now: grouped by type with counts and costs, each card's legality problem if any, and the status line."
    )]
    pub async fn editor_deck(&self) -> Result<CallToolResult, ErrorData> {
        Ok(self.editor(protocol::editor::EditorRequest::Deck).await)
    }

    #[tool(
        name = "editor_add_card",
        description = "Add copies of a card to the open deckbuilder's deck (only cards the engine can play; unknown names get a suggestion). The human sees the row highlighted."
    )]
    pub async fn editor_add_card(&self, Parameters(p): Parameters<EditorCardParams>) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .editor(protocol::editor::EditorRequest::AddCard {
                name: p.name,
                count: p.count.unwrap_or(1),
            })
            .await)
    }

    #[tool(
        name = "editor_remove_card",
        description = "Remove copies of a card from the open deckbuilder's deck, or every copy with all: true."
    )]
    pub async fn editor_remove_card(&self, Parameters(p): Parameters<EditorRemoveParams>) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .editor(protocol::editor::EditorRequest::RemoveCard {
                name: p.name,
                count: p.count.unwrap_or(1),
                all: p.all.unwrap_or(false),
            })
            .await)
    }

    #[tool(
        name = "editor_set_count",
        description = "Set a card to an exact number of copies in the open deckbuilder's deck; 0 removes it."
    )]
    pub async fn editor_set_count(&self, Parameters(p): Parameters<EditorSetCountParams>) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .editor(protocol::editor::EditorRequest::SetCount {
                name: p.name,
                count: p.count,
            })
            .await)
    }

    #[tool(
        name = "editor_replace_deck",
        description = "Replace the whole main deck in the open deckbuilder with a decklist, as one undoable step. Refused entirely if any card is unknown or unplayable."
    )]
    pub async fn editor_replace_deck(&self, Parameters(p): Parameters<EditorReplaceParams>) -> Result<CallToolResult, ErrorData> {
        Ok(self
            .editor(protocol::editor::EditorRequest::ReplaceDeck { decklist: p.decklist })
            .await)
    }

    #[tool(
        name = "editor_undo",
        description = "Undo the last change in the open deckbuilder, whether yours or the human's."
    )]
    pub async fn editor_undo(&self) -> Result<CallToolResult, ErrorData> {
        Ok(self.editor(protocol::editor::EditorRequest::Undo).await)
    }

    #[tool(
        name = "editor_stats",
        description = "Curve, colour pips against sources, interaction count, land odds, and sample opening hands for the deck as currently edited; also switches the deckbuilder's right pane to the same stats so the human sees them."
    )]
    pub async fn editor_stats(&self) -> Result<CallToolResult, ErrorData> {
        Ok(self.editor(protocol::editor::EditorRequest::Stats).await)
    }

    #[tool(
        name = "editor_save",
        description = "Ask the open deckbuilder to write its deck to its file, in canonical order."
    )]
    pub async fn editor_save(&self) -> Result<CallToolResult, ErrorData> {
        Ok(self.editor(protocol::editor::EditorRequest::Save).await)
    }

    #[tool(
        name = "deck_stats",
        description = "Analyse a decklist: card counts, mana curve, colour pips against sources, interaction count, land odds, and legality problems in this game's format."
    )]
    pub async fn deck_stats(&self, Parameters(p): Parameters<DeckStatsParams>) -> Result<CallToolResult, ErrorData> {
        if let Some(session) = self.session() {
            session.begin_call();
        }
        Ok(self.deck_stats_result(&p.decklist))
    }

    #[tool(
        name = "get_log",
        description = "The game log including table chat, seat-filtered, one line per event."
    )]
    pub async fn get_log(&self, Parameters(p): Parameters<GetLogParams>) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        session.begin_call();
        let lines = session.log_since(p.since_turn);
        let text = if lines.is_empty() {
            "(nothing yet)".to_string()
        } else {
            lines
                .iter()
                .map(|l| format!("T{}  {}", l.turn, l.text))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(text_and_json(text, serde_json::json!({ "lines": lines })))
    }

    #[tool(
        name = "say",
        description = "Say something to the table (parameter: text). It appears in the other players' logs."
    )]
    pub async fn say(&self, Parameters(p): Parameters<SayParams>) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        match session.client.chat(&p.text, None).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("said")])),
            Err(e) => Ok(tool_error(describe_client_error(e).to_string())),
        }
    }

    #[tool(name = "concede", description = "Concede the game. This ends it for you immediately.")]
    pub async fn concede(&self) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        session.begin_call();
        let version = session.current_version();
        match session.act(Action::Concede, version).await {
            Ok((_, view, _)) => {
                let text = match view.outcome {
                    Some(o) => format!("You conceded. {}", outcome_text(&session, o)),
                    None => "You conceded; the game continues for the others.".into(),
                };
                Ok(text_and_json(text, serde_json::json!({ "outcome": view.outcome })))
            }
            Err(e) => Ok(tool_error(describe_client_error(e).to_string())),
        }
    }

    #[tool(
        name = "list_decks",
        description = "Every deck you could play as it is, by name: card count, colours, whether it is legal in this format, and where it lives. Use get_deck to read one and submit_deck with its name to play it."
    )]
    pub async fn list_decks(&self) -> Result<CallToolResult, ErrorData> {
        if let Some(session) = self.session() {
            session.begin_call();
        }
        let mut text = String::new();
        let mut json = Vec::new();
        for d in available_decks() {
            let (cards, colours, legal, problems) = self.summarise(&d.text);
            let _ = writeln!(
                text,
                "{:<16} {:>3} cards  {:<6} {:<10} {}",
                d.name,
                cards,
                colours,
                if legal { "legal" } else { "not legal" },
                d.origin
            );
            json.push(serde_json::json!({ "name": d.name, "origin": d.origin, "cards": cards, "colors": colours, "legal": legal, "problems": problems }));
        }
        if json.is_empty() {
            text.push_str("no decks available\n");
        }
        let _ = writeln!(text, "\ndecks are looked up in: {}", cards::deck_dirs_text());
        if self.session().is_none() {
            match open_editor_in(self.runtime()) {
                Ok(Some(e)) => {
                    let _ = writeln!(
                        text,
                        "The human has {} open in the deckbuilder ({} format): the editor_* tools act on it.",
                        e.path.display(),
                        e.format
                    );
                }
                Ok(None) => text.push_str("No deckbuilder is open right now; the editor_* tools need one.\n"),
                Err(e) => {
                    let _ = writeln!(text, "{e}");
                }
            }
        }
        Ok(text_and_json(
            text,
            serde_json::json!({ "decks": json, "format": self.format().name }),
        ))
    }

    #[tool(
        name = "get_deck",
        description = "Read one deck from list_decks (or a file path): its full decklist text plus the same analysis deck_stats gives."
    )]
    pub async fn get_deck(&self, Parameters(p): Parameters<GetDeckParams>) -> Result<CallToolResult, ErrorData> {
        if let Some(session) = self.session() {
            session.begin_call();
        }
        let name = p.name;
        let Some(d) = find_deck(&name) else {
            return Ok(tool_error(format!("no deck named {name:?}; list_decks shows what is available")));
        };
        let stats = self.deck_stats_result(&d.text);
        let mut text = format!("{} ({})\n\n{}\n", d.name, d.origin, d.text.trim_end());
        if let Some(t) = stats.content.first().and_then(|c| c.as_text()) {
            text.push('\n');
            text.push_str(&t.text);
        }
        let mut json = serde_json::json!({ "name": d.name, "origin": d.origin, "decklist": d.text });
        if let Some(sc) = stats.structured_content {
            json["stats"] = sc;
        }
        Ok(text_and_json(text, json))
    }

    #[tool(
        name = "submit_deck",
        description = "Choose the deck you will play and ready up: the `name` of a deck from list_decks (played as-is), or a full decklist in the standard text format. Only needed if the game has not started and no deck was given for you."
    )]
    pub async fn submit_deck(&self, Parameters(p): Parameters<SubmitDeckParams>) -> Result<CallToolResult, ErrorData> {
        let session = match self.ensure_seated().await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        if session.started() {
            return Ok(tool_error("the game has already started"));
        }
        let decklist = match (p.name, p.decklist) {
            (Some(name), _) => match find_deck(&name) {
                Some(d) => d.text,
                None => return Ok(tool_error(format!("no deck named {name:?}; list_decks shows what is available"))),
            },
            (None, Some(text)) => text,
            (None, None) => return Ok(tool_error("pass a deck name from list_decks or a decklist")),
        };
        match session.client.set_deck(&decklist).await {
            Ok(Ok(())) => {}
            Ok(Err(violations)) => {
                let list: Vec<String> = violations.iter().map(|v| format!("- {v}")).collect();
                return Ok(tool_error(format!("deck rejected:\n{}", list.join("\n"))));
            }
            Err(e) => return Ok(tool_error(describe_client_error(e).to_string())),
        }
        match session.client.ready().await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "Deck accepted and you are ready. Call wait_for_turn.",
            )])),
            Err(e) => Ok(tool_error(describe_client_error(e).to_string())),
        }
    }

    #[tool(
        name = "sit_down",
        description = "Take a seat at the game the human has published, optionally a particular one (`seat`). You do not have to call this: the first game tool you use seats you in the next free agent seat. Call it to choose your seat, to see where you are sitting, or to move to a new game once the last one is over. Other agents may be at the same table, each in their own MCP session."
    )]
    pub async fn sit_down(&self, Parameters(p): Parameters<SitDownParams>) -> Result<CallToolResult, ErrorData> {
        if self.shared.fixed_seat.is_some() {
            return Ok(tool_error(
                "this session plays the seat it was given on the command line (--connect), so it cannot choose another",
            ));
        }
        if let Some(session) = self.session() {
            if session.outcome().is_none() {
                return Ok(tool_error(format!(
                    "you are already seated in game {} as seat {}; call leave first if you want a different seat",
                    session.game_id, session.me.0
                )));
            }
            // That game is over: give its seat back and look for a newer one.
            self.give_up_seat();
        }
        let session = match self.take_seat(p.seat).await {
            Ok(s) => s,
            Err(e) => return Ok(e),
        };
        let lobby = session.lobby();
        let slot = self.claimed_slot();
        let deck = slot.as_ref().and_then(|s| s.deck.clone());
        let name = slot.map(|s| s.name).unwrap_or_else(|| session.seat_name(session.me));
        let text = format!(
            "Seated at game {} as seat {} ({name}).{} {}",
            session.game_id,
            session.me.0,
            match &deck {
                Some(d) => format!(" Your deck is {d}."),
                None => " No deck was chosen for you: pick one with list_decks and submit_deck.".to_string(),
            },
            if lobby.started {
                "The game is under way: call wait_for_turn."
            } else {
                "Call wait_for_turn once you are ready; it returns when the game starts and you must act."
            }
        );
        Ok(text_and_json(
            text,
            serde_json::json!({
                "game_id": session.game_id,
                "seat": session.me,
                "name": name,
                "deck": deck,
                "started": lobby.started,
            }),
        ))
    }

    #[tool(
        name = "leave",
        description = "Give up your seat when you are done playing: the claim goes back so another agent can take it, and this session drops to card data only. Use it after a game is over, or to free a seat you took by mistake."
    )]
    pub async fn leave(&self) -> Result<CallToolResult, ErrorData> {
        if self.shared.fixed_seat.is_some() {
            return Ok(tool_error(
                "this server was given its seat on the command line (--connect); stop the server to leave the table",
            ));
        }
        let Some(session) = self.give_up_seat() else {
            return Ok(text_and_json(
                "You are not seated at a game; this session is serving card data only.".to_string(),
                serde_json::json!({ "seated": false }),
            ));
        };
        Ok(text_and_json(
            format!(
                "Left game {} (seat {}); the seat is free for another agent and this session is back to card data only.",
                session.game_id, session.me.0
            ),
            serde_json::json!({ "seated": false, "left_game": session.game_id, "seat": session.me }),
        ))
    }
}

fn card_text(name: &str, cost: &str, types: &[engine::CardType], subtypes: &[String], pt: Option<(i32, i32)>, text: &str) -> String {
    let mut s = format!("{name} {cost}\n");
    let t: Vec<String> = types.iter().map(|t| format!("{t:?}")).collect();
    s.push_str(&t.join(" "));
    if !subtypes.is_empty() {
        s.push_str(" — ");
        s.push_str(&subtypes.join(" "));
    }
    if let Some((p, t)) = pt {
        s.push_str(&format!("\n{p}/{t}"));
    }
    s.push('\n');
    s.push_str(if text.is_empty() { "(no rules text)" } else { text });
    s
}

#[prompt_router]
impl McpServer {
    #[prompt(
        name = "play-a-game",
        description = "The recommended loop for playing a game of Magic at this table."
    )]
    pub async fn play_a_game(&self) -> Vec<PromptMessage> {
        let seated = match self.session() {
            Some(session) => format!("as seat {}", session.me.0),
            None if self.runtime().newest_game().is_some() => {
                "at the game the human has published — your first game tool call takes the next free agent seat, \
                 or call `sit_down` to choose one"
                    .to_string()
            }
            None => {
                let text = format!(
                    "No game is published for agents right now: this manaline session serves card data (search_cards, deck_stats, get_card, `{CUBE_URI}`) for deckbuilding. \
                     To play, ask the human to start a game with `manaline play --vs claude`; your first game tool call will seat you at it."
                );
                return vec![PromptMessage::new_text(Role::User, text)];
            }
        };
        let text = format!(
            "You are playing Magic: The Gathering {seated} at a manaline table. Read the resource `{PRIMER_URI}` first if you have not played before.\n\n\
             Then loop:\n\
             1. Call `wait_for_turn`. It blocks until you must act. If it returns timed_out, the game is still on and the opponent is thinking: call it again immediately. Never stop looping or ask the user what to do while the game is in progress; only a reply with game_over: true ends the loop.\n\
             2. Read the state and the numbered legal actions it returns. Think about the board.\n\
             3. Call `take_action` with the id you chose and the state_version the list came from. If the reply says it is still your turn, choose again from the new list; when you have nothing worth doing, take the `Pass priority` action.\n\
             4. Go back to step 1.\n\n\
             Use `say` to greet your opponent and comment on the game now and then. Play to win: develop your mana, cast your best creatures, attack when it is profitable, block to survive. Do not concede unless the game is clearly lost. \
             When the game is over, `leave` frees your seat for another agent."
        );
        vec![PromptMessage::new_text(Role::User, text)]
    }
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for McpServer {
    /// The generated list, minus the deckbuilder tools once this session holds a seat.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let mut tools = Self::tool_router().list_all();
        if self.session().is_some() {
            tools.retain(|t| !DECKBUILDING_ONLY_TOOLS.contains(&t.name.as_ref()));
        }
        Ok(rmcp::model::ListToolsResult {
            result_type: Some(rmcp::model::ResultType::COMPLETE),
            tools,
            meta: None,
            next_cursor: None,
            ttl_ms: supports_cache_hints.then_some(0),
            cache_scope: supports_cache_hints.then_some(CacheScope::Public),
        })
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("manaline", env!("CARGO_PKG_VERSION")).with_title("manaline");
        match self.session() {
            Some(session) => info.with_instructions(format!(
                "manaline: you are seat {} in a game of Magic: The Gathering. Read `{PRIMER_URI}` for the rules, then loop wait_for_turn → take_action. Use `say` to talk to the table, and `leave` when you are done.",
                session.me.0
            )),
            None if self.runtime().newest_game().is_some() => info.with_instructions(format!(
                "manaline: a game is published for agents and you are not seated yet. Your first game tool call (or `sit_down`) takes the next free agent seat; other agents may be at the same table, each with its own MCP session. Read `{PRIMER_URI}` for the rules, then loop wait_for_turn → take_action."
            )),
            None => {
                let open = match open_editor_in(self.runtime()) {
                    Ok(Some(e)) => format!(" The human has {} open in the deckbuilder: the editor_* tools act on it.", e.path.display()),
                    _ => " No deckbuilder is open yet; ask the human to run `manaline deck edit <file>` before changing a deck.".to_string(),
                };
                info.with_instructions(format!(
                    "manaline card data (no game published): you are helping the human build a deck in their open deckbuilder. Look things up with search_cards, get_card, deck_stats, list_decks, and get_deck; change the deck only through editor_add_card, editor_remove_card, editor_set_count, editor_replace_deck, editor_undo, editor_stats, and editor_save. `{PRIMER_URI}` has the rules.{open}"
                ))
            }
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // Cache hints are required on results from protocol 2026-07-28 on (SEP-2549);
        // the tool and prompt handlers emit them, so the resource handlers must too.
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(PRIMER_URI, "rules-primer")
                .with_title("Rules primer")
                .with_description(
                    "A plain-English summary of turn structure, priority, combat, and the stack, for an agent that has never played.",
                )
                .with_mime_type("text/markdown"),
            Resource::new(CUBE_URI, "cube")
                .with_title("Card list")
                .with_description("Every card in this game's card set with its cost, types, stats and rules text.")
                .with_mime_type("text/plain"),
        ])
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Public))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let text = match request.uri.as_str() {
            PRIMER_URI => RULES_PRIMER.to_string(),
            CUBE_URI => cube_text(self.cards()),
            other => return Err(ErrorData::resource_not_found(format!("no resource {other}"), None)),
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)])
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Public)
            .into())
    }
}

pub fn cube_text(db: &engine::CardDb) -> String {
    let mut s = String::from("# Card set\n\n");
    for (_, c) in db.iter() {
        let pt = c.pt.map(|(p, t)| format!(" {p}/{t}")).unwrap_or_default();
        let types: Vec<String> = c.types.iter().map(|t| format!("{t:?}")).collect();
        let sub = if c.subtypes.is_empty() {
            String::new()
        } else {
            format!(" — {}", c.subtypes.join(" "))
        };
        s.push_str(&format!("{} {} · {}{sub}{pt}", c.name, c.cost, types.join(" ")));
        if !c.text.is_empty() {
            s.push_str(&format!(" · {}", c.text));
        }
        s.push('\n');
    }
    s
}

impl McpServer {
    /// For tests and the CLI: the outcome as this seat sees it.
    pub fn outcome(&self) -> Option<Outcome> {
        self.session().and_then(|s| s.outcome())
    }
}
