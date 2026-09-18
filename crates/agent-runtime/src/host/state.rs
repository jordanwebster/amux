//! The local agent registry, owned by [`super::PtyAgentHost`].
//!
//! Holds the live sessions plus the three event sources the runtime emits
//! into (agent up/down, session close, server shutdown). Compiled only with
//! the agent runtime; the rest of the core reaches it through the
//! [`super::LocalAgentHost`] seam.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use model::ShutdownReason;
use model::envelope::Envelope;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agents::{
    AgentDeps, AgentEvent, AgentRecord, AgentSession, SessionCloseReason, SummarizerHandle,
    SummarizerPublication, WorkingOn,
};
use crate::events::EventSource;
use crate::host::revision::InventoryRevisions;

pub(crate) type SharedAgentServiceState = Arc<RwLock<AgentServiceState>>;

pub(crate) struct AgentServiceState {
    pub(crate) local_agents: HashMap<Uuid, LocalAgentContext>,
    pub(crate) local_agent_events: EventSource<AgentEvent>,
    pub(crate) local_session_close_events: EventSource<(Uuid, SessionCloseReason)>,
    pub(crate) local_shutdown_events: EventSource<ShutdownReason>,
    pub(crate) outbound_envelopes: EventSource<Envelope>,
    pub(crate) deps: AgentDeps,
    pub(crate) recent_projects: crate::repositories::RecentProjects,
    pub(crate) summarizer_publications:
        Option<tokio::sync::mpsc::UnboundedSender<SummarizerPublication>>,
    inventory_revisions: InventoryRevisions,
}

pub(crate) struct LocalAgentContext {
    pub(crate) session: AgentSession,
    pub(crate) working_on: Option<WorkingOn>,
    pub(crate) inventory_revision: u64,
    pub(crate) summarizer: Option<SummarizerHandle>,
    pub(crate) summary: Option<model::SummaryEnvelope>,
    pub(crate) progress: Option<model::Progress>,
    /// The last activity a previous daemon recorded for this agent. A resumed
    /// session starts with an empty log, and without this every restarted
    /// agent would report its creation time as its last activity.
    pub(crate) remembered_activity: Option<DateTime<Utc>>,
    /// The last activity announced to inventory subscribers, so the activity
    /// publisher re-announces only agents whose activity has moved since.
    pub(crate) published_activity: DateTime<Utc>,
}

impl LocalAgentContext {
    pub(crate) fn record(&self, host_id: Uuid) -> AgentRecord {
        let mut record = self.session.to_agent(host_id);
        record.working_on.clone_from(&self.working_on);
        record.inventory_revision = self.inventory_revision;
        record.summary.clone_from(&self.summary);
        record.progress.clone_from(&self.progress);
        record.last_activity = self.last_activity();
        record
    }

    /// When this agent last did anything: the latest of its creation, what a
    /// previous daemon remembered, and what this session has seen.
    pub(crate) fn last_activity(&self) -> DateTime<Utc> {
        let session = self.session.active_at();
        self.remembered_activity
            .map_or(session, |remembered| remembered.max(session))
    }
}

impl AgentServiceState {
    #[cfg(test)]
    pub(crate) fn new(deps: AgentDeps) -> Self {
        let host_id = deps.mcp_launch_route.host_id();
        let state_path = deps.data_dir.join(format!("state-{host_id}.yaml"));
        Self::new_with_revision_path(deps, &state_path, host_id)
            .expect("test inventory revision store should open")
    }

    pub(crate) fn new_with_revision_path(
        deps: AgentDeps,
        state_path: &Path,
        host_id: Uuid,
    ) -> io::Result<Self> {
        Ok(Self {
            local_agents: HashMap::new(),
            local_agent_events: EventSource::default(),
            local_session_close_events: EventSource::default(),
            local_shutdown_events: EventSource::default(),
            outbound_envelopes: EventSource::default(),
            recent_projects: crate::repositories::RecentProjects::load(&deps.data_dir),
            summarizer_publications: None,
            inventory_revisions: InventoryRevisions::open(state_path, host_id)?,
            deps,
        })
    }

    /// Number of locally-hosted agents.
    pub(crate) fn local_agent_count(&self) -> usize {
        self.local_agents.len()
    }

