use std::collections::HashMap;

use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::{AgentServiceState, SharedAgentServiceState};
use crate::agents::{
    AgentEvent, AgentRecord, AgentSession, AgentType, LocalAgentNameSource, RenameAgentRequest,
    SessionEvent, StopPolicy, agent_from_suspended, new_agent,
};
use crate::envelope::{AgentSender, Envelope, EnvelopeKind, Sender};
use crate::suspend::{SuspendedAgent, SuspendedServerState};

/// Maximum local agents per user. Each agent holds a PTY and several tokio
/// tasks, so this provides an upper bound on per-user resource consumption.
const MAX_LOCAL_AGENTS: usize = 1024;

pub(crate) fn spawn_session_event_loop(
    agent_state: SharedAgentServiceState,
    mut event_rx: mpsc::Receiver<SessionEvent>,
    host_id: Uuid,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            handle_session_event(&agent_state, host_id, event).await;
        }
    })
}

async fn handle_session_event(
    agent_state: &SharedAgentServiceState,
    host_id: Uuid,
    event: SessionEvent,
) {
    match event {
        SessionEvent::Completed { agent_id, text } => {
            let mut state = agent_state.write().await;
            let envelope = state.local_agents.get(&agent_id).and_then(|context| {
                parent_envelope(&context.session, host_id, EnvelopeKind::Completed, text)
            });
            if let Some(envelope) = envelope {
                state.outbound_envelopes.emit(envelope);
            }
        }
        SessionEvent::Ended { agent_id } => {
            let mut state = agent_state.write().await;
            let envelope = state.local_agents.get(&agent_id).and_then(|context| {
                parent_envelope(
                    &context.session,
                    host_id,
                    EnvelopeKind::Exited,
                    String::new(),
                )
            });
            if let Some(envelope) = envelope {
                state.outbound_envelopes.emit(envelope);
            }
            let _ = withdraw_agent(&mut state, agent_id);
        }
        SessionEvent::Created {
            agent_id,
            agent_type,
            args,
        } => {
            if matches!(agent_type, AgentType::Claude)
                && args.contains(&"--fork-session".to_string())
                && let Some(pos) = args.iter().position(|a| a == "--resume")
                && let Some(source_id_str) = args.get(pos + 1)
                && let Ok(source_id) = source_id_str.parse::<Uuid>()
            {
                let withdrawn_session = {
                    let mut state = agent_state.write().await;
                    let is_readonly = state
                        .local_agents
                        .get(&source_id)
                        .is_some_and(|context| context.session.readonly());
                    if is_readonly {
                        withdraw_agent(&mut state, source_id)
                    } else {
                        None
                    }
                };
                if let Some(session) = withdrawn_session {
                    session.stop(StopPolicy::Interrupt).await;
                    tracing::info!(
                        source = %source_id,
                        fork = %agent_id,
                        "withdrew readonly session (forked)"
                    );
                }
            }
            tracing::debug!(agent_id = %agent_id, ?agent_type, ?args, "session created");
        }
        SessionEvent::NameCandidateChanged {
            agent_id,
            name,
            source,
        } => {
            let outcome = {
                let mut state = agent_state.write().await;
                apply_local_name_candidate(&mut state, host_id, agent_id, name.clone(), source)
            };
            tracing::debug!(
                agent_id = %agent_id,
                candidate = %name,
                ?source,
                ?outcome,
                "processed local name candidate"
            );
        }
    }
}

pub(super) fn parent_envelope(
    session: &AgentSession,
    host_id: Uuid,
    kind: EnvelopeKind,
    text: String,
) -> Option<Envelope> {
    Some(Envelope {
        id: Uuid::new_v4(),
        context: None,
        from: Sender::Agent(AgentSender {
            agent_id: session.agent_id(),
            host_id,
            name: session
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| session.agent_id().to_string()),
            kind: session.agent_type().to_string(),
        }),
        to: session.parent()?,
        kind,
        text,
    })
}

#[derive(Debug, Error)]
pub(crate) enum CreateAgentError {
    #[error("agent limit reached ({max} max)")]
    LimitReached { max: usize },
    #[error("Agent already exists: {0}")]
    AlreadyExists(String),
    #[error("{0}")]
    Start(String),
    #[error("{0}")]
    Register(String),
}

