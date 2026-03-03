//! Transport layer for amux protocol.
//!
//! Unix and TCP transports use length-prefixed framing:
//! - 4-byte big-endian length prefix
//! - Followed by payload bytes
//!
//! WebSocket transport uses binary MessagePack frames.

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
use std::future::Future;

/// 16MB limit to prevent DoS via huge length prefix
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Transport trait for reading and writing messages
pub trait Transport: Send + Sync {
    /// Read one raw frame from the transport (length-prefixed or WebSocket binary frame).
    fn read_frame(&mut self) -> impl Future<Output = Result<Vec<u8>>> + Send;

    /// Write one raw frame to the transport.
    fn write_frame(&mut self, data: &[u8]) -> impl Future<Output = Result<()>> + Send;

    /// Read and decode a Message from the transport
    fn read_message(&mut self) -> impl Future<Output = Result<Message>> + Send;

    /// Encode and write a Message to the transport
    fn write_message(&mut self, msg: &Message) -> impl Future<Output = Result<()>> + Send;
}

/// Read half of a split transport
pub trait MessageReader: Send {
    fn read_message(&mut self) -> impl Future<Output = Result<Message>> + Send;
}

/// Write half of a split transport
pub trait MessageWriter: Send {
    fn write_message(&mut self, msg: &Message) -> impl Future<Output = Result<()>> + Send;

    /// Perform background I/O (e.g., WebSocket pong responses).
    /// Called in select! alongside message writes. Default pends forever (no-op).
    fn background(&mut self) -> impl Future<Output = ()> + Send {
        std::future::pending()
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
