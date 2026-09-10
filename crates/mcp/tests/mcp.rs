//! The MCP server against a real daemon: a whole game played through the
//! tools, chat and log, card lookup, and the streamable HTTP transport.

use daemon::{CreateGame, Daemon, DaemonConfig, DaemonHandle};
use engine::{Action, Outcome, Seat};
use mcp::server::{
    DeckStatsParams, GetCardParams, GetDeckParams, GetLogParams, SaveDeckParams, SayParams, SearchParams, SubmitDeckParams,
    TakeActionParams, WaitParams,
};
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
    Running {
        handle,
        endpoint: Endpoint::Unix(info.socket.unwrap()),
        tokens: info.seat_tokens,
        task,
    }
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
    assert_eq!(server.session.as_ref().unwrap().me, Seat(1));
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
    let mut turns_taken = 0;
    let mut auto_passes = 0u64;
    let mut pass_only_wakeups = 0;
    let outcome = loop {
        let res = server
            .wait_for_turn(Parameters(WaitParams {
                timeout_seconds: Some(20),
                auto_pass: None,
            }))
            .await
            .unwrap();
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
            let res = server
                .take_action(Parameters(TakeActionParams {
                    action_id: Some(id),
                    action: None,
                    state_version: None,
                }))
                .await
                .unwrap();
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
    assert!(
        pass_only_wakeups == 0,
        "woken {pass_only_wakeups} times with only pass/concede available"
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
        !names.contains(&"save_deck"),
        "save_deck is for deckbuilding only, not a seat in a game: {names:?}"
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
    server.session.as_ref().unwrap().refresh().await;
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
    let res = server
        .get_deck(Parameters(GetDeckParams { name: Some("blue".into()) }))
        .await
        .unwrap();
    assert!(
        !is_error(&res) && text_of(&res).contains("Island") && text_of(&res).contains("curve"),
        "{}",
        text_of(&res)
    );
    let res = server
        .get_deck(Parameters(GetDeckParams {
            name: Some("no-such-deck".into()),
        }))
        .await
        .unwrap();
    assert!(is_error(&res));
    let res = server.get_deck(Parameters(GetDeckParams { name: None })).await.unwrap();
    assert!(is_error(&res), "seated: a name is required");
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: "17 Forest\n".into(),
            name: Some("x".into()),
            path: None,
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res) && text_of(&res).contains("only available"), "{}", text_of(&res));
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
    assert!(server.session.as_ref().unwrap().lobby().seats[1].deck_ok);

    r.handle.shutdown();
    r.task.await.unwrap();
}

#[tokio::test]
async fn a_standalone_server_serves_card_data_without_a_game() {
    let server = mcp::standalone(engine::Format::cube());
    let res = server
        .search_cards(Parameters(SearchParams {
            query: "t:creature kw:flying c:w mv<=3".into(),
            limit: Some(5),
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
    assert!(mcp::server::cube_text(&server.cards).contains("Serra Angel"));

    // save_deck writes canonical text, refuses unknown cards, and reports legality.
    let dir = std::env::temp_dir().join(format!("manaline-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("agent.txt");
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: "4 Grizly Bears\n17 Forest\n".into(),
            name: None,
            path: Some(path.display().to_string()),
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(
        is_error(&res) && text_of(&res).contains("did you mean Grizzly Bears"),
        "{}",
        text_of(&res)
    );
    assert!(!path.exists());
    let deck = format!(
        "17 Forest\n4 Grizzly Bears\n{}",
        "4 Llanowar Elves\n4 Centaur Courser\n4 Giant Growth\n4 Craw Wurm\n3 Elvish Visionary\n"
    );
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: deck.clone(),
            name: None,
            path: Some(path.display().to_string()),
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&res), "{}", text_of(&res));
    let sc = res.structured_content.clone().unwrap();
    assert_eq!(sc["cards"], 40);
    assert_eq!(sc["legal"], true);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("Deck\n4 Llanowar Elves") && text.ends_with("17 Forest\n"),
        "canonical order: {text}"
    );
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: deck,
            name: None,
            path: Some(path.display().to_string()),
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res) && text_of(&res).contains("already exists"));
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: "1 Forest".into(),
            name: Some("../evil".into()),
            path: None,
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res));

    // With a deckbuilder open on a file, no name or path means that file; get_deck reads it.
    let open_file = std::env::temp_dir()
        .join(format!("manaline-open-{}", std::process::id()))
        .join("tokens.txt");
    std::fs::create_dir_all(open_file.parent().unwrap()).unwrap();
    std::fs::write(&open_file, "17 Plains\n").unwrap();
    let announced = protocol::endpoint::EditorSession::announce(&open_file, "cube").unwrap();
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: "17 Plains\n4 Raise the Alarm\n4 Attended Knight\n".into(),
            name: None,
            path: None,
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(
        !is_error(&res) && text_of(&res).contains("open in the deckbuilder"),
        "{}",
        text_of(&res)
    );
    assert!(std::fs::read_to_string(&open_file).unwrap().contains("Attended Knight"));
    let res = server.get_deck(Parameters(GetDeckParams { name: None })).await.unwrap();
    assert!(!is_error(&res) && text_of(&res).contains("Attended Knight"), "{}", text_of(&res));
    let res = server.list_decks().await.unwrap();
    assert!(text_of(&res).contains("open in the deckbuilder"), "{}", text_of(&res));
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: "1 Forest".into(),
            name: Some("x".into()),
            path: Some("y".into()),
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res) && text_of(&res).contains("not both"));
    announced.withdraw();
    let res = server
        .save_deck(Parameters(SaveDeckParams {
            decklist: "1 Forest".into(),
            name: None,
            path: None,
            overwrite: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&res) && text_of(&res).contains("no deckbuilder open"), "{}", text_of(&res));
    let res = server.get_deck(Parameters(GetDeckParams { name: None })).await.unwrap();
    assert!(is_error(&res));
    let res = server.list_decks().await.unwrap();
    assert!(text_of(&res).contains("No deckbuilder is open"), "{}", text_of(&res));
}
