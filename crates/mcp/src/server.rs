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
use std::sync::Arc;
use std::time::Duration;

pub const PRIMER_URI: &str = "manaline://rules-primer";
pub const CUBE_URI: &str = "manaline://cube";
/// Short enough to come back before any MCP client gives up on the call.
pub const DEFAULT_WAIT_SECS: u64 = 45;

#[derive(Clone)]
pub struct McpServer {
    /// The seat's connection, or `None` when serving card data only
    /// (`manaline mcp` with no game: search, deck stats, the resources).
    pub session: Option<Arc<Session>>,
    pub cards: Arc<engine::CardDb>,
    pub format: engine::Format,
    index: Arc<std::sync::OnceLock<Arc<cardsearch::Index>>>,
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
pub struct SaveDeckParams {
    /// A decklist in the standard text format: one `N Card Name` per line.
    pub decklist: String,
    /// A deck name; saved as `<name>.txt` in the manaline decks folder. Pass this or `path`.
    #[serde(default)]
    pub name: Option<String>,
    /// An explicit file path to write instead.
    #[serde(default)]
    pub path: Option<String>,
    /// Replace an existing file. Default false: an existing file is an error.
    #[serde(default)]
    pub overwrite: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SubmitDeckParams {
    /// A decklist in the standard text format: one `N Card Name` per line.
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

/// The reply for game tools when this server has no game.
fn no_game() -> CallToolResult {
    tool_error(
        "No game is connected: this server is serving card data only (search_cards, deck_stats, get_card, and the resources). \
         Start a game with `manaline play --vs claude` and point your client at the URL it prints to play.",
    )
}

fn tool_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

impl McpServer {
    pub fn new(session: Arc<Session>) -> McpServer {
        McpServer {
            cards: Arc::new(session.cards.clone()),
            format: session.format.clone(),
            session: Some(session),
            index: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// A server with no game: card data, search, and deck analysis only.
    pub fn standalone(format: engine::Format) -> McpServer {
        McpServer {
            session: None,
            cards: Arc::new(cards::core()),
            format,
            index: Arc::new(std::sync::OnceLock::new()),
        }
    }

    pub fn card_index(&self) -> Arc<cardsearch::Index> {
        self.index.get_or_init(|| Arc::new(cardsearch::Index::load(&self.cards))).clone()
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
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
        };
        session.begin_call();
        session.refresh().await;
        let Some(view) = session.view() else {
            return Ok(self.not_started(session));
        };
        let legal = if view.must_act.contains_key(&session.me) {
            session.legal_actions().await.map(|(l, _, _)| l).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(self.state_result(session, &view, &legal, None))
    }

    #[tool(
        name = "get_legal_actions",
        description = "The numbered list of actions you may take right now, with descriptions. Empty if it is not your turn to act. Pass an id to take_action."
    )]
    pub async fn get_legal_actions(&self) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
        };
        session.begin_call();
        if !session.started() {
            session.refresh().await;
            if !session.started() {
                return Ok(self.not_started(session));
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
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
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
                text.push_str(&render_state(session, &view, &legal));
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
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
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
                            outcome_text(session, o),
                            render_state(session, &view, &[])
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
                    return Ok(self.state_result(session, &view, &legal, Some(extra)));
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
        let view = self.session.as_ref().and_then(|s| s.view());
        if let Some(id) = p.object_id {
            let id = ObjectId(id);
            let Some(o) = view.as_ref().and_then(|v| v.object(id).cloned()) else {
                return Ok(tool_error(format!("{id} is not visible to you (or does not exist)")));
            };
            let def = self.cards.lookup(&o.name).map(|c| self.cards.get(c).clone());
            let mut text = card_text(&o.name, &o.cost.to_string(), &o.types, &o.subtypes, o.pt, &o.text);
            let mut state = vec![
                format!("{:?}", o.zone).to_lowercase(),
                format!(
                    "controlled by {}",
                    self.session.as_ref().map(|s| s.seat_name(o.controller)).unwrap_or_default()
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
        let Some(id) = self.cards.lookup(&name) else {
            return Ok(tool_error(format!("no card named {name:?} in this game's card set")));
        };
        let def = self.cards.get(id);
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
        if let Some(session) = &self.session {
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
        name = "save_deck",
        description = "Write a decklist to a deck file the human can play or open in the deckbuilder. Give a `name` (saved to the manaline decks folder) or a `path`. Unknown card names are refused; the file is written in canonical order. Returns the path and the deck's legality in this format."
    )]
    pub async fn save_deck(&self, Parameters(p): Parameters<SaveDeckParams>) -> Result<CallToolResult, ErrorData> {
        if let Some(session) = &self.session {
            session.begin_call();
        }
        let list = match deckstats::parse(&p.decklist) {
            Ok(l) => l,
            Err(e) => return Ok(tool_error(format!("could not read the decklist: {e}"))),
        };
        let res = list.resolve(&self.cards);
        if !res.unresolved.is_empty() {
            let names: Vec<String> = res
                .unresolved
                .iter()
                .map(|u| match &u.suggestion {
                    Some(s) => format!("{} (did you mean {s}?)", u.entry.name),
                    None => u.entry.name.clone(),
                })
                .collect();
            return Ok(tool_error(format!("unknown cards, nothing written: {}", names.join(", "))));
        }
        let path = match (p.path, p.name) {
            (Some(path), _) => std::path::PathBuf::from(path),
            (None, Some(name)) => match cards::user_deck_path(&name) {
                Some(p) => p,
                None => return Ok(tool_error("give a plain deck name (no slashes) or an explicit path")),
            },
            (None, None) => return Ok(tool_error("pass a name or a path")),
        };
        if path.exists() && !p.overwrite.unwrap_or(false) {
            return Ok(tool_error(format!(
                "{} already exists; pass overwrite: true to replace it",
                path.display()
            )));
        }
        if let Some(dir) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return Ok(tool_error(format!("could not create {}: {e}", dir.display())));
            }
        }
        let text = list.to_text(&self.cards);
        if let Err(e) = std::fs::write(&path, &text) {
            return Ok(tool_error(format!("could not write {}: {e}", path.display())));
        }
        let report = deckstats::check::check(&list, &self.format, &self.cards, None);
        let problems: Vec<String> = report
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
            .collect();
        let count: u32 = list.main_count();
        let mut out = format!("Saved {count} cards to {}.\n", path.display());
        if report.is_legal() {
            out.push_str(&format!(
                "Legal in {}. Play it with: manaline play --deck {}",
                self.format.name,
                path.display()
            ));
        } else {
            out.push_str(&format!("Not yet legal in {}:\n", self.format.name));
            for pr in &problems {
                out.push_str(&format!("  - {pr}\n"));
            }
        }
        Ok(text_and_json(
            out,
            serde_json::json!({ "path": path, "cards": count, "legal": report.is_legal(), "problems": problems, "text": text }),
        ))
    }

    #[tool(
        name = "deck_stats",
        description = "Analyse a decklist: card counts, mana curve, colour pips against sources, interaction count, land odds, and legality problems in this game's format."
    )]
    pub async fn deck_stats(&self, Parameters(p): Parameters<DeckStatsParams>) -> Result<CallToolResult, ErrorData> {
        if let Some(session) = &self.session {
            session.begin_call();
        }
        let list = match deckstats::parse(&p.decklist) {
            Ok(l) => l,
            Err(e) => return Ok(tool_error(format!("could not read the decklist: {e}"))),
        };
        let db = self.cards.clone();
        let res = list.resolve(&db);
        let stats = deckstats::Stats::compute(&res.deck, &db);
        let report = deckstats::check::check(&list, &self.format, &db, None);
        let mut text = deckstats::stats::render(&stats, &self.format.name);
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
        let problems: Vec<String> = report
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
            .collect();
        Ok(text_and_json(
            text,
            serde_json::json!({
                "cards": stats.cards, "lands": stats.lands, "creatures": stats.creatures, "other_spells": stats.noncreature_spells,
                "average_mana_value": stats.average_mv, "median_mana_value": stats.median_mv, "interaction": stats.interaction,
                "curve": stats.curve.iter().map(|(mv, (c, o))| serde_json::json!({"mana_value": mv, "creatures": c, "other": o})).collect::<Vec<_>>(),
                "pips": stats.pips.iter().map(|(c, n)| (c.word().to_string(), n)).collect::<std::collections::BTreeMap<_, _>>(),
                "sources": stats.sources.iter().map(|(c, n)| (c.word().to_string(), n)).collect::<std::collections::BTreeMap<_, _>>(),
                "legal": report.is_legal(), "problems": problems,
            }),
        ))
    }

    #[tool(
        name = "get_log",
        description = "The game log including table chat, seat-filtered, one line per event."
    )]
    pub async fn get_log(&self, Parameters(p): Parameters<GetLogParams>) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
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
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
        };
        match session.client.chat(&p.text, None).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("said")])),
            Err(e) => Ok(tool_error(describe_client_error(e).to_string())),
        }
    }

    #[tool(name = "concede", description = "Concede the game. This ends it for you immediately.")]
    pub async fn concede(&self) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
        };
        session.begin_call();
        let version = session.current_version();
        match session.act(Action::Concede, version).await {
            Ok((_, view, _)) => {
                let text = match view.outcome {
                    Some(o) => format!("You conceded. {}", outcome_text(session, o)),
                    None => "You conceded; the game continues for the others.".into(),
                };
                Ok(text_and_json(text, serde_json::json!({ "outcome": view.outcome })))
            }
            Err(e) => Ok(tool_error(describe_client_error(e).to_string())),
        }
    }

    #[tool(
        name = "submit_deck",
        description = "Submit your decklist (standard text format, e.g. \"17 Forest\\n4 Grizzly Bears\") and ready up. Only needed if the game has not started and no deck was given for you."
    )]
    pub async fn submit_deck(&self, Parameters(p): Parameters<SubmitDeckParams>) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session.as_ref() else {
            return Ok(no_game());
        };
        if session.started() {
            return Ok(tool_error("the game has already started"));
        }
        match session.client.set_deck(&p.decklist).await {
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
        let Some(session) = &self.session else {
            let text = format!(
                "This manaline server has no game connected: it serves card data (search_cards, deck_stats, get_card, `{CUBE_URI}`) for deckbuilding. \
                 To play, start a game with `manaline play --vs claude` and connect to the URL it prints."
            );
            return vec![PromptMessage::new_text(Role::User, text)];
        };
        let me = session.me;
        let text = format!(
            "You are playing Magic: The Gathering as seat {} at a manaline table. Read the resource `{PRIMER_URI}` first if you have not played before.\n\n\
             Then loop:\n\
             1. Call `wait_for_turn`. It blocks until you must act. If it returns timed_out, the game is still on and the opponent is thinking: call it again immediately. Never stop looping or ask the user what to do while the game is in progress; only a reply with game_over: true ends the loop.\n\
             2. Read the state and the numbered legal actions it returns. Think about the board.\n\
             3. Call `take_action` with the id you chose and the state_version the list came from. If the reply says it is still your turn, choose again from the new list; when you have nothing worth doing, take the `Pass priority` action.\n\
             4. Go back to step 1.\n\n\
             Use `say` to greet your opponent and comment on the game now and then. Play to win: develop your mana, cast your best creatures, attack when it is profitable, block to survive. Do not concede unless the game is clearly lost.",
            me.0
        );
        vec![PromptMessage::new_text(Role::User, text)]
    }
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("manaline", env!("CARGO_PKG_VERSION")).with_title("manaline");
        match &self.session {
            Some(session) => info.with_instructions(format!(
                "manaline: you are seat {} in a game of Magic: The Gathering. Read `{PRIMER_URI}` for the rules, then loop wait_for_turn → take_action. Use `say` to talk to the table.",
                session.me.0
            )),
            None => info.with_instructions(format!(
                "manaline card data (no game connected): use search_cards, deck_stats, get_card, and `{CUBE_URI}` to build a deck; `{PRIMER_URI}` has the rules."
            )),
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
            CUBE_URI => cube_text(&self.cards),
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
        self.session.as_ref().and_then(|s| s.outcome())
    }
}
