use std::sync::Arc;

use tokio::sync::RwLock;

use crate::protocol::link::Link;
use crate::protocol::message::{GoAway, Message, ShutdownReason};
use crate::server::ServerUserState;

/// Best-effort notification for attached clients on shutdown/suspend.
///
/// This intentionally uses `try_send` so server shutdown does not block on a
/// slow or backlogged client link. Missing this notice only affects the exit
/// banner; update availability still comes from the marker file.
pub(super) async fn notify_other_clients(
    user_state: &Arc<RwLock<ServerUserState>>,
    exclude_link: &Link,
    reason: ShutdownReason,
) {
    let us = user_state.read().await;
    let msg = Message::GoAway(GoAway { reason });
    for (link, handle) in &us.topology.routes {
        if link == exclude_link {
            continue;
        }
        let _ = handle.try_send(msg.clone());
    }
}

/// Best-effort shutdown notification to all non-peer clients (local terminals).
///
/// Used when the server can't keep serving attached terminals (protocol
/// mismatch, cloud disconnect, etc.). Peer links are excluded since they
/// relay to their own clients.
pub(in crate::server) async fn notify_local_clients(
    user_state: &Arc<RwLock<ServerUserState>>,
    reason: ShutdownReason,
) {
    let us = user_state.read().await;
    let msg = Message::GoAway(GoAway { reason });
    for (link, handle) in &us.topology.routes {
        if us.topology.peer_links.contains(link) {
            continue;
        }
        let _ = handle.try_send(msg.clone());
    }
}
