//! Where a daemon listens and where its files go. Transport-agnostic clients
//! parse an `Endpoint` from one string: a socket path or a `host:port`.

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
pub fn runtime_dir() -> PathBuf {
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

/// An open deckbuilder announces itself here so an MCP server helping the
/// human can find the file they are editing: one JSON file per editor
/// process under `<runtime dir>/editors/`, removed when the editor exits and
/// ignored once its process is gone.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EditorSession {
    pub pid: u32,
    pub path: PathBuf,
    pub format: String,
    /// The editor's Unix socket, if it accepts requests (see `crate::editor`).
    #[serde(default)]
    pub socket: Option<PathBuf>,
}

/// Where editor markers live: `<runtime dir>/editors`.
pub fn editor_sessions_dir() -> PathBuf {
    runtime_dir().join("editors")
}

/// Where the editor running as `pid` should listen: `<editors dir>/<pid>.sock`.
pub fn socket_path_for(pid: u32) -> PathBuf {
    editor_sessions_dir().join(format!("{pid}.sock"))
}

impl EditorSession {
    /// Record this process as editing `path` (canonicalised when possible). Writes `<dir>/<pid>.json`.
    pub fn announce(path: &Path, format: &str) -> std::io::Result<EditorSession> {
        Self::write_marker(path, format, None)
    }

    /// As `announce`, but also record the socket this editor listens on for agent requests.
    pub fn announce_with_socket(path: &Path, format: &str, socket: &Path) -> std::io::Result<EditorSession> {
        Self::write_marker(path, format, Some(socket.to_path_buf()))
    }

    fn write_marker(path: &Path, format: &str, socket: Option<PathBuf>) -> std::io::Result<EditorSession> {
        let dir = editor_sessions_dir();
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
            socket,
        };
        let json = serde_json::to_vec_pretty(&session).map_err(std::io::Error::other)?;
        std::fs::write(session.marker_path(), json)?;
        Ok(session)
    }

    /// Remove this editor's marker file.
    pub fn withdraw(&self) {
        let _ = std::fs::remove_file(self.marker_path());
    }

    /// Every editor whose process is still alive, sorted by marker file name.
    /// Markers that fail to parse or whose process is gone are deleted on the way.
    pub fn live() -> Vec<EditorSession> {
        let dir = editor_sessions_dir();
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
                Some(s) if process_alive(s.pid) => out.push(s),
                _ => {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
        out
    }

    fn marker_path(&self) -> PathBuf {
        editor_sessions_dir().join(format!("{}.json", self.pid))
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
    fn editor_sessions_announce_and_expire() {
        let deck = std::env::temp_dir().join(format!("manaline-editor-test-{}.txt", std::process::id()));
        std::fs::write(&deck, "4 Lightning Bolt\n").unwrap();
        let session = EditorSession::announce(&deck, "modern").unwrap();
        assert_eq!(session.pid, std::process::id());
        assert_eq!(session.format, "modern");
        assert_eq!(session.socket, None);
        assert!(EditorSession::live().contains(&session));

        // A marker for a process that does not exist is dropped and deleted.
        let dead = EditorSession {
            pid: 4_000_000_000,
            path: deck.clone(),
            format: "modern".into(),
            socket: None,
        };
        let dead_marker = editor_sessions_dir().join(format!("{}.json", dead.pid));
        std::fs::write(&dead_marker, serde_json::to_vec(&dead).unwrap()).unwrap();
        let live = EditorSession::live();
        assert!(live.contains(&session));
        assert!(!live.iter().any(|s| s.pid == dead.pid));
        assert!(!dead_marker.exists());

        // An editor that accepts requests records its socket, and `live()` reports it.
        let sock = socket_path_for(std::process::id());
        let with_socket = EditorSession::announce_with_socket(&deck, "modern", &sock).unwrap();
        assert_eq!(with_socket.socket.as_deref(), Some(sock.as_path()));
        assert!(sock.to_string_lossy().ends_with(&format!("{}.sock", std::process::id())));
        let live = EditorSession::live();
        assert_eq!(live.iter().find(|s| s.pid == with_socket.pid), Some(&with_socket));

        with_socket.withdraw();
        assert!(!EditorSession::live().contains(&with_socket));
        session.withdraw();
        assert!(!EditorSession::live().contains(&session));
        let _ = std::fs::remove_file(&deck);
    }
}
