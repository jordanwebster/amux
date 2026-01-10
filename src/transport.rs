use crate::error::{AmuxError, Result};
use crate::message::Message;
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

/// Maximum frame size (16MB) to prevent DoS via huge length prefix
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Transport trait for reading and writing messages
#[async_trait]
pub trait Transport: Send {
    /// Read the next framed message
    async fn read_frame(&mut self) -> Result<Vec<u8>>;

    /// Write a framed message
    async fn write_frame(&mut self, data: &[u8]) -> Result<()>;

    /// Read raw bytes (no framing) - for post-subscribe streaming
    async fn read_raw(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Write raw bytes (no framing) - for post-subscribe streaming
    async fn write_raw(&mut self, data: &[u8]) -> Result<()>;

    /// Flush any buffered data
    async fn flush(&mut self) -> Result<()>;
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

    /// Read and decode a Message from the transport
    pub async fn read_message(&mut self) -> Result<Message> {
        let data = self.read_frame().await?;
        Message::decode(&data).map_err(AmuxError::Serialization)
    }

    /// Encode and write a Message to the transport
    pub async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(AmuxError::Serialization)?;
        self.write_frame(&data).await
    }

    /// Get the reader half for raw streaming
    pub fn into_reader(self) -> OwnedReadHalf {
        self.reader
    }

    /// Get the writer half for raw streaming
    pub fn into_writer(self) -> OwnedWriteHalf {
        self.writer
    }

    /// Split into reader and writer halves
    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        (self.reader, self.writer)
    }
}

#[async_trait]
impl Transport for UnixTransport {
    /// Read a length-prefixed frame
    ///
    /// Frame format: 4-byte big-endian length + payload
    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        // Read length prefix
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        // Validate length
        if len > MAX_FRAME_SIZE {
            return Err(AmuxError::InvalidMessage);
        }

        // Read payload
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).await?;

        Ok(buf)
    }

    /// Write a length-prefixed frame
    ///
    /// Frame format: 4-byte big-endian length + payload
    async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        let len = data.len() as u32;
        self.writer.write_all(&len.to_be_bytes()).await?;
        self.writer.write_all(data).await?;
        Ok(())
    }

    /// Read raw bytes without framing
    async fn read_raw(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self.reader.read(buf).await?;
        if n == 0 {
            return Err(AmuxError::ConnectionClosed);
        }
        Ok(n)
    }

    /// Write raw bytes without framing
    async fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data).await?;
        Ok(())
    }

    /// Flush buffered data
    async fn flush(&mut self) -> Result<()> {
        self.writer.flush().await?;
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
    async fn test_frame_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let data = b"hello world";
        client.write_frame(data).await.unwrap();
        client.flush().await.unwrap();

        let received = server.read_frame().await.unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_message_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let msg = Message::CreateAgent {
            agent_id: "test".to_string(),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/tmp"),
            rows: 24,
            cols: 80,
        };

        client.write_message(&msg).await.unwrap();
        client.flush().await.unwrap();

        let received = server.read_message().await.unwrap();
        if let Message::CreateAgent {
            agent_id,
            command,
            working_dir,
            rows,
            cols,
        } = received
        {
            assert_eq!(agent_id, "test");
            assert_eq!(command, "claude");
            assert_eq!(working_dir, std::path::PathBuf::from("/tmp"));
            assert_eq!(rows, 24);
            assert_eq!(cols, 80);
        } else {
            panic!("Expected CreateAgent");
        }
    }

    #[tokio::test]
    async fn test_raw_roundtrip() {
        let (mut client, mut server) = create_socket_pair().await;

        let data = b"raw bytes";
        client.write_raw(data).await.unwrap();
        client.flush().await.unwrap();

        let mut buf = [0u8; 1024];
        let n = server.read_raw(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], data);
    }

    #[tokio::test]
    async fn test_empty_frame() {
        let (mut client, mut server) = create_socket_pair().await;

        let data: &[u8] = b"";
        client.write_frame(data).await.unwrap();
        client.flush().await.unwrap();

        let received = server.read_frame().await.unwrap();
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn test_frame_length_prefix() {
        let (client, mut server) = create_socket_pair().await;

        // Write a frame manually to verify format
        let data = b"test";
        let len = (data.len() as u32).to_be_bytes();

        // Write using raw to bypass framing
        let (_, mut writer) = client.into_split();
        writer.write_all(&len).await.unwrap();
        writer.write_all(data).await.unwrap();
        writer.flush().await.unwrap();

        // Read using framing
        let received = server.read_frame().await.unwrap();
        assert_eq!(received, data);
    }
}
