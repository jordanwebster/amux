//! Tunnel primitives: the in-process byte channel that tonic uses for
//! host-to-host calls over routed tunnel frames.
//!
//! A tunnel's lifecycle mirrors a link's: `TunnelOpen` (the only allocator
//! of endpoint state; carries the reply address once) · `TunnelData`
//! (payload-capped frames) · `TunnelClose` (or link death). Frames are
//! addressed by destination host_id; a tunnel is pinned to the link its
//! first frame used and dies with that link.

mod pool;
mod transport;
mod types;

use bytes::Bytes;
pub(crate) use pool::{TunnelDebug, TunnelPool, TunnelPoolError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
pub(crate) use transport::TunnelTransport;

use crate::HostId;
use crate::protocol::wire as pb;
use crate::routing::LinkOutputTx;
pub(crate) use crate::tunnel::types::TunnelId;

pub(crate) const TUNNEL_DATA_PAYLOAD_MAX: usize = 64 * 1024;
const BUF_SIZE: usize = TUNNEL_DATA_PAYLOAD_MAX;
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
/// and leave on `outbound_link_tx` — the link the tunnel is pinned to. An
/// initiator endpoint (`open_as = Some(self host_id)`) sends `TunnelOpen`
/// ahead of its first `TunnelData`; a hosted endpoint (`open_as = None`)
/// was allocated by the peer's Open and never opens.
pub(crate) fn create_tunnel(
    id: TunnelId,
    peer: HostId,
    open_as: Option<HostId>,
    outbound_link_tx: LinkOutputTx,
) -> (Tunnel, TunnelTransport) {
    let (grpc_half, routing_half) = tokio::io::duplex(BUF_SIZE);
    let (mut routing_read, mut routing_write) = tokio::io::split(routing_half);
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<Bytes>(INBOUND_DEPTH);

    let reader_task = tokio::spawn(async move {
        let mut open_pending = open_as;
        let mut buf = vec![0_u8; BUF_SIZE];
        loop {
            let read = match routing_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            if let Some(src) = open_pending.take()
                && outbound_link_tx
                    .send(tunnel_open_message(id, src, peer))
                    .await
                    .is_err()
            {
                break;
            }
            let message = tunnel_data_message(id, peer, buf[..read].to_vec());
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

fn tunnel_open_message(id: TunnelId, src: HostId, dst: HostId) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelOpen(pb::TunnelOpen {
            tunnel_id: id.to_wire(),
            src: src.as_bytes().to_vec(),
            dst: dst.as_bytes().to_vec(),
        })),
    }
}

fn tunnel_data_message(id: TunnelId, dst: HostId, payload: Vec<u8>) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelData(pb::TunnelData {
            tunnel_id: id.to_wire(),
            dst: dst.as_bytes().to_vec(),
            payload,
        })),
    }
}

pub(crate) fn tunnel_close_message(id: TunnelId, dst: HostId) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelClose(pb::TunnelClose {
            tunnel_id: id.to_wire(),
            dst: dst.as_bytes().to_vec(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn an_initiator_endpoint_opens_before_its_first_data_frame() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let id = TunnelId::new();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(2);

        let (tunnel, mut transport) = create_tunnel(id, target, Some(initiator), outbound_tx);

        transport.write_all(b"hello").await.unwrap();
        let Some(pb::message::Body::TunnelOpen(open)) = outbound_rx.recv().await.unwrap().body
        else {
            panic!("expected TunnelOpen before any data");
        };
        assert_eq!(open.tunnel_id, id.to_wire());
        assert_eq!(open.src, initiator.as_bytes().to_vec());
        assert_eq!(open.dst, target.as_bytes().to_vec());

        let Some(pb::message::Body::TunnelData(data)) = outbound_rx.recv().await.unwrap().body
        else {
            panic!("expected TunnelData");
        };
        assert_eq!(data.payload, b"hello");
        assert_eq!(data.dst, target.as_bytes().to_vec());
        assert_eq!(data.tunnel_id, id.to_wire());

        tunnel
            .inbound_sender()
            .send(Bytes::from_static(b"reply"))
            .await
            .unwrap();
        let mut buf = [0_u8; 5];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"reply");
    }

    #[tokio::test]
    async fn a_hosted_endpoint_replies_with_data_and_never_opens() {
        let initiator = HostId::from_u128(1);
        let id = TunnelId::new();
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);

        let (_tunnel, mut transport) = create_tunnel(id, initiator, None, outbound_tx);

        transport.write_all(b"reply").await.unwrap();
        let Some(pb::message::Body::TunnelData(data)) = outbound_rx.recv().await.unwrap().body
        else {
            panic!("expected TunnelData, never TunnelOpen, from a hosted endpoint");
        };
        assert_eq!(data.payload, b"reply");
        assert_eq!(data.dst, initiator.as_bytes().to_vec());
    }
}