    pub(crate) fn agent_session_mut(&mut self, agent_id: &Uuid) -> Option<&mut AgentSession> {
        self.local_agents
            .get_mut(agent_id)
            .map(|context| &mut context.session)
    }

    pub(crate) fn local_agent_info(&self, host_id: Uuid, agent_id: &Uuid) -> Option<AgentRecord> {
        self.local_agents
            .get(agent_id)
            .map(|context| context.record(host_id))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn insert_registered_local_agent(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
    ) -> Result<AgentEvent, String> {
        self.register_local_agent_context(host_id, agent_id, session)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_local_agent_context(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
    ) -> Result<AgentEvent, String> {
        self.register_local_agent_context_with_status(host_id, agent_id, session, None, None)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn register_local_agent_context_with_status(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
        working_on: Option<WorkingOn>,
        remembered_activity: Option<DateTime<Utc>>,
    ) -> Result<AgentEvent, String> {
        self.register_local_agent_context_with_summarizer(
            host_id,
            agent_id,
            session,
            working_on,
            None,
            remembered_activity,
        )
    }

    pub(crate) fn register_local_agent_context_with_summarizer(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
        working_on: Option<WorkingOn>,
        summarizer: Option<SummarizerHandle>,
        remembered_activity: Option<DateTime<Utc>>,
    ) -> Result<AgentEvent, String> {
        if self.contains_agent_id(&agent_id) {
            return Err(format!("Agent already exists: {agent_id}"));
        }
        if let Some(name) = session.name()
            && self.name_taken_by_other(name, agent_id)
        {
            return Err(format!("Agent already exists: {name}"));
        }

        let revision = self
            .inventory_revisions
            .reserve()
            .map_err(|error| format!("failed to reserve inventory revision: {error}"))?;
        let mut record = session.to_agent(host_id);
        record.working_on.clone_from(&working_on);
        record.inventory_revision = revision;
        record.last_activity = record
            .last_activity
            .max(remembered_activity.unwrap_or(record.created_at));
        self.recent_projects
            .record(&record.working_dir, record.created_at);
        let summary = summarizer.as_ref().map(|handle| {
            let cut = handle.snapshot();
            model::SummaryEnvelope {
                through: cut.through,
                producer_version: cut.producer_version,
                observed_at: cut.observed_at,
                stale: cut.stale,
                revision,
                summary: cut.summary,
            }
        });
        record.summary.clone_from(&summary);
        let event = record.agent_event();
        self.local_agents.insert(
            agent_id,
            LocalAgentContext {
                session,
                working_on,
                inventory_revision: revision,
                summarizer,
                summary,
                progress: None,
                remembered_activity,
                published_activity: record.last_activity,
            },
        );
        Ok(event)
    }

    pub(crate) fn through_inventory_revision(&self) -> u64 {
        self.inventory_revisions.through()
    }

    pub(crate) fn reserve_authoritative_revision(&mut self) -> io::Result<u64> {
        self.inventory_revisions.reserve()
    }

    pub(crate) fn inventory_revision_path(&self) -> &Path {
        self.inventory_revisions.path()
    }

    pub(crate) fn updated_agent_event(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
    ) -> Result<AgentEvent, String> {
        let revision = self
            .inventory_revisions
            .reserve()
            .map_err(|error| format!("failed to reserve inventory revision: {error}"))?;
        let context = self
            .local_agents
            .get_mut(&agent_id)
            .ok_or_else(|| format!("Agent not found: {agent_id}"))?;
        context.inventory_revision = revision;
        Ok(context.record(host_id).agent_updated_event())
    }

    pub(crate) fn down_agent_event(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
    ) -> Result<AgentEvent, String> {
        let inventory_revision = self
            .inventory_revisions
            .reserve()
            .map_err(|error| format!("failed to reserve inventory revision: {error}"))?;
        Ok(AgentEvent::AgentDown {
            host_id,
            agent_id,
            inventory_revision,
        })
    }

    pub(crate) fn contains_agent_id(&self, agent_id: &Uuid) -> bool {
        self.local_agents.contains_key(agent_id)
    }

    pub(crate) fn name_taken_by_other(&self, name: &str, agent_id: Uuid) -> bool {
        self.local_agents.values().any(|context| {
            context.session.agent_id() != agent_id && context.session.name() == Some(name)
        })
    }
}
