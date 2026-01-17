//! Transport layer for amux protocol.
//!
//! Unix and TCP transports use length-prefixed framing:
//! - 4-byte big-endian length prefix
//! - Followed by payload bytes
//!
//! WebSocket transport uses JSON-encoded messages.

use crate::error::{AmuxError, Result};
use crate::message::Message;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf as TcpReadHalf, OwnedWriteHalf as TcpWriteHalf};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpStream, UnixStream};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

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

/// Unix socket transport with length-prefixed framing
pub struct UnixTransport {
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
}

impl UnixTransport {
    /// Create a new transport from a Unix stream
    pub fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self { reader, writer }
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > MAX_FRAME_SIZE {
            return Err(AmuxError::InvalidMessage);
        }

        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).await?;

        Ok(buf)
    }

    async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        let len = data.len() as u32;
        self.writer.write_all(&len.to_be_bytes()).await?;
        self.writer.write_all(data).await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for UnixTransport {
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.read_frame().await?;
        Message::decode(&data).map_err(AmuxError::Serialization)
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(AmuxError::Serialization)?;
        self.write_frame(&data).await
    }
}

/// TCP transport with length-prefixed framing (for server-to-server connections)
pub struct TcpTransport {
    reader: TcpReadHalf,
    writer: TcpWriteHalf,
}

impl TcpTransport {
    /// Create a new transport from a TCP stream
    pub fn new(stream: TcpStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self { reader, writer }
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > MAX_FRAME_SIZE {
            return Err(AmuxError::InvalidMessage);
        }

        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).await?;

        Ok(buf)
    }

    async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        let len = data.len() as u32;
        self.writer.write_all(&len.to_be_bytes()).await?;
        self.writer.write_all(data).await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for TcpTransport {
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.read_frame().await?;
        Message::decode(&data).map_err(AmuxError::Serialization)
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(AmuxError::Serialization)?;
        self.write_frame(&data).await
    }
}

/// WebSocket transport with JSON serialization (for rich clients)
pub struct WebSocketTransport {
    stream: WebSocketStream<TcpStream>,
}

impl WebSocketTransport {
    /// Create a new transport from a WebSocket stream
    pub fn new(stream: WebSocketStream<TcpStream>) -> Self {
        Self { stream }
    }
}

#[async_trait]
impl Transport for WebSocketTransport {
    async fn read_message(&mut self) -> Result<Message> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    return serde_json::from_str(&text)
                        .map_err(|e| AmuxError::Config(format!("JSON parse error: {}", e)));
                }
                Some(Ok(WsMessage::Close(_))) | None => {
                    return Err(AmuxError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "WebSocket closed",
                    )));
                }
                Some(Ok(WsMessage::Ping(data))) => {
                    // Respond to ping with pong
                    let _ = self.stream.send(WsMessage::Pong(data)).await;
                }
                Some(Ok(_)) => {
                    // Ignore other message types (binary, pong, frame)
                    continue;
                }
                Some(Err(e)) => {
                    return Err(AmuxError::Io(std::io::Error::other(e.to_string())));
                }
            }
        }
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let json = serde_json::to_string(msg)
            .map_err(|e| AmuxError::Config(format!("JSON serialize error: {}", e)))?;
        self.stream
            .send(WsMessage::Text(json))
            .await
            .map_err(|e| AmuxError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

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
        use crate::message::{AgentType, CreateAgentRequest};
        use uuid::Uuid;

        let (mut client, mut server) = create_socket_pair().await;

        let test_uuid = Uuid::new_v4();
        let msg = Message::CreateAgent(CreateAgentRequest {
            agent_id: test_uuid,
            alias: Some("test".to_string()),
            agent_type: AgentType::Claude,
            working_dir: std::path::PathBuf::from("/tmp"),
            rows: 24,
            cols: 80,
        });

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::CreateAgent(req) = received {
            assert_eq!(req.agent_id, test_uuid);
            assert_eq!(req.alias, Some("test".to_string()));
            assert_eq!(req.agent_type, AgentType::Claude);
            assert_eq!(req.working_dir, std::path::PathBuf::from("/tmp"));
            assert_eq!(req.rows, 24);
            assert_eq!(req.cols, 80);
        } else {
            panic!("Expected CreateAgent");
        }
    }

    #[tokio::test]
    async fn test_output_message_roundtrip() {
        use uuid::Uuid;

        let (mut client, mut server) = create_socket_pair().await;

        let msg = Message::Output {
            src_host: "host-a".to_string(),
            dst_host: "host-b".to_string(),
            agent_id: Uuid::new_v4().to_string(),
            data: b"hello world".to_vec(),
        };

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Output { data, .. } = received {
            assert_eq!(data, b"hello world");
        } else {
            panic!("Expected Output");
        }
    }

    #[tokio::test]
    async fn test_input_message_roundtrip() {
        use uuid::Uuid;

        let (mut client, mut server) = create_socket_pair().await;

        let msg = Message::Input {
            src_host: "host-a".to_string(),
            dst_host: "host-b".to_string(),
            agent_id: Uuid::new_v4().to_string(),
            data: b"user input".to_vec(),
        };

        client.write_message(&msg).await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::Input { data, .. } = received {
            assert_eq!(data, b"user input");
        } else {
            panic!("Expected Input");
        }
    }
}