pub(crate) async fn create_agent_record(
    agent_state: &SharedAgentServiceState,
    event_tx: &mpsc::Sender<SessionEvent>,
    req: crate::agents::CreateAgentRequest,
    host_id: Uuid,
) -> std::result::Result<AgentRecord, CreateAgentError> {
    let agent_type = req.agent_type.clone();
    let args = req.args.clone();
    let agent_id = req.agent_id;
    let req_name = req.name.clone();

    let (agent_count, info) = {
        let mut state = agent_state.write().await;

        if state.local_agents.len() >= MAX_LOCAL_AGENTS {
            return Err(CreateAgentError::LimitReached {
                max: MAX_LOCAL_AGENTS,
            });
        }

        if state.contains_agent_id(&req.agent_id) {
            return Err(CreateAgentError::AlreadyExists(req.agent_id.to_string()));
        }

        if let Some(ref a) = req.name
            && state.name_taken_by_other(a, req.agent_id)
        {
            return Err(CreateAgentError::AlreadyExists(a.clone()));
        }

        let mut session = new_agent(&req, &state.deps)
            .map_err(|error| CreateAgentError::Start(error.to_string()))?;
        let exit_handle = session.start(event_tx).map_err(|error| {
            CreateAgentError::Start(format!("failed to start local agent {agent_id}: {error}"))
        })?;
        let info = session.to_agent(host_id);
        let announce = state
            .register_local_agent_context(host_id, agent_id, session)
            .map_err(|error| {
                CreateAgentError::Register(format!(
                    "failed to register local agent {agent_id}: {error}"
                ))
            })?;
        if let Some(context) = state.local_agents.get_mut(&agent_id) {
            context.session.maybe_start_name_sniffer(event_tx);
        }

        // Task: Monitor exit handle and notify server when agent exits
        let exit_event_tx = event_tx.clone();
        tokio::spawn(async move {
            let _ = exit_handle.await;
            let _ = exit_event_tx.send(SessionEvent::Ended { agent_id }).await;
        });

        state.local_agent_events.emit(announce);

        (state.local_agents.len(), info)
    }; // write lock dropped here

    let _ = event_tx
        .send(SessionEvent::Created {
            agent_id,
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
    Ok(info)
}

/// Remove an agent from local state and broadcast withdrawal.
pub(crate) fn withdraw_agent(
    state: &mut AgentServiceState,
    agent_id: Uuid,
) -> Option<AgentSession> {
    let context = state.local_agents.remove(&agent_id)?;
    state
        .local_agent_events
        .emit(AgentEvent::AgentDown { agent_id });
    tracing::info!(
        %agent_id,
        name = ?context.session.name(),
        remaining = state.local_agents.len(),
        "agent withdrawn"
    );
    Some(context.session)
}

pub(crate) fn delete_local_agent(
    state: &mut AgentServiceState,
    agent_id: Uuid,
) -> Option<AgentSession> {
    withdraw_agent(state, agent_id)
}

/// Shutdown the server — shuts down all agents for the given user state
pub(crate) async fn shutdown_server(agent_state: &SharedAgentServiceState) {
    let sessions: HashMap<Uuid, AgentSession> = {
        let mut state = agent_state.write().await;
        std::mem::take(&mut state.local_agents)
            .into_iter()
            .map(|(id, context)| (id, context.session))
            .collect()
    };
    for (id, session) in &sessions {
        tracing::info!(agent_id = %id, "shutting down agent");
        session.stop(StopPolicy::Interrupt).await;
    }
}

/// Prepare serializable suspend state without stopping agents.
/// Returns (suspended_agents, errors) where errors are agent IDs that cannot be suspended.
pub(crate) async fn prepare_server_suspend(
    agent_state: &SharedAgentServiceState,
) -> (SuspendedServerState, Vec<String>) {
    let mut suspended = Vec::new();
    let mut errors = Vec::new();

    let state = agent_state.read().await;
    for (id, context) in &state.local_agents {
        match context.session.suspended_state() {
            Ok(sa) => suspended.push(sa),
            Err(e) => {
                tracing::error!(agent_id = %id, error = %e, "failed to prepare suspended agent state");
                errors.push(format!("agent {id}: {e}"));
            }
        }
    }
    (SuspendedServerState { agents: suspended }, errors)
}

/// Commit a prepared suspend by withdrawing and stopping all local agents.
pub(crate) async fn commit_server_suspend(agent_state: &SharedAgentServiceState) {
    let sessions: HashMap<Uuid, AgentSession> = {
        let mut state = agent_state.write().await;
        let contexts = std::mem::take(&mut state.local_agents);
        for id in contexts.keys() {
            state
                .local_agent_events
                .emit(AgentEvent::AgentDown { agent_id: *id });
        }
        contexts
            .into_iter()
            .map(|(id, context)| (id, context.session))
            .collect()
    };

    for (id, session) in sessions {
        tracing::info!(agent_id = %id, "stopping suspended agent");
        session.stop(StopPolicy::Interrupt).await;
    }
}

/// Resume agents from suspended state and keep failed records retryable.
pub(crate) struct ResumeAgentsResult {
    pub(crate) resumed_count: usize,
    pub(crate) failed_count: usize,
    pub(crate) failed_agents: Vec<SuspendedAgent>,
}

pub(crate) async fn resume_agents(
    agent_state: &SharedAgentServiceState,
    event_tx: &mpsc::Sender<SessionEvent>,
    suspended: Vec<SuspendedAgent>,
    host_id: Uuid,
) -> ResumeAgentsResult {
    let mut resumed = 0usize;
    let mut failed = 0usize;
    let mut failed_agents = Vec::new();
    let deps = agent_state.read().await.deps.clone();

    for sa in suspended {
        let original = sa.clone();
        let agent_id = sa.agent_id();
        let name = sa.name().map(String::from);
        tracing::info!(agent_id = %agent_id, name = ?name, "resuming agent");

        let mut session = agent_from_suspended(sa, &deps);
        match session.start(event_tx) {
            Ok(exit_handle) => {
                let info = session.to_agent(host_id);
                let mut session = Some(session);
                let registered = {
                    let mut state = agent_state.write().await;
                    let registration = if state.contains_agent_id(&agent_id) {
                        Err(format!("Agent already exists: {agent_id}"))
                    } else if let Some(name) = &info.name {
                        if state.name_taken_by_other(name, agent_id) {
                            Err(format!("Agent already exists: {name}"))
                        } else {
                            state.register_local_agent_context(
                                host_id,
                                agent_id,
                                session
                                    .take()
                                    .expect("resumed session should still be available"),
                            )
                        }
                    } else {
                        state.register_local_agent_context(
                            host_id,
                            agent_id,
                            session
                                .take()
                                .expect("resumed session should still be available"),
                        )
                    };

                    match registration {
                        Ok(announce) => {
                            if let Some(context) = state.local_agents.get_mut(&agent_id) {
                                context.session.maybe_start_name_sniffer(event_tx);
                            }
                            state.local_agent_events.emit(announce);
                            true
                        }
                        Err(e) => {
                            tracing::error!(agent_id = %agent_id, error = %e, "failed to register resumed agent");
                            false
                        }
                    }
                };

                if !registered {
                    if let Some(session) = session {
                        session.stop(StopPolicy::Interrupt).await;
                    }
                    exit_handle.abort();
                    failed += 1;
                    failed_agents.push(original);
                    continue;
                }

                // Task: Monitor exit handle and notify server when agent exits
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    let _ = exit_handle.await;
                    let _ = event_tx.send(SessionEvent::Ended { agent_id }).await;
                });

                resumed += 1;
            }
            Err(e) => {
                tracing::error!(agent_id = %agent_id, error = %e, "failed to resume agent");
                failed += 1;
                failed_agents.push(original);
            }
        }
    }

    tracing::info!(resumed, failed, "resume complete");
    ResumeAgentsResult {
        resumed_count: resumed,
        failed_count: failed,
        failed_agents,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalNameUpdateOutcome {
    Updated,
    ProvenanceUpdated,
    Skipped,
    Collision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalAgentRenameOutcome {
    Updated,
    ProvenanceUpdated,
    Unchanged,
}

#[derive(Debug, Error)]
pub(crate) enum RenameAgentError {
    #[error("Agent not found: {0}")]
    NotFound(Uuid),
    #[error("Agent already exists: {0}")]
    AlreadyExists(String),
    #[error("failed to update local agent metadata: {0}")]
    Update(String),
}

fn commit_local_name_update(
    state: &mut AgentServiceState,
    updated: AgentRecord,
    source: LocalAgentNameSource,
) -> std::result::Result<(), String> {
    let agent_id = updated.id;
    if !state.local_agents.contains_key(&agent_id) {
        return Err(format!("Agent not found: {agent_id}"));
    }
    if let Some(name) = &updated.name
        && state.name_taken_by_other(name, agent_id)
    {
        return Err(format!("Agent already exists: {name}"));
    }
    let context = state
        .local_agents
        .get_mut(&agent_id)
        .expect("local agent existence checked above");
    context.session.set_local_name(updated.name.clone(), source);
    state.local_agent_events.emit(updated.agent_updated_event());
    Ok(())
}

fn rename_local_agent_inner(
    state: &mut AgentServiceState,
    host_id: Uuid,
    req: &RenameAgentRequest,
) -> std::result::Result<LocalAgentRenameOutcome, RenameAgentError> {
    let session = state
        .local_agents
        .get(&req.agent_id)
        .map(|context| &context.session)
        .ok_or(RenameAgentError::NotFound(req.agent_id))?;

    let current_name = session.name().map(str::to_owned);
    let current_source = session.local_name_source();
    let mut updated = session.to_agent(host_id);
    let mut metadata_changed = false;
    let mut provenance_changed = false;

    if current_name.as_deref() != Some(req.name.as_str()) {
        updated.name = Some(req.name.clone());
        metadata_changed = true;
    } else if current_source.is_some() && current_source != Some(LocalAgentNameSource::Amux) {
        provenance_changed = true;
    }

    if !metadata_changed && !provenance_changed {
        return Ok(LocalAgentRenameOutcome::Unchanged);
    }

    if metadata_changed {
        return commit_local_name_update(state, updated, LocalAgentNameSource::Amux)
            .map(|()| LocalAgentRenameOutcome::Updated)
            .map_err(update_error_to_rename_error);
    }

    state
        .local_agents
        .get_mut(&req.agent_id)
        .expect("agent present: read-only validation above confirmed it")
        .session
        .set_local_name(current_name, LocalAgentNameSource::Amux);
    Ok(LocalAgentRenameOutcome::ProvenanceUpdated)
}

pub(crate) fn rename_local_agent_record(
    state: &mut AgentServiceState,
    host_id: Uuid,
    req: &RenameAgentRequest,
) -> std::result::Result<AgentRecord, RenameAgentError> {
    rename_local_agent_inner(state, host_id, req)?;
    state
        .local_agent_info(host_id, &req.agent_id)
        .ok_or(RenameAgentError::NotFound(req.agent_id))
}

pub(crate) fn apply_local_name_candidate(
    state: &mut AgentServiceState,
    host_id: Uuid,
    agent_id: Uuid,
    name: String,
    source: LocalAgentNameSource,
) -> LocalNameUpdateOutcome {
    // Phase 1: read-only validation
    let session = match state.local_agents.get(&agent_id) {
        Some(context) => &context.session,
        None => return LocalNameUpdateOutcome::Skipped,
    };
    let Some(current_source) = session.local_name_source() else {
        return LocalNameUpdateOutcome::Skipped;
    };
    if !current_source.is_automatic() || !source.is_automatic() {
        return LocalNameUpdateOutcome::Skipped;
    }
    if source.rank() < current_source.rank() {
        return LocalNameUpdateOutcome::Skipped;
    }

    let current_name = session.name().map(str::to_owned);

    if current_name.as_deref() == Some(name.as_str()) {
        if source.rank() > current_source.rank() {
            state
                .local_agents
                .get_mut(&agent_id)
                .expect("agent present: session borrow above proved it")
                .session
                .set_local_name(Some(name), source);
            return LocalNameUpdateOutcome::ProvenanceUpdated;
        }
        return LocalNameUpdateOutcome::Skipped;
    }

    let mut updated = session.to_agent(host_id);
    updated.name = Some(name.clone());
    // session borrow is dropped here

    // Phase 2: registry + session mutation
    match commit_local_name_update(state, updated, source) {
        Ok(()) => LocalNameUpdateOutcome::Updated,
        Err(err) if err.starts_with("Agent already exists:") => {
            tracing::info!(
                agent_id = %agent_id,
                candidate = %name,
                ?source,
                current_name = ?current_name,
                current_source = ?current_source,
                "skipping local rename: alias collision"
            );
            LocalNameUpdateOutcome::Collision
        }
        Err(err) => {
            tracing::warn!(
                agent_id = %agent_id,
                candidate = %name,
                ?source,
                error = %err,
                "failed to update local agent metadata"
            );
            LocalNameUpdateOutcome::Skipped
        }
    }
}

fn update_error_to_rename_error(error: String) -> RenameAgentError {
    const PREFIX: &str = "Agent already exists: ";
    if let Some(name) = error.strip_prefix(PREFIX) {
        RenameAgentError::AlreadyExists(name.to_string())
    } else {
        RenameAgentError::Update(error)
    }
}

#[cfg(all(test, feature = "local-agents"))]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use tokio::sync::RwLock;

    use super::*;
    use crate::agents::{
        AgentDeps, AgentType, CreateAgentRequest, TEST_ECHO_COMMAND, TestAgentSession,
    };
    use crate::suspend::SuspendedAgent;

    #[tokio::test]
    async fn resume_registration_failure_does_not_replace_existing_agent_session() {
        let agent_state = Arc::new(RwLock::new(AgentServiceState::new(AgentDeps::new(
            std::env::temp_dir(),
            std::env::temp_dir().join("amux-test-codex.sock"),
        ))));
        let (event_tx, _event_rx) = mpsc::channel(16);
        let host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();

        {
            let mut state = agent_state.write().await;
            let session: AgentSession = Box::new(TestAgentSession::echo_for_tests(
                agent_id,
                Some("existing".to_string()),
            ));
            state
                .insert_registered_local_agent(host_id, agent_id, session)
                .unwrap();
        }

        let suspended = SuspendedAgent::TestAgent {
            agent_id,
            name: Some("resumed".to_string()),
            command: TEST_ECHO_COMMAND.to_string(),
            working_dir: std::env::temp_dir(),
            terminal_size: None,
            created_at: Utc::now(),
            parent: None,
            working_on: None,
        };

        let result = resume_agents(&agent_state, &event_tx, vec![suspended], host_id).await;

        assert_eq!(result.resumed_count, 0);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.failed_agents.len(), 1);

        let state = agent_state.read().await;
        assert_eq!(state.local_agents.len(), 1);
        assert_eq!(
            state
                .local_agent_info(host_id, &agent_id)
                .and_then(|agent| agent.name),
            Some("existing".to_string())
        );
    }

    #[tokio::test]
    async fn prepare_suspend_failure_leaves_local_agents_registered() {
        let agent_state = Arc::new(RwLock::new(AgentServiceState::new(AgentDeps::new(
            std::env::temp_dir(),
            std::env::temp_dir().join("amux-test-codex.sock"),
        ))));
        let host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();

        {
            let mut state = agent_state.write().await;
            let session = new_agent(
                &CreateAgentRequest {
                    agent_id,
                    host_id: None,
                    name: Some("not-ready".to_string()),
                    agent_type: AgentType::Claude,
                    working_dir: std::env::temp_dir(),
                    terminal_size: None,
                    args: vec![],
                    parent: None,
                    initial_prompt: None,
                },
                &state.deps,
            )
            .unwrap();
            state
                .insert_registered_local_agent(host_id, agent_id, session)
                .unwrap();
        }

        let (suspended, errors) = prepare_server_suspend(&agent_state).await;

        assert!(suspended.agents.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(agent_state.read().await.contains_agent_id(&agent_id));
    }
}
