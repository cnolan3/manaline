//! A client-side helper shared by the TUI, the built-in bot, and the MCP
//! server: connects to an endpoint, numbers requests, matches replies, and
//! queues pushed messages for the caller to drain.

use crate::endpoint::Endpoint;
use crate::framing::{Connection, FrameError};
use crate::messages::{
    ClientEnvelope, ClientMessage, LegalAction, LobbyView, ProtocolError, Role, ServerEnvelope, ServerMessage, Token,
    PROTOCOL_VERSION,
};
use engine::{Action, EventView, Format, GameView};
use std::collections::VecDeque;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("unexpected reply: {0:?}")]
    Unexpected(Box<ServerMessage>),
    #[error("connect failed: {0}")]
    Connect(std::io::Error),
}

pub type BoxedRead = Box<dyn AsyncRead + Unpin + Send>;
pub type BoxedWrite = Box<dyn AsyncWrite + Unpin + Send>;

pub struct Client {
    conn: Connection<BoxedRead, BoxedWrite, ServerEnvelope, ClientEnvelope>,
    next_req: u64,
    pushed: VecDeque<ServerMessage>,
}

/// What `hello` told us.
#[derive(Clone, Debug)]
pub struct Welcome {
    pub role: Role,
    pub game_id: crate::messages::GameId,
    pub format: Format,
    pub lobby: LobbyView,
    pub state: Option<GameView>,
}

impl Client {
    pub async fn connect(endpoint: &Endpoint) -> Result<Client, ClientError> {
        match endpoint {
            Endpoint::Unix(path) => {
                let stream = tokio::net::UnixStream::connect(path).await.map_err(ClientError::Connect)?;
                let (r, w) = stream.into_split();
                Ok(Client::from_parts(Box::new(r), Box::new(w)))
            }
            Endpoint::Tcp(addr) => {
                let stream = tokio::net::TcpStream::connect(addr).await.map_err(ClientError::Connect)?;
                stream.set_nodelay(true).ok();
                let (r, w) = stream.into_split();
                Ok(Client::from_parts(Box::new(r), Box::new(w)))
            }
        }
    }

    pub fn from_parts(reader: BoxedRead, writer: BoxedWrite) -> Client {
        Client { conn: Connection::new(reader, writer), next_req: 1, pushed: VecDeque::new() }
    }

    /// Send one request and wait for its reply, queueing any pushed messages
    /// that arrive in between.
    pub async fn request(&mut self, msg: ClientMessage) -> Result<ServerMessage, ClientError> {
        let req = self.next_req;
        self.next_req += 1;
        self.conn.send(&ClientEnvelope { req: Some(req), msg }).await?;
        loop {
            let env = self.conn.recv().await?;
            match env.req {
                Some(r) if r == req => {
                    return match env.msg {
                        ServerMessage::Error(e) => Err(ClientError::Protocol(e)),
                        m => Ok(m),
                    };
                }
                Some(_) => continue, // a reply to a request we no longer care about
                None => self.pushed.push_back(env.msg),
            }
        }
    }

    /// The next pushed message: from the queue if any, otherwise waiting on the connection.
    pub async fn next_push(&mut self) -> Result<ServerMessage, ClientError> {
        if let Some(m) = self.pushed.pop_front() {
            return Ok(m);
        }
        loop {
            let env = self.conn.recv().await?;
            if env.is_push() {
                return Ok(env.msg);
            }
        }
    }

    /// Pushed messages already received, without waiting.
    pub fn drain_pushed(&mut self) -> Vec<ServerMessage> {
        self.pushed.drain(..).collect()
    }

    // ----- typed conveniences -----

