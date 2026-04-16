//! Transport layer for amux protocol.
//!
//! Unix and TCP transports use length-prefixed framing:
//! - 4-byte big-endian length prefix
//! - Followed by payload bytes
//!
//! WebSocket transport uses binary MessagePack frames.

mod framing;
mod handshake;
mod local;
#[cfg(windows)]
mod named_pipe;
mod tcp;
mod tls;
#[cfg(unix)]
mod unix;
mod websocket;

use std::future::Future;

pub(crate) use handshake::{HandshakeError, connect_handshake};
pub(crate) use local::{LocalListener, LocalMessageReader, LocalMessageWriter, LocalTransport};
pub(crate) use tcp::TcpTransport;
use thiserror::Error;
pub(crate) use tls::{create_tls_acceptor, tls_connect};
pub(crate) use websocket::WebSocketTransport;

use crate::protocol::message::Message;

pub(crate) type Result<T> = std::result::Result<T, TransportError>;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    SerializationEncode(#[from] rmp_serde::encode::Error),
    #[error("Deserialization error: {0}")]
    SerializationDecode(#[from] rmp_serde::decode::Error),
    #[error("Invalid message: {0}")]
    InvalidMessage(String),
    #[error("Transport config error: {0}")]
    Config(String),
}

/// Transport trait for reading and writing messages
pub(crate) trait Transport: Send + Sync {
    /// Read one raw frame from the transport (length-prefixed or WebSocket binary frame).
    fn read_frame(&mut self) -> impl Future<Output = Result<Vec<u8>>> + Send;

    /// Write one raw frame to the transport.
    fn write_frame(&mut self, data: &[u8]) -> impl Future<Output = Result<()>> + Send;

    /// Read and decode a Message from the transport
    #[allow(dead_code)]
    fn read_message(&mut self) -> impl Future<Output = Result<Message>> + Send;

    /// Encode and write a Message to the transport
    #[allow(dead_code)]
    fn write_message(&mut self, msg: &Message) -> impl Future<Output = Result<()>> + Send;
}

/// Read half of a split transport
pub(crate) trait MessageReader: Send {
    fn read_message(&mut self) -> impl Future<Output = Result<Message>> + Send;
}

/// Write half of a split transport
pub(crate) trait MessageWriter: Send {
    fn write_message(&mut self, msg: &Message) -> impl Future<Output = Result<()>> + Send;

    /// Perform background I/O (e.g., WebSocket pong responses).
    /// Called in select! alongside message writes. Default pends forever (no-op).
    fn background(&mut self) -> impl Future<Output = ()> + Send {
        std::future::pending()
    }
}

/// A transport that can be split into independent reader/writer halves.
/// The reader can live in a dedicated task that is never cancelled by select!.
pub(crate) trait TransportSplit: Transport {
    type Reader: MessageReader + 'static;
    type Writer: MessageWriter + 'static;
    fn into_split(self) -> (Self::Reader, Self::Writer);
}

pub(in crate::transport) use framing::LengthPrefixed;
pub(crate) use framing::MAX_FRAME_SIZE;
pub(crate) use tcp::configure_tcp_keepalive;
