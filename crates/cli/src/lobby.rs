//! The client side of a lobby server (§2.2, tier 1): `manaline create` makes a
//! game on someone else's `manaline server` and prints the code, and
//! `manaline join <code> --server …` turns that code into a seat token.
//!
//! The server is reached over any transport an `Endpoint` names — `host:port`,
//! `ws://…`, `wss://…` — and nothing above the transport knows which.

use anyhow::{anyhow, bail, Context, Result};
use protocol::{Client, Endpoint, GameId, Token};

/// The game-code alphabet the daemon draws from: six characters with no
/// look-alikes (no `0`/`O`, no `1`/`I`/`L`), and nothing a URL or a path uses.
const CODE_ALPHABET: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LEN: usize = 6;

/// Whether a `join` argument is a game code rather than somewhere to connect.
/// The two can never be confused: a code has no `:`, `/` or `.`, so it is
/// neither a `host:port`, a socket path, nor a URL.
pub fn looks_like_game_code(s: &str) -> bool {
    let s = s.trim();
    s.len() == CODE_LEN && s.chars().all(|c| CODE_ALPHABET.contains(c.to_ascii_uppercase()))
}

/// What `manaline join <target>` was pointed at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JoinTarget {
    /// A game on a lobby server: the code, and the server to ask.
    Code { code: String, server: Endpoint },
    /// A daemon to connect to directly (tier 0), with the token that seats us.
    Direct { endpoint: Endpoint, token: Token },
}

/// How `join` reaches the table, from the argument and the flags alone. A code
/// needs a server to ask; an endpoint needs a token, because a one-game daemon
/// has no lobby to hand one out.
pub fn join_target(target: &str, server: Option<&str>, token: Option<&str>) -> Result<JoinTarget> {
    let code = looks_like_game_code(target);
    match (code, server) {
        (true, Some(s)) => Ok(JoinTarget::Code {
            code: target.trim().to_ascii_uppercase(),
            server: Endpoint::parse(s).map_err(|e| anyhow!(e))?,
        }),
        (true, None) => bail!(
            "{target:?} is a game code, so it needs the server it is on: \
             `manaline join {target} --server <host:port|ws://…|wss://…> --deck <deck>`"
        ),
        (false, Some(s)) => bail!(
            "--server {s} expects a six-character game code, but {target:?} is not one. \
             Drop --server to connect straight to a daemon at {target:?}."
        ),
        (false, None) => {
            let token = token.ok_or_else(|| {
                anyhow!("joining {target:?} directly needs the seat token whoever set the table up sent you: pass --token <t>")
            })?;
            Ok(JoinTarget::Direct {
                endpoint: Endpoint::parse(target).map_err(|e| anyhow!(e))?,
                token: Token(token.to_string()),
            })
        }
    }
}

/// A game made on a lobby server.
pub struct Created {
    pub game_id: GameId,
    pub seat_tokens: Vec<Token>,
    pub spectator_token: Token,
}

/// Make a game on a server: connect, `create_game`, and hang up. The lobby
/// holds the game for whoever turns up with the code; this connection has no
/// standing at the table it just made.
pub async fn create_on(server: &Endpoint, format: &str, seats: u8, seed: Option<u64>) -> Result<Created> {
    let mut client = Client::connect(server)
        .await
        .with_context(|| format!("connecting to the server at {server}"))?;
    let (game_id, seat_tokens, spectator_token) = client
        .create_game(format, seats, seed)
        .await
        .with_context(|| format!("creating a {seats}-seat {format} game on {server}"))?;
    Ok(Created {
        game_id,
        seat_tokens,
        spectator_token,
    })
}

#[derive(clap::Args)]
pub struct CreateArgs {
    /// The lobby server to make the game on: `host:port`, `ws://…`, or `wss://…`.
    #[arg(long, value_name = "ENDPOINT")]
    pub server: String,
    /// The format to play (built-in name; see `list formats`).
    #[arg(long, default_value = "cube")]
    pub format: String,
    /// How many seats the table has. The format decides what it will allow.
    #[arg(long, default_value_t = 2)]
    pub seats: u8,
    /// Game seed, for a reproducible game.
    #[arg(long)]
    pub seed: Option<u64>,
}

/// `manaline create`: a game on a server, and the one line each player needs.
pub async fn create(args: CreateArgs) -> Result<()> {
    let server = Endpoint::parse(&args.server).map_err(|e| anyhow!(e))?;
    let created = create_on(&server, &args.format, args.seats, args.seed).await?;
    print!("{}", create_report(&created, &args.server, &args.format));
    Ok(())
}

