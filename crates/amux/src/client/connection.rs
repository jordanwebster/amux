use std::sync::Arc;

use crate::TransportError;
use crate::protocol::link::Link;
use crate::protocol::message::Message;
#[cfg(test)]
use crate::transport::memory::{MemoryMessageReader, MemoryMessageWriter, MemoryTransport};
use crate::transport::{
    LocalMessageReader, LocalMessageWriter, LocalTransport, MessageReader, MessageWriter,
    TransportSplit,
};

/// A connection to an amux server.
///
/// Wraps a split local transport behind `tokio::Mutex` so that `send` and `recv`
/// both take `&self`. Consumers can use them in `select!` or across tasks
/// without needing to split the connection.
pub struct Connection {
    reader: Arc<tokio::sync::Mutex<ConnectionReader>>,
    writer: Arc<tokio::sync::Mutex<ConnectionWriter>>,
    link: Link,
}

enum ConnectionReader {
    Local(LocalMessageReader),
    #[cfg(test)]
    Memory(MemoryMessageReader),
}

enum ConnectionWriter {
    Local(LocalMessageWriter),
    #[cfg(test)]
    Memory(MemoryMessageWriter),
}

impl Connection {
    pub(crate) fn new(transport: LocalTransport, link: Link) -> Self {
        let (reader, writer) = transport.into_split();
        Self {
            reader: Arc::new(tokio::sync::Mutex::new(ConnectionReader::Local(reader))),
            writer: Arc::new(tokio::sync::Mutex::new(ConnectionWriter::Local(writer))),
            link,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_memory(transport: MemoryTransport, link: Link) -> Self {
        let (reader, writer) = transport.into_split();
        Self {
            reader: Arc::new(tokio::sync::Mutex::new(ConnectionReader::Memory(reader))),
            writer: Arc::new(tokio::sync::Mutex::new(ConnectionWriter::Memory(writer))),
            link,
        }
    }

    /// The link name assigned during handshake.
    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Send a message to the server.
    ///
    /// Holds the writer lock across the write to preserve frame-level atomicity —
    /// concurrent callers are serialized so length-prefixed frames can't interleave.
    pub async fn send(&self, message: &Message) -> std::result::Result<(), TransportError> {
        let writer: &mut ConnectionWriter = &mut *self.writer.lock().await;
        writer.write_message(message).await
    }

    /// Receive a message from the server.
    pub async fn recv(&self) -> std::result::Result<Message, TransportError> {
        let reader: &mut ConnectionReader = &mut *self.reader.lock().await;
        reader.read_message().await
    }

    pub(crate) fn clone_for_reader(&self) -> Self {
        Self {
            reader: self.reader.clone(),
            writer: self.writer.clone(),
            link: self.link.clone(),
        }
    }
}

impl MessageReader for ConnectionReader {
    async fn read_message(&mut self) -> std::result::Result<Message, TransportError> {
        match self {
            Self::Local(reader) => reader.read_message().await,
            #[cfg(test)]
            Self::Memory(reader) => reader.read_message().await,
        }
    }
}

impl MessageWriter for ConnectionWriter {
    async fn write_message(&mut self, msg: &Message) -> std::result::Result<(), TransportError> {
        match self {
            Self::Local(writer) => writer.write_message(msg).await,
            #[cfg(test)]
            Self::Memory(writer) => writer.write_message(msg).await,
        }
    }

    async fn background(&mut self) {
        match self {
            Self::Local(writer) => writer.background().await,
            #[cfg(test)]
            Self::Memory(writer) => writer.background().await,
        }
    }
}
