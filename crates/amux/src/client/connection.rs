use crate::TransportError;
use crate::protocol::link::Link;
use crate::protocol::message::Message;
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
    reader: tokio::sync::Mutex<LocalMessageReader>,
    writer: tokio::sync::Mutex<LocalMessageWriter>,
    link: Link,
}

impl Connection {
    pub(crate) fn new(transport: LocalTransport, link: Link) -> Self {
        let (reader, writer) = transport.into_split();
        Self {
            reader: tokio::sync::Mutex::new(reader),
            writer: tokio::sync::Mutex::new(writer),
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
        let writer: &mut LocalMessageWriter = &mut *self.writer.lock().await;
        writer.write_message(message).await
    }

    /// Receive a message from the server.
    pub async fn recv(&self) -> std::result::Result<Message, TransportError> {
        let reader: &mut LocalMessageReader = &mut *self.reader.lock().await;
        reader.read_message().await
    }
}
