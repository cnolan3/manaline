//! `manaline-daemon`: hosts exactly one `Game`, speaks the protocol over a
//! Unix socket and/or TCP, keeps the replay log, and never sends one seat's
//! hidden information to another (docs/SPEC.md §5).

pub mod lobby;
pub mod replay;
pub mod server;

pub use server::{CreateGame, Daemon, DaemonConfig, DaemonError, DaemonHandle, StartupInfo, Status};
