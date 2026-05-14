use prost::Message as _;
use uuid::Uuid;

use crate::protocol::link::Link;
use crate::protocol::message::{
    AgentEvent, CallId, Frame, FrameBody, Message, RequestFrame, RoutingEvent,
};
use crate::protocol::method;
use crate::protocol::wire;
use crate::server::routing::TopologyEvent;
use crate::server::{ServerOriginOutboundStart, ServerUserState, peer_routing_dedup_key};

fn peer_event_message(link: Link, call_id: CallId, event: &RoutingEvent) -> Message {
    Message::Frame(Frame {
        src: crate::protocol::Route::from_link(link),
        dst: crate::protocol::Route::empty(),
        call_id,
        body: FrameBody::StreamItem(
            wire::encode_routing_event(event).expect("known routing event should encode"),
        ),
    })
}

/// Send a routing event to all peer links, optionally excluding one.
pub(in crate::server) fn broadcast_to_peers(
    us: &mut ServerUserState,
    event: &RoutingEvent,
    exclude_link: Option<&Link>,
) {
    let mut sent = 0usize;
    let mut failed = 0usize;
    let links = us.peer_links();
    for link in links {
        if exclude_link == Some(&link) {
            continue;
        }
        let Some(context_rpc) = us.rpc_for_link(&link) else {
            tracing::warn!(peer = %link, "peer has no route context");
            failed += 1;
            continue;
        };
        let Some(call_id) =
            context_rpc.active_inbound_call_id_for_dedup_key(&peer_routing_dedup_key(&link))
        else {
            tracing::warn!(peer = %link, "peer has no routing stream call id");
            failed += 1;
            continue;
        };
        let wire_msg = peer_event_message(link.clone(), call_id, event);
        let route = crate::protocol::Route::from_link(link.clone());
        if !us.send_via_route(
            &route,
            wire_msg,
            format!(
                "peer route queue overflow while broadcasting {}",
                event.type_label()
            ),
        ) {
            tracing::warn!(peer = %link, "peer route queue unavailable; requested close");
            failed += 1;
        } else {
            sent += 1;
        }
    }
    tracing::debug!(sent, failed, "broadcast to peers");
}

/// Project a canonical topology event onto every active peer routing stream.
pub(crate) fn broadcast_topology_event(
    us: &mut ServerUserState,
    event: &TopologyEvent,
    exclude_link: Option<&Link>,
) {
    match event {
        TopologyEvent::HostUp { .. } | TopologyEvent::HostDown { .. } => {
            broadcast_to_peers(us, &event.to_routing_event(), exclude_link);
        }
        TopologyEvent::AgentUp { .. } | TopologyEvent::AgentDown { .. } => {
            broadcast_agent_event_to_subscribers(us, &event.to_agent_event());
        }
    }
}

pub(crate) fn maybe_start_agent_subscription(
    us: &mut ServerUserState,
    host_id: Uuid,
    is_cloud_server: bool,
) {
    if is_cloud_server {
        return;
    }
    let Some((_host, route)) = us.agent_subscription_candidate(host_id) else {
        return;
    };
    if route.is_empty() {
        return;
    }

    let Some((src, dst)) = crate::protocol::Route::send(route.clone()) else {
        return;
    };
    let call_id = CallId::from(Uuid::new_v4());
    let request_payload = wire::SubscribeAgentEventsRequest {
        host_id: host_id.as_bytes().to_vec(),
    }
    .encode_to_vec();
    let body = FrameBody::Request(RequestFrame {
        method: method::AGENT_SUBSCRIBE_EVENTS_NAME.to_string(),
        payload: request_payload,
    });

    let Some(rpc) = us.route_rpc_for_counterparty(&route) else {
        tracing::warn!(host_id = %host_id, route = %route, "missing route RPC for agent subscription");
        return;
    };
    match rpc.register_server_origin_outbound(ServerOriginOutboundStart {
        call_id: call_id.clone(),
        method: method::AGENT_SUBSCRIBE_EVENTS,
    }) {
        Ok(()) => {}
        Err(error) => {
            tracing::warn!(host_id = %host_id, route = %route, error = ?error, "failed to register agent subscription");
            return;
        }
    }
    us.set_agent_subscription(host_id, route.clone(), call_id.clone());

    let message = Message::Frame(Frame {
        src: src.clone(),
        dst,
        call_id: call_id.clone(),
        body,
    });
    if !us.send_via_route(
        &src,
        message,
        format!("route queue overflow while starting agent subscription to {host_id}"),
    ) {
        rpc.remove_server_origin_outbound(&call_id, method::AGENT_SUBSCRIBE_EVENTS);
        us.clear_agent_subscription_for_route(&call_id, &route);
    }
}

pub(in crate::server) fn broadcast_agent_event_to_subscribers(
    us: &mut ServerUserState,
    event: &AgentEvent,
) {
    let mut sent = 0usize;
    let mut failed = 0usize;
    let payload = match wire::encode_agent_event(event) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(error = %error, "failed to encode agent event");
            return;
        }
    };
    for (rpc, sink) in us.active_endpoint_stream_sinks_for_method(method::AGENT_SUBSCRIBE_EVENTS) {
        if !sink.output.try_send_stream_item(payload.clone()) {
            tracing::warn!("agent subscription stream queue unavailable");
            if let Some(connection) = us.connections.get(&sink.owner_link) {
                connection
                    .handle
                    .request_close("route queue overflow while broadcasting agent event");
            }
            rpc.remove_inbound_for_handle(&sink.handle);
            failed += 1;
        } else {
            sent += 1;
        }
    }
    tracing::debug!(sent, failed, "broadcast agent event to subscribers");
}

/// Build initial host events for a newly connected peer.
/// Filters out hosts that were learned from this same peer (no echo-back).
pub(in crate::server::routing) fn initial_host_events(
    us: &ServerUserState,
    peer_link: &Link,
) -> Vec<RoutingEvent> {
    let mut events = Vec::new();
    for (route, info, _) in us.host_contexts_sorted() {
        if route.is_empty() {
            continue;
        }
        if let Some(link) = route.peek()
            && link == peer_link
        {
            continue;
        }
        events.push(RoutingEvent::HostUp {
            host: info.clone(),
            route: route.clone(),
        });
    }
    events
}

/// Build all initial routing events (hosts + snapshot complete) for a newly connected peer.
pub(crate) fn initial_routing_events(us: &ServerUserState, peer_link: &Link) -> Vec<RoutingEvent> {
    let mut events = initial_host_events(us, peer_link);
    let hosts = events.len();
    events.push(RoutingEvent::SnapshotComplete);
    tracing::info!(
        peer = %peer_link,
        hosts,
        "built initial announcements"
    );
    events
}
