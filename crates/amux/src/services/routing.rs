//! RoutingService implementation for peer routing-event streams.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::protocol::link::Link;
use crate::protocol::message::{ProtocolError, RoutingEvent};
use crate::protocol::wire;
use crate::rpc::{RpcInboundServerStream, RpcPeerSnapshotSendError};
use crate::server::{ServerState, ServerUserState, initial_routing_events};

pub(crate) struct RoutingService;

#[derive(Clone)]
pub(crate) struct RoutingServiceCtx {
    state: Arc<RwLock<ServerState>>,
    user_state: Arc<RwLock<ServerUserState>>,
    link: Link,
}

impl RoutingServiceCtx {
    pub(crate) fn new(
        state: Arc<RwLock<ServerState>>,
        user_state: Arc<RwLock<ServerUserState>>,
        link: Link,
    ) -> Self {
        Self {
            state,
            user_state,
            link,
        }
    }

    fn link(&self) -> &Link {
        &self.link
    }

    async fn host_snapshot(&self) -> (uuid::Uuid, String, bool) {
        let state = self.state.read().await;
        (
            state.host_id(),
            state.host_name().to_string(),
            state.is_cloud_server(),
        )
    }

    fn user_state(&self) -> &Arc<RwLock<ServerUserState>> {
        &self.user_state
    }
}

pub(crate) enum SubscribeRoutingEventsStartError {
    Response(ProtocolError),
    ResponseThenClose {
        error: ProtocolError,
        reason: String,
    },
    ConnectionClosed {
        reason: String,
    },
}

fn enqueue_initial_snapshot(
    stream: &RpcInboundServerStream,
    events: Vec<RoutingEvent>,
) -> Result<(), PeerSnapshotEnqueueError> {
    let payloads: Vec<_> = events
        .into_iter()
        .map(|event| wire::encode_routing_event(&event).expect("known routing event should encode"))
        .collect();
    stream
        .output
        .try_send_snapshot(payloads)
        .map_err(|error| match error {
            RpcPeerSnapshotSendError::Full => PeerSnapshotEnqueueError::Full,
            RpcPeerSnapshotSendError::Closed => PeerSnapshotEnqueueError::Closed,
        })
}

enum PeerSnapshotEnqueueError {
    Full,
    Closed,
}

impl RoutingService {
    pub(crate) async fn subscribe_routing_events(
        ctx: &RoutingServiceCtx,
        stream: &RpcInboundServerStream,
    ) -> Result<(), SubscribeRoutingEventsStartError> {
        let (host_id, host_name, is_cloud_server) = ctx.host_snapshot().await;

        let us = ctx.user_state().read().await;
        if !us.is_peer_link(ctx.link()) {
            return Err(SubscribeRoutingEventsStartError::ResponseThenClose {
                error: ProtocolError::InvalidArgument {
                    message: "routing event subscription is only valid for peer connections"
                        .to_string(),
                },
                reason: "received peer routing subscription on non-peer connection".to_string(),
            });
        }

        let events = initial_routing_events(&us, host_id, &host_name, is_cloud_server, ctx.link());
        match enqueue_initial_snapshot(stream, events) {
            Ok(()) => Ok(()),
            Err(PeerSnapshotEnqueueError::Full) => Err(SubscribeRoutingEventsStartError::Response(
                ProtocolError::ResourceExhausted {
                    message: "outgoing channel full while starting routing event stream"
                        .to_string(),
                },
            )),
            Err(PeerSnapshotEnqueueError::Closed) => {
                Err(SubscribeRoutingEventsStartError::ConnectionClosed {
                    reason: "outgoing channel closed while starting routing event stream"
                        .to_string(),
                })
            }
        }
    }
}
