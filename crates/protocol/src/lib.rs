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
pub mod ws;

pub use async_client::{AsyncClient, ConnState, Joined, ReconnectConfig, ReconnectPolicy};
pub use client::{Client, ClientError, Welcome};
pub use editor::{DeckCard, DeckGroup, EditorDeck, EditorError, EditorReply, EditorRequest, EditorStatus};
pub use endpoint::Endpoint;
pub use framing::{
    BoxedTransport, Connection, FrameError, FramedReader, FramedWriter, MessageConnection, MessageReader, MessageTransport, MessageWriter,
};
pub use messages::{
    ClientEnvelope, ClientMessage, ErrorCode, GameId, LegalAction, LobbyView, ProtocolError, Role, SeatStatus, ServerEnvelope,
    ServerMessage, Token, PROTOCOL_VERSION,
};
pub use ws::TlsOptions;

pub use engine::{EventView, GameView};
