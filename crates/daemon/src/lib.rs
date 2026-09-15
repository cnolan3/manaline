//! `manaline-daemon`: hosts games, speaks the protocol over a Unix socket,
//! TCP, or WebSockets, keeps the replay log, and never sends one seat's hidden
//! information to another (docs/SPEC.md §5).
//!
//! One game or many is a flag, not a fork in the code: `server::DaemonConfig`
//! with `serve: false` is the one-game daemon `play` spawns, and with
//! `serve: true` it is `manaline server`, the tier-1 lobby of §2.2. The
//! structure is `server` (process, listeners, routing) over `registry` (codes,
//! tokens, games) over `game_task` (one table, entirely on its own).

pub mod game_task;
pub mod lobby;
pub(crate) mod registry;
pub mod replay;
pub mod server;

pub use server::{CreateGame, Daemon, DaemonConfig, DaemonError, DaemonHandle, IdlePolicy, StartupInfo, Status, TlsConfig};
