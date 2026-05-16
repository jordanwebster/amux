use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tonic::transport::server::Connected;

use crate::HostId;

pub(crate) struct TunnelTransport {
    inner: DuplexStream,
    peer: HostId,
}

impl TunnelTransport {
    pub(crate) fn new(inner: DuplexStream, peer: HostId) -> Self {
        Self { inner, peer }
    }

    #[cfg(test)]
    pub(crate) fn peer(&self) -> HostId {
        self.peer
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TunnelConnectInfo {
    pub(crate) peer: HostId,
}

impl Connected for TunnelTransport {
    type ConnectInfo = TunnelConnectInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        TunnelConnectInfo { peer: self.peer }
    }
}

impl AsyncRead for TunnelTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TunnelTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn delegates_io_and_exposes_peer_connect_info() {
        let peer = HostId::from_u128(7);
        let (inner, mut other) = tokio::io::duplex(64);
        let mut transport = TunnelTransport::new(inner, peer);

        assert_eq!(transport.peer(), peer);
        assert_eq!(transport.connect_info(), TunnelConnectInfo { peer });

        transport.write_all(b"ping").await.unwrap();
        let mut buf = [0_u8; 4];
        other.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");

        other.write_all(b"pong").await.unwrap();
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }
}
