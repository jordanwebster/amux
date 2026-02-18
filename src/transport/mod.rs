//! Transport layer for amux protocol.
//!
//! Unix and TCP transports use length-prefixed framing:
//! - 4-byte big-endian length prefix
//! - Followed by payload bytes
//!
//! WebSocket transport uses JSON-encoded messages.

mod framing;
mod tcp;
mod tls;
mod unix;
mod websocket;

pub use tcp::TcpTransport;
pub use tls::{create_tls_acceptor, tls_connect};
pub use unix::UnixTransport;
pub use websocket::WebSocketTransport;

use crate::error::Result;
use crate::message::Message;
use async_trait::async_trait;

/// 16MB limit to prevent DoS via huge length prefix
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Transport trait for reading and writing messages
#[async_trait]
pub trait Transport: Send + Sync {
    /// Read and decode a Message from the transport
    async fn read_message(&mut self) -> Result<Message>;

    /// Encode and write a Message to the transport
    async fn write_message(&mut self, msg: &Message) -> Result<()>;
}

/// Read half of a split transport
#[async_trait]
pub trait MessageReader: Send {
    async fn read_message(&mut self) -> Result<Message>;
}

/// Write half of a split transport
#[async_trait]
pub trait MessageWriter: Send {
    async fn write_message(&mut self, msg: &Message) -> Result<()>;

    /// Perform background I/O (e.g., WebSocket pong responses).
    /// Called in select! alongside message writes. Default pends forever (no-op).
    async fn background(&mut self) {
        std::future::pending().await
    }
}

/// A transport that can be split into independent reader/writer halves.
/// The reader can live in a dedicated task that is never cancelled by select!.
pub trait TransportSplit: Transport {
    type Reader: MessageReader + 'static;
    type Writer: MessageWriter + 'static;
    fn into_split(self) -> (Self::Reader, Self::Writer);
}

pub(crate) use framing::LengthPrefixed;

/// Configure TCP keepalive on a stream: 30s idle before first probe, 10s between probes.
pub(crate) fn configure_tcp_keepalive(stream: &tokio::net::TcpStream) {
    use socket2::SockRef;
    use std::time::Duration;

    let sock = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(30))
        .with_interval(Duration::from_secs(10));
    if let Err(e) = sock.set_tcp_keepalive(&keepalive) {
        tracing::warn!(error = %e, "failed to set TCP keepalive");
    }
}
