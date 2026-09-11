//! Where a daemon listens and where its files go. Transport-agnostic clients
//! parse an `Endpoint` from one string: a socket path or a `host:port`.

use crate::messages::Token;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Endpoint {
    Unix(PathBuf),
    Tcp(String),
}

impl Endpoint {
    /// `unix:<path>`, `tcp:<host:port>`, a bare path (contains a `/`), or a bare `host:port`.
    pub fn parse(s: &str) -> Result<Endpoint, String> {
        let s = s.trim();
        if let Some(p) = s.strip_prefix("unix:") {
            return Ok(Endpoint::Unix(PathBuf::from(p)));
        }
        if let Some(a) = s.strip_prefix("tcp:") {
            return Ok(Endpoint::Tcp(a.to_string()));
        }
        if s.contains('/') || s.ends_with(".sock") {
            return Ok(Endpoint::Unix(PathBuf::from(s)));
        }
        if s.rsplit_once(':').map(|(_, port)| port.parse::<u16>().is_ok()).unwrap_or(false) {
            return Ok(Endpoint::Tcp(s.to_string()));
        }
        Err(format!("{s:?} is neither a socket path nor host:port"))
    }
}

impl FromStr for Endpoint {
    type Err = String;
    fn from_str(s: &str) -> Result<Endpoint, String> {
        Endpoint::parse(s)
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Unix(p) => write!(f, "unix:{}", p.display()),
            Endpoint::Tcp(a) => write!(f, "tcp:{a}"),
        }
    }
}

/// The per-user runtime directory for sockets: `$XDG_RUNTIME_DIR/manaline`
/// on Linux; macOS has no runtime dir, so `$TMPDIR/manaline-<uid>` (mode 0700).
/// The per-user runtime directory: `MANALINE_RUNTIME_DIR` if set (tests and
/// scripts isolate themselves with it), else `$XDG_RUNTIME_DIR/manaline` or
/// a per-uid directory under the temp dir.
pub fn runtime_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("MANALINE_RUNTIME_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = dirs::runtime_dir() {
        return d.join("manaline");
    }
    let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(format!("manaline-{}", uid()))
}

/// Create the runtime directory with private permissions and return it.
pub fn ensure_runtime_dir() -> std::io::Result<PathBuf> {
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

pub fn socket_path(game_id: &str) -> PathBuf {
    runtime_dir().join(format!("{game_id}.sock"))
}

/// `~/.local/share/manaline` (or the platform equivalent).
pub fn data_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("manaline")
}

/// Where a game's replay log lives: `<data_dir>/games/<game-id>.jsonl`.
pub fn replay_path(game_id: &str) -> PathBuf {
    data_dir().join("games").join(format!("{game_id}.jsonl"))
}

#[cfg(unix)]
fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

pub fn display_path(p: &Path) -> String {
    p.display().to_string()
}

/// A runtime directory to look things up in and announce things under.
/// `Runtime::default()` is the user's real one; tests and tools that must not
/// see each other use `Runtime::at(dir)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    pub dir: PathBuf,
}

impl Default for Runtime {
    fn default() -> Self {
        Runtime { dir: runtime_dir() }
    }
}

impl Runtime {
    pub fn at(dir: impl Into<PathBuf>) -> Runtime {
        Runtime { dir: dir.into() }
    }

    /// Where editor markers live: `<runtime dir>/editors`.
    pub fn editors_dir(&self) -> PathBuf {
        self.dir.join("editors")
    }

    /// Where the editor running as `pid` should listen: `<editors dir>/<pid>.sock`.
    pub fn editor_socket_for(&self, pid: u32) -> PathBuf {
        self.editors_dir().join(format!("{pid}.sock"))
    }

    /// Record this process as editing `path` (canonicalised when possible),
    /// with the socket it answers agent requests on if it has one. Writes
    /// `<editors dir>/<pid>.json`.
    pub fn announce_editor(&self, path: &Path, format: &str, socket: Option<&Path>) -> std::io::Result<EditorSession> {
        let dir = self.editors_dir();
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let session = EditorSession {
            pid: std::process::id(),
            path: std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            format: format.to_string(),
            socket: socket.map(Path::to_path_buf),
            dir: dir.clone(),
        };
        let json = serde_json::to_vec_pretty(&session).map_err(std::io::Error::other)?;
        std::fs::write(session.marker_path(), json)?;
        Ok(session)
    }

