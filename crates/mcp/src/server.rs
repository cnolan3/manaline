//! The MCP server: tools, the play-a-game prompt, and the two resources (§7).

use crate::primer::RULES_PRIMER;
use crate::render::{outcome_text, reason_text, render_legal, render_state};
use crate::session::{describe_client_error, Session, Wait};
use engine::{Action, GameView, ObjectId, Outcome};
use protocol::{ClientError, LegalAction};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CacheScope, CallToolResult, ContentBlock, ErrorData, Implementation, ListResourcesResult, PaginatedRequestParams, PromptMessage,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{prompt, prompt_handler, prompt_router, tool, tool_handler, tool_router, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

pub const PRIMER_URI: &str = "manaline://rules-primer";
pub const CUBE_URI: &str = "manaline://cube";
pub const DEFAULT_WAIT_SECS: u64 = 300;

#[derive(Clone)]
pub struct McpServer {
    pub session: Arc<Session>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TakeActionParams {
    /// The id of an action from the last `get_legal_actions` or `wait_for_turn` result.
    #[serde(default)]
    pub action_id: Option<u32>,
    /// A full action object instead of an id (the `action` field of a legal action entry).
    #[serde(default)]
    pub action: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaitParams {
    /// Give up after this many seconds and return `{ "timed_out": true }`. Default 300.
    #[serde(default)]
    pub timeout_seconds: Option<u32>,
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
pub struct SubmitDeckParams {
    /// A decklist in the standard text format: one `N Card Name` per line.
    pub decklist: String,
}

fn text_and_json(text: String, json: serde_json::Value) -> CallToolResult {
    let mut r = CallToolResult::success(vec![ContentBlock::text(text)]);
    r.structured_content = Some(json);
    r
}

fn tool_error(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

impl McpServer {
    pub fn new(session: Arc<Session>) -> McpServer {
        McpServer { session }
    }

    fn state_result(&self, view: &GameView, legal: &[LegalAction], extra: Option<serde_json::Value>) -> CallToolResult {
        let text = render_state(&self.session, view, legal);
        let mut json = serde_json::json!({
            "state": view,
            "legal_actions": legal,
            "your_seat": self.session.me,
            "must_act": view.must_act.get(&self.session.me).map(|r| serde_json::to_value(r).unwrap()),
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

    fn not_started(&self) -> CallToolResult {
        let lobby = self.session.lobby();
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
            if lobby.seats.get(self.session.me.index()).map(|s| !s.deck_ok).unwrap_or(false) {
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
        self.session.refresh().await;
        let Some(view) = self.session.view() else { return Ok(self.not_started()) };
        let legal = if view.must_act.contains_key(&self.session.me) {
            self.session.legal_actions().await.map(|(l, _, _)| l).unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(self.state_result(&view, &legal, None))
    }

    #[tool(
        name = "get_legal_actions",
        description = "The numbered list of actions you may take right now, with descriptions. Empty if it is not your turn to act. Pass an id to take_action."
    )]
    pub async fn get_legal_actions(&self) -> Result<CallToolResult, ErrorData> {
        if !self.session.started() {
            self.session.refresh().await;
            if !self.session.started() {
                return Ok(self.not_started());
            }
        }
        match self.session.legal_actions().await {
            Ok((legal, version, reason)) => {
                let text = if legal.is_empty() {
                    "It is not your turn to act. Call wait_for_turn.".to_string()
                } else {
                    format!("Your turn to act: {}.\n{}", reason.map(reason_text).unwrap_or("act"), render_legal(&legal))
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
        description = "Take one of your legal actions, by id from the last list. The reply says whether you still must act and lists the next legal actions if so."
    )]
    pub async fn take_action(&self, Parameters(p): Parameters<TakeActionParams>) -> Result<CallToolResult, ErrorData> {
        let (action, version) = match (p.action_id, p.action) {
            (Some(id), None) => match self.session.action_by_id(id) {
                Ok(x) => x,
                Err(e) => return Ok(tool_error(e.to_string())),
            },
            (None, Some(value)) => match serde_json::from_value::<Action>(value) {
                Ok(a) => (a, self.session.current_version()),
                Err(e) => return Ok(tool_error(format!("that is not a valid action object: {e}"))),
            },
            _ => return Ok(tool_error("pass exactly one of action_id or action")),
        };
        match self.session.act(action.clone(), version).await {
            Ok((events, view, legal)) => {
                let names = |id: ObjectId| format!("{} {id}", self.session.name_of(id));
                let seats = |s: engine::Seat| self.session.seat_name(s);
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
                text.push_str(&render_state(&self.session, &view, &legal));
                let json = serde_json::json!({
                    "applied": action,
                    "events": events,
                    "state": view,
                    "legal_actions": legal,
                    "still_your_turn": view.must_act.contains_key(&self.session.me),
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
        description = "Block until it is your turn to act (priority, blockers, a mulligan, a choice) or the game ends. Returns the state, why you must act, and your legal actions. On timeout returns { \"timed_out\": true }; just call it again."
    )]
    pub async fn wait_for_turn(&self, Parameters(p): Parameters<WaitParams>) -> Result<CallToolResult, ErrorData> {
        let secs = p.timeout_seconds.map(u64::from).unwrap_or(DEFAULT_WAIT_SECS).max(1);
        if !self.session.started() {
            self.session.refresh().await;
        }
        match self.session.wait_for_turn(Duration::from_secs(secs)).await {
            Wait::TimedOut => Ok(text_and_json(
                format!("Still waiting after {secs}s: not your turn yet. Call wait_for_turn again."),
                serde_json::json!({ "timed_out": true }),
            )),
            Wait::Ready(view) => {
                if let Some(o) = view.outcome {
                    let text = format!("The game is over: {}.\n\n{}", outcome_text(&self.session, o), render_state(&self.session, &view, &[]));
                    return Ok(text_and_json(text, serde_json::json!({ "game_over": true, "outcome": o, "state": view })));
                }
                let (legal, _, reason) = self.session.legal_actions().await.unwrap_or_default();
                let extra = serde_json::json!({ "reason": reason, "timed_out": false });
                Ok(self.state_result(&view, &legal, Some(extra)))
            }
        }
    }

    #[tool(
        name = "get_card",
        description = "Look up a card by name or by object id: its cost, types, power/toughness, and rules text, plus its current state if it is on the battlefield."
    )]
    pub async fn get_card(&self, Parameters(p): Parameters<GetCardParams>) -> Result<CallToolResult, ErrorData> {
        let view = self.session.view();
        if let Some(id) = p.object_id {
            let id = ObjectId(id);
            let Some(o) = view.as_ref().and_then(|v| v.object(id).cloned()) else {
                return Ok(tool_error(format!("{id} is not visible to you (or does not exist)")));
            };
            let def = self.session.cards.lookup(&o.name).map(|c| self.session.cards.get(c).clone());
            let mut text = card_text(&o.name, &o.cost.to_string(), &o.types, &o.subtypes, o.pt, &o.text);
            let mut state = vec![format!("{:?}", o.zone).to_lowercase(), format!("controlled by {}", self.session.seat_name(o.controller))];
            if o.tapped {
                state.push("tapped".into());
            }
            if o.summoning_sick && o.pt.is_some() {
                state.push("summoning sick".into());
            }
            if o.damage > 0 {
                state.push(format!("{} damage marked", o.damage));
            }
            if o.attacking.is_some() {
                state.push("attacking".into());
            }
            if !o.blocking.is_empty() {
                state.push(format!("blocking {}", o.blocking.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", ")));
            }
            text.push_str(&format!("\nState: {}", state.join(", ")));
            let mut json = serde_json::json!({ "object": o });
            if p.include_ir.unwrap_or(false) {
                json["definition"] = serde_json::to_value(&def).unwrap_or_default();
            }
            return Ok(text_and_json(text, json));
        }
        let Some(name) = p.name else { return Ok(tool_error("pass name or object_id")) };
        let Some(id) = self.session.cards.lookup(&name) else {
            return Ok(tool_error(format!("no card named {name:?} in this game's card set")));
        };
        let def = self.session.cards.get(id);
        let text = card_text(&def.name, &def.cost.to_string(), &def.types, &def.subtypes, def.pt, &def.text);
        let mut json = serde_json::json!({ "card": { "name": def.name, "cost": def.cost.to_string(), "types": def.types, "subtypes": def.subtypes, "pt": def.pt, "text": def.text } });
        if p.include_ir.unwrap_or(false) {
            json["definition"] = serde_json::to_value(def).unwrap_or_default();
        }
        Ok(text_and_json(text, json))
    }

    #[tool(name = "get_log", description = "The game log including table chat, seat-filtered, one line per event.")]
    pub async fn get_log(&self, Parameters(p): Parameters<GetLogParams>) -> Result<CallToolResult, ErrorData> {
        let lines = self.session.log_since(p.since_turn);
        let text = if lines.is_empty() {
            "(nothing yet)".to_string()
        } else {
            lines.iter().map(|l| format!("T{}  {}", l.turn, l.text)).collect::<Vec<_>>().join("\n")
        };
        Ok(text_and_json(text, serde_json::json!({ "lines": lines })))
    }

    #[tool(name = "say", description = "Say something to the table (parameter: text). It appears in the other players' logs.")]
    pub async fn say(&self, Parameters(p): Parameters<SayParams>) -> Result<CallToolResult, ErrorData> {
        match self.session.client.chat(&p.text, None).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text("said")])),
            Err(e) => Ok(tool_error(describe_client_error(e).to_string())),
        }
    }

    #[tool(name = "concede", description = "Concede the game. This ends it for you immediately.")]
    pub async fn concede(&self) -> Result<CallToolResult, ErrorData> {
        let version = self.session.current_version();
        match self.session.act(Action::Concede, version).await {
            Ok((_, view, _)) => {
                let text = match view.outcome {
                    Some(o) => format!("You conceded. {}", outcome_text(&self.session, o)),
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
        if self.session.started() {
            return Ok(tool_error("the game has already started"));
        }
        match self.session.client.set_deck(&p.decklist).await {
            Ok(Ok(())) => {}
            Ok(Err(violations)) => {
                let list: Vec<String> = violations.iter().map(|v| format!("- {v}")).collect();
                return Ok(tool_error(format!("deck rejected:\n{}", list.join("\n"))));
            }
            Err(e) => return Ok(tool_error(describe_client_error(e).to_string())),
        }
        match self.session.client.ready().await {
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
        let me = self.session.me;
        let text = format!(
            "You are playing Magic: The Gathering as seat {} at a manaline table. Read the resource `{PRIMER_URI}` first if you have not played before.\n\n\
             Then loop:\n\
             1. Call `wait_for_turn` (it blocks until you must act; if it returns timed_out, call it again).\n\
             2. Read the state and the numbered legal actions it returns. Think about the board.\n\
             3. Call `take_action` with the id you chose. If the reply says it is still your turn, choose again from the new list; when you have nothing worth doing, take the `Pass priority` action.\n\
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
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_prompts().enable_resources().build());
        info.server_info = Implementation::new("manaline", env!("CARGO_PKG_VERSION")).with_title("manaline");
        info.with_instructions(format!(
                "manaline: you are seat {} in a game of Magic: The Gathering. Read `{PRIMER_URI}` for the rules, then loop wait_for_turn → take_action. Use `say` to talk to the table.",
                self.session.me.0
            ))
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
                .with_description("A plain-English summary of turn structure, priority, combat, and the stack, for an agent that has never played.")
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
            CUBE_URI => cube_text(&self.session.cards),
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
        let sub = if c.subtypes.is_empty() { String::new() } else { format!(" — {}", c.subtypes.join(" ")) };
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
        self.session.outcome()
    }
}
