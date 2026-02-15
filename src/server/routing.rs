use super::ServerUserState;
use super::connection::cancel_streams_matching;
use crate::buffer::MultiplexReader;
use crate::message::{CreateAgentRequest, LocalMessage, Message, PermissionResponse};
use crate::route::Route;
use crate::session::{LocalAgentSession, SessionEvent};
use anyhow::{Result, anyhow};
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

pub(super) async fn connection_tx(
    user_state: &Arc<RwLock<ServerUserState>>,
    link_name: &str,
) -> Option<mpsc::Sender<crate::message::Message>> {
    let us = user_state.read().await;
    us.routes.get(link_name).cloned()
}

/// Create a new agent
pub(super) async fn create_agent(
    user_state: &Arc<RwLock<ServerUserState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: CreateAgentRequest,
    user_id: Uuid,
) -> Result<()> {
    let mut us = user_state.write().await;

    if us.registry.contains(&req.agent_id) {
        return Err(anyhow!("Agent already exists: {}", &req.agent_id));
    }

    if let Some(ref a) = req.alias {
        if us.registry.alias_taken(a) {
            return Err(anyhow!("Agent already exists: {}", a));
        }
    }

    let session = LocalAgentSession::new(&req, event_tx.clone(), user_id)?;
    let info = session.to_agent_info();
    let alias = req.alias.clone();
    let command = session.command.clone();
    let working_dir = session.working_dir.clone();
    us.agents.insert(req.agent_id, Arc::new(session));
    us.registry
        .register_local(info)
        .expect("uniqueness already checked");

    broadcast_to_peers(
        &mut us,
        &LocalMessage::AnnounceAgent {
            agent_id: req.agent_id,
            alias,
            command,
            working_dir,
            route: Route::empty(),
        },
        None,
    );

    tracing::info!(agent_id = %req.agent_id, alias = ?req.alias, "agent created");
    Ok(())
}

/// Handle subscribe request by UUID.
pub(super) async fn handle_subscribe(
    user_state: &Arc<RwLock<ServerUserState>>,
    agent_id: &Uuid,
    rows: u16,
    cols: u16,
) -> Result<MultiplexReader> {
    let session = {
        let us = user_state.read().await;
        us.agents
            .get(agent_id)
            .ok_or(anyhow!("Agent not found: {}", &agent_id))?
            .clone()
    };

    session.resize(rows, cols).await?;

    let (reader, _input_tx) = session
        .subscribe()
        .await
        .ok_or(anyhow!("Agent not found: {}", &agent_id))?;
    Ok(reader)
}

/// Shutdown the server — shuts down all agents for the given user state
pub(super) async fn shutdown_server(user_state: &Arc<RwLock<ServerUserState>>) {
    let sessions: Vec<_> = {
        let us = user_state.read().await;
        us.agents.iter().map(|(id, s)| (*id, s.clone())).collect()
    };
    for (id, session) in &sessions {
        tracing::info!(agent_id = %id, "shutting down agent");
        session.shutdown().await;
    }
    let mut us = user_state.write().await;
    us.agents.clear();
}

/// Send a LocalMessage to all peer links, optionally excluding one.
pub(super) fn broadcast_to_peers(
    us: &mut ServerUserState,
    msg: &LocalMessage,
    exclude_link: Option<&str>,
) {
    let wire_msg = Message::Local(msg.clone());
    for link in &us.peer_links {
        if exclude_link == Some(link.as_str()) {
            continue;
        }
        if let Some(tx) = us.routes.get(link) {
            if tx.try_send(wire_msg.clone()).is_err() {
                tracing::warn!(peer = %link, "failed to send to peer");
            }
        }
    }
}

/// Announce all known agents (local + remote) to a newly connected peer.
/// Filters out agents that were learned from this same peer (no echo-back).
pub(super) fn send_initial_announcements(us: &ServerUserState, peer_link: &str) {
    let Some(tx) = us.routes.get(peer_link) else {
        return;
    };

    for (uuid, info) in us.registry.iter_entries() {
        if let Some(link) = info.route.peek()
            && link == peer_link
        {
            continue;
        }
        let msg = Message::Local(LocalMessage::AnnounceAgent {
            agent_id: *uuid,
            alias: info.alias.clone(),
            command: info.command.clone(),
            working_dir: info.working_dir.clone(),
            route: info.route.clone(),
        });
        if tx.try_send(msg).is_err() {
            tracing::warn!(agent_id = %uuid, peer = %peer_link, "failed to announce agent");
        }
    }
}

/// Handle a peer disconnecting: remove route, peer_links entry, remote agents,
/// cancel unreachable streams, and propagate withdrawals to remaining peers.
pub(super) fn handle_peer_disconnect(us: &mut ServerUserState, link_name: &str) {
    us.routes.remove(link_name);
    us.peer_links.remove(link_name);

    // Cancel streams spawned for subscribers on this link (they hold cloned senders
    // to the link's outgoing channel — must be dropped for writer task to exit)
    // and streams whose route passes through this link (unreachable).
    let cancelled = cancel_streams_matching(us, |entry| {
        entry.link == link_name || entry.dst.contains_link(link_name)
    });
    if cancelled > 0 {
        tracing::info!(count = cancelled, peer = %link_name, "cancelled streams for disconnected peer");
    }

    let removed_ids = us.registry.remove_for_link(link_name);
    for agent_id in removed_ids {
        tracing::info!(agent_id = %agent_id, peer = %link_name, "withdrawing agent");
        broadcast_to_peers(us, &LocalMessage::WithdrawAgent { agent_id }, None);
    }
}

/// Convert a permission response to the keystroke to send to Claude Code's TUI.
/// Claude Code's permission UI accepts:
/// - 1: Yes (accept this edit)
/// - 2: Yes (accept all edits)
/// - 3: No (deny)
pub(super) fn permission_response_keystroke(response: &PermissionResponse) -> &'static [u8] {
    match response {
        PermissionResponse::Yes => b"1",
        PermissionResponse::YesAll => b"2",
        PermissionResponse::No => b"3",
    }
}
