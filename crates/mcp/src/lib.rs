//! The manaline MCP server (docs/SPEC.md §7): a thin proxy that lets any
//! MCP-capable agent play a seat. Speaks stdio and streamable HTTP.

pub mod primer;
pub mod render;
pub mod server;
pub mod session;

pub use server::{McpServer, Shared};
pub use session::{Session, SessionConfig};

use anyhow::{Context, Result};
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServiceExt;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Connect to the daemon and build the server.
pub async fn connect(config: SessionConfig) -> Result<McpServer> {
    let session = Session::connect(config).await?;
    Ok(McpServer::new(session))
}

/// A server with no seat of its own: card search, deck analysis and the
/// resources for deckbuilding, and a seat at whatever game the human has
/// published as soon as a session calls a game tool.
pub fn standalone(format: engine::Format) -> McpServer {
    McpServer::standalone(format)
}

/// Serve over stdin/stdout until the client goes away: one session, so one
/// seat. Nothing else may write to stdout in this mode.
pub async fn serve_stdio(server: McpServer) -> Result<()> {
    let running = server.serve(rmcp::transport::stdio()).await.context("MCP stdio handshake")?;
    running.waiting().await.context("MCP stdio session")?;
    Ok(())
}

/// A running streamable HTTP listener.
pub struct HttpServer {
    pub addr: SocketAddr,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl HttpServer {
    /// `http://<addr>/mcp`
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }

    pub async fn wait(self) -> Result<()> {
        self.task.await.context("HTTP server task")?
    }
}

/// Serve streamable HTTP at `/mcp` on `addr`, one session per connecting
/// client and one seat per session. `127.0.0.1:0` picks a free port.
pub async fn serve_http(server: McpServer, addr: &str) -> Result<HttpServer> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let addr = listener.local_addr()?;
    let cancel = CancellationToken::new();
    let config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_cancellation_token(cancel.child_token());
    // One handler per MCP session, all over the same cards and runtime
    // directory: each session takes its own seat at the table.
    let service = StreamableHttpService::new(move || Ok(server.new_session()), Arc::new(LocalSessionManager::default()), config);
    let router = axum::Router::new().nest_service("/mcp", service);
    let ct = cancel.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { ct.cancelled().await })
            .await
            .context("serving HTTP")
    });
    Ok(HttpServer { addr, cancel, task })
}

/// Exit when the parent process goes away (`--parent-pid`).
pub fn watch_parent(pid: u32, on_gone: impl FnOnce() + Send + 'static) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            // SAFETY: signal 0 checks for existence only.
            if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
                on_gone();
                return;
            }
        }
    });
}
