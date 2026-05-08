use thiserror::Error;
use uuid::Uuid;

use super::peers::broadcast_topology_event;
use crate::agent::{Agent, LocalAgentNameSource};
use crate::protocol::message::RenameAgentRequest;
use crate::server::ServerUserState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server) enum LocalNameUpdateOutcome {
    Updated,
    ProvenanceUpdated,
    Skipped,
    Collision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::server) enum LocalAgentRenameOutcome {
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
    us: &mut ServerUserState,
    updated: Agent,
    source: LocalAgentNameSource,
) -> std::result::Result<(), String> {
    let agent_id = updated.id;
    let event = us.update_local_agent_info(updated.clone())?;
    if let Some(context) = us.local_agents.get_mut(&agent_id) {
        context.session.set_local_name(updated.name.clone(), source);
    }
    broadcast_topology_event(us, &event, None);
    Ok(())
}

fn rename_local_agent_inner(
    us: &mut ServerUserState,
    host_id: Uuid,
    req: &RenameAgentRequest,
) -> std::result::Result<LocalAgentRenameOutcome, RenameAgentError> {
    let session = us
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
        return commit_local_name_update(us, updated, LocalAgentNameSource::Amux)
            .map(|()| LocalAgentRenameOutcome::Updated)
            .map_err(update_error_to_rename_error);
    }

    us.local_agents
        .get_mut(&req.agent_id)
        .expect("agent present: read-only validation above confirmed it")
        .session
        .set_local_name(current_name, LocalAgentNameSource::Amux);
    Ok(LocalAgentRenameOutcome::ProvenanceUpdated)
}

pub(crate) fn rename_local_agent_record(
    us: &mut ServerUserState,
    host_id: Uuid,
    req: &RenameAgentRequest,
) -> std::result::Result<Agent, RenameAgentError> {
    rename_local_agent_inner(us, host_id, req)?;
    us.local_agent_info(&req.agent_id)
        .cloned()
        .ok_or(RenameAgentError::NotFound(req.agent_id))
}

pub(in crate::server) fn apply_local_name_candidate(
    us: &mut ServerUserState,
    host_id: Uuid,
    agent_id: Uuid,
    name: String,
    source: LocalAgentNameSource,
) -> LocalNameUpdateOutcome {
    // Phase 1: read-only validation
    let session = match us.local_agents.get(&agent_id) {
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
            us.local_agents
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
    match commit_local_name_update(us, updated, source) {
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
