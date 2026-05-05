//! Windows named-pipe transport used for local CLI-to-server communication.

#![cfg(windows)]

use tokio::io::{AsyncRead, AsyncWrite, ReadHalf, WriteHalf, split};
use tokio::net::windows::named_pipe::NamedPipeClient;

use super::framing::{FrameReader, FrameWriter};
use super::{
    LengthPrefixed, MAX_FRAME_SIZE, MessageReader, MessageWriter, Transport, TransportSplit,
};
use crate::protocol::message::Message;
use crate::transport::{Result, TransportError};

type NamedPipeReadHalf<S> = ReadHalf<S>;
type NamedPipeWriteHalf<S> = WriteHalf<S>;

pub(crate) type NamedPipeClientTransport = NamedPipeTransport<NamedPipeClient>;

/// Named-pipe transport with length-prefixed framing.
pub(crate) struct NamedPipeTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    framed: LengthPrefixed<NamedPipeReadHalf<S>, NamedPipeWriteHalf<S>>,
}

impl<S> NamedPipeTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    pub(crate) fn new(stream: S) -> Self {
        let (reader, writer) = split(stream);
        Self {
            framed: LengthPrefixed::new(reader, writer, false),
        }
    }
}

impl<S> Transport for NamedPipeTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
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

pub(crate) struct NamedPipeMessageReader<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    reader: FrameReader<NamedPipeReadHalf<S>>,
}

impl<S> MessageReader for NamedPipeMessageReader<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn read_message(&mut self) -> Result<Message> {
        let data = self.reader.read_frame(MAX_FRAME_SIZE).await?;
        Message::decode(&data).map_err(TransportError::from)
    }
}

pub(crate) struct NamedPipeMessageWriter<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    writer: FrameWriter<NamedPipeWriteHalf<S>>,
}

impl<S> MessageWriter for NamedPipeMessageWriter<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    async fn write_message(&mut self, msg: &Message) -> Result<()> {
        let data = msg.encode().map_err(TransportError::from)?;
        self.writer.write_frame(&data).await
    }
}

impl<S> TransportSplit for NamedPipeTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    type Reader = NamedPipeMessageReader<S>;
    type Writer = NamedPipeMessageWriter<S>;

    fn into_split(self) -> (Self::Reader, Self::Writer) {
        let (reader, writer) = self.framed.into_split();
        (
            NamedPipeMessageReader { reader },
            NamedPipeMessageWriter { writer },
        )
    }
}
