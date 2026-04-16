use crate::TransportError;
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
    link_name: String,
}

impl Connection {
    pub(crate) fn new(transport: LocalTransport, link_name: String) -> Self {
        let (reader, writer) = transport.into_split();
        Self {
            reader: tokio::sync::Mutex::new(reader),
            writer: tokio::sync::Mutex::new(writer),
            link_name,
        }
    }

    /// The link name assigned during handshake.
    pub fn link_name(&self) -> &str {
        &self.link_name
    }

    /// Send a message to the server.
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
