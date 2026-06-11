//! New architecture tunnel primitives.
//!
//! It provides the in-process byte channel that tonic uses for host-to-host
//! services over routed `TunnelFrame` messages. Frames are addressed by
//! destination host_id; a tunnel is pinned to the link its first frame used
//! and dies with that link.

mod pool;
mod transport;
mod types;

use bytes::Bytes;
pub(crate) use pool::{TunnelPool, TunnelPoolError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
pub(crate) use transport::TunnelTransport;

use crate::HostId;
use crate::protocol::wire as pb;
use crate::routing::LinkOutputTx;
pub(crate) use crate::tunnel::types::TunnelId;

pub(crate) const TUNNEL_FRAME_PAYLOAD_MAX: usize = 64 * 1024;
const BUF_SIZE: usize = TUNNEL_FRAME_PAYLOAD_MAX;
const INBOUND_DEPTH: usize = 32;

pub(crate) struct Tunnel {
    inbound_tx: mpsc::Sender<Bytes>,
    reader_task: tokio::task::JoinHandle<()>,
    writer_task: tokio::task::JoinHandle<()>,
}

impl Tunnel {
    pub(crate) fn inbound_sender(&self) -> mpsc::Sender<Bytes> {
        self.inbound_tx.clone()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.reader_task.abort();
        self.writer_task.abort();
    }
}

/// Creates a tunnel endpoint to `peer`. Outbound frames carry `dst = peer`
/// and leave on `outbound_link_tx` — the link the tunnel is pinned to.
pub(crate) fn create_tunnel(
    id: TunnelId,
    peer: HostId,
    outbound_link_tx: LinkOutputTx,
) -> (Tunnel, TunnelTransport) {
    let (grpc_half, routing_half) = tokio::io::duplex(BUF_SIZE);
    let (mut routing_read, mut routing_write) = tokio::io::split(routing_half);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Bytes>(INBOUND_DEPTH);

    let reader_id = id;
    let reader_task = tokio::spawn(async move {
        let mut buf = vec![0_u8; BUF_SIZE];
        loop {
            let read = match routing_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let message = tunnel_frame_message(reader_id, peer, buf[..read].to_vec());
            if outbound_link_tx.send(message).await.is_err() {
                break;
            }
        }
    });

    let writer_task = tokio::spawn(async move {
        while let Some(payload) = inbound_rx.recv().await {
            if routing_write.write_all(&payload).await.is_err() {
                break;
            }
        }
    });

    (
        Tunnel {
            inbound_tx,
            reader_task,
            writer_task,
        },
        TunnelTransport::new(grpc_half, peer),
    )
}

fn tunnel_frame_message(id: TunnelId, dst: HostId, payload: Vec<u8>) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelFrame(pb::TunnelFrame {
            dst: dst.as_bytes().to_vec(),
            tunnel_id: Some(id.into()),
            payload,
        })),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn create_tunnel_wraps_outbound_bytes_and_delivers_inbound_bytes() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let id = TunnelId::from_parts(initiator, uuid::Uuid::from_u128(42));
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);

        let (tunnel, mut transport) = create_tunnel(id, target, outbound_tx);

        transport.write_all(b"hello").await.unwrap();
        let frame = outbound_rx.recv().await.unwrap();
        let Some(pb::message::Body::TunnelFrame(frame)) = frame.body else {
            panic!("expected tunnel frame");
        };
        assert_eq!(frame.payload, b"hello");
        assert_eq!(frame.dst, target.as_bytes().to_vec());
        assert_eq!(TunnelId::try_from(frame.tunnel_id.unwrap()).unwrap(), id);

        tunnel
            .inbound_sender()
            .send(Bytes::from_static(b"reply"))
            .await
            .unwrap();
        let mut buf = [0_u8; 5];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"reply");
    }
}
