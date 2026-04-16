use uuid::Uuid;

use crate::agent::Agent;
use crate::protocol::link::Link;
use crate::protocol::message::{DirectMessage, Message};
use crate::protocol::route::Route;
use crate::server::ServerUserState;
use crate::server::connection::cancel_subscriptions_matching;

pub(in crate::server) fn announce_agent_message(info: &Agent) -> DirectMessage {
    DirectMessage::AnnounceAgent {
        agent_id: info.id,
        host_id: info.host_id,
        name: info.name.clone(),
        command: info.command.clone(),
        working_dir: info.working_dir.clone(),
        agent_type: info.agent_type.clone(),
        structured_protocol: info.structured_protocol.clone(),
        readonly: info.readonly,
        args: info.args.clone(),
        created_at: info.created_at,
    }
}

/// Send a DirectMessage to all peer links, optionally excluding one.
pub(in crate::server) fn broadcast_to_peers(
    us: &mut ServerUserState,
    msg: &DirectMessage,
    exclude_link: Option<&Link>,
) {
    let wire_msg = Message::Direct {
        message: msg.clone(),
    };
    let mut sent = 0usize;
    let mut failed = 0usize;
    for link in &us.peer_links {
        if exclude_link == Some(link) {
            continue;
        }
        if let Some(handle) = us.routes.get(link) {
            if !handle.try_send(wire_msg.clone()) {
                tracing::warn!(peer = %link, "failed to send to peer");
                failed += 1;
            } else {
                sent += 1;
            }
        }
    }
    tracing::debug!(sent, failed, "broadcast to peers");
}

/// Announce all known agents (local + remote) to a newly connected peer.
/// Filters out agents that were learned from this same peer (no echo-back).
/// Returns the number of agents announced.
pub(in crate::server::routing) fn send_initial_agent_announcements(
    us: &ServerUserState,
    peer_link: &Link,
) -> usize {
    let Some(handle) = us.routes.get(peer_link) else {
        return 0;
    };

    let mut count = 0usize;
    for (uuid, info) in us.registry.iter_entries() {
        if info.is_remote() {
            let Some(host) = us.hosts.get(&info.host_id) else {
                tracing::debug!(agent_id = %uuid, host_id = %info.host_id, "skipping remote announce for agent with unknown host");
                continue;
            };
            if let Some(link) = host.route.peek()
                && link == peer_link
            {
                continue;
            }
        }
        let msg = Message::Direct {
            message: info.announce_message(),
        };
        if !handle.try_send(msg) {
            tracing::warn!(agent_id = %uuid, peer = %peer_link, "failed to announce agent");
        } else {
            count += 1;
        }
    }
    count
}

