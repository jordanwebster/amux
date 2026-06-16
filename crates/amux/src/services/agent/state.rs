//! The local agent registry, owned by [`super::PtyAgentHost`].
//!
//! Holds the live sessions plus the three event sources the runtime emits
//! into (agent up/down, session close, server shutdown). Compiled only with
//! `local-agents`; the rest of the core reaches it through the
//! [`super::LocalAgentHost`] seam.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agents::{AgentEvent, AgentRecord, AgentSession, SessionCloseReason};
use crate::routing::EventSource;
use crate::server::ShutdownReason;

pub(crate) type SharedAgentServiceState = Arc<RwLock<AgentServiceState>>;

#[derive(Default)]
pub(crate) struct AgentServiceState {
    pub(crate) local_agents: HashMap<Uuid, LocalAgentContext>,
    pub(crate) local_agent_events: EventSource<AgentEvent>,
    pub(crate) local_session_close_events: EventSource<(Uuid, SessionCloseReason)>,
    pub(crate) local_shutdown_events: EventSource<ShutdownReason>,
}

pub(crate) struct LocalAgentContext {
    pub(crate) session: AgentSession,
}

impl AgentServiceState {
    pub(crate) fn new() -> Self {
        Self::default()
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
            .map(|context| context.session.to_agent(host_id))
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
        if self.contains_agent_id(&agent_id) {
            return Err(format!("Agent already exists: {agent_id}"));
        }
        if let Some(name) = session.name()
            && self.name_taken_by_other(name, agent_id)
        {
            return Err(format!("Agent already exists: {name}"));
        }

        let event = session.to_agent(host_id).agent_event();
        self.local_agents
            .insert(agent_id, LocalAgentContext { session });
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
