//! RoutingService implementation for peer routing-event streams.

use std::sync::Arc;

use tokio::sync::RwLock;

use crate::protocol::handshake::RoutingRole;
use crate::protocol::link::Link;
use crate::protocol::message::{ProtocolError, RoutingEvent};
use crate::protocol::wire;
use crate::server::{
    EndpointServerStream, ServerStreamSnapshotSendError, ServerUserState, initial_routing_events,
};

pub(crate) struct RoutingService;

#[derive(Clone)]
pub(crate) struct RoutingServiceCtx {
    user_state: Arc<RwLock<ServerUserState>>,
    link: Link,
    routing_role: RoutingRole,
}

impl RoutingServiceCtx {
    pub(crate) fn new(
        user_state: Arc<RwLock<ServerUserState>>,
        link: Link,
        routing_role: RoutingRole,
    ) -> Self {
        Self {
            user_state,
            link,
            routing_role,
        }
    }

    fn link(&self) -> &Link {
        &self.link
    }

    fn user_state(&self) -> &Arc<RwLock<ServerUserState>> {
        &self.user_state
    }

    fn serves_routing_events(&self) -> bool {
        self.routing_role.serves_routing_events()
    }
}

pub(crate) enum SubscribeRoutingEventsStartError {
    Response(ProtocolError),
    ConnectionClosed { reason: String },
}

fn enqueue_initial_snapshot(
    stream: &EndpointServerStream,
    events: Vec<RoutingEvent>,
) -> Result<(), RoutingSnapshotEnqueueError> {
    let payloads: Vec<_> = events
        .into_iter()
        .map(|event| wire::encode_routing_event(&event).expect("known routing event should encode"))
        .collect();
    stream
        .output
        .try_send_snapshot(payloads)
        .map_err(|error| match error {
            ServerStreamSnapshotSendError::Full => RoutingSnapshotEnqueueError::Full,
            ServerStreamSnapshotSendError::Closed => RoutingSnapshotEnqueueError::Closed,
        })
}

enum RoutingSnapshotEnqueueError {
    Full,
    Closed,
}

impl RoutingService {
    pub(crate) async fn subscribe_routing_events(
        ctx: &RoutingServiceCtx,
        stream: &EndpointServerStream,
    ) -> Result<(), SubscribeRoutingEventsStartError> {
        let us = ctx.user_state().read().await;
        // The right precondition is "this server participates in routing,"
        // i.e. the role we advertised on this link is Host or Relay. Whether
        // the caller is a peer or a local connection is orthogonal.
        if !ctx.serves_routing_events() {
            return Err(SubscribeRoutingEventsStartError::Response(
                ProtocolError::FailedPrecondition {
                    message: "this server's role on the link does not serve routing events"
                        .to_string(),
                },
            ));
        }

        let events = initial_routing_events(&us, ctx.link());
        match enqueue_initial_snapshot(stream, events) {
            Ok(()) => Ok(()),
            Err(RoutingSnapshotEnqueueError::Full) => Err(
                SubscribeRoutingEventsStartError::Response(ProtocolError::ResourceExhausted {
                    message: "outgoing channel full while starting routing event stream"
                        .to_string(),
                }),
            ),
            Err(RoutingSnapshotEnqueueError::Closed) => {
                Err(SubscribeRoutingEventsStartError::ConnectionClosed {
                    reason: "outgoing channel closed while starting routing event stream"
                        .to_string(),
                })
            }
        }
    }
}
