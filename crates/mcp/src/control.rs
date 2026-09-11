//! One MCP server per machine, found and steered by the other commands.
//!
//! A running `manaline mcp` writes a marker under the runtime directory
//! (its pid, URL, and mode) and listens on a control socket next to it.
//! `deck edit` starts a server if none is running; `play` attaches the
//! running server to the game's seat instead of starting a second one, and
//! detaches it when the game ends. Agents therefore keep one URL.

use crate::{McpServer, Session, SessionConfig};
use protocol::endpoint::runtime_dir;
use protocol::{Endpoint, FramedReader, FramedWriter, Token};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// What a running server advertises.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub pid: u32,
    /// `http://host:port/mcp`
    pub url: String,
    pub control_socket: PathBuf,
    /// "card data only" or "game <id>, seat <n>".
    pub mode: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    Status,
    /// Join a game as a seat: the server connects to the daemon and, if a
    /// decklist is given, submits it and readies up.
    Attach {
        endpoint: String,
        token: String,
        name: String,
        #[serde(default)]
        decklist: Option<String>,
    },
    /// Leave the game: back to card data only.
    Detach,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlReply {
    Ok { mode: String, url: String },
    Error { message: String },
}

pub fn marker_path() -> PathBuf {
    runtime_dir().join("mcp.json")
}

pub fn control_socket_path() -> PathBuf {
    runtime_dir().join("mcp.sock")
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 checks for existence only.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// The server running on this machine, if its process is still alive. A
/// stale marker is removed.
pub fn running() -> Option<Marker> {
    running_at(&marker_path())
}

pub fn running_at(path: &Path) -> Option<Marker> {
    let bytes = std::fs::read(path).ok()?;
    let Ok(marker) = serde_json::from_slice::<Marker>(&bytes) else {
        let _ = std::fs::remove_file(path);
        return None;
    };
    if process_alive(marker.pid) {
        Some(marker)
    } else {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(&marker.control_socket);
        None
    }
}

/// Writes the marker on creation and removes it (and the socket) on drop.
pub struct Advertised {
    marker_path: PathBuf,
    socket: PathBuf,
}

impl Advertised {
    pub fn write(marker_path: &Path, marker: &Marker) -> std::io::Result<Advertised> {
        if let Some(dir) = marker_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(marker_path, serde_json::to_vec(marker)?)?;
        Ok(Advertised {
            marker_path: marker_path.to_path_buf(),
            socket: marker.control_socket.clone(),
        })
    }

    /// Keep the advertised mode current after an attach or detach.
    pub fn update(&self, marker: &Marker) {
        let _ = std::fs::write(&self.marker_path, serde_json::to_vec(marker).unwrap_or_default());
    }
}

impl Drop for Advertised {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.marker_path);
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Send one control request to the running server.
pub async fn request(socket: &Path, req: &ControlRequest) -> anyhow::Result<ControlReply> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| anyhow::anyhow!("no MCP server listening at {} ({e})", socket.display()))?;
    let (r, w) = stream.into_split();
    let mut writer: FramedWriter<_, ControlRequest> = FramedWriter::new(w);
    let mut reader: FramedReader<_, ControlReply> = FramedReader::new(r);
    writer.send(req).await?;
    Ok(reader.recv().await?)
}

/// Serve control requests for `server`: one request per connection.
pub async fn serve(listener: tokio::net::UnixListener, server: McpServer, url: String, advertised: std::sync::Arc<Advertised>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { return };
        let server = server.clone();
        let url = url.clone();
        let advertised = advertised.clone();
        tokio::spawn(async move {
            let (r, w) = stream.into_split();
            let mut reader: FramedReader<_, ControlRequest> = FramedReader::new(r);
            let mut writer: FramedWriter<_, ControlReply> = FramedWriter::new(w);
            let reply = match reader.recv().await {
                Ok(req) => handle(&server, req, &url).await,
                Err(e) => ControlReply::Error {
                    message: format!("bad request: {e}"),
                },
            };
            advertised.update(&Marker {
                pid: std::process::id(),
                url: url.clone(),
                control_socket: control_socket_path_of(&advertised),
                mode: server.mode(),
            });
            let _ = writer.send(&reply).await;
        });
    }
}

fn control_socket_path_of(a: &Advertised) -> PathBuf {
    a.socket.clone()
}

/// Apply a control request to the server.
pub async fn handle(server: &McpServer, req: ControlRequest, url: &str) -> ControlReply {
    match req {
        ControlRequest::Status => ControlReply::Ok {
            mode: server.mode(),
            url: url.to_string(),
        },
        ControlRequest::Attach {
            endpoint,
            token,
            name,
            decklist,
        } => {
            if let Some(s) = server.session() {
                if s.outcome().is_none() {
                    return ControlReply::Error {
                        message: format!("already seated in game {}; detach first", s.game_id),
                    };
                }
            }
            let endpoint = match Endpoint::parse(&endpoint) {
                Ok(e) => e,
                Err(e) => return ControlReply::Error { message: e },
            };
            let config = SessionConfig {
                endpoint,
                token: Token(token),
                name,
                decklist,
            };
            match Session::connect(config).await {
                Ok(session) => {
                    server.attach(session);
                    ControlReply::Ok {
                        mode: server.mode(),
                        url: url.to_string(),
                    }
                }
                Err(e) => ControlReply::Error {
                    message: format!("could not join the game: {e:#}"),
                },
            }
        }
        ControlRequest::Detach => {
            server.detach();
            ControlReply::Ok {
                mode: server.mode(),
                url: url.to_string(),
            }
        }
    }
}
