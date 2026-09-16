//! The local agent registry, owned by [`super::PtyAgentHost`].
//!
//! Holds the live sessions plus the three event sources the runtime emits
//! into (agent up/down, session close, server shutdown). Compiled only with
//! the agent runtime; the rest of the core reaches it through the
//! [`super::LocalAgentHost`] seam.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use model::ShutdownReason;
use model::envelope::Envelope;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agents::{
    AgentDeps, AgentEvent, AgentRecord, AgentSession, SessionCloseReason, WorkingOn,
};
use crate::events::EventSource;

pub(crate) type SharedAgentServiceState = Arc<RwLock<AgentServiceState>>;

pub(crate) struct AgentServiceState {
    pub(crate) local_agents: HashMap<Uuid, LocalAgentContext>,
    pub(crate) local_agent_events: EventSource<AgentEvent>,
    pub(crate) local_session_close_events: EventSource<(Uuid, SessionCloseReason)>,
    pub(crate) local_shutdown_events: EventSource<ShutdownReason>,
    pub(crate) outbound_envelopes: EventSource<Envelope>,
    pub(crate) deps: AgentDeps,
    pub(crate) recent_projects: crate::repositories::RecentProjects,
}

pub(crate) struct LocalAgentContext {
    pub(crate) session: AgentSession,
    pub(crate) working_on: Option<WorkingOn>,
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
    pub(crate) fn new(deps: AgentDeps) -> Self {
        Self {
            local_agents: HashMap::new(),
            local_agent_events: EventSource::default(),
            local_session_close_events: EventSource::default(),
            local_shutdown_events: EventSource::default(),
            outbound_envelopes: EventSource::default(),
            recent_projects: crate::repositories::RecentProjects::load(&deps.data_dir),
            deps,
        }
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

    pub(crate) fn insert_registered_local_agent(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
    ) -> Result<AgentEvent, String> {
        self.register_local_agent_context(host_id, agent_id, session)
    }

    pub(crate) fn register_local_agent_context(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
    ) -> Result<AgentEvent, String> {
        self.register_local_agent_context_with_status(host_id, agent_id, session, None, None)
    }

    pub(crate) fn register_local_agent_context_with_status(
        &mut self,
        host_id: Uuid,
        agent_id: Uuid,
        session: AgentSession,
        working_on: Option<WorkingOn>,
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

        let mut context = LocalAgentContext {
            session,
            working_on,
            remembered_activity,
            published_activity: DateTime::<Utc>::MIN_UTC,
        };
        let record = context.record(host_id);
        context.published_activity = record.last_activity;
        self.recent_projects
            .record(&record.working_dir, record.created_at);
        let event = record.agent_event();
        self.local_agents.insert(agent_id, context);
        Ok(event)
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
