use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result as AnyhowResult, bail};
use thiserror::Error;
use tokio::sync::{RwLock, mpsc};
use uuid::Uuid;

use super::peers::{announce_agent_message, broadcast_to_peers};
use crate::agent::{AgentSession, SessionEvent, StopPolicy};
use crate::buffer::MultiplexByteReader;
use crate::protocol::message::{AgentType, DirectMessage, TerminalSize};
use crate::server::ServerUserState;
use crate::suspend::SuspendedServerState;

/// Maximum local agents per user. Each agent holds a PTY and several tokio
/// tasks, so this provides an upper bound on per-user resource consumption.
pub(in crate::server::routing) const MAX_LOCAL_AGENTS: usize = 1024;

#[derive(Debug, Error)]
pub(in crate::server) enum SubscribeError {
    #[error("Agent not found: {0}")]
    AgentNotFound(String),
    #[error("agent {0} has no PTY")]
    MissingPty(Uuid),
    #[error("agent {0} session has ended")]
    SessionEnded(Uuid),
    #[error("{0}")]
    Operation(String),
}

pub(in crate::server) async fn create_agent(
    user_state: &Arc<RwLock<ServerUserState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: crate::protocol::CreateAgentRequest,
    user_id: Uuid,
    host_id: Uuid,
) -> std::result::Result<(), String> {
    create_agent_inner(user_state, event_tx, req, user_id, host_id)
        .await
        .map_err(|error| error.to_string())
}

async fn create_agent_inner(
    user_state: &Arc<RwLock<ServerUserState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: crate::protocol::CreateAgentRequest,
    user_id: Uuid,
    host_id: Uuid,
) -> AnyhowResult<()> {
    let agent_type = req.agent_type.clone();
    let args = req.args.clone();
    let agent_id = req.agent_id;
    let req_name = req.name.clone();

    let agent_count = {
        let mut us = user_state.write().await;

        if us.agents.len() >= MAX_LOCAL_AGENTS {
            bail!("agent limit reached ({MAX_LOCAL_AGENTS} max)");
        }

        if us.registry.contains(&req.agent_id) {
            bail!("Agent already exists: {}", req.agent_id);
        }

        if let Some(ref a) = req.name
            && us.registry.name_taken(a)
        {
            bail!("Agent already exists: {a}");
        }

        if matches!(req.agent_type, AgentType::Unknown) {
            bail!("unknown agent type");
        }

        let mut session = AgentSession::try_new(&req)?;
        let exit_handle = session
            .start()
            .with_context(|| format!("failed to start local agent {agent_id}"))?;
        let info = session.to_agent(host_id);
        let announce = announce_agent_message(&info);
        us.agents.insert(agent_id, session);
        us.registry
            .register_local(info)
            .with_context(|| format!("failed to register local agent {agent_id}"))?;
        if let Some(session) = us.agents.get_mut(&agent_id) {
            session.maybe_start_name_sniffer(user_id, event_tx);
        }

        // Task: Monitor exit handle and notify server when agent exits
        let exit_event_tx = event_tx.clone();
        tokio::spawn(async move {
            let _ = exit_handle.await;
            let _ = exit_event_tx
                .send(SessionEvent::Ended { agent_id, user_id })
                .await;
        });

        broadcast_to_peers(&mut us, &announce, None);

        us.agents.len()
    }; // write lock dropped here

    let _ = event_tx
        .send(SessionEvent::Created {
            agent_id,
            user_id,
            agent_type,
            args,
        })
        .await;

    tracing::info!(
        agent_id = %agent_id,
        host_id = %host_id,
        name = ?req_name,
        agents = agent_count,
        "agent created"
    );
    Ok(())
}

/// Remove an agent from local state and broadcast withdrawal.
///
/// Active subscriptions are left registered until their underlying buffers
/// close and each stream task can emit its terminal EOF notification before
/// cleaning itself up.
pub(in crate::server) fn withdraw_agent(
    us: &mut ServerUserState,
    agent_id: Uuid,
) -> Option<AgentSession> {
    if !us.agents.contains_key(&agent_id) {
        return None;
    }
    let name = us.registry.get(&agent_id).and_then(|a| a.name.clone());
    let session = us.agents.remove(&agent_id);
    us.registry.remove(&agent_id);
    let active_subscriptions = us
        .active_subscriptions
        .values()
        .filter(|entry| entry.agent_id == agent_id)
        .count();
    broadcast_to_peers(us, &DirectMessage::WithdrawAgent { agent_id }, None);
    tracing::info!(
        %agent_id,
        ?name,
        active_subscriptions,
        remaining = us.agents.len(),
        "agent withdrawn"
    );
    session
}

