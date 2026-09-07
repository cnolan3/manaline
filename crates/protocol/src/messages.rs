//! The daemon protocol (§2): one message vocabulary, identical over every
//! transport. Client messages carry a `type` tag; daemon messages likewise.
//! An optional `req` id on a client message is echoed on the direct reply so
//! a client can tell replies from pushed events.

use engine::{ActReason, Action, EventView, Format, GameView, RulesError, Seat, Violation};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Bumped on any incompatible change. The daemon refuses a `hello` with a
/// version it does not speak and says which ones it does.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GameId(pub String);

impl fmt::Display for GameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A seat or spectator token. The only authentication there is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(pub String);

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Seat(Seat),
    Spectator,
}

impl Role {
    pub fn seat(self) -> Option<Seat> {
        match self {
            Role::Seat(s) => Some(s),
            Role::Spectator => None,
        }
    }
}

/// One entry of a `legal_actions` reply: the action, a stable-within-one-
/// state-version id, and a human-readable description.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalAction {
    pub id: u32,
    pub action: Action,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatStatus {
    pub seat: Seat,
    pub name: Option<String>,
    pub connected: bool,
    pub deck_ok: bool,
    pub ready: bool,
}

/// Where the lobby stands. Pushed to every connection whenever it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyView {
    pub seats: Vec<SeatStatus>,
    pub started: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Create the daemon's one game. Only valid once, before any `hello`.
    CreateGame {
        /// A built-in format name.
        format: String,
        seats: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u64>,
    },
    /// Authenticate this connection as a seat or spectator.
    Hello {
        token: Token,
        protocol_version: u32,
        /// Display name for the seat; ignored for spectators.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Limited formats only: this seat's card pool.
    GetPool,
    SetDeck {
        /// The standard text deck format (§4.5).
        decklist: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commander: Option<String>,
    },
    Ready,
    GetState,
    GetLegalActions,
    /// Exactly one of `action_id` (from the last `legal_actions`) or `action`.
    Act {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_id: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<Action>,
        state_version: u64,
    },
    /// Start receiving pushed `event` and `lobby` messages on this connection.
    Subscribe,
    Chat {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Seat>,
    },
    Ping,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    GameCreated {
        game_id: GameId,
        seat_tokens: Vec<Token>,
        spectator_token: Token,
    },
    Welcome {
        role: Role,
        game_id: GameId,
        format: Format,
        protocol_version: u32,
        lobby: LobbyView,
        /// `None` until the game has started.
        state: Option<GameView>,
    },
    Pool {
        cards: Vec<engine::CardId>,
    },
    DeckOk,
    DeckRejected {
        violations: Vec<Violation>,
    },
    /// Reply to `ready`, `subscribe`, and `chat`.
    Ok,
    State {
        state: GameView,
    },
    LegalActions {
        actions: Vec<LegalAction>,
        state_version: u64,
        /// Why this seat must act, if it must.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<ActReason>,
    },
    Ack {
        applied: Action,
        events: Vec<EventView>,
        state: GameView,
        /// The next legal actions if this seat still must act; empty otherwise.
        legal_actions: Vec<LegalAction>,
    },
    /// A pushed game event, seat-filtered. `state_version` is the version
    /// after the batch this event belongs to.
    Event {
        event: EventView,
        state_version: u64,
    },
    /// A pushed lobby snapshot.
    Lobby {
        lobby: LobbyView,
    },
    /// Pushed to a seat that must act when another client asks to re-notify
    /// it (the TUI's Enter-to-nudge, §6).
    MustAct {
        reason: ActReason,
        state_version: u64,
    },
    Error(ProtocolError),
    Pong,
}

