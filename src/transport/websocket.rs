use super::Transport;
use crate::error::{AmuxError, Result};
use crate::message::Message;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

/// WebSocket transport with JSON serialization
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
                    let _ = self.stream.send(WsMessage::Pong(data)).await;
                }
                Some(Ok(_)) => {
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
