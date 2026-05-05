//! Unix domain socket transport.
//!
//! Used for local CLI-to-server communication (commands, hook events, terminal attach).

use tokio::net::UnixStream;

use super::framing::{FrameReader, FrameWriter};
use super::{
    LengthPrefixed, MAX_FRAME_SIZE, MessageReader, MessageWriter, Transport, TransportSplit,
};
use crate::protocol::message::Message;
use crate::transport::{Result, TransportError};

/// Unix socket transport with length-prefixed framing
pub(crate) struct UnixTransport {
    framed: LengthPrefixed<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>,
}

impl UnixTransport {
    pub(crate) fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            framed: LengthPrefixed::new(reader, writer, false),
        }
    }
}

impl Transport for UnixTransport {
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

/// Read half of a split Unix transport
pub(crate) struct UnixMessageReader {
    reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
}

impl MessageReader for UnixMessageReader {
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.reader.read_frame(MAX_FRAME_SIZE).await?;
        Message::decode(&data).map_err(TransportError::from)
    }
}

/// Write half of a split Unix transport
pub(crate) struct UnixMessageWriter {
    writer: FrameWriter<tokio::net::unix::OwnedWriteHalf>,
}

impl MessageWriter for UnixMessageWriter {
    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(TransportError::from)?;
        self.writer.write_frame(&data).await
    }
}

impl TransportSplit for UnixTransport {
    type Reader = UnixMessageReader;
    type Writer = UnixMessageWriter;

    fn into_split(self) -> (Self::Reader, Self::Writer) {
        let (reader, writer) = self.framed.into_split();
        (UnixMessageReader { reader }, UnixMessageWriter { writer })
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::protocol::link::Link;
    use crate::protocol::message::{RoutedCallId, RoutedFrame, RoutedFrameMessage};
    use crate::protocol::route::Route;

    async fn create_socket_pair() -> (UnixTransport, UnixTransport) {
        let (client_stream, server_stream) = UnixStream::pair().unwrap();

        (
            UnixTransport::new(client_stream),
            UnixTransport::new(server_stream),
        )
    }

    #[tokio::test]
    async fn test_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let payload = b"hello".to_vec();
        let msg = Message::Routed(RoutedFrame {
            src: Route::from_link(Link::new("term-abc").unwrap()),
            dst: Route::empty(),
            call_id: RoutedCallId::from(Uuid::new_v4()),
            message: RoutedFrameMessage::Payload(payload.clone()),
        });

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Routed(RoutedFrame {
            message: RoutedFrameMessage::Payload(decoded),
            ..
        }) = received
        {
            assert_eq!(decoded, payload);
        } else {
            panic!("Expected routed payload");
        }
    }

    #[tokio::test]
    async fn test_output_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let payload = b"hello world".to_vec();
        let msg = Message::Routed(RoutedFrame {
            src: Route::from_link(Link::new("host-a").unwrap()),
            dst: Route::from_link(Link::new("host-b").unwrap()),
            call_id: RoutedCallId::from(Uuid::new_v4()),
            message: RoutedFrameMessage::Payload(payload.clone()),
        });

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Routed(RoutedFrame {
            message: RoutedFrameMessage::Payload(decoded),
            ..
        }) = received
        {
            assert_eq!(decoded, payload);
        } else {
            panic!("Expected routed payload");
        }
    }

    #[tokio::test]
    async fn test_input_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let payload = b"user input".to_vec();
        let msg = Message::Routed(RoutedFrame {
            src: Route::from_link(Link::new("host-a").unwrap()),
            dst: Route::from_link(Link::new("host-b").unwrap()),
            call_id: RoutedCallId::from(Uuid::new_v4()),
            message: RoutedFrameMessage::Payload(payload.clone()),
        });

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Routed(RoutedFrame {
            message: RoutedFrameMessage::Payload(decoded),
            ..
        }) = received
        {
            assert_eq!(decoded, payload);
        } else {
            panic!("Expected routed payload");
        }
    }
}