    /// Every editor whose process is still alive, sorted by marker file name.
    /// Markers that fail to parse or whose process is gone are deleted on the way.
    pub fn live_editors(&self) -> Vec<EditorSession> {
        let dir = self.editors_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        files.sort();
        let mut out = Vec::new();
        for file in files {
            if file.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let parsed = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<EditorSession>(&bytes).ok());
            match parsed {
                Some(mut s) if process_alive(s.pid) => {
                    s.dir = dir.clone();
                    out.push(s);
                }
                _ => {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
        out
    }
}

/// What `play` publishes so agents can find the table: where the daemon
/// listens and, per seat, who sits there. Agent seats carry their token;
/// an agent claims one (`Runtime::claim_seat`) and joins with it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GameMarker {
    pub game_id: String,
    /// The `play` process; the marker is stale once it is gone.
    pub pid: u32,
    pub socket: Option<PathBuf>,
    pub tcp: Option<String>,
    pub format: String,
    #[serde(default)]
    pub spectator_token: Option<Token>,
    pub seats: Vec<SeatSlot>,
    /// The games directory this marker lives in (not part of the file).
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeatSlot {
    pub seat: u8,
    pub kind: SeatKind,
    pub name: String,
    /// The deck assigned to the seat (a name or path), if `play` chose one.
    #[serde(default)]
    pub deck: Option<String>,
    /// The seat token; only agent seats publish theirs.
    #[serde(default)]
    pub token: Option<Token>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatKind {
    Human,
    Bot,
    Agent,
}

impl GameMarker {
    pub fn endpoint(&self) -> Option<Endpoint> {
        match (&self.socket, &self.tcp) {
            (Some(p), _) => Some(Endpoint::Unix(p.clone())),
            (None, Some(t)) => Some(Endpoint::Tcp(t.clone())),
            (None, None) => None,
        }
    }

    fn marker_path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.game_id))
    }

    /// Where this game's seat claims live: `<games dir>/<game id>/seat-<n>`.
    fn claims_dir(&self) -> PathBuf {
        self.dir.join(&self.game_id)
    }

    /// Remove the marker and every claim on it (`play` exiting).
    pub fn withdraw(&self) {
        let _ = std::fs::remove_file(self.marker_path());
        let _ = std::fs::remove_dir_all(self.claims_dir());
    }

    /// Which agent seats are claimed, and by which process.
    pub fn claims(&self) -> Vec<(u8, u32)> {
        let Ok(entries) = std::fs::read_dir(self.claims_dir()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(n) = name
                .to_str()
                .and_then(|s| s.strip_prefix("seat-"))
                .and_then(|s| s.parse::<u8>().ok())
            else {
                continue;
            };
            let pid = std::fs::read_to_string(e.path()).ok().and_then(|s| s.trim().parse::<u32>().ok());
            if let Some(pid) = pid.filter(|p| process_alive(*p)) {
                out.push((n, pid));
            } else {
                let _ = std::fs::remove_file(e.path());
            }
        }
        out.sort();
        out
    }
}

/// A seat an agent process has claimed; released on drop or explicitly.
#[derive(Debug)]
pub struct SeatClaim {
    pub game: GameMarker,
    pub slot: SeatSlot,
    path: PathBuf,
}

impl SeatClaim {
    pub fn seat(&self) -> u8 {
        self.slot.seat
    }

    /// Give the seat back so another agent may take it.
    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
        std::mem::forget(self);
    }
}

impl Drop for SeatClaim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Runtime {
    /// Where game markers live: `<runtime dir>/games`.
    pub fn games_dir(&self) -> PathBuf {
        self.dir.join("games")
    }

    /// Publish a game for agents to find. Writes `<games dir>/<game id>.json`.
    pub fn publish_game(&self, marker: &GameMarker) -> std::io::Result<GameMarker> {
        let dir = self.games_dir();
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut marker = marker.clone();
        marker.dir = dir;
        let _ = std::fs::remove_dir_all(marker.claims_dir());
        let json = serde_json::to_vec_pretty(&marker).map_err(std::io::Error::other)?;
        std::fs::write(marker.marker_path(), json)?;
        Ok(marker)
    }

    /// Every published game whose `play` is still running, newest first.
    /// Markers that fail to parse or whose process is gone are deleted.
    pub fn live_games(&self) -> Vec<GameMarker> {
        let dir = self.games_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
        let mut found: Vec<(std::time::SystemTime, GameMarker)> = Vec::new();
        for e in entries.flatten() {
            let file = e.path();
            if file.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let parsed = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<GameMarker>(&bytes).ok());
            match parsed {
                Some(mut m) if process_alive(m.pid) => {
                    m.dir = dir.clone();
                    let at = e.metadata().and_then(|md| md.modified()).unwrap_or(std::time::UNIX_EPOCH);
                    found.push((at, m));
                }
                _ => {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.game_id.cmp(&b.1.game_id)));
        found.into_iter().map(|(_, m)| m).collect()
    }

    /// The game an agent should join: the most recently published live one.
    pub fn newest_game(&self) -> Option<GameMarker> {
        self.live_games().into_iter().next()
    }

