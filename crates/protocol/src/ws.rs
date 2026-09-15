//! The WebSocket transport (§2.2): one text frame is one protocol message.
//!
//! This is the transport lobby servers speak, because `wss://` on 443 passes
//! every home firewall and hotel network without anyone opening a port. The
//! messages are byte-for-byte the ones the Unix-socket and TCP transports
//! carry; only the framing differs, so nothing above `framing` changes.
//!
//! Certificates are files on disk here. Getting them is a deployment concern —
//! a reverse proxy, or Let's Encrypt tooling — not this binary's job.

use crate::framing::{BoxedReceiver, BoxedSender, BoxedTransport, FrameError, MessageReceiver, MessageSender, MessageTransport};
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

pub use tokio_rustls::server::TlsStream;
pub use tokio_rustls::TlsAcceptor;

/// How a client verifies a `wss://` server.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsOptions {
    /// A PEM file of trust anchors to use instead of the public roots, for a
    /// server with a self-signed or private-CA certificate.
    pub ca: Option<PathBuf>,
}

impl TlsOptions {
    /// The defaults, plus `MANALINE_TLS_CA` if it is set: the public roots
    /// unless the user points us at their own CA.
    pub fn from_env() -> TlsOptions {
        TlsOptions {
            ca: std::env::var_os("MANALINE_TLS_CA").map(PathBuf::from),
        }
    }

    pub fn with_ca(ca: impl Into<PathBuf>) -> TlsOptions {
        TlsOptions { ca: Some(ca.into()) }
    }
}

/// Dial a `ws://` or `wss://` URL and hand back a message transport.
pub async fn connect(url: &str, tls: &TlsOptions) -> Result<BoxedTransport, FrameError> {
    let (stream, _response) = match &tls.ca {
        None => tokio_tungstenite::connect_async(url).await.map_err(ws_error)?,
        Some(ca) => {
            let connector = tokio_tungstenite::Connector::Rustls(Arc::new(client_config(ca)?));
            tokio_tungstenite::connect_async_tls_with_config(url, None, false, Some(connector))
                .await
                .map_err(ws_error)?
        }
    };
    Ok(Box::new(WsTransport::new(stream)))
}

/// Complete the server-side handshake on an accepted socket.
pub async fn accept<S>(stream: S) -> Result<BoxedTransport, FrameError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let ws = tokio_tungstenite::accept_async(stream).await.map_err(ws_error)?;
    Ok(Box::new(WsTransport::new(ws)))
}

/// A TLS acceptor from a PEM certificate chain and its private key.
pub fn tls_acceptor(cert: &Path, key: &Path) -> std::io::Result<TlsAcceptor> {
    let mut chain_pem = std::io::BufReader::new(std::fs::File::open(cert)?);
    let chain = rustls_pemfile::certs(&mut chain_pem).collect::<Result<Vec<_>, _>>()?;
    if chain.is_empty() {
        return Err(std::io::Error::other(format!("no certificates in {}", cert.display())));
    }
    let mut key_pem = std::io::BufReader::new(std::fs::File::open(key)?);
    let private =
        rustls_pemfile::private_key(&mut key_pem)?.ok_or_else(|| std::io::Error::other(format!("no private key in {}", key.display())))?;
    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, private)
        .map_err(std::io::Error::other)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Trust exactly the anchors in `ca`, for a server whose certificate the
/// public roots do not vouch for.
fn client_config(ca: &Path) -> Result<tokio_rustls::rustls::ClientConfig, FrameError> {
    let mut pem = std::io::BufReader::new(std::fs::File::open(ca)?);
    let mut roots = tokio_rustls::rustls::RootCertStore::empty();
    for cert in rustls_pemfile::certs(&mut pem) {
        roots.add(cert?).map_err(|e| FrameError::Ws(e.to_string()))?;
    }
    if roots.is_empty() {
        return Err(FrameError::Ws(format!("no certificates in {}", ca.display())));
    }
    Ok(tokio_rustls::rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn ws_error(e: tokio_tungstenite::tungstenite::Error) -> FrameError {
    use tokio_tungstenite::tungstenite::Error;
    match e {
        Error::ConnectionClosed | Error::AlreadyClosed => FrameError::Closed,
        Error::Io(e) => FrameError::Io(e),
        other => FrameError::Ws(other.to_string()),
    }
}

/// One protocol message per WebSocket text frame.
pub struct WsTransport<S> {
    ws: WebSocketStream<S>,
}

impl<S> WsTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(ws: WebSocketStream<S>) -> WsTransport<S> {
        WsTransport { ws }
    }
}

#[async_trait::async_trait]
impl<S> MessageTransport for WsTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), FrameError> {
        self.ws.send(text_frame(bytes)?).await.map_err(ws_error)
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FrameError> {
        next_message(&mut self.ws).await
    }

