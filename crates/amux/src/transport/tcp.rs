//! TCP transport for server-to-server peering.
//!
//! Generic over the stream type (`TcpStream` or `TlsStream<TcpStream>`) so the
//! same framing logic serves both plain and TLS connections.

use tokio::io::{AsyncRead, AsyncWrite};

use super::framing::{FrameReader, FrameWriter};
use super::{
    LengthPrefixed, MAX_FRAME_SIZE, MessageReader, MessageWriter, Transport, TransportSplit,
};
use crate::protocol::message::Message;
use crate::transport::{Result, TransportError};

/// TCP transport with length-prefixed framing (for server-to-server connections).
///
/// Generic over stream type to support both plain TCP and TLS streams.
pub(crate) struct TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    framed: LengthPrefixed<tokio::io::ReadHalf<S>, tokio::io::WriteHalf<S>>,
}

impl<S> TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    pub(crate) fn new(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            framed: LengthPrefixed::new(reader, writer, true),
        }
    }
}

impl<S> Transport for TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync,
{
    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        self.framed.read_frame(MAX_FRAME_SIZE).await
    }

    async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        self.framed.write_frame(data).await
    }

    async fn read_message(&mut self) -> Result<Message> {
        let data = self.read_frame().await?;
        Message::decode(&data).map_err(TransportError::from)
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(TransportError::from)?;
        self.write_frame(&data).await
    }
}

/// Read half of a split TCP transport
pub(crate) struct TcpMessageReader<S> {
    reader: FrameReader<tokio::io::ReadHalf<S>>,
}

impl<S: AsyncRead + Unpin + Send> MessageReader for TcpMessageReader<S> {
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.reader.read_frame(MAX_FRAME_SIZE).await?;
        Message::decode(&data).map_err(TransportError::from)
    }
}

/// Write half of a split TCP transport
pub(crate) struct TcpMessageWriter<S> {
    writer: FrameWriter<tokio::io::WriteHalf<S>>,
}

impl<S: AsyncWrite + Unpin + Send> MessageWriter for TcpMessageWriter<S> {
    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(TransportError::from)?;
        self.writer.write_frame(&data).await
    }
}

impl<S> TransportSplit for TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    type Reader = TcpMessageReader<S>;
    type Writer = TcpMessageWriter<S>;

    fn into_split(self) -> (Self::Reader, Self::Writer) {
        let (reader, writer) = self.framed.into_split();
        (TcpMessageReader { reader }, TcpMessageWriter { writer })
    }
}

/// Configure TCP keepalive on a stream: 30s idle before first probe, 10s between probes.
pub(crate) fn configure_tcp_keepalive(stream: &tokio::net::TcpStream) {
    use std::time::Duration;

    use socket2::SockRef;

    let sock = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    if let Err(error) = sock.set_tcp_keepalive(&keepalive) {
        tracing::warn!(error = %error, "failed to set TCP keepalive");
    }
}
