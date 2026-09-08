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
}
