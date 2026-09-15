//! One JSON message in, one JSON message out, over whatever carries bytes.
//!
//! Two framings implement [`MessageTransport`]: newline-delimited JSON over an
//! async byte stream (Unix sockets and TCP), and one WebSocket text frame per
//! message (`crate::ws`). Nothing above this module can tell them apart.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Lines longer than this are treated as a protocol violation.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("connection closed")]
    Closed,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("message exceeds {MAX_LINE_BYTES} bytes")]
    TooLong,
    #[error("websocket error: {0}")]
    Ws(String),
}

/// Serialize one message as a single line.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    Ok(line)
}

pub fn decode<T: DeserializeOwned>(line: &[u8]) -> Result<T, FrameError> {
    Ok(serde_json::from_slice(line)?)
}

/// The receiving half of a framed connection.
pub struct FramedReader<R, In> {
    reader: BufReader<R>,
    buf: Vec<u8>,
    _t: PhantomData<In>,
}

impl<R: AsyncRead + Unpin, In: DeserializeOwned> FramedReader<R, In> {
    pub fn new(reader: R) -> Self {
        FramedReader {
            reader: BufReader::new(reader),
            buf: Vec::with_capacity(4096),
            _t: PhantomData,
        }
    }

    /// The next message, or `Closed` at end of stream. Blank lines are skipped.
    pub async fn recv(&mut self) -> Result<In, FrameError> {
        loop {
            self.buf.clear();
            let n = AsyncReadExt::take(&mut self.reader, MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut self.buf)
                .await?;
            if n == 0 {
                return Err(FrameError::Closed);
            }
            if self.buf.len() > MAX_LINE_BYTES {
                return Err(FrameError::TooLong);
            }
            let line = trim_line(&self.buf);
            if line.is_empty() {
                continue;
            }
            return decode(line);
        }
    }

    /// The next raw line, undecoded. For tests that inspect exactly what went over the wire.
    pub async fn recv_raw(&mut self) -> Result<Vec<u8>, FrameError> {
        loop {
            self.buf.clear();
            let n = AsyncReadExt::take(&mut self.reader, MAX_LINE_BYTES as u64 + 1)
                .read_until(b'\n', &mut self.buf)
                .await?;
            if n == 0 {
                return Err(FrameError::Closed);
            }
            let line = trim_line(&self.buf);
            if line.is_empty() {
                continue;
            }
            return Ok(line.to_vec());
        }
    }
}

/// The sending half of a framed connection.
pub struct FramedWriter<W, Out> {
    writer: W,
    _t: PhantomData<Out>,
}

impl<W: AsyncWrite + Unpin, Out: Serialize> FramedWriter<W, Out> {
    pub fn new(writer: W) -> Self {
        FramedWriter { writer, _t: PhantomData }
    }

    pub async fn send(&mut self, msg: &Out) -> Result<(), FrameError> {
        let line = encode(msg)?;
        self.writer.write_all(&line).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        self.writer.write_all(bytes).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) -> Result<(), FrameError> {
        self.writer.shutdown().await?;
        Ok(())
    }
}

/// A typed, framed connection: sends `Out`, receives `In`.
pub struct Connection<R, W, In, Out> {
    pub reader: FramedReader<R, In>,
    pub writer: FramedWriter<W, Out>,
}

impl<R, W, In, Out> Connection<R, W, In, Out>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    In: DeserializeOwned,
    Out: Serialize,
{
    pub fn new(reader: R, writer: W) -> Self {
        Connection {
            reader: FramedReader::new(reader),
            writer: FramedWriter::new(writer),
        }
    }

    pub async fn send(&mut self, msg: &Out) -> Result<(), FrameError> {
        self.writer.send(msg).await
    }

    pub async fn recv(&mut self) -> Result<In, FrameError> {
        self.reader.recv().await
    }

    pub fn split(self) -> (FramedReader<R, In>, FramedWriter<W, Out>) {
        (self.reader, self.writer)
    }
}

// ---------------------------------------------------------------------------
// Transports: one message in, one message out, whatever the framing.
// ---------------------------------------------------------------------------

