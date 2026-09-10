//! Driving a running deck editor from outside it (§2): an MCP server helping
//! the human at the keyboard opens the editor's Unix socket, sends one
//! request, reads one reply, and the editor closes the connection. The wire
//! format is the crate's usual newline-delimited JSON.

use crate::framing::{FrameError, FramedReader, FramedWriter};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A request from an MCP server to a running deck editor. One request per connection; the editor replies once and closes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditorRequest {
    Status,
    Deck,
    AddCard { name: String, count: u32 },
    RemoveCard { name: String, count: u32, all: bool },
    SetCount { name: String, count: u32 },
    ReplaceDeck { decklist: String },
    Undo,
    Stats,
    Save,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditorReply {
    Status(EditorStatus),
    Deck(EditorDeck),
    /// After an edit: what changed, in words, and the new status.
    Changed {
        message: String,
        status: EditorStatus,
    },
    /// Curve, colour sources, sample hands: `text` is the rendered analysis; `json` the structured form.
    Stats {
        text: String,
        json: serde_json::Value,
        status: EditorStatus,
    },
    Saved {
        path: PathBuf,
        status: EditorStatus,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorStatus {
    pub path: PathBuf,
    pub format: String,
    pub cards: u32,
    pub dirty: bool,
    pub legal: bool,
    /// "40 cards · legal in Starter Cube" or the problems.
    pub legality: String,
    /// The last thing an agent did through the editor, if anything ("added 4 Raise the Alarm").
    pub last_agent_action: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EditorDeck {
    pub status: EditorStatus,
    pub groups: Vec<DeckGroup>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeckGroup {
    /// "Creatures", "Lands", ...
    pub title: String,
    pub count: u32,
    pub cards: Vec<DeckCard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeckCard {
    pub name: String,
    pub count: u32,
    pub cost: String,
    /// Empty when fine; otherwise a problem such as "5 copies, at most 4 allowed".
    pub problem: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("no deckbuilder is listening at {0}")]
    Connect(PathBuf),
    #[error("{0}")]
    Frame(#[from] FrameError),
}

/// Send one request to the editor listening on `socket` and read its reply.
pub async fn request(socket: &Path, req: &EditorRequest) -> Result<EditorReply, EditorError> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|_| EditorError::Connect(socket.to_path_buf()))?;
    let (read_half, write_half) = stream.into_split();
    let mut writer: FramedWriter<_, EditorRequest> = FramedWriter::new(write_half);
    let mut reader: FramedReader<_, EditorReply> = FramedReader::new(read_half);
    writer.send(req).await?;
    Ok(reader.recv().await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> EditorStatus {
        EditorStatus {
            path: PathBuf::from("/decks/aggro.txt"),
            format: "starter-cube".into(),
            cards: 40,
            dirty: true,
            legal: true,
            legality: "40 cards · legal in Starter Cube".into(),
            last_agent_action: Some("added 4 Raise the Alarm".into()),
        }
    }

    #[test]
    fn requests_and_replies_round_trip_as_tagged_json() {
        let req = EditorRequest::AddCard {
            name: "Raise the Alarm".into(),
            count: 4,
        };
        let value: serde_json::Value = serde_json::to_value(&req).unwrap();
        assert_eq!(value["type"], "add_card");
        assert_eq!(serde_json::from_value::<EditorRequest>(value).unwrap(), req);

        let reply = EditorReply::Changed {
            message: "added 4 Raise the Alarm".into(),
            status: status(),
        };
        let value: serde_json::Value = serde_json::to_value(&reply).unwrap();
        assert_eq!(value["type"], "changed");
        let text = serde_json::to_string(&reply).unwrap();
        assert_eq!(serde_json::from_str::<EditorReply>(&text).unwrap(), reply);
    }

    #[tokio::test]
    async fn request_talks_to_a_listening_editor() {
        let socket = std::env::temp_dir().join(format!("manaline-editor-req-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, write_half) = stream.into_split();
            let mut reader: FramedReader<_, EditorRequest> = FramedReader::new(read_half);
            let mut writer: FramedWriter<_, EditorReply> = FramedWriter::new(write_half);
            let got = reader.recv().await.unwrap();
            writer.send(&EditorReply::Error { message: "hi".into() }).await.unwrap();
            got
        });

        let reply = request(&socket, &EditorRequest::Status).await.unwrap();
        assert_eq!(reply, EditorReply::Error { message: "hi".into() });
        assert_eq!(server.await.unwrap(), EditorRequest::Status);
        let _ = std::fs::remove_file(&socket);
    }

    #[tokio::test]
    async fn a_missing_socket_is_a_connect_error() {
        let socket = std::env::temp_dir().join(format!("manaline-editor-absent-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let err = request(&socket, &EditorRequest::Status).await.unwrap_err();
        assert!(matches!(err, EditorError::Connect(p) if p == socket));
    }
}