/// What `create` prints: the code, a join command per seat, and the raw tokens
/// for whatever needs one. Seats are taken in order by whoever joins first, so
/// the join lines are all the same — one per player, not one per seat number.
pub fn create_report(created: &Created, server: &str, format: &str) -> String {
    let seats = created.seat_tokens.len();
    let mut out = format!(
        "Game {} created on {server}: {format}, {seats} seat{}.\n\n",
        created.game_id,
        if seats == 1 { "" } else { "s" }
    );
    out.push_str(&format!(
        "Send this to each player (the first {seats} to run it take the {seats} seats):\n"
    ));
    for _ in 0..seats {
        out.push_str(&format!(
            "  manaline join {} --server {server} --deck <their deck>\n",
            created.game_id
        ));
    }
    out.push_str("\nSeat tokens, for a client that wants one instead of the code:\n");
    for (i, token) in created.seat_tokens.iter().enumerate() {
        out.push_str(&format!("  seat {i}  {token}\n"));
    }
    out.push_str(&format!("Spectator token: {}\n", created.spectator_token));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_code_is_never_an_endpoint() {
        // The daemon's own alphabet, in either case.
        assert!(looks_like_game_code("K7QMPX"));
        assert!(looks_like_game_code("k7qmpx"));
        assert!(looks_like_game_code(" K7QMPX "));
        assert!(looks_like_game_code("ABCDEF"));
        assert!(looks_like_game_code("234567"));

        // Everything `join` also accepts.
        for not_a_code in [
            "127.0.0.1:7454",
            "play.example:7454",
            "ws://127.0.0.1:7454",
            "wss://play.example",
            "/run/manaline/K7QMPX.sock",
            "unix:g.sock",
            "tcp:h:1",
        ] {
            assert!(!looks_like_game_code(not_a_code), "{not_a_code}");
        }
        // Wrong length, and characters the alphabet leaves out on purpose.
        assert!(!looks_like_game_code("ABCDE"));
        assert!(!looks_like_game_code("ABCDEFG"));
        assert!(!looks_like_game_code(""));
        for look_alike in ["ABCDE0", "ABCDE1", "ABCDEI", "ABCDEL", "ABCDEO"] {
            assert!(!looks_like_game_code(look_alike), "{look_alike}");
        }
    }

    #[test]
    fn join_reads_a_code_or_an_endpoint() {
        // A code plus a server is the lobby path; the code is normalised.
        let t = join_target("k7qmpx", Some("wss://play.example"), None).unwrap();
        assert_eq!(
            t,
            JoinTarget::Code {
                code: "K7QMPX".into(),
                server: Endpoint::Ws("wss://play.example".into()),
            }
        );
        // A token with a code is allowed: it skips asking the lobby.
        assert!(matches!(
            join_target("K7QMPX", Some("127.0.0.1:7454"), Some("tok")).unwrap(),
            JoinTarget::Code { .. }
        ));

        // An endpoint plus a token is the direct path (tier 0), unchanged.
        let t = join_target("127.0.0.1:7454", None, Some("tok")).unwrap();
        assert_eq!(
            t,
            JoinTarget::Direct {
                endpoint: Endpoint::Tcp("127.0.0.1:7454".into()),
                token: Token("tok".into()),
            }
        );
        assert!(matches!(
            join_target("/run/g.sock", None, Some("tok")).unwrap(),
            JoinTarget::Direct { .. }
        ));
        assert!(matches!(
            join_target("wss://play.example", None, Some("tok")).unwrap(),
            JoinTarget::Direct { .. }
        ));

        // Each mistake says what to do about it.
        let e = join_target("K7QMPX", None, None).unwrap_err().to_string();
        assert!(e.contains("is a game code") && e.contains("--server"), "{e}");
        let e = join_target("K7QMPX", None, Some("tok")).unwrap_err().to_string();
        assert!(e.contains("--server"), "{e}");
        let e = join_target("127.0.0.1:7454", Some("wss://play.example"), None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("game code") && e.contains("Drop --server"), "{e}");
        let e = join_target("127.0.0.1:7454", None, None).unwrap_err().to_string();
        assert!(e.contains("--token"), "{e}");
        assert!(join_target("nonsense!", None, Some("tok")).is_err(), "not an endpoint either");
    }

    fn created(seats: usize) -> Created {
        Created {
            game_id: GameId("K7QMPX".into()),
            seat_tokens: (0..seats).map(|i| Token(format!("tok{i}"))).collect(),
            spectator_token: Token("spec".into()),
        }
    }

    #[test]
    fn create_prints_a_join_line_per_seat_and_the_raw_tokens() {
        let out = create_report(&created(3), "wss://play.example", "free-for-all");
        assert!(out.contains("Game K7QMPX created on wss://play.example: free-for-all, 3 seats."));
        assert_eq!(
            out.matches("manaline join K7QMPX --server wss://play.example --deck <their deck>")
                .count(),
            3,
            "one join line per seat:\n{out}"
        );
        for token in ["seat 0  tok0", "seat 1  tok1", "seat 2  tok2"] {
            assert!(out.contains(token), "{out}");
        }
        assert!(out.contains("Spectator token: spec"), "{out}");

        // Six seats is as many as any format allows, and two is the usual.
        let out = create_report(&created(6), "127.0.0.1:7454", "free-for-all");
        assert_eq!(out.matches("manaline join K7QMPX").count(), 6);
        assert!(out.contains("seat 5  tok5"));
        let out = create_report(&created(2), "127.0.0.1:7454", "cube");
        assert!(out.contains("cube, 2 seats."), "{out}");
    }

    /// The whole tier-1 round trip against a real `manaline server`: create a
    /// game, then take every seat by code, the way `manaline join <code>` does.
    #[tokio::test]
    async fn a_game_made_on_a_server_fills_its_seats_in_order() {
        use daemon::{Daemon, DaemonConfig};
        let dir = std::env::temp_dir().join(format!("manaline-create-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = DaemonConfig {
            socket: None,
            no_socket: true,
            tcp: Some("127.0.0.1:0".into()),
            ws: None,
            tls: None,
            parent_pid: None,
            replay_dir: Some(dir.join("games")),
            // A server makes no game of its own; clients do (§2.2).
            create: None,
            serve: true,
            cards: std::sync::Arc::new(cards::core()),
            legality: None,
            idle: None,
            abandon_after: None,
        };
        let d = Daemon::bind(config).await.unwrap();
        let addr = d.info().tcp.unwrap().to_string();
        let handle = d.handle();
        let task = tokio::spawn(async move { d.run().await.unwrap() });
        let server = Endpoint::parse(&addr).unwrap();

        // `manaline create --seats 3`: one game, one code, a token per seat.
        let created = create_on(&server, "free-for-all", 3, Some(7)).await.unwrap();
        assert_eq!(created.seat_tokens.len(), 3);
        assert!(
            looks_like_game_code(&created.game_id.0),
            "the server's code is one `join` recognises: {}",
            created.game_id
        );
        let report = create_report(&created, &addr, "free-for-all");
        assert_eq!(report.matches(&format!("manaline join {}", created.game_id)).count(), 3);

        // `manaline join <code>`: the lobby gives each arrival the next free
        // seat, and the token it hands back is that seat's own.
        let mut seats = Vec::new();
        for i in 0..3u8 {
            let mut asking = Client::connect(&server).await.unwrap();
            let name = format!("P{i}");
            // A code is case-insensitive, as a thing people retype must be.
            let (token, seat, game_id) = asking.join_game(&created.game_id.0.to_lowercase(), Some(&name)).await.unwrap();
            assert_eq!(game_id, created.game_id);
            assert_eq!(token, created.seat_tokens[seat.index()], "seat {seat} got its own token");
            seats.push(seat.0);
            drop(asking);

            // The client that plays the seat opens its own connection with
            // nothing but that token — the reconnect path, which never learns
            // there was a lobby.
            let mut playing = Client::connect(&server).await.unwrap();
            let welcome = playing.hello(&token, Some(&name)).await.unwrap();
            assert_eq!(welcome.role.seat(), Some(seat));
            assert_eq!(welcome.game_id, created.game_id);
        }
        assert_eq!(seats, vec![0, 1, 2], "seats fill in order, one per arrival");

        // A fourth player finds the table full; an unknown code is unknown.
        let mut late = Client::connect(&server).await.unwrap();
        let e = late.join_game(&created.game_id.0, None).await.unwrap_err().to_string();
        assert!(e.contains("full"), "{e}");
        let e = late.join_game("ZZZZZZ", None).await.unwrap_err().to_string();
        assert!(e.contains("no game"), "{e}");

        // And a server holds many games at once, each with its own code.
        let second = create_on(&server, "cube", 2, None).await.unwrap();
        assert_ne!(second.game_id, created.game_id);
        assert_eq!(handle.games().await, 2);

        handle.shutdown();
        task.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