/// Every failure is one shape. `code` is for programs, `message` for people
/// and agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_version: Option<u64>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadToken,
    UnsupportedVersion,
    IllegalAction,
    StaleStateVersion,
    NotYourTurnToAct,
    DeckRejected,
    GameOver,
    /// A message that is malformed or not valid in the connection's current
    /// state (for example `act` before `hello`, or `create_game` twice).
    BadRequest,
    Internal,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> ProtocolError {
        let retryable = matches!(code, ErrorCode::StaleStateVersion | ErrorCode::NotYourTurnToAct);
        ProtocolError {
            code,
            message: message.into(),
            retryable,
            state_version: None,
        }
    }

    pub fn with_version(mut self, v: u64) -> ProtocolError {
        self.state_version = Some(v);
        self
    }

    pub fn bad_request(message: impl Into<String>) -> ProtocolError {
        ProtocolError::new(ErrorCode::BadRequest, message)
    }

    pub fn internal(message: impl Into<String>) -> ProtocolError {
        ProtocolError::new(ErrorCode::Internal, message)
    }
}

impl From<RulesError> for ProtocolError {
    fn from(e: RulesError) -> ProtocolError {
        let code = match &e {
            RulesError::GameOver { .. } => ErrorCode::GameOver,
            RulesError::NotYourTurnToAct { .. } => ErrorCode::NotYourTurnToAct,
            RulesError::IllegalAction { .. } | RulesError::Unsupported { .. } => ErrorCode::IllegalAction,
            RulesError::Setup { .. } => ErrorCode::DeckRejected,
        };
        ProtocolError::new(code, e.to_string())
    }
}

/// A client message with its optional request id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req: Option<u64>,
    #[serde(flatten)]
    pub msg: ClientMessage,
}

/// A daemon message; `req` echoes the request it answers, and is absent on
/// pushed messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub req: Option<u64>,
    #[serde(flatten)]
    pub msg: ServerMessage,
}

impl ServerEnvelope {
    /// Pushed messages have no request id.
    pub fn is_push(&self) -> bool {
        self.req.is_none()
    }
}

impl From<ClientMessage> for ClientEnvelope {
    fn from(msg: ClientMessage) -> ClientEnvelope {
        ClientEnvelope { req: None, msg }
    }
}

impl From<ServerMessage> for ServerEnvelope {
    fn from(msg: ServerMessage) -> ServerEnvelope {
        ServerEnvelope { req: None, msg }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{ObjectId, Seat};

    #[test]
    fn wire_shapes() {
        let act = ClientEnvelope {
            req: Some(7),
            msg: ClientMessage::Act {
                action_id: Some(3),
                action: None,
                state_version: 12,
            },
        };
        let json = serde_json::to_string(&act).unwrap();
        assert_eq!(json, r#"{"req":7,"type":"act","action_id":3,"state_version":12}"#);
        let back: ClientEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, act);

        let full = ClientMessage::Act {
            action_id: None,
            action: Some(Action::PlayLand { object: ObjectId(4) }),
            state_version: 1,
        };
        let json = serde_json::to_string(&full).unwrap();
        assert_eq!(json, r#"{"type":"act","action":{"kind":"play_land","object":4},"state_version":1}"#);

        let hello = ClientMessage::Hello {
            token: Token("abc".into()),
            protocol_version: 1,
            name: None,
        };
        assert_eq!(
            serde_json::to_string(&hello).unwrap(),
            r#"{"type":"hello","token":"abc","protocol_version":1}"#
        );

        let err = ServerEnvelope {
            req: Some(7),
            msg: ServerMessage::Error(ProtocolError::new(ErrorCode::StaleStateVersion, "state moved on").with_version(13)),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"req":7,"type":"error","code":"stale_state_version","message":"state moved on","retryable":true,"state_version":13}"#
        );
        let back: ServerEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, err);
        assert!(!back.is_push());

        let push: ServerEnvelope = serde_json::from_str(r#"{"type":"pong"}"#).unwrap();
        assert!(push.is_push());
        assert_eq!(push.msg, ServerMessage::Pong);
    }

    #[test]
    fn rules_errors_map_to_codes() {
        let e: ProtocolError = RulesError::NotYourTurnToAct { seat: Seat(1) }.into();
        assert_eq!(e.code, ErrorCode::NotYourTurnToAct);
        assert!(e.retryable);
        let e: ProtocolError = RulesError::illegal("nope").into();
        assert_eq!(e.code, ErrorCode::IllegalAction);
        assert!(!e.retryable);
    }
}
