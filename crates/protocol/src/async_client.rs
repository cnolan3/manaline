//! A client whose reader runs in its own task: requests are awaited by any
//! number of holders of a cloneable handle, and pushed messages arrive on a
//! channel. This is what an interactive client (the TUI) needs, since it
//! must react to pushes while a request is in flight.

use crate::client::{BoxedRead, BoxedWrite, ClientError, Welcome};
use crate::endpoint::Endpoint;
use crate::framing::{FrameError, FramedReader, FramedWriter};
use crate::messages::{ClientEnvelope, ClientMessage, LegalAction, ServerEnvelope, ServerMessage, Token, PROTOCOL_VERSION};
use engine::{Action, EventView, GameView};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<ServerMessage>>>>;

#[derive(Clone)]
pub struct AsyncClient {
    out: mpsc::Sender<ClientEnvelope>,
    pending: Pending,
    next_req: Arc<AtomicU64>,
}

/// Connect and spawn the reader and writer tasks. Pushed messages arrive on
/// the returned receiver; it closes when the connection does.
pub async fn connect(endpoint: &Endpoint) -> Result<(AsyncClient, mpsc::Receiver<ServerMessage>), ClientError> {
    let (r, w): (BoxedRead, BoxedWrite) = match endpoint {
        Endpoint::Unix(path) => {
            let s = tokio::net::UnixStream::connect(path).await.map_err(ClientError::Connect)?;
            let (r, w) = s.into_split();
            (Box::new(r), Box::new(w))
        }
        Endpoint::Tcp(addr) => {
            let s = tokio::net::TcpStream::connect(addr).await.map_err(ClientError::Connect)?;
            s.set_nodelay(true).ok();
            let (r, w) = s.into_split();
            (Box::new(r), Box::new(w))
        }
    };
    Ok(spawn(r, w))
}

pub fn spawn(reader: BoxedRead, writer: BoxedWrite) -> (AsyncClient, mpsc::Receiver<ServerMessage>) {
    let (out_tx, mut out_rx) = mpsc::channel::<ClientEnvelope>(64);
    let (push_tx, push_rx) = mpsc::channel::<ServerMessage>(256);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

    let mut writer: FramedWriter<BoxedWrite, ClientEnvelope> = FramedWriter::new(writer);
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if writer.send(&msg).await.is_err() {
                break;
            }
        }
    });

    let mut reader: FramedReader<BoxedRead, ServerEnvelope> = FramedReader::new(reader);
    let pending_r = pending.clone();
    tokio::spawn(async move {
        loop {
            match reader.recv().await {
                Ok(env) => match env.req {
                    Some(req) => {
                        let waiter = pending_r.lock().unwrap().remove(&req);
                        if let Some(w) = waiter {
                            let _ = w.send(env.msg);
                        }
                    }
                    None => {
                        if push_tx.send(env.msg).await.is_err() {
                            break;
                        }
                    }
                },
                Err(FrameError::Json(_)) => continue,
                Err(_) => break,
            }
        }
        // Wake every waiter with a closed error by dropping their senders.
        pending_r.lock().unwrap().clear();
    });

    (
        AsyncClient {
            out: out_tx,
            pending,
            next_req: Arc::new(AtomicU64::new(1)),
        },
        push_rx,
    )
}

impl AsyncClient {
    pub async fn request(&self, msg: ClientMessage) -> Result<ServerMessage, ClientError> {
        let req = self.next_req.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(req, tx);
        self.out
            .send(ClientEnvelope { req: Some(req), msg })
            .await
            .map_err(|_| ClientError::Frame(FrameError::Closed))?;
        match rx.await {
            Ok(ServerMessage::Error(e)) => Err(ClientError::Protocol(e)),
            Ok(m) => Ok(m),
            Err(_) => Err(ClientError::Frame(FrameError::Closed)),
        }
    }

    pub async fn hello(&self, token: &Token, name: Option<&str>) -> Result<Welcome, ClientError> {
        let msg = ClientMessage::Hello {
            token: token.clone(),
            protocol_version: PROTOCOL_VERSION,
            name: name.map(String::from),
        };
        match self.request(msg).await? {
            ServerMessage::Welcome {
                role,
                game_id,
                format,
                lobby,
                state,
                ..
            } => Ok(Welcome {
                role,
                game_id,
                format,
                lobby,
                state,
            }),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn set_deck(&self, decklist: &str) -> Result<Result<(), Vec<engine::Violation>>, ClientError> {
        match self
            .request(ClientMessage::SetDeck {
                decklist: decklist.into(),
                commander: None,
            })
            .await?
        {
            ServerMessage::DeckOk => Ok(Ok(())),
            ServerMessage::DeckRejected { violations } => Ok(Err(violations)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    async fn expect_ok(&self, msg: ClientMessage) -> Result<(), ClientError> {
        match self.request(msg).await? {
            ServerMessage::Ok => Ok(()),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn ready(&self) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Ready).await
    }

    pub async fn subscribe(&self) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Subscribe).await
    }

    pub async fn chat(&self, text: &str, to: Option<engine::Seat>) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Chat { text: text.into(), to }).await
    }

    pub async fn get_state(&self) -> Result<GameView, ClientError> {
        match self.request(ClientMessage::GetState).await? {
            ServerMessage::State { state } => Ok(state),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn get_legal_actions(&self) -> Result<(Vec<LegalAction>, u64, Option<engine::ActReason>), ClientError> {
        match self.request(ClientMessage::GetLegalActions).await? {
            ServerMessage::LegalActions {
                actions,
                state_version,
                reason,
            } => Ok((actions, state_version, reason)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn act(&self, action: Action, state_version: u64) -> Result<(Vec<EventView>, GameView, Vec<LegalAction>), ClientError> {
        let msg = ClientMessage::Act {
            action_id: None,
            action: Some(action),
            state_version,
        };
        match self.request(msg).await? {
            ServerMessage::Ack {
                events,
                state,
                legal_actions,
                ..
            } => Ok((events, state, legal_actions)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }
}
