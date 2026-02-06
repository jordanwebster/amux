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

pub(crate) use framing::LengthPrefixed;
