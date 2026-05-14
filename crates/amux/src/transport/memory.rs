//! Test-only in-memory transport.
//!
//! This transport is intentionally below the protocol level: callers can write
//! raw frames to exercise decode failures, while message helpers still use the
//! real protobuf `Message` encoder/decoder.

use tokio::sync::mpsc;

use super::{MessageReader, MessageWriter, Transport, TransportError, TransportSplit};
use crate::protocol::message::Message;

pub(crate) struct MemoryTransport {
    rx: mpsc::Receiver<Vec<u8>>,
    tx: mpsc::Sender<Vec<u8>>,
}

pub(crate) struct MemoryMessageReader {
    rx: mpsc::Receiver<Vec<u8>>,
}

pub(crate) struct MemoryMessageWriter {
    tx: mpsc::Sender<Vec<u8>>,
}

pub(crate) fn pair(buffer: usize) -> (MemoryTransport, MemoryTransport) {
    let (a_tx, a_rx) = mpsc::channel(buffer);
    let (b_tx, b_rx) = mpsc::channel(buffer);

    (
        MemoryTransport { rx: a_rx, tx: b_tx },
        MemoryTransport { rx: b_rx, tx: a_tx },
    )
}

fn closed() -> TransportError {
    TransportError::Io(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "in-memory transport closed",
    ))
}

fn send_error() -> TransportError {
    TransportError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "in-memory transport peer closed",
    ))
}

impl Transport for MemoryTransport {
    async fn read_frame(&mut self) -> super::Result<Vec<u8>> {
        self.rx.recv().await.ok_or_else(closed)
    }

    async fn write_frame(&mut self, data: &[u8]) -> super::Result<()> {
        self.tx.send(data.to_vec()).await.map_err(|_| send_error())
    }

    async fn read_message(&mut self) -> super::Result<Message> {
        let data = self.read_frame().await?;
        Message::decode(&data).map_err(TransportError::from)
    }

    async fn write_message(&mut self, msg: &Message) -> super::Result<()> {
        let data = msg.encode().map_err(TransportError::from)?;
        self.write_frame(&data).await
    }
}

impl MessageReader for MemoryMessageReader {
    async fn read_message(&mut self) -> super::Result<Message> {
        let data = self.rx.recv().await.ok_or_else(closed)?;
        Message::decode(&data).map_err(TransportError::from)
    }
}

impl MessageWriter for MemoryMessageWriter {
    async fn write_message(&mut self, msg: &Message) -> super::Result<()> {
        let data = msg.encode().map_err(TransportError::from)?;
        self.tx.send(data).await.map_err(|_| send_error())
    }
}

impl TransportSplit for MemoryTransport {
    type Reader = MemoryMessageReader;
    type Writer = MemoryMessageWriter;

    fn into_split(self) -> (Self::Reader, Self::Writer) {
        (
            MemoryMessageReader { rx: self.rx },
            MemoryMessageWriter { tx: self.tx },
        )
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::protocol::link::Link;
    use crate::protocol::message::{CallId, Frame, FrameBody};
    use crate::protocol::route::Route;

    #[tokio::test]
    async fn message_roundtrips_through_memory_pair() {
        let (mut a, mut b) = pair(8);
        let msg = Message::Frame(Frame {
            src: Route::from_link(Link::new("a").unwrap()),
            dst: Route::from_link(Link::new("b").unwrap()),
            call_id: CallId::from(Uuid::new_v4()),
            body: FrameBody::StreamItem(b"opaque".to_vec()),
        });

        a.write_message(&msg).await.unwrap();

        assert_eq!(b.read_message().await.unwrap(), msg);
    }

    #[tokio::test]
    async fn raw_non_protobuf_frame_reports_decode_error() {
        let (mut a, mut b) = pair(8);

        a.write_frame(b"not protobuf").await.unwrap();

        let error = b.read_message().await.unwrap_err();
        assert!(matches!(error, TransportError::ProtocolDecode(_)));
    }
}