/// A bidirectional stream of whole protocol messages. `bytes` is one message's
/// JSON with no framing of its own: the line framing adds the newline, the
/// WebSocket framing puts it in a text frame.
#[async_trait::async_trait]
pub trait MessageTransport: Send {
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), FrameError>;
    async fn recv(&mut self) -> Result<Vec<u8>, FrameError>;
    async fn close(&mut self) -> Result<(), FrameError>;
    /// Halves that can be sent and received on at the same time. A server
    /// connection needs this: it reads requests while pushing events.
    fn split_boxed(self: Box<Self>) -> (BoxedSender, BoxedReceiver);
}

/// The sending half of a [`MessageTransport`].
#[async_trait::async_trait]
pub trait MessageSender: Send {
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), FrameError>;
    async fn close(&mut self) -> Result<(), FrameError>;
}

/// The receiving half of a [`MessageTransport`].
#[async_trait::async_trait]
pub trait MessageReceiver: Send {
    async fn recv(&mut self) -> Result<Vec<u8>, FrameError>;
}

pub type BoxedTransport = Box<dyn MessageTransport>;
pub type BoxedSender = Box<dyn MessageSender>;
pub type BoxedReceiver = Box<dyn MessageReceiver>;

/// Newline-delimited JSON over a byte stream: the Unix-socket and TCP framing.
pub struct LineTransport<R, W> {
    reader: FramedReader<R, ()>,
    writer: FramedWriter<W, ()>,
}

impl<R, W> LineTransport<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    pub fn new(reader: R, writer: W) -> LineTransport<R, W> {
        LineTransport {
            reader: FramedReader::new(reader),
            writer: FramedWriter::new(writer),
        }
    }

    /// The same thing, boxed, ready for [`MessageConnection::new`].
    pub fn boxed(reader: R, writer: W) -> BoxedTransport {
        Box::new(LineTransport::new(reader, writer))
    }
}

#[async_trait::async_trait]
impl<R, W> MessageTransport for LineTransport<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), FrameError> {
        MessageSender::send(&mut self.writer, bytes).await
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FrameError> {
        self.reader.recv_raw().await
    }

    async fn close(&mut self) -> Result<(), FrameError> {
        self.writer.shutdown().await
    }

    fn split_boxed(self: Box<Self>) -> (BoxedSender, BoxedReceiver) {
        let LineTransport { reader, writer } = *self;
        (Box::new(writer), Box::new(reader))
    }
}

#[async_trait::async_trait]
impl<W: AsyncWrite + Unpin + Send> MessageSender for FramedWriter<W, ()> {
    async fn send(&mut self, bytes: Vec<u8>) -> Result<(), FrameError> {
        let mut line = bytes;
        line.push(b'\n');
        self.send_raw(&line).await
    }

    async fn close(&mut self) -> Result<(), FrameError> {
        self.shutdown().await
    }
}

#[async_trait::async_trait]
impl<R: AsyncRead + Unpin + Send> MessageReceiver for FramedReader<R, ()> {
    async fn recv(&mut self) -> Result<Vec<u8>, FrameError> {
        self.recv_raw().await
    }
}

/// A typed connection over any transport: sends `Out`, receives `In`. The
/// transport-agnostic twin of [`Connection`], which is tied to a byte stream.
pub struct MessageConnection<In, Out> {
    transport: BoxedTransport,
    _t: PhantomData<fn() -> (In, Out)>,
}