/// Announce all known hosts (remote + own) to a newly connected peer.
/// Filters out hosts that were learned from this same peer (no echo-back).
/// Cloud servers are stateless relays and don't announce themselves as hosts.
/// Returns the number of hosts announced.
pub(in crate::server::routing) fn send_initial_host_announcements(
    us: &ServerUserState,
    host_id: Uuid,
    host_name: &str,
    is_cloud_server: bool,
    peer_link: &Link,
) -> usize {
    let Some(handle) = us.routes.get(peer_link) else {
        return 0;
    };

    let mut count = 0usize;
    for info in us.hosts.values() {
        if let Some(link) = info.route.peek()
            && link == peer_link
        {
            continue;
        }
        let msg = Message::Direct {
            message: DirectMessage::AnnounceHost {
                id: info.id,
                name: info.name.clone(),
                route: info.route.clone(),
                version: info.version.clone(),
            },
        };
        if !handle.try_send(msg) {
            tracing::warn!(host_id = %info.id, peer = %peer_link, "failed to announce host");
        } else {
            count += 1;
        }
    }

    if !is_cloud_server {
        let msg = Message::Direct {
            message: DirectMessage::AnnounceHost {
                id: host_id,
                name: host_name.to_string(),
                route: crate::protocol::route::Route::empty(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };
        if !handle.try_send(msg) {
            tracing::warn!(peer = %peer_link, "failed to announce own host");
        } else {
            count += 1;
        }
    }
    count
}

fn send_initial_sync_complete(us: &ServerUserState, peer_link: &Link) -> bool {
    let Some(handle) = us.routes.get(peer_link) else {
        return false;
    };

    if !handle.try_send(Message::Direct {
        message: DirectMessage::InitialSyncComplete,
    }) {
        tracing::warn!(peer = %peer_link, "failed to send initial sync complete");
        false
    } else {
        true
    }
}

/// Send all initial announcements (agents + hosts) to a newly connected peer.
/// `host_id`, `host_name`, and `is_cloud_server` are extracted from global state by the caller
/// to avoid holding both locks simultaneously.
pub(in crate::server) fn send_initial_announcements(
    us: &ServerUserState,
    host_id: Uuid,
    host_name: &str,
    is_cloud_server: bool,
    peer_link: &Link,
) {
    let hosts = send_initial_host_announcements(us, host_id, host_name, is_cloud_server, peer_link);
    let agents = send_initial_agent_announcements(us, peer_link);
    let sync_complete = send_initial_sync_complete(us, peer_link);
    tracing::info!(
        peer = %peer_link,
        agents,
        hosts,
        sync_complete,
        "sent initial announcements"
    );
}

fn disconnected_hosts(us: &ServerUserState, link: &Link) -> Vec<(Uuid, Route)> {
    let prefix = Route::from_link(link.clone());
    let mut hosts: Vec<_> = us
        .hosts
        .iter()
        .filter(|(_, info)| info.route.starts_with_route(&prefix))
        .map(|(id, info)| (*id, info.route.clone()))
        .collect();
    hosts.sort_unstable_by(|(id_a, route_a), (id_b, route_b)| {
        route_a
            .to_string()
            .cmp(&route_b.to_string())
            .then_with(|| id_a.as_u128().cmp(&id_b.as_u128()))
    });
    hosts
}

fn disconnected_host_roots(hosts: &[(Uuid, Route)]) -> Vec<(Uuid, Route)> {
    hosts
        .iter()
        .filter(|(_, route)| {
            !hosts.iter().any(|(_, other_route)| {
                route != other_route && route.starts_with_route(other_route)
            })
        })
        .cloned()
        .collect()
}

/// Handle a peer disconnecting: remove route, peer_links entry, remote agents,
/// cancel unreachable streams, and propagate withdrawals to remaining peers.
pub(in crate::server) fn handle_peer_disconnect(us: &mut ServerUserState, link: &Link) {
    tracing::info!(peer = %link, "peer disconnected");

    us.routes.remove(link);
    us.peer_links.remove(link);

    // Cancel streams spawned for subscribers on this link (they hold cloned senders
    // to the link's outgoing channel — must be dropped for writer task to exit)
    // and streams whose route passes through this link (unreachable).
    let cancelled =
        cancel_subscriptions_matching(us, |entry| entry.dst.contains_link(link.as_str()));
    if !cancelled.is_empty() {
        tracing::info!(
            count = cancelled.len(),
            peer = %link,
            "cancelled subscriptions for disconnected peer"
        );
    }

    let prefix = Route::from_link(link.clone());
    let hosts = &us.hosts;
    let removed_ids = us.registry.remove_where(
        |hid| hosts.get(&hid).map(|h| h.route.clone()),
        |r| r.starts_with_route(&prefix),
    );
    if !removed_ids.is_empty() {
        tracing::info!(count = removed_ids.len(), peer = %link, "removed agents for disconnected peer");
    }

    // Propagate one withdrawal per disconnected root host. Descendants are local
    // bookkeeping and are removed from each receiver's host table independently.
    let removed_hosts = disconnected_hosts(us, link);
    let withdrawn_roots = disconnected_host_roots(&removed_hosts);
    if !removed_hosts.is_empty() {
        tracing::info!(count = removed_hosts.len(), peer = %link, "removed hosts for disconnected peer");
    }
    for (id, _) in &removed_hosts {
        us.hosts.remove(id);
    }
    for (id, route) in withdrawn_roots {
        tracing::info!(host_id = %id, peer = %link, "withdrawing host");
        broadcast_to_peers(us, &DirectMessage::WithdrawHost { id, route }, None);
    }
}
