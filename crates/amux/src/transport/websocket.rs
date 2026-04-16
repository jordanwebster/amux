//! WebSocket transport with binary MessagePack frames.
//!
//! Used for browser-based clients and cloud relay connections. Unlike the
//! byte-stream transports, WebSocket provides its own message framing so
//! no length prefix is needed. Ping/Pong handling is split: the reader
//! forwards ping payloads to the writer via a channel.

use super::{MessageReader, MessageWriter, Transport, TransportSplit};
use crate::protocol::message::Message;
use crate::transport::{Result, TransportError};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

/// WebSocket transport with binary MessagePack serialization
pub(crate) struct WebSocketTransport {
    stream: WebSocketStream<TcpStream>,
}

impl WebSocketTransport {
    pub(crate) fn new(stream: WebSocketStream<TcpStream>) -> Self {
        Self { stream }
    }
}

impl Transport for WebSocketTransport {
    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Binary(data))) => return Ok(data.to_vec()),
                Some(Ok(WsMessage::Close(_))) | None => {
                    return Err(TransportError::Io(std::io::Error::new(
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
                    return Err(TransportError::Io(std::io::Error::other(e.to_string())));
                }
            }
        }
    }

    async fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        self.stream
            .send(WsMessage::Binary(data.to_vec()))
            .await
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<Message> {
        let data = self.read_frame().await?;
        Message::decode(&data).map_err(TransportError::SerializationDecode)
    }

    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(TransportError::SerializationEncode)?;
        self.write_frame(&data).await
    }
}

type WsSplitSink = futures_util::stream::SplitSink<WebSocketStream<TcpStream>, WsMessage>;
type WsSplitStream = futures_util::stream::SplitStream<WebSocketStream<TcpStream>>;

/// Read half of a split WebSocket transport.
/// Forwards Ping payloads to the writer via a channel since the reader can't write.
pub(crate) struct WsMessageReader {
    stream: WsSplitStream,
    pong_tx: mpsc::Sender<Vec<u8>>,
}

impl MessageReader for WsMessageReader {
    async fn read_message(&mut self) -> Result<Message> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Binary(data))) => {
                    return Message::decode(&data).map_err(TransportError::SerializationDecode);
                }
                Some(Ok(WsMessage::Close(_))) | None => {
                    return Err(TransportError::Io(std::io::Error::new(
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
                    return Err(TransportError::Io(std::io::Error::other(e.to_string())));
                }
            }
        }
    }
}

/// Write half of a split WebSocket transport.
/// Also drains a pong channel to send Pong frames in response to Pings.
pub(crate) struct WsMessageWriter {
    sink: WsSplitSink,
    pong_rx: mpsc::Receiver<Vec<u8>>,
}

impl MessageWriter for WsMessageWriter {
    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        // Drain any pending pong payloads first
        while let Ok(data) = self.pong_rx.try_recv() {
            let _ = self.sink.send(WsMessage::Pong(data)).await;
        }

        let data = msg.encode().map_err(TransportError::SerializationEncode)?;
        self.sink
            .send(WsMessage::Binary(data))
            .await
            .map_err(|e| TransportError::Io(std::io::Error::other(e.to_string())))?;
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
