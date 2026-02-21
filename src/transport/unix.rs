use super::framing::{FrameReader, FrameWriter};
use super::{
    LengthPrefixed, MAX_FRAME_SIZE, MessageReader, MessageWriter, Transport, TransportSplit,
};
use crate::error::{AmuxError, Result};
use crate::message::Message;
use async_trait::async_trait;
use tokio::net::UnixStream;

/// Unix socket transport with length-prefixed framing
pub struct UnixTransport {
    framed: LengthPrefixed<tokio::net::unix::OwnedReadHalf, tokio::net::unix::OwnedWriteHalf>,
}

impl UnixTransport {
    /// Create a new transport from a Unix stream
    pub fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            framed: LengthPrefixed::new(reader, writer, false),
        }
    }
}

#[async_trait]
impl Transport for UnixTransport {
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.framed.read_frame(MAX_FRAME_SIZE).await?;
        Message::decode(&data).map_err(AmuxError::SerializationDecode)
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(AmuxError::SerializationEncode)?;
        self.framed.write_frame(&data).await
    }
}

/// Read half of a split Unix transport
pub struct UnixMessageReader {
    reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
}

#[async_trait]
impl MessageReader for UnixMessageReader {
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.reader.read_frame(MAX_FRAME_SIZE).await?;
        Message::decode(&data).map_err(AmuxError::SerializationDecode)
    }
}

/// Write half of a split Unix transport
pub struct UnixMessageWriter {
    writer: FrameWriter<tokio::net::unix::OwnedWriteHalf>,
}

#[async_trait]
impl MessageWriter for UnixMessageWriter {
    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(AmuxError::SerializationEncode)?;
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
    use super::*;
    use crate::message::{AgentType, CreateAgentRequest, RoutableMessage, TerminalSize};
    use crate::route::Route;
    use tokio::net::UnixListener;
    use uuid::Uuid;

    async fn create_socket_pair() -> (UnixTransport, UnixTransport) {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();

        let client_future = UnixStream::connect(&socket_path);
        let server_future = listener.accept();

        let (client_result, server_result) = tokio::join!(client_future, server_future);
        let client_stream = client_result.unwrap();
        let (server_stream, _) = server_result.unwrap();

        (
            UnixTransport::new(client_stream),
            UnixTransport::new(server_stream),
        )
    }

    #[tokio::test]
    async fn test_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let test_uuid = Uuid::new_v4();
        let msg = Message::Routable {
            src: Route::from_link("term-abc"),
            dst: Route::empty(),
            message: RoutableMessage::CreateAgent(CreateAgentRequest {
                agent_id: test_uuid,
                name: Some("test".to_string()),
                agent_type: AgentType::Claude,
                working_dir: std::path::PathBuf::from("/tmp"),
                terminal_size: Some(TerminalSize { rows: 24, cols: 80 }),
            }),
        };

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Routable {
            message: RoutableMessage::CreateAgent(req),
            ..
        } = received
        {
            assert_eq!(req.agent_id, test_uuid);
            assert_eq!(req.name, Some("test".to_string()));
            assert_eq!(req.agent_type, AgentType::Claude);
            assert_eq!(req.working_dir, std::path::PathBuf::from("/tmp"));
            assert_eq!(req.terminal_size, Some(TerminalSize { rows: 24, cols: 80 }));
        } else {
            panic!("Expected CreateAgent");
        }
    }

    #[tokio::test]
    async fn test_output_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let msg = Message::Routable {
            src: Route::from_link("host-a"),
            dst: Route::from_link("host-b"),
            message: RoutableMessage::RawOutput {
                agent_id: Uuid::new_v4(),
                data: b"hello world".to_vec(),
            },
        };

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Routable {
            message: RoutableMessage::RawOutput { data, .. },
            ..
        } = received
        {
            assert_eq!(data, b"hello world");
        } else {
            panic!("Expected Output");
        }
    }

    #[tokio::test]
    async fn test_input_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let msg = Message::Routable {
            src: Route::from_link("host-a"),
            dst: Route::from_link("host-b"),
            message: RoutableMessage::RawInput {
                agent_id: Uuid::new_v4(),
                data: b"user input".to_vec(),
            },
        };

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Routable {
            message: RoutableMessage::RawInput { data, .. },
            ..
        } = received
        {
            assert_eq!(data, b"user input");
        } else {
            panic!("Expected InputBytes");
        }
    }
}