impl<In, Out> MessageConnection<In, Out>
where
    In: DeserializeOwned,
    Out: Serialize,
{
    pub fn new(transport: BoxedTransport) -> MessageConnection<In, Out> {
        MessageConnection {
            transport,
            _t: PhantomData,
        }
    }

    /// Line framing over a byte stream, for callers that already have halves.
    pub fn over_stream<R, W>(reader: R, writer: W) -> MessageConnection<In, Out>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        MessageConnection::new(LineTransport::boxed(reader, writer))
    }

    pub async fn send(&mut self, msg: &Out) -> Result<(), FrameError> {
        self.transport.send(serde_json::to_vec(msg)?).await
    }

    pub async fn recv(&mut self) -> Result<In, FrameError> {
        decode(&self.transport.recv().await?)
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        self.transport.send(bytes.to_vec()).await
    }

    /// The next message's bytes, undecoded. For tests that inspect exactly what
    /// went over the wire.
    pub async fn recv_raw(&mut self) -> Result<Vec<u8>, FrameError> {
        self.transport.recv().await
    }

    pub async fn close(&mut self) -> Result<(), FrameError> {
        self.transport.close().await
    }

    /// Halves that can be read and written concurrently.
    pub fn split(self) -> (MessageReader<In>, MessageWriter<Out>) {
        let (tx, rx) = self.transport.split_boxed();
        (MessageReader { rx, _t: PhantomData }, MessageWriter { tx, _t: PhantomData })
    }
}

/// The receiving half of a [`MessageConnection`].
pub struct MessageReader<In> {
    rx: BoxedReceiver,
    _t: PhantomData<fn() -> In>,
}

impl<In: DeserializeOwned> MessageReader<In> {
    pub async fn recv(&mut self) -> Result<In, FrameError> {
        decode(&self.rx.recv().await?)
    }

    /// The next message's bytes, undecoded.
    pub async fn recv_raw(&mut self) -> Result<Vec<u8>, FrameError> {
        self.rx.recv().await
    }
}

/// The sending half of a [`MessageConnection`].
pub struct MessageWriter<Out> {
    tx: BoxedSender,
    _t: PhantomData<fn() -> Out>,
}

impl<Out: Serialize> MessageWriter<Out> {
    pub async fn send(&mut self, msg: &Out) -> Result<(), FrameError> {
        self.tx.send(serde_json::to_vec(msg)?).await
    }

    pub async fn send_raw(&mut self, bytes: &[u8]) -> Result<(), FrameError> {
        self.tx.send(bytes.to_vec()).await
    }

    pub async fn shutdown(&mut self) -> Result<(), FrameError> {
        self.tx.close().await
    }
}

fn trim_line(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    while end > 0 && (buf[end - 1] == b'\n' || buf[end - 1] == b'\r' || buf[end - 1] == b' ') {
        end -= 1;
    }
    &buf[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{ClientEnvelope, ClientMessage, ServerEnvelope, ServerMessage};

    #[tokio::test]
    async fn round_trips_over_a_duplex() {
        let (a, b) = tokio::io::duplex(1024);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut client: Connection<_, _, ServerEnvelope, ClientEnvelope> = Connection::new(ar, aw);
        let mut server: Connection<_, _, ClientEnvelope, ServerEnvelope> = Connection::new(br, bw);

        let ping = ClientEnvelope {
            req: Some(1),
            msg: ClientMessage::Ping,
        };
        client.send(&ping).await.unwrap();
        let got = server.recv().await.unwrap();
        assert_eq!(got, ping);
        server
            .send(&ServerEnvelope {
                req: Some(1),
                msg: ServerMessage::Pong,
            })
            .await
            .unwrap();
        // Blank lines between messages are ignored.
        server.writer.send_raw(b"\n\r\n").await.unwrap();
        server.send(&ServerEnvelope::from(ServerMessage::Pong)).await.unwrap();
        assert_eq!(client.recv().await.unwrap().req, Some(1));
        assert!(client.recv().await.unwrap().is_push());
        drop(server);
        assert!(matches!(client.recv().await, Err(FrameError::Closed)));
    }

    #[tokio::test]
    async fn malformed_lines_are_errors_not_panics() {
        let (a, b) = tokio::io::duplex(1024);
        let (ar, aw) = tokio::io::split(a);
        let (_br, mut bw) = tokio::io::split(b);
        let mut client: Connection<_, _, ServerEnvelope, ClientEnvelope> = Connection::new(ar, aw);
        bw.write_all(b"{not json\n").await.unwrap();
        assert!(matches!(client.recv().await, Err(FrameError::Json(_))));
    }
}
