//! The MCP server against a real daemon: a whole game played through the
//! tools, chat and log, card lookup, and the streamable HTTP transport.

use daemon::{CreateGame, Daemon, DaemonConfig, DaemonHandle};
use engine::{Action, Outcome, Seat};
use mcp::server::{
    DeckStatsParams, EditorCardParams, EditorRemoveParams, EditorReplaceParams, EditorSetCountParams, GetCardParams, GetDeckParams,
    GetLogParams, SayParams, SearchParams, SitDownParams, SubmitDeckParams, TakeActionParams, WaitParams,
};
use mcp::SessionConfig;
use protocol::endpoint::{GameMarker, Runtime, SeatKind, SeatSlot};
use protocol::{Client, ClientError, Endpoint, ServerMessage, Token};
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Running {
    handle: DaemonHandle,
    endpoint: Endpoint,
    tokens: Vec<Token>,
    task: tokio::task::JoinHandle<()>,
    /// This test's own runtime directory, so published games and seat claims
    /// are invisible to the other tests (and to the real machine).
    runtime: Runtime,
    socket: std::path::PathBuf,
    game_id: String,
}

async fn start(seed: u64) -> Running {
    let dir = std::env::temp_dir().join(format!("manaline-mcp-test-{}-{seed}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = DaemonConfig {
        socket: Some(dir.join("game.sock")),
        no_socket: false,
        tcp: None,
        parent_pid: None,
        replay_dir: Some(dir.join("games")),
        create: Some(CreateGame {
            format: "cube".into(),
            seats: 2,
            seed: Some(seed),
        }),
        cards: Arc::new(cards::core()),
        legality: None,
    };
    let d = Daemon::bind(config).await.unwrap();
    let info = d.info().clone();
    let handle = d.handle();
    let task = tokio::spawn(async move { d.run().await.unwrap() });
    let socket = info.socket.unwrap();
    Running {
        handle,
        endpoint: Endpoint::Unix(socket.clone()),
        tokens: info.seat_tokens,
        task,
        runtime: Runtime::at(dir.join("runtime")),
        socket,
        game_id: info.game_id.map(|g| g.0).unwrap_or_else(|| "game".into()),
    }
}

/// Publish this daemon in `rt` for agents to find, as `play` does: one seat
/// slot per entry, agent seats carrying that seat's real token.
fn publish(rt: &Runtime, r: &Running, seats: &[(SeatKind, Option<&str>)]) -> GameMarker {
    let slots: Vec<SeatSlot> = seats
        .iter()
        .enumerate()
        .map(|(i, (kind, deck))| SeatSlot {
            seat: i as u8,
            kind: *kind,
            name: match kind {
                SeatKind::Human => "Connor".into(),
                _ => format!("Claude {i}"),
            },
            deck: deck.map(str::to_string),
            token: (*kind == SeatKind::Agent).then(|| r.tokens[i].clone()),
        })
        .collect();
    let marker = GameMarker {
        game_id: r.game_id.clone(),
        pid: std::process::id(),
        socket: Some(r.socket.clone()),
        tcp: None,
        format: "cube".into(),
        spectator_token: None,
        seats: slots,
        dir: std::path::PathBuf::new(),
    };
    rt.publish_game(&marker).unwrap()
}

fn text_of(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_error(r: &CallToolResult) -> bool {
    r.is_error.unwrap_or(false)
}

fn legal_ids(r: &CallToolResult) -> Vec<(u32, String)> {
    let v = r.structured_content.as_ref().unwrap();
    let list = v
        .get("legal_actions")
        .or_else(|| v.get("actions"))
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    list.iter()
        .map(|a| (a["id"].as_u64().unwrap() as u32, a["description"].as_str().unwrap().to_string()))
        .collect()
}

/// The human seat: random legal play, never conceding.
async fn human_loop(mut c: Client, seed: u64) -> Client {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    loop {
        let (acts, version) = c.get_legal_actions().await.unwrap();
        let playable: Vec<_> = acts.iter().filter(|a| !matches!(a.action, Action::Concede)).collect();
        if let Some(pick) = playable.choose(&mut rng) {
            match c.act_by_id(pick.id, version).await {
                Ok((_, state, _)) if state.outcome.is_some() => return c,
                Ok(_) => continue,
                Err(ClientError::Protocol(e)) if e.retryable => continue,
                Err(e) => panic!("{e}"),
            }
        }
        let state = c.get_state().await.unwrap();
        if state.outcome.is_some() {
            return c;
        }
        match c.next_push().await {
            Ok(ServerMessage::Event {
                event: engine::EventBase::GameOver { .. },
                ..
            }) => return c,
            Ok(_) => continue,
            Err(_) => return c,
        }
    }
}

#[derive(Debug, Default)]
struct Played {
    outcome: Option<Outcome>,
    actions: u32,
    auto_passed: u64,
    /// Times the agent was woken with nothing but pass/concede to choose from.
    pass_only_wakeups: u32,
}

/// Play a seat to the end through the tools, the way the prompt tells an agent
/// to: `wait_for_turn`, then the first action that does something.
async fn agent_loop(server: &mcp::McpServer, timeout_seconds: u32) -> Played {
    let mut played = Played::default();
    loop {
        let res = server
            .wait_for_turn(Parameters(WaitParams {
                timeout_seconds: Some(timeout_seconds),
                auto_pass: None,
            }))
            .await
            .unwrap();
        let sc = res.structured_content.clone().unwrap();
        played.auto_passed += sc.get("auto_passed").and_then(|n| n.as_u64()).unwrap_or(0);
        if sc.get("game_over").and_then(|g| g.as_bool()).unwrap_or(false) {
            played.outcome = Some(serde_json::from_value::<Outcome>(sc["outcome"].clone()).unwrap());
            return played;
        }
        if sc.get("timed_out").and_then(|t| t.as_bool()).unwrap_or(false) {
            panic!("agent waited {timeout_seconds}s without a turn");
        }
        assert!(text_of(&res).contains("IT IS YOUR TURN TO ACT"), "{}", text_of(&res));
        let mut ids = legal_ids(&res);
        if ids.iter().all(|(_, d)| d == "Pass priority" || d == "Concede") {
            played.pass_only_wakeups += 1;
        }
        loop {
            let (id, _) = ids
                .iter()
                .find(|(_, d)| d.starts_with("Keep") || d.starts_with("Play ") || d.starts_with("Cast "))
                .or_else(|| ids.iter().find(|(_, d)| d != "Concede"))
                .cloned()
                .expect("a non-concede action");
            let res = server
                .take_action(Parameters(TakeActionParams {
                    action_id: Some(id),
                    action: None,
                    state_version: None,
                }))
                .await
                .unwrap();
            assert!(!is_error(&res), "{}", text_of(&res));
            played.actions += 1;
            let sc = res.structured_content.clone().unwrap();
            if sc["state"]["outcome"].is_null() && sc["still_your_turn"].as_bool().unwrap() {
                ids = legal_ids(&res);
                continue;
            }
            break;
        }
        if let Some(o) = server.outcome() {
            played.outcome = Some(o);
            return played;
        }
    }
}

#[tokio::test]
async fn an_agent_plays_a_whole_game_through_the_tools() {
    let r = start(17).await;

    // The agent joins first with a deck; the state tools say the game has not started.
    let server = mcp::connect(SessionConfig {
        endpoint: r.endpoint.clone(),
        token: r.tokens[1].clone(),
        name: "Claude".into(),
        decklist: Some(cards::deck_text("red").unwrap()),
    })
    .await
    .unwrap();
    assert_eq!(server.session().unwrap().me, Seat(1));
    let res = server.get_game_state().await.unwrap();
    assert!(is_error(&res));
    assert!(text_of(&res).contains("has not started"), "{}", text_of(&res));
    let res = server
        .wait_for_turn(Parameters(WaitParams {
            timeout_seconds: Some(1),
            auto_pass: None,
        }))
        .await
        .unwrap();
    assert_eq!(res.structured_content.as_ref().unwrap()["timed_out"], true);

    // The human sits down and readies; the game starts.
    let mut human = Client::connect(&r.endpoint).await.unwrap();
    human.hello(&r.tokens[0], Some("Connor")).await.unwrap();
    human.subscribe().await.unwrap();
    human.set_deck(&cards::deck_text("green").unwrap()).await.unwrap().unwrap();
    human.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();

    // Chat round trip: the human hears it, and it lands in the agent's log.
    let res = server.say(Parameters(SayParams { text: "gl hf".into() })).await.unwrap();
    assert!(!is_error(&res));
    loop {
        match human.next_push().await.unwrap() {
            ServerMessage::Event {
                event: engine::EventBase::Chat { from, text, .. },
                ..
            } => {
                assert_eq!(from, Seat(1));
                assert_eq!(text, "gl hf");
                break;
            }
            _ => continue,
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let log = server.get_log(Parameters(GetLogParams { since_turn: None })).await.unwrap();
    assert!(text_of(&log).contains("You: gl hf"), "{}", text_of(&log));
    assert!(text_of(&log).contains("Game started"), "{}", text_of(&log));

    // Card lookup by name.
    let card = server
        .get_card(Parameters(GetCardParams {
            name: Some("hill giant".into()),
            object_id: None,
            include_ir: None,
        }))
        .await
        .unwrap();
    assert!(text_of(&card).contains("Hill Giant {3}{R}"), "{}", text_of(&card));
    assert!(text_of(&card).contains("3/3"));

    // Play the game out: the human at random, the agent through wait_for_turn / take_action.
    let human_task = tokio::spawn(human_loop(human, 3));
    let played = agent_loop(&server, 20).await;
    human_task.await.unwrap();
    assert!(played.actions > 20, "the agent took {} actions", played.actions);
    assert!(matches!(played.outcome, Some(Outcome::Winner(_))));
    assert!(
        played.auto_passed > 0,
        "wait_for_turn never passed a nothing-to-do moment for the agent"
    );
    assert!(
        played.pass_only_wakeups == 0,
        "woken {} times with only pass/concede available",
        played.pass_only_wakeups
    );

    // Once over, tools say so instead of erroring.
    let res = server.get_game_state().await.unwrap();
    assert!(text_of(&res).contains("GAME OVER"));
    let res = server
        .wait_for_turn(Parameters(WaitParams {
            timeout_seconds: Some(1),
            auto_pass: None,
        }))
        .await
        .unwrap();
    assert_eq!(res.structured_content.as_ref().unwrap()["game_over"], true);

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn stale_ids_and_wrong_turns_come_back_as_tool_errors() {
    let r = start(5).await;
    let server = mcp::connect(SessionConfig {
        endpoint: r.endpoint.clone(),
        token: r.tokens[1].clone(),
        name: "Agent".into(),
        decklist: Some(cards::deck_text("blue").unwrap()),
    })
    .await
    .unwrap();
    let mut human = Client::connect(&r.endpoint).await.unwrap();
    human.hello(&r.tokens[0], Some("Connor")).await.unwrap();
    human.set_deck(&cards::deck_text("green").unwrap()).await.unwrap().unwrap();
    human.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();

    let res = server
        .take_action(Parameters(TakeActionParams {
            action_id: Some(0),
            action: None,
            state_version: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    assert!(text_of(&res).contains("get_legal_actions first"), "{}", text_of(&res));
    let res = server
        .take_action(Parameters(TakeActionParams {
            action_id: None,
            action: None,
            state_version: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    let res = server
        .take_action(Parameters(TakeActionParams {
            action_id: None,
            action: Some(serde_json::json!({"kind": "pass_priority"})),
            state_version: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res), "acting out of turn or illegally is reported: {}", text_of(&res));
    let res = server
        .get_card(Parameters(GetCardParams {
            name: Some("Black Lotus".into()),
            object_id: None,
            include_ir: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    let res = server.concede().await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert_eq!(server.outcome(), Some(Outcome::Winner(Seat(0))));

    r.handle.shutdown();
    r.task.await.unwrap();
}

/// One JSON-RPC request over HTTP/1.1 by hand, so the transport is tested
/// without an MCP client dependency. Returns the JSON body (JSON or SSE).
async fn rpc(addr: &std::net::SocketAddr, session: &Option<String>, body: serde_json::Value) -> (Option<String>, serde_json::Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.to_string();
    let mut req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2025-06-18\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(id) = session {
        req.push_str(&format!("Mcp-Session-Id: {id}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").expect("http response");
    let sid = head.lines().find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.eq_ignore_ascii_case("mcp-session-id"))
            .map(|(_, v)| v.trim().to_string())
    });
    // Chunked bodies: strip chunk sizes; SSE bodies: take data: lines.
    let mut payload = String::new();
    let chunked = head.to_ascii_lowercase().contains("transfer-encoding: chunked");
    let body = if chunked {
        let mut out = String::new();
        let mut rest = body;
        while let Some((size, after)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if n == 0 {
                break;
            }
            out.push_str(&after[..n]);
            rest = &after[n + 2..];
        }
        out
    } else {
        body.to_string()
    };
    for line in body.lines() {
        if let Some(d) = line.strip_prefix("data:") {
            payload.push_str(d.trim());
        }
    }
    if payload.is_empty() {
        payload = body.trim().to_string();
    }
    let value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&payload).unwrap_or_else(|e| panic!("{e}: {payload}"))
    };
    (sid, value)
}

#[tokio::test]
async fn streamable_http_lists_tools_and_serves_resources() {
    let r = start(9).await;
    let server = mcp::connect(SessionConfig {
        endpoint: r.endpoint.clone(),
        token: r.tokens[1].clone(),
        name: "Agent".into(),
        decklist: None,
    })
    .await
    .unwrap();
    let http = mcp::serve_http(server, "127.0.0.1:0").await.unwrap();
    let addr = http.addr;
    assert!(http.url().ends_with("/mcp"));

    let (sid, init) = rpc(
        &addr,
        &None,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
    )
    .await;
    assert_eq!(init["result"]["serverInfo"]["name"], "manaline", "{init}");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("seat 1"));
    let _ = rpc(
        &addr,
        &sid,
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;

    let (_, tools) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await;
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "get_game_state",
        "get_legal_actions",
        "take_action",
        "wait_for_turn",
        "get_card",
        "get_log",
        "say",
        "concede",
        "submit_deck",
        "search_cards",
        "deck_stats",
    ] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    assert!(
        !names.iter().any(|n| n.starts_with("editor_")),
        "deckbuilder tools are hidden from a seat in a game: {names:?}"
    );
    let take = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "take_action")
        .unwrap();
    assert!(take["inputSchema"]["properties"]["action_id"].is_object(), "{take}");

    let (_, res) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":3,"method":"resources/list"})).await;
    let uris: Vec<&str> = res["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["uri"].as_str().unwrap())
        .collect();
    assert!(
        uris.contains(&"manaline://rules-primer") && uris.contains(&"manaline://cube"),
        "{uris:?}"
    );
    // Strict 2026-07-28 clients require the cache hints on every result.
    assert_eq!(res["result"]["ttlMs"], 0, "{res}");
    assert_eq!(res["result"]["cacheScope"], "public");
    let (_, primer) = rpc(
        &addr,
        &sid,
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"manaline://rules-primer"}}),
    )
    .await;
    assert!(primer["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Priority and passing"));
    assert_eq!(primer["result"]["ttlMs"], 0, "{primer}");
    assert_eq!(primer["result"]["cacheScope"], "public");
    let (_, tl) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":30,"method":"tools/list"})).await;
    let _ = tl;
    let (_, cube) = rpc(
        &addr,
        &sid,
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"manaline://cube"}}),
    )
    .await;
    assert!(cube["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Grizzly Bears {1}{G}"));

    let (_, prompts) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":6,"method":"prompts/list"})).await;
    assert_eq!(prompts["result"]["prompts"][0]["name"], "play-a-game");
    let (_, prompt) = rpc(
        &addr,
        &sid,
        serde_json::json!({"jsonrpc":"2.0","id":7,"method":"prompts/get","params":{"name":"play-a-game"}}),
    )
    .await;
    assert!(prompt["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .contains("wait_for_turn"));

    // `say` accepts `message` as well as `text`.
    let (_, said) = rpc(
        &addr,
        &sid,
        serde_json::json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"say","arguments":{"message":"hello table"}}}),
    )
    .await;
    assert_eq!(said["result"]["content"][0]["text"], "said", "{said}");

    // A tool call over the wire: the game has not started, which is a tool error, not a protocol error.
    let (_, call) = rpc(
        &addr,
        &sid,
        serde_json::json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"get_game_state","arguments":{}}}),
    )
    .await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("not started"));

    http.shutdown().await;
    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn action_ids_are_bound_to_their_state_version() {
    let r = start(11).await;
    let server = mcp::connect(SessionConfig {
        endpoint: r.endpoint.clone(),
        token: r.tokens[1].clone(),
        name: "Agent".into(),
        decklist: Some(cards::deck_text("blue").unwrap()),
    })
    .await
    .unwrap();
    let mut human = Client::connect(&r.endpoint).await.unwrap();
    human.hello(&r.tokens[0], Some("Connor")).await.unwrap();
    human.set_deck(&cards::deck_text("green").unwrap()).await.unwrap().unwrap();
    human.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();

    // If the agent decides first, keep (with the right version) so the human is up.
    if status.borrow().must_act.contains_key(&Seat(1)) {
        let res = server.get_legal_actions().await.unwrap();
        let v = res.structured_content.as_ref().unwrap()["state_version"].as_u64().unwrap();
        let wrong = server
            .take_action(Parameters(TakeActionParams {
                action_id: Some(0),
                action: None,
                state_version: Some(v + 7),
            }))
            .await
            .unwrap();
        assert!(is_error(&wrong) && text_of(&wrong).contains("named version"), "{}", text_of(&wrong));
        let ok = server
            .take_action(Parameters(TakeActionParams {
                action_id: Some(0),
                action: None,
                state_version: Some(v),
            }))
            .await
            .unwrap();
        assert!(!is_error(&ok), "{}", text_of(&ok));
    }
    status.wait_for(|s| s.must_act.contains_key(&Seat(0))).await.unwrap();

    // The agent fetches a list while the human is deciding, the human acts, and
    // the stale id is refused rather than remapped onto the new list.
    let res = server.get_legal_actions().await.unwrap();
    let stale_version = res.structured_content.as_ref().unwrap()["state_version"].as_u64().unwrap();
    let (acts, version) = human.get_legal_actions().await.unwrap();
    let keep = acts.iter().find(|a| a.description.starts_with("Keep")).unwrap();
    human.act_by_id(keep.id, version).await.unwrap();
    server.session().unwrap().refresh().await;
    let res = server
        .take_action(Parameters(TakeActionParams {
            action_id: Some(0),
            action: None,
            state_version: Some(stale_version),
        }))
        .await
        .unwrap();
    assert!(is_error(&res), "{}", text_of(&res));
    assert!(text_of(&res).contains("moved on"), "{}", text_of(&res));

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn a_newer_call_supersedes_an_abandoned_wait() {
    let r = start(12).await;
    let server = mcp::connect(SessionConfig {
        endpoint: r.endpoint.clone(),
        token: r.tokens[1].clone(),
        name: "Agent".into(),
        decklist: Some(cards::deck_text("blue").unwrap()),
    })
    .await
    .unwrap();
    // The game has not started: a wait would block for its whole timeout.
    let waiter = server.clone();
    let task = tokio::spawn(async move {
        waiter
            .wait_for_turn(Parameters(WaitParams {
                timeout_seconds: Some(30),
                auto_pass: None,
            }))
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = server.get_legal_actions().await.unwrap();
    let res = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("the wait returns promptly")
        .unwrap();
    assert_eq!(res.structured_content.as_ref().unwrap()["superseded"], true, "{}", text_of(&res));

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn search_and_deck_stats_tools_work_before_the_game_starts() {
    let r = start(13).await;
    let server = mcp::connect(SessionConfig {
        endpoint: r.endpoint.clone(),
        token: r.tokens[1].clone(),
        name: "Agent".into(),
        decklist: None,
    })
    .await
    .unwrap();
    let res = server
        .search_cards(Parameters(SearchParams {
            query: "t:creature c:g mv<=1 kw:deathtouch or name:archdruid".into(),
            limit: Some(10),
            include_unimplemented: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&res));
    let text = text_of(&res);
    assert!(text.contains("Elvish Archdruid"), "{text}");
    assert!(!text.contains("not implemented"), "playable only by default: {text}");
    let res = server
        .search_cards(Parameters(SearchParams {
            query: "frob:1".into(),
            limit: None,
            include_unimplemented: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));

    let res = server
        .deck_stats(Parameters(DeckStatsParams {
            decklist: cards::deck_text("green").unwrap(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&res));
    let sc = res.structured_content.clone().unwrap();
    assert_eq!(sc["cards"], 40);
    assert_eq!(sc["lands"], 17);
    assert_eq!(sc["legal"], true);
    assert!(text_of(&res).contains("curve"));
    let res = server
        .deck_stats(Parameters(DeckStatsParams {
            decklist: "4 Grizzly Bears\n4 Black Lotus\n".into(),
        }))
        .await
        .unwrap();
    let sc = res.structured_content.clone().unwrap();
    assert_eq!(sc["legal"], false);
    assert!(
        sc["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("Black Lotus")),
        "{sc}"
    );

    // A seat started without a deck is told to pick one; it lists, reads, and submits by name.
    let res = server.get_legal_actions().await.unwrap();
    assert!(is_error(&res) && text_of(&res).contains("submit_deck"), "{}", text_of(&res));
    let res = server.list_decks().await.unwrap();
    let listing = text_of(&res);
    assert!(listing.contains("rg-stompy") && listing.contains("legal"), "{listing}");
    assert!(
        !listing.contains("deckbuilder"),
        "seated servers say nothing about an editor: {listing}"
    );
    let res = server.get_deck(Parameters(GetDeckParams { name: "blue".into() })).await.unwrap();
    assert!(
        !is_error(&res) && text_of(&res).contains("Island") && text_of(&res).contains("curve"),
        "{}",
        text_of(&res)
    );
    let res = server
        .get_deck(Parameters(GetDeckParams {
            name: "no-such-deck".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    let res = server.editor_status().await.unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("only available"),
        "seated: no deckbuilder tools: {}",
        text_of(&res)
    );
    let res = server
        .submit_deck(Parameters(SubmitDeckParams {
            name: Some("no-such-deck".into()),
            decklist: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    let res = server
        .submit_deck(Parameters(SubmitDeckParams {
            name: Some("blue".into()),
            decklist: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("ready"), "{}", text_of(&res));
    assert!(server.session().unwrap().lobby().seats[1].deck_ok);

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn a_standalone_server_serves_card_data_without_a_game() {
    let rt = std::env::temp_dir().join(format!("manaline-standalone-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&rt);
    let server = mcp::standalone(engine::Format::cube()).with_runtime(Runtime::at(&rt));
    let res = server
        .search_cards(Parameters(SearchParams {
            query: "t:creature kw:flying c:w mv<=3".into(),
            limit: Some(20),
            include_unimplemented: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("Suntail Hawk"), "{}", text_of(&res));
    let res = server
        .deck_stats(Parameters(DeckStatsParams {
            decklist: "36 Serra Angel\n24 Plains\n".into(),
        }))
        .await
        .unwrap();
    let sc = res.structured_content.clone().unwrap();
    assert_eq!(sc["legal"], false);
    assert!(text_of(&res).contains("at most 4 allowed"), "{}", text_of(&res));
    let res = server
        .get_card(Parameters(GetCardParams {
            name: Some("Grizzly Bears".into()),
            object_id: None,
            include_ir: None,
        }))
        .await
        .unwrap();
    assert!(text_of(&res).contains("2/2"));
    // Game tools explain themselves instead of failing to connect.
    let res = server.get_game_state().await.unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("No game is connected"),
        "{}",
        text_of(&res)
    );
    let res = server
        .wait_for_turn(Parameters(WaitParams {
            timeout_seconds: Some(1),
            auto_pass: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    assert!(mcp::server::cube_text(server.cards()).contains("Serra Angel"));

    // With no deckbuilder open, the editor tools say so.
    let res = server.editor_status().await.unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("no deckbuilder is open"),
        "{}",
        text_of(&res)
    );
    let res = server.list_decks().await.unwrap();
    assert!(text_of(&res).contains("No deckbuilder is open"), "{}", text_of(&res));

    // A deckbuilder open on a file: every change goes through it, is visible in its state, and only
    // reaches the file on save.
    let dir = std::env::temp_dir().join(format!("manaline-editor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("tokens.txt");
    std::fs::write(&file, "17 Plains\n").unwrap();
    let db = std::sync::Arc::new(cards::core());
    let index = std::sync::Arc::new(cardsearch::Index::from_db(&db));
    let editor = tui::editor::Editor::new(tui::editor::EditorSetup {
        banner: None,
        path: Some(file.clone()),
        text: "17 Plains\n".into(),
        format: engine::Format::cube(),
        db,
        index,
        known: None,
        theme: Default::default(),
    })
    .unwrap();
    let editor = std::sync::Arc::new(std::sync::Mutex::new(editor));
    let socket = dir.join("editor.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let service = tokio::spawn(tui::editor_service(listener, editor.clone()));
    let announced = server.runtime().announce_editor(&file, "cube", Some(&socket)).unwrap();

    let res = server.editor_status().await.unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("17 cards"), "{}", text_of(&res));
    let res = server.list_decks().await.unwrap();
    assert!(text_of(&res).contains("open in the deckbuilder"), "{}", text_of(&res));
    let res = server
        .editor_add_card(Parameters(EditorCardParams {
            name: "Raise the Alarm".into(),
            count: Some(4),
        }))
        .await
        .unwrap();
    assert!(
        !is_error(&res) && text_of(&res).contains("added 4 Raise the Alarm"),
        "{}",
        text_of(&res)
    );
    assert!(
        editor.lock().unwrap().agent_marked("Raise the Alarm"),
        "the human sees the change marked"
    );
    assert!(editor.lock().unwrap().dirty);
    let res = server
        .editor_add_card(Parameters(EditorCardParams {
            name: "Raise teh Alarm".into(),
            count: None,
        }))
        .await
        .unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("did you mean Raise the Alarm"),
        "{}",
        text_of(&res)
    );
    let res = server
        .editor_set_count(Parameters(EditorSetCountParams {
            name: "Attended Knight".into(),
            count: 3,
        }))
        .await
        .unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    let res = server
        .editor_remove_card(Parameters(EditorRemoveParams {
            name: "Attended Knight".into(),
            count: None,
            all: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("now 2"), "{}", text_of(&res));
    let res = server.editor_deck().await.unwrap();
    let deck = text_of(&res);
    assert!(
        deck.contains("Creatures (2)") && deck.contains(" 4 Raise the Alarm") && deck.contains("Lands (17)"),
        "{deck}"
    );
    let res = server.editor_undo().await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert!(text_of(&server.editor_deck().await.unwrap()).contains("Creatures (3)"));
    let res = server.editor_stats().await.unwrap();
    assert!(
        !is_error(&res) && text_of(&res).contains("curve") && text_of(&res).contains("sample opening hands"),
        "{}",
        text_of(&res)
    );
    assert!(editor.lock().unwrap().show_stats, "the human sees the stats pane too");
    let res = server
        .editor_replace_deck(Parameters(EditorReplaceParams {
            decklist: "17 Plains\n4 Grizly Bears\n".into(),
        }))
        .await
        .unwrap();
    assert!(is_error(&res) && text_of(&res).contains("nothing changed"), "{}", text_of(&res));
    let res = server
        .editor_replace_deck(Parameters(EditorReplaceParams {
            decklist: "17 Plains\n4 Suntail Hawk\n".into(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("21 cards"), "{}", text_of(&res));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "17 Plains\n",
        "nothing touched the file yet"
    );
    let res = server.editor_save().await.unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("Saved"), "{}", text_of(&res));
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.starts_with("Deck\n4 Suntail Hawk\n17 Plains\n"), "canonical order: {saved}");
    assert!(!editor.lock().unwrap().dirty);

    announced.withdraw();
    service.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_session_seats_itself_from_the_published_game() {
    let r = start(21).await;
    let game = publish(&r.runtime, &r, &[(SeatKind::Human, None), (SeatKind::Agent, Some("red"))]);
    let server = mcp::standalone(engine::Format::cube()).with_runtime(r.runtime.clone());
    assert!(server.session().is_none(), "a fresh session holds no seat");

    // The first game tool call sits the session down in the one agent seat,
    // with the deck the marker named for it.
    let res = server.get_game_state().await.unwrap();
    assert!(is_error(&res) && text_of(&res).contains("has not started"), "{}", text_of(&res));
    assert_eq!(server.seat(), Some(Seat(1)));
    assert_eq!(server.mode(), format!("game {}, seat 1", r.game_id));
    assert_eq!(game.claims(), vec![(1, std::process::id())], "the claim names this process");
    assert!(
        server.session().unwrap().lobby().seats[1].deck_ok,
        "the deck from the marker was submitted"
    );

    // The human's seat is not on offer to another agent, and the agent seat is taken.
    let nosy = server.new_session();
    let res = nosy.sit_down(Parameters(SitDownParams { seat: Some(0) })).await.unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("not a free agent seat"),
        "{}",
        text_of(&res)
    );
    let res = nosy.sit_down(Parameters(SitDownParams::default())).await.unwrap();
    assert!(is_error(&res) && text_of(&res).contains("every agent seat"), "{}", text_of(&res));
    assert!(nosy.session().is_none());

    // The human sits down for real and the game plays out through the tools.
    let mut human = Client::connect(&r.endpoint).await.unwrap();
    human.hello(&r.tokens[0], Some("Connor")).await.unwrap();
    human.subscribe().await.unwrap();
    human.set_deck(&cards::deck_text("green").unwrap()).await.unwrap().unwrap();
    human.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();
    let human_task = tokio::spawn(human_loop(human, 4));
    let played = agent_loop(&server, 20).await;
    human_task.await.unwrap();
    assert!(matches!(played.outcome, Some(Outcome::Winner(_))), "{played:?}");

    // The seat is kept after the game ends, so the final state is still readable.
    let res = server.get_game_state().await.unwrap();
    assert!(text_of(&res).contains("GAME OVER"), "{}", text_of(&res));
    assert_eq!(game.claims(), vec![(1, std::process::id())]);

    // A newer game is published: sit_down moves to it and gives the old seat back.
    let r2 = start(22).await;
    let newer = publish(&r.runtime, &r2, &[(SeatKind::Human, None), (SeatKind::Agent, Some("blue"))]);
    let res = server.sit_down(Parameters(SitDownParams::default())).await.unwrap();
    assert!(!is_error(&res) && text_of(&res).contains(&r2.game_id), "{}", text_of(&res));
    assert_eq!(server.seat(), Some(Seat(1)));
    assert!(game.claims().is_empty(), "the finished game's seat was released");
    assert_eq!(newer.claims(), vec![(1, std::process::id())]);

    // Leaving gives the claim back and drops to card data only.
    let res = server.leave().await.unwrap();
    assert!(
        !is_error(&res) && text_of(&res).contains("free for another agent"),
        "{}",
        text_of(&res)
    );
    assert!(server.session().is_none());
    assert_eq!(server.mode(), "card data only");
    assert!(newer.claims().is_empty());
    let res = server.leave().await.unwrap();
    assert!(text_of(&res).contains("not seated"), "{}", text_of(&res));

    r2.handle.shutdown();
    r2.task.await.unwrap();
    r.handle.shutdown();
    r.task.await.unwrap();
}

/// One MCP session over streamable HTTP: initialize once, then one tool call
/// per request, carrying this session's id.
struct HttpSession {
    addr: std::net::SocketAddr,
    sid: Option<String>,
    next_id: u64,
}

impl HttpSession {
    async fn open(addr: std::net::SocketAddr) -> HttpSession {
        let (sid, init) = rpc(
            &addr,
            &None,
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
        )
        .await;
        assert_eq!(init["result"]["serverInfo"]["name"], "manaline", "{init}");
        assert!(sid.is_some(), "the server hands out a session id: {init}");
        let _ = rpc(
            &addr,
            &sid,
            serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        HttpSession { addr, sid, next_id: 2 }
    }

    async fn call(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        let (_, res) = rpc(
            &self.addr,
            &self.sid,
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}}),
        )
        .await;
        assert!(res["result"].is_object(), "{name}: {res}");
        res["result"].clone()
    }
}

/// The first legal action worth taking: something that develops the board, or
/// anything at all as long as it is not conceding.
fn pick_action(legal: &serde_json::Value) -> Option<u64> {
    let list = legal.as_array()?;
    let starting = |prefix: &str| {
        list.iter()
            .find(|a| a["description"].as_str().is_some_and(|d| d.starts_with(prefix)))
            .cloned()
    };
    starting("Keep")
        .or_else(|| starting("Play "))
        .or_else(|| starting("Cast "))
        .or_else(|| list.iter().find(|a| a["description"] != "Concede").cloned())
        .and_then(|a| a["id"].as_u64())
}

/// An agent playing one seat over its own HTTP session: sit down, then
/// wait_for_turn / take_action until the game is over. Returns its seat and
/// the outcome it was told about.
async fn http_agent(addr: std::net::SocketAddr) -> (u64, serde_json::Value) {
    let mut s = HttpSession::open(addr).await;
    let sat = s.call("sit_down", serde_json::json!({})).await;
    assert_ne!(sat["isError"], serde_json::Value::Bool(true), "sit_down: {sat}");
    let seat = sat["structuredContent"]["seat"].as_u64().expect("a seat number");
    loop {
        let res = s.call("wait_for_turn", serde_json::json!({ "timeout_seconds": 20 })).await;
        let sc = res["structuredContent"].clone();
        if sc["game_over"].as_bool().unwrap_or(false) {
            return (seat, sc["outcome"].clone());
        }
        if sc["timed_out"].as_bool().unwrap_or(false) {
            continue;
        }
        assert_ne!(res["isError"], serde_json::Value::Bool(true), "wait_for_turn: {res}");
        let mut legal = sc["legal_actions"].clone();
        let mut version = sc["state_version"].as_u64();
        while let Some(id) = pick_action(&legal) {
            let mut args = serde_json::json!({ "action_id": id });
            if let Some(v) = version {
                args["state_version"] = v.into();
            }
            let res = s.call("take_action", args).await;
            if res["isError"] == serde_json::Value::Bool(true) {
                break; // the game moved on; wait for a fresh list
            }
            let sc = res["structuredContent"].clone();
            if !sc["still_your_turn"].as_bool().unwrap_or(false) || !sc["state"]["outcome"].is_null() {
                break;
            }
            legal = sc["legal_actions"].clone();
            version = sc["state_version"].as_u64();
        }
    }
}

#[tokio::test]
async fn two_http_sessions_take_two_seats_and_play_each_other() {
    let r = start(23).await;
    let game = publish(&r.runtime, &r, &[(SeatKind::Agent, Some("red")), (SeatKind::Agent, Some("green"))]);
    // One process, one set of cards, two MCP sessions: two seats.
    let server = mcp::standalone(engine::Format::cube()).with_runtime(r.runtime.clone());
    let http = mcp::serve_http(server, "127.0.0.1:0").await.unwrap();
    let addr = http.addr;
    let one = tokio::spawn(http_agent(addr));
    let two = tokio::spawn(http_agent(addr));
    let limit = std::time::Duration::from_secs(180);
    let (seat_one, outcome_one) = tokio::time::timeout(limit, one).await.expect("agent one finished").unwrap();
    let (seat_two, outcome_two) = tokio::time::timeout(limit, two).await.expect("agent two finished").unwrap();

    let mut seats = [seat_one, seat_two];
    seats.sort();
    assert_eq!(seats, [0, 1], "the two sessions took different seats");
    assert!(!outcome_one.is_null() && !outcome_two.is_null(), "{outcome_one} {outcome_two}");
    assert_eq!(outcome_one, outcome_two, "both agents saw the same outcome");
    assert_eq!(game.claims().len(), 2, "both seats were claimed by this process");

    http.shutdown().await;
    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn a_third_session_finds_every_seat_taken() {
    let r = start(24).await;
    let game = publish(&r.runtime, &r, &[(SeatKind::Agent, Some("red")), (SeatKind::Agent, Some("green"))]);
    let server = mcp::standalone(engine::Format::cube()).with_runtime(r.runtime.clone());
    let first = server.new_session();
    let second = server.new_session();
    let third = server.new_session();

    let res = first.sit_down(Parameters(SitDownParams::default())).await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert_eq!(first.seat(), Some(Seat(0)));
    let res = second.sit_down(Parameters(SitDownParams::default())).await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert_eq!(second.seat(), Some(Seat(1)));

    // The third agent is told what is wrong, both through sit_down and through
    // a game tool that would have seated it.
    let res = third.sit_down(Parameters(SitDownParams::default())).await.unwrap();
    assert!(is_error(&res) && text_of(&res).contains("every agent seat"), "{}", text_of(&res));
    let res = third.get_game_state().await.unwrap();
    assert!(is_error(&res) && text_of(&res).contains("every agent seat"), "{}", text_of(&res));
    assert!(third.session().is_none());
    // It can still do card work without a seat.
    let res = third.get_deck(Parameters(GetDeckParams { name: "red".into() })).await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));

    // When the first agent leaves, its seat is free for the third.
    let res = first.leave().await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert_eq!(game.claims(), vec![(1, std::process::id())]);
    let res = third.sit_down(Parameters(SitDownParams::default())).await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert_eq!(third.seat(), Some(Seat(0)));
    assert_eq!(game.claims(), vec![(0, std::process::id()), (1, std::process::id())]);

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn sit_down_picks_a_specific_seat() {
    let r = start(25).await;
    let game = publish(&r.runtime, &r, &[(SeatKind::Agent, Some("red")), (SeatKind::Agent, Some("green"))]);
    let server = mcp::standalone(engine::Format::cube()).with_runtime(r.runtime.clone());
    let picky = server.new_session();
    let res = picky.sit_down(Parameters(SitDownParams { seat: Some(1) })).await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    let sc = res.structured_content.clone().unwrap();
    assert_eq!(sc["seat"], 1);
    assert_eq!(sc["deck"], "green");
    assert_eq!(picky.seat(), Some(Seat(1)));

    // Already seated at a live game: sit_down says so instead of moving.
    let res = picky.sit_down(Parameters(SitDownParams { seat: Some(0) })).await.unwrap();
    assert!(is_error(&res) && text_of(&res).contains("already seated"), "{}", text_of(&res));
    assert_eq!(picky.seat(), Some(Seat(1)));

    // Another session is refused that seat and takes the other one.
    let other = server.new_session();
    let res = other.sit_down(Parameters(SitDownParams { seat: Some(1) })).await.unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("not a free agent seat"),
        "{}",
        text_of(&res)
    );
    let res = other.sit_down(Parameters(SitDownParams { seat: Some(0) })).await.unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    assert_eq!(other.seat(), Some(Seat(0)));
    assert_eq!(game.claims(), vec![(0, std::process::id()), (1, std::process::id())]);

    // A seat that does not exist is refused too.
    let late = server.new_session();
    let res = late.sit_down(Parameters(SitDownParams { seat: Some(7) })).await.unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("not a free agent seat"),
        "{}",
        text_of(&res)
    );

    r.handle.shutdown();
    r.task.await.unwrap();
}