    async fn close(&mut self) -> Result<(), FrameError> {
        self.ws.close(None).await.map_err(ws_error)
    }

    fn split_boxed(self: Box<Self>) -> (BoxedSender, BoxedReceiver) {
        let (tx, rx) = self.ws.split();
        (Box::new(WsSender { tx }), Box::new(WsReceiver { rx }))
    }
}

struct WsSender<S> {
    tx: SplitSink<WebSocketStream<S>, Message>,
}

#[async_trait::async_trait]
impl<S> MessageSender for WsSender<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), FrameError> {
        self.tx.send(text_frame(bytes)?).await.map_err(ws_error)
    }

    async fn close(&mut self) -> Result<(), FrameError> {
        self.tx.close().await.map_err(ws_error)
    }
}

struct WsReceiver<S> {
    rx: SplitStream<WebSocketStream<S>>,
}

#[async_trait::async_trait]
impl<S> MessageReceiver for WsReceiver<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    async fn recv(&mut self) -> Result<Vec<u8>, FrameError> {
        next_message(&mut self.rx).await
    }
}

/// One message, one text frame: no trailing newline, no batching.
fn text_frame(bytes: Vec<u8>) -> Result<Message, FrameError> {
    let text = String::from_utf8(bytes).map_err(|e| FrameError::Ws(e.to_string()))?;
    Ok(Message::Text(text.into()))
}

/// The next data frame, skipping the control frames tungstenite answers for us.
async fn next_message<T>(stream: &mut T) -> Result<Vec<u8>, FrameError>
where
    T: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin + Send,
{
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => return Ok(t.as_bytes().to_vec()),
            Some(Ok(Message::Binary(b))) => return Ok(b.to_vec()),
            Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
            Some(Ok(Message::Close(_))) | None => return Err(FrameError::Closed),
            Some(Err(e)) => return Err(ws_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::MessageConnection;
    use crate::messages::{ClientEnvelope, ClientMessage, ServerEnvelope, ServerMessage};

    /// One message is one text frame, with no framing of its own — and the
    /// halves can be used at the same time.
    #[tokio::test]
    async fn one_message_is_one_frame() {
        let (a, b) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let transport = accept(b).await.unwrap();
            let mut conn: MessageConnection<ClientEnvelope, ServerEnvelope> = MessageConnection::new(transport);
            let got = conn.recv().await.unwrap();
            assert_eq!(got.msg, ClientMessage::Ping);
            conn.send(&ServerEnvelope {
                req: got.req,
                msg: ServerMessage::Pong,
            })
            .await
            .unwrap();
            conn.send(&ServerEnvelope::from(ServerMessage::Pong)).await.unwrap();
            conn.close().await.unwrap();
        });

        let (ws, _) = tokio_tungstenite::client_async("ws://manaline.test/", a).await.unwrap();
        let conn: MessageConnection<ServerEnvelope, ClientEnvelope> = MessageConnection::new(Box::new(WsTransport::new(ws)));
        let (mut reader, mut writer) = conn.split();
        writer
            .send(&ClientEnvelope {
                req: Some(7),
                msg: ClientMessage::Ping,
            })
            .await
            .unwrap();
        let bytes = reader.recv_raw().await.unwrap();
        assert!(!bytes.ends_with(b"\n"), "no trailing newline inside a frame");
        assert_eq!(serde_json::from_slice::<ServerEnvelope>(&bytes).unwrap().req, Some(7));
        assert!(reader.recv().await.unwrap().is_push());
        assert!(matches!(reader.recv().await, Err(FrameError::Closed)));
        server.await.unwrap();
    }
}
