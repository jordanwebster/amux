use super::{LengthPrefixed, Transport, MAX_FRAME_SIZE};
use crate::error::{AmuxError, Result};
use crate::message::Message;
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

/// TCP transport with length-prefixed framing (for server-to-server connections).
///
/// Generic over stream type to support both plain TCP and TLS streams.
pub struct TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    framed: LengthPrefixed<tokio::io::ReadHalf<S>, tokio::io::WriteHalf<S>>,
}

impl<S> TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    pub fn new(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            framed: LengthPrefixed::new(reader, writer, true),
        }
    }
}

#[async_trait]
impl<S> Transport for TcpTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync,
{
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.framed.read_frame(MAX_FRAME_SIZE).await?;
        Message::decode(&data).map_err(AmuxError::SerializationDecode)
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(AmuxError::SerializationEncode)?;
        self.framed.write_frame(&data).await
    }
}