pub(in crate::server) fn delete_local_agent(
    us: &mut ServerUserState,
    agent_id: Uuid,
) -> Option<AgentSession> {
    withdraw_agent(us, agent_id)
}

/// Handle subscribe request by UUID.
pub(in crate::server) async fn handle_subscribe(
    user_state: &Arc<RwLock<ServerUserState>>,
    agent_id: &Uuid,
    terminal_size: Option<TerminalSize>,
) -> std::result::Result<MultiplexByteReader, SubscribeError> {
    let us = user_state.read().await;
    let session = us
        .agents
        .get(agent_id)
        .ok_or_else(|| SubscribeError::AgentNotFound(agent_id.to_string()))?;

    let pty = session
        .pty_handle()
        .ok_or(SubscribeError::MissingPty(*agent_id))?;

    if let Some(size) = terminal_size {
        pty.resize(size)
            .await
            .map_err(|error| SubscribeError::Operation(error.to_string()))?;
    }

    pty.subscribe()
        .await
        .ok_or(SubscribeError::SessionEnded(*agent_id))
}

/// Shutdown the server — shuts down all agents for the given user state
pub(in crate::server) async fn shutdown_server(user_state: &Arc<RwLock<ServerUserState>>) {
    let sessions: HashMap<Uuid, AgentSession> = {
        let mut us = user_state.write().await;
        std::mem::take(&mut us.agents)
    };
    for (id, session) in &sessions {
        tracing::info!(agent_id = %id, "shutting down agent");
        session.stop(StopPolicy::Interrupt).await;
    }
}

/// Suspend the server — stop all agents and return serializable state.
/// Returns (suspended_agents, errors) where errors are agent IDs that failed to suspend.
pub(in crate::server) async fn suspend_server(
    user_state: &Arc<RwLock<ServerUserState>>,
) -> (SuspendedServerState, Vec<String>) {
    let sessions: HashMap<Uuid, AgentSession> = {
        let mut us = user_state.write().await;
        std::mem::take(&mut us.agents)
    };

    let mut suspended = Vec::new();
    let mut errors = Vec::new();

    for (id, session) in sessions {
        // Remove from registry before suspending
        {
            let mut us = user_state.write().await;
            us.registry.remove(&id);
            broadcast_to_peers(
                &mut us,
                &DirectMessage::WithdrawAgent { agent_id: id },
                None,
            );
        }

        tracing::info!(agent_id = %id, "suspending agent");
        match session.suspend().await {
            Ok(sa) => suspended.push(sa),
            Err(e) => {
                tracing::error!(agent_id = %id, error = %e, "failed to suspend agent");
                errors.push(format!("agent {id}: {e}"));
            }
        }
    }
    (SuspendedServerState { agents: suspended }, errors)
}

/// Resume agents from suspended state. Returns (resumed_count, failed_count).
pub(in crate::server) async fn resume_agents(
    user_state: &Arc<RwLock<ServerUserState>>,
    event_tx: &mpsc::Sender<SessionEvent>,
    user_id: Uuid,
    suspended: Vec<crate::suspend::SuspendedAgent>,
    host_id: Uuid,
) -> (usize, usize) {
    let mut resumed = 0usize;
    let mut failed = 0usize;

    for sa in suspended {
        let agent_id = sa.agent_id();
        let name = sa.name().map(String::from);
        tracing::info!(agent_id = %agent_id, name = ?name, "resuming agent");

        let mut session = AgentSession::from_suspended(sa);
        match session.start() {
            Ok(exit_handle) => {
                let info = session.to_agent(host_id);
                let announce = announce_agent_message(&info);
                {
                    let mut us = user_state.write().await;
                    us.agents.insert(agent_id, session);
                    if let Err(e) = us.registry.register_local(info) {
                        tracing::error!(agent_id = %agent_id, error = %e, "failed to register resumed agent");
                    } else if let Some(session) = us.agents.get_mut(&agent_id) {
                        session.maybe_start_name_sniffer(user_id, event_tx);
                    }
                    broadcast_to_peers(&mut us, &announce, None);
                }

                // Task: Monitor exit handle and notify server when agent exits
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    let _ = exit_handle.await;
                    let _ = event_tx
                        .send(SessionEvent::Ended { agent_id, user_id })
                        .await;
                });

                resumed += 1;
            }
            Err(e) => {
                tracing::error!(agent_id = %agent_id, error = %e, "failed to resume agent");
                failed += 1;
            }
        }
    }

    tracing::info!(resumed, failed, "resume complete");
    (resumed, failed)
}