    /// Claim an agent seat for this process: the one asked for, or the first
    /// free one. Atomic across processes (the claim file is created
    /// exclusively); a claim left by a dead process is taken over. `None`
    /// when every agent seat is taken or the seat asked for is not an
    /// agent's.
    pub fn claim_seat(&self, game: &GameMarker, seat: Option<u8>) -> std::io::Result<Option<SeatClaim>> {
        let dir = game.claims_dir();
        std::fs::create_dir_all(&dir)?;
        let candidates = game
            .seats
            .iter()
            .filter(|s| s.kind == SeatKind::Agent && seat.is_none_or(|want| want == s.seat));
        for slot in candidates {
            let path = dir.join(format!("seat-{}", slot.seat));
            for _ in 0..2 {
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(mut f) => {
                        use std::io::Write;
                        write!(f, "{}", std::process::id())?;
                        return Ok(Some(SeatClaim {
                            game: game.clone(),
                            slot: slot.clone(),
                            path,
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        let holder = std::fs::read_to_string(&path).ok().and_then(|s| s.trim().parse::<u32>().ok());
                        match holder {
                            Some(pid) if process_alive(pid) => break,
                            _ => {
                                let _ = std::fs::remove_file(&path);
                            }
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(None)
    }
}

/// An open deckbuilder announces itself so an MCP server helping the human
/// can find the file they are editing: one JSON file per editor process
/// under `<runtime dir>/editors/`, removed when the editor exits and
/// ignored once its process is gone.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditorSession {
    pub pid: u32,
    pub path: PathBuf,
    pub format: String,
    /// The editor's Unix socket, if it accepts requests (see `crate::editor`).
    #[serde(default)]
    pub socket: Option<PathBuf>,
    /// The editors directory this marker lives in (not part of the file).
    #[serde(skip)]
    pub dir: PathBuf,
}

/// Where editor markers live in the default runtime: `<runtime dir>/editors`.
pub fn editor_sessions_dir() -> PathBuf {
    Runtime::default().editors_dir()
}

/// Where the editor running as `pid` should listen, in the default runtime.
pub fn socket_path_for(pid: u32) -> PathBuf {
    Runtime::default().editor_socket_for(pid)
}

impl EditorSession {
    /// `Runtime::default().announce_editor(path, format, None)`.
    pub fn announce(path: &Path, format: &str) -> std::io::Result<EditorSession> {
        Runtime::default().announce_editor(path, format, None)
    }

    /// `Runtime::default().announce_editor(path, format, Some(socket))`.
    pub fn announce_with_socket(path: &Path, format: &str, socket: &Path) -> std::io::Result<EditorSession> {
        Runtime::default().announce_editor(path, format, Some(socket))
    }

    /// Remove this editor's marker file.
    pub fn withdraw(&self) {
        let _ = std::fs::remove_file(self.marker_path());
    }

    /// `Runtime::default().live_editors()`.
    pub fn live() -> Vec<EditorSession> {
        Runtime::default().live_editors()
    }

    fn marker_path(&self) -> PathBuf {
        self.dir.join(format!("{}.json", self.pid))
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for the process's existence; kill has no preconditions.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_endpoints() {
        assert_eq!(Endpoint::parse("/tmp/x.sock").unwrap(), Endpoint::Unix("/tmp/x.sock".into()));
        assert_eq!(Endpoint::parse("unix:rel.sock").unwrap(), Endpoint::Unix("rel.sock".into()));
        assert_eq!(Endpoint::parse("127.0.0.1:7454").unwrap(), Endpoint::Tcp("127.0.0.1:7454".into()));
        assert_eq!(
            Endpoint::parse("tcp:example.com:7454").unwrap(),
            Endpoint::Tcp("example.com:7454".into())
        );
        assert!(Endpoint::parse("nonsense").is_err());
        assert_eq!(Endpoint::parse("127.0.0.1:7454").unwrap().to_string(), "tcp:127.0.0.1:7454");
    }

    #[test]
    fn paths_are_under_manaline_dirs() {
        assert!(runtime_dir().to_string_lossy().contains("manaline"));
        assert!(socket_path("abc").to_string_lossy().ends_with("abc.sock"));
        assert!(replay_path("abc").to_string_lossy().ends_with("games/abc.jsonl"));
    }
    #[test]
    fn the_runtime_dir_can_be_overridden() {
        // Set once for this test binary; the other tests only check the
        // "manaline" naming, which the override keeps.
        let dir = std::env::temp_dir().join(format!("manaline-rt-{}", std::process::id()));
        std::env::set_var("MANALINE_RUNTIME_DIR", &dir);
        assert_eq!(runtime_dir(), dir);
        assert_eq!(Runtime::default().dir, dir);
        assert_eq!(socket_path("g").parent().unwrap(), dir);
    }

    #[test]
    fn games_are_published_and_seats_claimed_once() {
        let rt = Runtime::at(std::env::temp_dir().join(format!("manaline-games-rt-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&rt.dir);
        let agent = |seat: u8| SeatSlot {
            seat,
            kind: SeatKind::Agent,
            name: "Claude".into(),
            deck: Some("red".into()),
            token: Some(Token(format!("tok{seat}"))),
        };
        let marker = GameMarker {
            game_id: "quiet-owl".into(),
            pid: std::process::id(),
            socket: Some(rt.dir.join("quiet-owl.sock")),
            tcp: None,
            format: "cube".into(),
            spectator_token: Some(Token("spec".into())),
            seats: vec![
                SeatSlot {
                    seat: 0,
                    kind: SeatKind::Human,
                    name: "Connor".into(),
                    deck: None,
                    token: None,
                },
                agent(1),
                agent(2),
            ],
            dir: PathBuf::new(),
        };
        let published = rt.publish_game(&marker).unwrap();
        assert_eq!(rt.live_games().len(), 1);
        assert_eq!(rt.newest_game().unwrap().game_id, "quiet-owl");
        assert_eq!(published.endpoint(), Some(Endpoint::Unix(rt.dir.join("quiet-owl.sock"))));

        // First come, first seated; the human's seat is never offered.
        let first = rt.claim_seat(&published, None).unwrap().unwrap();
        assert_eq!(first.seat(), 1);
        assert_eq!(first.slot.token, Some(Token("tok1".into())));
        let second = rt.claim_seat(&published, None).unwrap().unwrap();
        assert_eq!(second.seat(), 2);
        assert!(rt.claim_seat(&published, None).unwrap().is_none(), "no third agent seat");
        assert!(rt.claim_seat(&published, Some(0)).unwrap().is_none(), "seat 0 is the human's");
        assert_eq!(published.claims(), vec![(1, std::process::id()), (2, std::process::id())]);

        // Releasing frees the seat; a dead claimant's file is taken over.
        second.release();
        std::fs::write(published.claims_dir().join("seat-2"), "4000000000").unwrap();
        let again = rt.claim_seat(&published, Some(2)).unwrap().unwrap();
        assert_eq!(again.seat(), 2);
        drop(again);
        assert_eq!(published.claims(), vec![(1, std::process::id())]);

        // A marker whose `play` is gone is not live, and is cleaned up.
        let dead = GameMarker {
            game_id: "gone".into(),
            pid: 4_000_000_000,
            ..marker.clone()
        };
        rt.publish_game(&dead).unwrap();
        assert_eq!(rt.live_games().len(), 1);
        assert!(!rt.games_dir().join("gone.json").exists());

        published.withdraw();
        assert!(rt.live_games().is_empty());
        assert!(!published.claims_dir().exists());
    }

    #[test]
    fn editor_sessions_announce_and_expire() {
        let rt = Runtime::at(std::env::temp_dir().join(format!("manaline-editor-rt-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&rt.dir);
        let deck = std::env::temp_dir().join(format!("manaline-editor-test-{}.txt", std::process::id()));
        std::fs::write(&deck, "4 Lightning Bolt\n").unwrap();
        let session = rt.announce_editor(&deck, "modern", None).unwrap();
        assert_eq!(session.pid, std::process::id());
        assert_eq!(session.format, "modern");
        assert_eq!(session.socket, None);
        assert!(rt.live_editors().contains(&session));

        // A marker for a process that does not exist is dropped and deleted.
        let dead = EditorSession {
            pid: 4_000_000_000,
            path: deck.clone(),
            format: "modern".into(),
            socket: None,
            dir: rt.editors_dir(),
        };
        let dead_marker = rt.editors_dir().join(format!("{}.json", dead.pid));
        std::fs::write(&dead_marker, serde_json::to_vec(&dead).unwrap()).unwrap();
        let live = rt.live_editors();
        assert!(live.contains(&session));
        assert!(!live.iter().any(|s| s.pid == dead.pid));
        assert!(!dead_marker.exists());

        // An editor that accepts requests records its socket, and `live()` reports it.
        let sock = rt.editor_socket_for(std::process::id());
        let with_socket = rt.announce_editor(&deck, "modern", Some(&sock)).unwrap();
        assert_eq!(with_socket.socket.as_deref(), Some(sock.as_path()));
        assert!(sock.to_string_lossy().ends_with(&format!("{}.sock", std::process::id())));
        let live = rt.live_editors();
        assert_eq!(live.iter().find(|s| s.pid == with_socket.pid), Some(&with_socket));

        with_socket.withdraw();
        assert!(!rt.live_editors().contains(&with_socket));
        session.withdraw();
        assert!(!rt.live_editors().contains(&session));
        let _ = std::fs::remove_file(&deck);
    }
}
