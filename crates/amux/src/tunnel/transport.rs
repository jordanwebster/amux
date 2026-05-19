use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tonic::transport::server::Connected;

use crate::HostId;
use crate::tunnel::TunnelId;

type DropHook = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct TunnelTransport {
    inner: DuplexStream,
    peer: HostId,
    tunnel_id: Option<TunnelId>,
    cloud_pairing_reachability: bool,
    on_drop: Option<DropHook>,
}

impl TunnelTransport {
    pub(crate) fn new(inner: DuplexStream, peer: HostId) -> Self {
        Self {
            inner,
            peer,
            tunnel_id: None,
            cloud_pairing_reachability: false,
            on_drop: None,
        }
    }

    pub(crate) fn with_tunnel_id(mut self, tunnel_id: TunnelId) -> Self {
        self.tunnel_id = Some(tunnel_id);
        self
    }

    pub(crate) fn tunnel_id(&self) -> Option<TunnelId> {
        self.tunnel_id
    }

    pub(crate) fn with_cloud_pairing_reachability(mut self, cloud: bool) -> Self {
        self.cloud_pairing_reachability = cloud;
        self
    }

    pub(crate) fn has_cloud_pairing_reachability(&self) -> bool {
        self.cloud_pairing_reachability
    }

    pub(crate) fn with_drop_hook<F>(mut self, hook: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        self.on_drop = Some(Box::new(hook));
        self
    }

    #[cfg(test)]
    pub(crate) fn peer(&self) -> HostId {
        self.peer
    }
}

impl Drop for TunnelTransport {
    fn drop(&mut self) {
        if let Some(hook) = self.on_drop.take() {
            hook();
        }
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
