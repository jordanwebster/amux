//! In-process tonic transport helpers.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(test)]
use futures_util::{Stream, stream};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Endpoint};

use super::{GrpcIo, channel_from_single_io};

pub(crate) const IN_PROCESS_BUF_SIZE: usize = 64 * 1024;

pub(crate) type InProcessTransport = GrpcIo<DuplexStream>;

pub(crate) struct InProcessConnection {
    cancellation: CancellationToken,
}

impl InProcessConnection {
    pub(crate) fn close(&self) {
        self.cancellation.cancel();
    }
}

pub(crate) fn in_process_transport_pair() -> (InProcessTransport, InProcessTransport) {
    let (client_io, server_io) = tokio::io::duplex(IN_PROCESS_BUF_SIZE);
    (
        InProcessTransport::new(client_io),
        InProcessTransport::new(server_io),
    )
}

pub(crate) fn managed_in_process_transport_pair() -> (
    InProcessTransport,
    impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    InProcessConnection,
) {
    let (client, server) = in_process_transport_pair();
    let cancellation = CancellationToken::new();
    let cancelled = Box::pin(cancellation.clone().cancelled_owned());
    (
        client,
        ShutdownIo {
            inner: server,
            cancelled,
        },
        InProcessConnection { cancellation },
    )
}

pub(crate) struct ShutdownIo<T> {
    inner: T,
    cancelled: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<T> ShutdownIo<T> {
    pub(crate) fn new(inner: T, cancellation: CancellationToken) -> Self {
        Self {
            inner,
            cancelled: Box::pin(cancellation.cancelled_owned()),
        }
    }
}

impl<T: tonic::transport::server::Connected> tonic::transport::server::Connected for ShutdownIo<T> {
    type ConnectInfo = T::ConnectInfo;
    fn connect_info(&self) -> Self::ConnectInfo {
        self.inner.connect_info()
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for ShutdownIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ShutdownIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "profile connection closed",
            )));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "profile connection closed",
            )));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
pub(crate) fn in_process_incoming(
    transport: InProcessTransport,
) -> impl Stream<Item = io::Result<InProcessTransport>> + Send + 'static {
    stream::once(async move { Ok(transport) })
}

pub(crate) fn in_process_channel(transport: InProcessTransport) -> Channel {
    channel_from_single_io(
        Endpoint::from_static("http://in-process"),
        "in-process transport",
        transport,
    )
}
