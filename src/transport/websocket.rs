use super::{MessageReader, MessageWriter, Transport, TransportSplit};
use crate::error::{AmuxError, Result};
use crate::message::Message;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
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

type WsSplitSink = futures_util::stream::SplitSink<WebSocketStream<TcpStream>, WsMessage>;
type WsSplitStream = futures_util::stream::SplitStream<WebSocketStream<TcpStream>>;

/// Read half of a split WebSocket transport.
/// Forwards Ping payloads to the writer via a channel since the reader can't write.
pub struct WsMessageReader {
    stream: WsSplitStream,
    pong_tx: mpsc::Sender<Vec<u8>>,
}

#[async_trait]
impl MessageReader for WsMessageReader {
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
                    // Forward pong payload to writer task; drop silently if full
                    let _ = self.pong_tx.try_send(data);
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
}

/// Write half of a split WebSocket transport.
/// Also drains a pong channel to send Pong frames in response to Pings.
pub struct WsMessageWriter {
    sink: WsSplitSink,
    pong_rx: mpsc::Receiver<Vec<u8>>,
}

#[async_trait]
impl MessageWriter for WsMessageWriter {
    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        // Drain any pending pong payloads first
        while let Ok(data) = self.pong_rx.try_recv() {
            let _ = self.sink.send(WsMessage::Pong(data)).await;
        }

        let json = serde_json::to_string(msg)
            .map_err(|e| AmuxError::Config(format!("JSON serialize error: {}", e)))?;
        self.sink
            .send(WsMessage::Text(json))
            .await
            .map_err(|e| AmuxError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    /// Send pong responses during idle periods (no message traffic).
    /// When the reader is gone (pong_rx closed), pends forever to avoid busy-spinning.
    async fn background(&mut self) {
        match self.pong_rx.recv().await {
            Some(data) => {
                let _ = self.sink.send(WsMessage::Pong(data)).await;
            }
            None => {
                // Reader dropped pong_tx — pend forever to stop this select arm firing
                std::future::pending::<()>().await;
            }
        }
    }
}

impl TransportSplit for WebSocketTransport {
    type Reader = WsMessageReader;
    type Writer = WsMessageWriter;

    fn into_split(self) -> (Self::Reader, Self::Writer) {
        let (sink, stream) = self.stream.split();
        let (pong_tx, pong_rx) = mpsc::channel(4);
        (
            WsMessageReader { stream, pong_tx },
            WsMessageWriter { sink, pong_rx },
        )
    }
}
