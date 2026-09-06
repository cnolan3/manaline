//! The MCP server against a real daemon: a whole game played through the
//! tools, chat and log, card lookup, and the streamable HTTP transport.

use daemon::{CreateGame, Daemon, DaemonConfig, DaemonHandle};
use engine::{Action, Outcome, Seat};
use mcp::server::{GetCardParams, GetLogParams, SayParams, TakeActionParams, WaitParams};
use mcp::SessionConfig;
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
}

async fn start(seed: u64) -> Running {
    let dir = std::env::temp_dir().join(format!("manaline-mcp-test-{}-{seed}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = DaemonConfig {
        socket: Some(dir.join("game.sock")),
        no_socket: false,
        tcp: None,
        parent_pid: None,
        replay_dir: Some(dir.join("games")),
        create: Some(CreateGame { format: "cube".into(), seats: 2, seed: Some(seed) }),
        cards: Arc::new(cards::core()),
    };
    let d = Daemon::bind(config).await.unwrap();
    let info = d.info().clone();
    let handle = d.handle();
    let task = tokio::spawn(async move { d.run().await.unwrap() });
    Running { handle, endpoint: Endpoint::Unix(info.socket.unwrap()), tokens: info.seat_tokens, task }
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
    let list = v.get("legal_actions").or_else(|| v.get("actions")).and_then(|a| a.as_array()).cloned().unwrap_or_default();
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
            Ok(ServerMessage::Event { event: engine::EventBase::GameOver { .. }, .. }) => return c,
            Ok(_) => continue,
            Err(_) => return c,
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
        decklist: Some(cards::deck_text("red").unwrap().into()),
    })
    .await
    .unwrap();
    assert_eq!(server.session.me, Seat(1));
    let res = server.get_game_state().await.unwrap();
    assert!(is_error(&res));
    assert!(text_of(&res).contains("has not started"), "{}", text_of(&res));
    let res = server.wait_for_turn(Parameters(WaitParams { timeout_seconds: Some(1), auto_pass: None })).await.unwrap();
    assert_eq!(res.structured_content.as_ref().unwrap()["timed_out"], true);

    // The human sits down and readies; the game starts.
    let mut human = Client::connect(&r.endpoint).await.unwrap();
    human.hello(&r.tokens[0], Some("Connor")).await.unwrap();
    human.subscribe().await.unwrap();
    human.set_deck(cards::deck_text("green").unwrap()).await.unwrap().unwrap();
    human.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();

    // Chat round trip: the human hears it, and it lands in the agent's log.
    let res = server.say(Parameters(SayParams { text: "gl hf".into() })).await.unwrap();
    assert!(!is_error(&res));
    loop {
        match human.next_push().await.unwrap() {
            ServerMessage::Event { event: engine::EventBase::Chat { from, text, .. }, .. } => {
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
    let card = server.get_card(Parameters(GetCardParams { name: Some("hill giant".into()), object_id: None, include_ir: None })).await.unwrap();
    assert!(text_of(&card).contains("Hill Giant {3}{R}"), "{}", text_of(&card));
    assert!(text_of(&card).contains("3/3"));

    // Play the game out: the human at random, the agent through wait_for_turn / take_action.
    let human_task = tokio::spawn(human_loop(human, 3));
    let mut turns_taken = 0;
    let mut auto_passes = 0u64;
    let mut pass_only_wakeups = 0;
    let outcome = loop {
        let res = server.wait_for_turn(Parameters(WaitParams { timeout_seconds: Some(20), auto_pass: None })).await.unwrap();
        let sc = res.structured_content.clone().unwrap();
        auto_passes += sc.get("auto_passed").and_then(|n| n.as_u64()).unwrap_or(0);
        if sc.get("game_over").and_then(|g| g.as_bool()).unwrap_or(false) {
            break serde_json::from_value::<Outcome>(sc["outcome"].clone()).unwrap();
        }
        if sc.get("timed_out").and_then(|t| t.as_bool()).unwrap_or(false) {
            panic!("agent waited 20s without a turn");
        }
        assert!(text_of(&res).contains("IT IS YOUR TURN TO ACT"), "{}", text_of(&res));
        let mut ids = legal_ids(&res);
        if ids.iter().all(|(_, d)| d == "Pass priority" || d == "Concede") {
            pass_only_wakeups += 1;
        }
        loop {
            let (id, _) = ids
                .iter()
                .find(|(_, d)| d.starts_with("Keep") || d.starts_with("Play ") || d.starts_with("Cast "))
                .or_else(|| ids.iter().find(|(_, d)| d != "Concede"))
                .cloned()
                .expect("a non-concede action");
            let res = server.take_action(Parameters(TakeActionParams { action_id: Some(id), action: None })).await.unwrap();
            assert!(!is_error(&res), "{}", text_of(&res));
            turns_taken += 1;
            let sc = res.structured_content.clone().unwrap();
            if sc["state"]["outcome"].is_null() && sc["still_your_turn"].as_bool().unwrap() {
                ids = legal_ids(&res);
                continue;
            }
            break;
        }
        if let Some(o) = server.outcome() {
            break o;
        }
    };
    human_task.await.unwrap();
    assert!(turns_taken > 20, "the agent took {turns_taken} actions");
    assert!(matches!(outcome, Outcome::Winner(_)));
    assert!(auto_passes > 0, "wait_for_turn never passed a nothing-to-do moment for the agent");
    assert!(pass_only_wakeups == 0, "woken {pass_only_wakeups} times with only pass/concede available");

    // Once over, tools say so instead of erroring.
    let res = server.get_game_state().await.unwrap();
    assert!(text_of(&res).contains("GAME OVER"));
    let res = server.wait_for_turn(Parameters(WaitParams { timeout_seconds: Some(1), auto_pass: None })).await.unwrap();
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
        decklist: Some(cards::deck_text("blue").unwrap().into()),
    })
    .await
    .unwrap();
    let mut human = Client::connect(&r.endpoint).await.unwrap();
    human.hello(&r.tokens[0], Some("Connor")).await.unwrap();
    human.set_deck(cards::deck_text("green").unwrap()).await.unwrap().unwrap();
    human.ready().await.unwrap();
    let mut status = r.handle.status();
    status.wait_for(|s| !s.must_act.is_empty()).await.unwrap();

    let res = server.take_action(Parameters(TakeActionParams { action_id: Some(0), action: None })).await.unwrap();
    assert!(is_error(&res));
    assert!(text_of(&res).contains("get_legal_actions first"), "{}", text_of(&res));
    let res = server.take_action(Parameters(TakeActionParams { action_id: None, action: None })).await.unwrap();
    assert!(is_error(&res));
    let res = server
        .take_action(Parameters(TakeActionParams { action_id: None, action: Some(serde_json::json!({"kind": "pass_priority"})) }))
        .await
        .unwrap();
    assert!(is_error(&res), "acting out of turn or illegally is reported: {}", text_of(&res));
    let res = server.get_card(Parameters(GetCardParams { name: Some("Black Lotus".into()), object_id: None, include_ir: None })).await.unwrap();
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
    let sid = head
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("mcp-session-id")).map(|(_, v)| v.trim().to_string()));
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
    let value = if payload.is_empty() { serde_json::Value::Null } else { serde_json::from_str(&payload).unwrap_or_else(|e| panic!("{e}: {payload}")) };
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
    let _ = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;

    let (_, tools) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await;
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in ["get_game_state", "get_legal_actions", "take_action", "wait_for_turn", "get_card", "get_log", "say", "concede", "submit_deck"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    let take = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "take_action").unwrap();
    assert!(take["inputSchema"]["properties"]["action_id"].is_object(), "{take}");

    let (_, res) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":3,"method":"resources/list"})).await;
    let uris: Vec<&str> = res["result"]["resources"].as_array().unwrap().iter().map(|t| t["uri"].as_str().unwrap()).collect();
    assert!(uris.contains(&"manaline://rules-primer") && uris.contains(&"manaline://cube"), "{uris:?}");
    // Strict 2026-07-28 clients require the cache hints on every result.
    assert_eq!(res["result"]["ttlMs"], 0, "{res}");
    assert_eq!(res["result"]["cacheScope"], "public");
    let (_, primer) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"manaline://rules-primer"}})).await;
    assert!(primer["result"]["contents"][0]["text"].as_str().unwrap().contains("Priority and passing"));
    assert_eq!(primer["result"]["ttlMs"], 0, "{primer}");
    assert_eq!(primer["result"]["cacheScope"], "public");
    let (_, tl) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":30,"method":"tools/list"})).await;
    let _ = tl;
    let (_, cube) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"manaline://cube"}})).await;
    assert!(cube["result"]["contents"][0]["text"].as_str().unwrap().contains("Grizzly Bears {1}{G}"));

    let (_, prompts) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":6,"method":"prompts/list"})).await;
    assert_eq!(prompts["result"]["prompts"][0]["name"], "play-a-game");
    let (_, prompt) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":7,"method":"prompts/get","params":{"name":"play-a-game"}})).await;
    assert!(prompt["result"]["messages"][0]["content"]["text"].as_str().unwrap().contains("wait_for_turn"));

    // `say` accepts `message` as well as `text`.
    let (_, said) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"say","arguments":{"message":"hello table"}}})).await;
    assert_eq!(said["result"]["content"][0]["text"], "said", "{said}");

    // A tool call over the wire: the game has not started, which is a tool error, not a protocol error.
    let (_, call) = rpc(&addr, &sid, serde_json::json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"get_game_state","arguments":{}}})).await;
    assert_eq!(call["result"]["isError"], true, "{call}");
    assert!(call["result"]["content"][0]["text"].as_str().unwrap().contains("not started"));

    http.shutdown().await;
    r.handle.shutdown();
    r.task.await.unwrap();
}
