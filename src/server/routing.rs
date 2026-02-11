use super::connection::cancel_streams_matching;
use super::ServerState;
use crate::agent_registry::AgentKind;
use crate::buffer::MultiplexReader;
use crate::error::{AmuxError, Result};
use crate::message::{CreateAgentRequest, LocalMessage, Message, PermissionResponse};
use crate::route::Route;
use crate::session::{LocalAgentSession, SessionEvent};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

pub(super) async fn connection_tx(
    state: &Arc<RwLock<ServerState>>,
    link_name: &str,
) -> Option<mpsc::Sender<crate::message::Message>> {
    let state = state.read().await;
    state.routes.get(link_name).cloned()
}

/// Create a new agent
pub(super) async fn create_agent(
    state: &Arc<RwLock<ServerState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: CreateAgentRequest,
) -> Result<()> {
    let mut state = state.write().await;

    if state.registry.contains(&req.agent_id) {
        return Err(AmuxError::AgentAlreadyExists(req.agent_id.to_string()));
    }

    if let Some(ref a) = req.alias {
        if state.registry.alias_taken(a) {
            return Err(AmuxError::AgentAlreadyExists(a.clone()));
        }
    }

    let session = LocalAgentSession::new(&req, event_tx.clone())?;
    let info = session.to_agent_info();
    let alias = req.alias.clone();
    let command = session.command.clone();
    let working_dir = session.working_dir.clone();
    state.agents.insert(req.agent_id, Arc::new(session));
    state
        .registry
        .register_local(info)
        .expect("uniqueness already checked");

    broadcast_to_peers(
        &mut state,
        &LocalMessage::AnnounceAgent {
            agent_id: req.agent_id,
            alias,
            command,
            working_dir,
            route: Route::empty(),
        },
        None,
    );

    log!(
        "server: created agent {} (alias={:?})",
        req.agent_id,
        req.alias
    );
    Ok(())
}

/// Handle subscribe request by UUID.
/// Returns the output reader; input goes through state.agents.get().send_input().
pub(super) async fn handle_subscribe(
    state: &Arc<RwLock<ServerState>>,
    agent_id: &Uuid,
    rows: u16,
    cols: u16,
) -> Result<MultiplexReader> {
    let state = state.read().await;

    let session = state
        .agents
        .get(agent_id)
        .ok_or_else(|| AmuxError::AgentNotFound(agent_id.to_string()))?
        .clone();

    session.resize(rows, cols).await?;

    let (reader, _input_tx) = session
        .subscribe()
        .await
        .ok_or_else(|| AmuxError::AgentNotFound(agent_id.to_string()))?;
    Ok(reader)
}

/// Shutdown the server
pub(super) async fn shutdown_server(state: &Arc<RwLock<ServerState>>) {
    let mut state = state.write().await;
    for (name, session) in state.agents.iter() {
        log!("server: shutting down agent {}", name);
        session.shutdown().await;
    }
    state.agents.clear();
}

/// Send a LocalMessage to all peer links, optionally excluding one.
pub(super) fn broadcast_to_peers(
    state: &mut ServerState,
    msg: &LocalMessage,
    exclude_link: Option<&str>,
) {
    let wire_msg = Message::Local(msg.clone());
    for link in &state.peer_links {
        if exclude_link == Some(link.as_str()) {
            continue;
        }
        if let Some(tx) = state.routes.get(link) {
            if tx.try_send(wire_msg.clone()).is_err() {
                log!("server: failed to send to peer {}", link);
            }
        }
    }
}

/// Announce all known agents (local + remote) to a newly connected peer.
/// Filters out agents that were learned from this same peer (no echo-back).
pub(super) fn send_initial_announcements(state: &ServerState, peer_link: &str) {
    let Some(tx) = state.routes.get(peer_link) else {
        return;
    };

    for (uuid, entry) in state.registry.iter_entries() {
        match &entry.kind {
            AgentKind::Local => {
                let msg = Message::Local(LocalMessage::AnnounceAgent {
                    agent_id: *uuid,
                    alias: entry.info.alias.clone(),
                    command: entry.info.command.clone(),
                    working_dir: entry.info.working_dir.clone(),
                    route: Route::empty(),
                });
                if tx.try_send(msg).is_err() {
                    log!(
                        "server: failed to announce local agent {} to {}",
                        uuid,
                        peer_link
                    );
                }
            }
            AgentKind::Remote { route, link } => {
                if link == peer_link {
                    continue;
                }
                let msg = Message::Local(LocalMessage::AnnounceAgent {
                    agent_id: *uuid,
                    alias: entry.info.alias.clone(),
                    command: entry.info.command.clone(),
                    working_dir: entry.info.working_dir.clone(),
                    route: route.clone(),
                });
                if tx.try_send(msg).is_err() {
                    log!(
                        "server: failed to announce remote agent {} to {}",
                        uuid,
                        peer_link
                    );
                }
            }
        }
    }
}

/// Handle a peer disconnecting: remove route, peer_links entry, remote agents,
/// cancel unreachable streams, and propagate withdrawals to remaining peers.
pub(super) fn handle_peer_disconnect(state: &mut ServerState, link_name: &str) {
    state.routes.remove(link_name);
    state.peer_links.remove(link_name);

    // Cancel streams spawned for subscribers on this link (they hold cloned senders
    // to the link's outgoing channel — must be dropped for writer task to exit)
    // and streams whose route passes through this link (unreachable).
    let cancelled = cancel_streams_matching(state, |entry| {
        entry.link == link_name || entry.dst.contains_link(link_name)
    });
    if cancelled > 0 {
        log!(
            "server: cancelled {} streams (peer {} disconnected)",
            cancelled,
            link_name
        );
    }

    let removed_ids = state.registry.remove_for_link(link_name);
    for agent_id in removed_ids {
        log!(
            "server: withdrawing agent {} (peer {} disconnected)",
            agent_id,
            link_name
        );
        broadcast_to_peers(state, &LocalMessage::WithdrawAgent { agent_id }, None);
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
