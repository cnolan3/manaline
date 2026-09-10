//! The daemon protocol (docs/SPEC.md §2): newline-delimited JSON, identical
//! over every transport. This crate holds the message types, the error
//! shape, the framing, endpoint and path conventions, and a client helper.
//! State views come from the engine, which is the only thing that produces them.

pub mod async_client;
pub mod client;
pub mod editor;
pub mod endpoint;
pub mod framing;
pub mod messages;

pub use async_client::AsyncClient;
pub use client::{Client, ClientError, Welcome};
pub use editor::{DeckCard, DeckGroup, EditorDeck, EditorError, EditorReply, EditorRequest, EditorStatus};
pub use endpoint::Endpoint;
pub use framing::{Connection, FrameError, FramedReader, FramedWriter};
pub use messages::{
    ClientEnvelope, ClientMessage, ErrorCode, GameId, LegalAction, LobbyView, ProtocolError, Role, SeatStatus, ServerEnvelope,
    ServerMessage, Token, PROTOCOL_VERSION,
};

pub use engine::{EventView, GameView};