    pub async fn create_game(
        &mut self,
        format: &str,
        seats: u8,
        seed: Option<u64>,
    ) -> Result<(crate::messages::GameId, Vec<Token>, Token), ClientError> {
        match self.request(ClientMessage::CreateGame { format: format.into(), seats, seed }).await? {
            ServerMessage::GameCreated { game_id, seat_tokens, spectator_token } => {
                Ok((game_id, seat_tokens, spectator_token))
            }
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn hello(&mut self, token: &Token, name: Option<&str>) -> Result<Welcome, ClientError> {
        let msg = ClientMessage::Hello {
            token: token.clone(),
            protocol_version: PROTOCOL_VERSION,
            name: name.map(String::from),
        };
        match self.request(msg).await? {
            ServerMessage::Welcome { role, game_id, format, lobby, state, .. } => {
                Ok(Welcome { role, game_id, format, lobby, state })
            }
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    /// `Ok(())` if the deck was accepted, `Err(violations)` if rejected.
    pub async fn set_deck(&mut self, decklist: &str) -> Result<Result<(), Vec<engine::Violation>>, ClientError> {
        match self.request(ClientMessage::SetDeck { decklist: decklist.into(), commander: None }).await? {
            ServerMessage::DeckOk => Ok(Ok(())),
            ServerMessage::DeckRejected { violations } => Ok(Err(violations)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn ready(&mut self) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Ready).await
    }

    pub async fn subscribe(&mut self) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Subscribe).await
    }

    pub async fn chat(&mut self, text: &str, to: Option<engine::Seat>) -> Result<(), ClientError> {
        self.expect_ok(ClientMessage::Chat { text: text.into(), to }).await
    }

    async fn expect_ok(&mut self, msg: ClientMessage) -> Result<(), ClientError> {
        match self.request(msg).await? {
            ServerMessage::Ok => Ok(()),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn get_state(&mut self) -> Result<GameView, ClientError> {
        match self.request(ClientMessage::GetState).await? {
            ServerMessage::State { state } => Ok(state),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn get_legal_actions(&mut self) -> Result<(Vec<LegalAction>, u64), ClientError> {
        match self.request(ClientMessage::GetLegalActions).await? {
            ServerMessage::LegalActions { actions, state_version, .. } => Ok((actions, state_version)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    /// Submit a full action. Returns the events, the new state, and the next legal actions.
    pub async fn act(
        &mut self,
        action: Action,
        state_version: u64,
    ) -> Result<(Vec<EventView>, GameView, Vec<LegalAction>), ClientError> {
        let msg = ClientMessage::Act { action_id: None, action: Some(action), state_version };
        match self.request(msg).await? {
            ServerMessage::Ack { events, state, legal_actions, .. } => Ok((events, state, legal_actions)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn act_by_id(
        &mut self,
        action_id: u32,
        state_version: u64,
    ) -> Result<(Vec<EventView>, GameView, Vec<LegalAction>), ClientError> {
        let msg = ClientMessage::Act { action_id: Some(action_id), action: None, state_version };
        match self.request(msg).await? {
            ServerMessage::Ack { events, state, legal_actions, .. } => Ok((events, state, legal_actions)),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }

    pub async fn ping(&mut self) -> Result<(), ClientError> {
        match self.request(ClientMessage::Ping).await? {
            ServerMessage::Pong => Ok(()),
            other => Err(ClientError::Unexpected(Box::new(other))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_reply_with_interleaved_pushes() {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut client = Client::from_parts(Box::new(ar), Box::new(aw));
        let mut server: Connection<_, _, ClientEnvelope, ServerEnvelope> = Connection::new(br, bw);

        let server_task = tokio::spawn(async move {
            let env = server.recv().await.unwrap();
            assert_eq!(env.msg, ClientMessage::Ping);
            // A push arrives before the reply.
            server.send(&ServerEnvelope::from(ServerMessage::Lobby { lobby: LobbyView::default() })).await.unwrap();
            server.send(&ServerEnvelope { req: env.req, msg: ServerMessage::Pong }).await.unwrap();
            let env = server.recv().await.unwrap();
            let err = ProtocolError::new(crate::messages::ErrorCode::BadRequest, "no");
            server.send(&ServerEnvelope { req: env.req, msg: ServerMessage::Error(err) }).await.unwrap();
            let (_, mut w) = server.split();
            w.shutdown().await.unwrap();
        });

        client.ping().await.unwrap();
        assert!(matches!(client.next_push().await.unwrap(), ServerMessage::Lobby { .. }));
        let err = client.ping().await.unwrap_err();
        assert!(matches!(err, ClientError::Protocol(e) if e.code == crate::messages::ErrorCode::BadRequest));
        server_task.await.unwrap();
    }
}
