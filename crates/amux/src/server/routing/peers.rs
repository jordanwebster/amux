use uuid::Uuid;

use crate::protocol::link::Link;
use crate::protocol::message::{FrameBody, Message, PeerFrame, RoutedCallId, RoutingEvent};
use crate::protocol::{Route, method, wire};
use crate::server::ServerUserState;
use crate::server::routing::TopologyEvent;

fn peer_event_message(call_id: RoutedCallId, event: &RoutingEvent) -> Message {
    Message::Peer(PeerFrame {
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
    let links: Vec<_> = us.topology.peer_links.iter().cloned().collect();
    for link in links {
        if exclude_link == Some(&link) {
            continue;
        }
        let Some(call_id) = us.rpc.active_inbound_call_id_for_route_and_method(
            &Route::from_link(link.clone()),
            method::ROUTING_SUBSCRIBE_EVENTS,
        ) else {
            tracing::warn!(peer = %link, "peer has no routing stream call id");
            failed += 1;
            continue;
        };
        if let Some(handle) = us.topology.route(&link) {
            let wire_msg = peer_event_message(call_id, event);
            if !handle.try_send_or_close(
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
    }
    tracing::debug!(sent, failed, "broadcast to peers");
}

/// Project a canonical topology event onto every active peer routing stream.
pub(crate) fn broadcast_topology_event(
    us: &mut ServerUserState,
    event: &TopologyEvent,
    exclude_link: Option<&Link>,
) {
    broadcast_to_peers(us, &event.to_routing_event(), exclude_link);
}

/// Build initial agent announcements for a newly connected peer.
/// Filters out agents that were learned from this same peer (no echo-back).
pub(in crate::server::routing) fn initial_agent_events(
    us: &ServerUserState,
    peer_link: &Link,
) -> Vec<RoutingEvent> {
    let mut events = Vec::new();
    for (uuid, info) in us.topology.registry.iter_entries() {
        if info.is_remote() {
            let Some(host) = us.topology.hosts.get(&info.host_id) else {
                tracing::debug!(agent_id = %uuid, host_id = %info.host_id, "skipping remote announce for agent with unknown host");
                continue;
            };
            if let Some(link) = host.route.peek()
                && link == peer_link
            {
                continue;
            }
        }
        events.push(info.routing_event());
    }
    events
}

/// Build initial host events for a newly connected peer.
/// Filters out hosts that were learned from this same peer (no echo-back).
/// Cloud servers are stateless relays and don't announce themselves as hosts.
pub(in crate::server::routing) fn initial_host_events(
    us: &ServerUserState,
    host_id: Uuid,
    host_name: &str,
    is_cloud_server: bool,
    peer_link: &Link,
) -> Vec<RoutingEvent> {
    let mut events = Vec::new();
    for info in us.topology.hosts.values() {
        if let Some(link) = info.route.peek()
            && link == peer_link
        {
            continue;
        }
        events.push(RoutingEvent::HostUp {
            id: info.id,
            name: info.name.clone(),
            route: info.route.clone(),
            version: info.version.clone(),
        });
    }

    if !is_cloud_server {
        events.push(RoutingEvent::HostUp {
            id: host_id,
            name: host_name.to_string(),
            route: crate::protocol::route::Route::empty(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        });
    }
    events
}

/// Build all initial routing events (hosts + agents + snapshot complete) for a newly connected peer.
/// `host_id`, `host_name`, and `is_cloud_server` are extracted from global state by the caller
/// to avoid holding both locks simultaneously.
pub(crate) fn initial_routing_events(
    us: &ServerUserState,
    host_id: Uuid,
    host_name: &str,
    is_cloud_server: bool,
    peer_link: &Link,
) -> Vec<RoutingEvent> {
    let mut events = initial_host_events(us, host_id, host_name, is_cloud_server, peer_link);
    let hosts = events.len();
    let agents = initial_agent_events(us, peer_link);
    let agent_count = agents.len();
    events.extend(agents);
    events.push(RoutingEvent::SnapshotComplete);
    tracing::info!(
        peer = %peer_link,
        agents = agent_count,
        hosts,
        "built initial announcements"
    );
    events
}
