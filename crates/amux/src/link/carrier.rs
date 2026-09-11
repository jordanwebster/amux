use std::future::Future;
use std::io;
use std::pin::Pin;

use futures_util::future::BoxFuture;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::wire::{self, pb};

/// A bidirectional byte stream carried by a link.
pub(crate) trait AsyncStream: AsyncRead + AsyncWrite + Send + Unpin {
    /// Gracefully closes the writing side of the stream.
    fn finish(&mut self) -> BoxFuture<'_, io::Result<()>>;

    /// Rejects an unopened stream, preserving the application refusal reason.
    fn reset(&mut self, code: pb::StreamRefusal) -> BoxFuture<'_, io::Result<()>>;
}

pub(crate) type ByteStream = Box<dyn AsyncStream>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CarrierKind {
    Quic,
    RelayQuic,
    RelayTcp,
    Ssh,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    #[error("stream refused: {0:?}")]
    Refused(pb::StreamRefusal),
    #[error("link closed")]
    LinkClosed,
    #[error("stream I/O failed: {0}")]
    Io(#[source] io::Error),
}

pub(crate) trait LinkCarrier: Send + Sync + 'static {
    fn kind(&self) -> CarrierKind;
    fn control(&self) -> (ControlSink, ControlSource);
    fn open_stream(
        &self,
        preface: pb::StreamPreface,
    ) -> BoxFuture<'_, Result<ByteStream, OpenError>>;
    fn accept_stream(&self) -> BoxFuture<'_, Option<(pb::StreamPreface, ByteStream)>>;
    fn close(&self, reason: pb::LinkCloseReason);
    fn closed(&self) -> BoxFuture<'_, pb::LinkCloseReason>;
}

pub(super) struct ControlWrite {
    pub(super) bytes: Vec<u8>,
    pub(super) result: oneshot::Sender<io::Result<()>>,
}

pub(crate) struct ControlSink {
    pub(super) tx: mpsc::Sender<ControlWrite>,
}

pub(crate) struct ControlSource {
    pub(super) rx: mpsc::Receiver<io::Result<pb::Message>>,
}

pub(crate) async fn write_message(sink: &mut ControlSink, message: &pb::Message) -> io::Result<()> {
    let encoded_len = message.encoded_len();
    if encoded_len > wire::MESSAGE_SIZE_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "control message is {encoded_len} bytes; limit is {}",
                wire::MESSAGE_SIZE_LIMIT
            ),
        ));
    }

    let mut bytes = Vec::with_capacity(encoded_len);
    message
        .encode(&mut bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let (result, completed) = oneshot::channel();
    sink.tx
        .send(ControlWrite { bytes, result })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control stream closed"))?;
    completed
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control stream closed"))?
}

pub(crate) async fn read_message(source: &mut ControlSource) -> io::Result<Option<pb::Message>> {
    source.rx.recv().await.transpose()
}

pub(super) type BoxIoFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;
