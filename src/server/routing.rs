use super::ServerState;
use crate::buffer::MultiplexReader;
use crate::error::{AmuxError, Result};
use crate::message::{CreateAgentRequest, PermissionResponse};
use crate::session::{LocalAgentSession, SessionEvent};
use std::collections::HashMap;
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

/// Resolve an agent by UUID or alias
pub(super) fn resolve_agent<'a>(
    agents: &'a HashMap<Uuid, Arc<LocalAgentSession>>,
    identifier: &str,
) -> Option<&'a Arc<LocalAgentSession>> {
    if let Ok(uuid) = Uuid::parse_str(identifier) {
        if let Some(agent) = agents.get(&uuid) {
            return Some(agent);
        }
    }
    agents
        .values()
        .find(|a| a.alias.as_deref() == Some(identifier))
}

/// Create a new agent
pub(super) async fn create_agent(
    state: &Arc<RwLock<ServerState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: CreateAgentRequest,
) -> Result<()> {
    let mut state = state.write().await;

    if state.agents.contains_key(&req.agent_id) {
        return Err(AmuxError::AgentAlreadyExists(req.agent_id.to_string()));
    }

    if let Some(ref a) = req.alias {
        if state.agents.values().any(|s| s.alias.as_deref() == Some(a)) {
            return Err(AmuxError::AgentAlreadyExists(a.clone()));
        }
    }

    let session = LocalAgentSession::new(&req, event_tx.clone())?;
    state.agents.insert(req.agent_id, Arc::new(session));

    log!(
        "server: created agent {} (alias={:?})",
        req.agent_id,
        req.alias
    );
    Ok(())
}

/// Handle subscribe request (identifier can be UUID or alias).
/// Returns the output reader; input goes through resolve_agent().send_input().
pub(super) async fn handle_subscribe(
    state: &Arc<RwLock<ServerState>>,
    identifier: &str,
    rows: u16,
    cols: u16,
) -> Result<MultiplexReader> {
    let state = state.read().await;

    let session = resolve_agent(&state.agents, identifier)
        .ok_or_else(|| AmuxError::AgentNotFound(identifier.to_string()))?
        .clone();

    session.resize(rows, cols).await?;

    let (reader, _input_tx) = session
        .subscribe()
        .await
        .ok_or_else(|| AmuxError::AgentNotFound(identifier.to_string()))?;
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
