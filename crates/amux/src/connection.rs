use crate::error::Result;
use crate::message::Message;
use crate::transport::unix::{UnixMessageReader, UnixMessageWriter};
use crate::transport::{MessageReader, MessageWriter, TransportSplit, UnixTransport};

/// A connection to an amux server.
///
/// Wraps a split Unix transport behind `tokio::Mutex` so that `send` and `recv`
/// both take `&self`. Consumers can use them in `select!` or across tasks
/// without needing to split the connection.
pub struct Connection {
    reader: tokio::sync::Mutex<UnixMessageReader>,
    writer: tokio::sync::Mutex<UnixMessageWriter>,
    link_name: String,
}

impl Connection {
    pub(crate) fn new(transport: UnixTransport, link_name: String) -> Self {
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
    pub async fn send(&self, message: &Message) -> Result<()> {
        let writer: &mut UnixMessageWriter = &mut *self.writer.lock().await;
        writer.write_message(message).await
    }

    /// Receive a message from the server.
    pub async fn recv(&self) -> Result<Message> {
        let reader: &mut UnixMessageReader = &mut *self.reader.lock().await;
        reader.read_message().await
    }
}
