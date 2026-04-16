use anyhow::{Result as AnyhowResult, anyhow};
use uuid::Uuid;

use super::peers::{announce_agent_message, broadcast_to_peers};
use crate::agent::{Agent, LocalAgentNameSource};
use crate::protocol::message::RenameAgentRequest;
use crate::server::ServerUserState;
use crate::server::registry::AgentRegistryError;

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

fn reannounce_local_agent(us: &mut ServerUserState, updated: Agent) {
    let announce = announce_agent_message(&updated);
    broadcast_to_peers(us, &announce, None);
}

fn commit_local_name_update(
    us: &mut ServerUserState,
    updated: Agent,
    source: LocalAgentNameSource,
) -> std::result::Result<(), AgentRegistryError> {
    let agent_id = updated.id;
    us.registry.update_local(updated.clone())?;
    if let Some(session) = us.agents.get_mut(&agent_id) {
        session.set_local_name(updated.name.clone(), source);
    }
    reannounce_local_agent(us, updated);
    Ok(())
}

pub(in crate::server) fn rename_local_agent(
    us: &mut ServerUserState,
    host_id: Uuid,
    req: &RenameAgentRequest,
) -> std::result::Result<LocalAgentRenameOutcome, String> {
    rename_local_agent_inner(us, host_id, req).map_err(|error| error.to_string())
}

fn rename_local_agent_inner(
    us: &mut ServerUserState,
    host_id: Uuid,
    req: &RenameAgentRequest,
) -> AnyhowResult<LocalAgentRenameOutcome> {
    let session = us
        .agents
        .get(&req.agent_id)
        .ok_or_else(|| anyhow!("Agent not found: {}", req.agent_id))?;

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
            .map_err(|err| match err {
                AgentRegistryError::AlreadyExists(name) => anyhow!("Agent already exists: {name}"),
                other => anyhow!("failed to update local agent metadata: {other}"),
            });
    }

    us.agents
        .get_mut(&req.agent_id)
        .expect("agent present: read-only validation above confirmed it")
        .set_local_name(current_name, LocalAgentNameSource::Amux);
    Ok(LocalAgentRenameOutcome::ProvenanceUpdated)
}

pub(in crate::server) fn apply_local_name_candidate(
    us: &mut ServerUserState,
    host_id: Uuid,
    agent_id: Uuid,
    name: String,
    source: LocalAgentNameSource,
) -> LocalNameUpdateOutcome {
    // Phase 1: read-only validation
    let session = match us.agents.get(&agent_id) {
        Some(s) => s,
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
            us.agents
                .get_mut(&agent_id)
                .expect("agent present: session borrow above proved it")
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
        Err(AgentRegistryError::AlreadyExists(_)) => {
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
