//! Agent data types shared by the runtime and the rest of the core.
//!
//! These carry no live process state — [`AgentRecord`] is the registry's view
//! of an agent, [`SessionEvent`] is the runtime→service notification, and
//! [`StopPolicy`] selects a stop behavior. They stay compiled in every build
//! (the runtime in [`super::session`] is the part gated behind `local-agents`).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agents::{Agent, AgentEvent, AgentParent, AgentType, LocalAgentNameSource, WorkingOn};

/// Internal agent metadata owned by the runtime.
#[derive(Debug, Clone)]
pub(crate) struct AgentRecord {
    pub(crate) id: Uuid,
    pub(crate) host_id: Uuid,
    pub(crate) name: Option<String>,
    pub(crate) command: String,
    pub(crate) working_dir: PathBuf,
    pub(crate) agent_type: String,
    pub(crate) io_protocols: Vec<String>,
    pub(crate) readonly: bool,
    pub(crate) args: Vec<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) parent: Option<AgentParent>,
    pub(crate) working_on: Option<WorkingOn>,
}

impl AgentRecord {
    pub(crate) fn agent_event(&self) -> AgentEvent {
        AgentEvent::AgentUp {
            agent: Agent::from(self),
        }
    }

    pub(crate) fn agent_updated_event(&self) -> AgentEvent {
        AgentEvent::AgentUpdated {
            agent: Agent::from(self),
        }
    }
}

impl From<&AgentRecord> for Agent {
    fn from(agent: &AgentRecord) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name.clone(),
            command: agent.command.clone(),
            working_dir: agent.working_dir.clone(),
            agent_type: agent.agent_type.clone(),
            io_protocols: agent.io_protocols.clone(),
            readonly: agent.readonly,
            args: agent.args.clone(),
            created_at: agent.created_at,
            parent: agent.parent,
            working_on: agent.working_on.clone(),
        }
    }
}

impl From<AgentRecord> for Agent {
    fn from(agent: AgentRecord) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name,
            command: agent.command,
            working_dir: agent.working_dir,
            agent_type: agent.agent_type,
            io_protocols: agent.io_protocols.clone(),
            readonly: agent.readonly,
            args: agent.args,
            created_at: agent.created_at,
            parent: agent.parent,
            working_on: agent.working_on,
        }
    }
}

/// Events sent from agent sessions to their owning AgentService.
#[derive(Clone)]
pub(crate) enum SessionEvent {
    /// Session ended (agent exited)
    Ended { agent_id: Uuid },
    /// Session created (for post-creation side effects like fork detection)
    Created {
        agent_id: Uuid,
        agent_type: AgentType,
        args: Vec<String>,
    },
    /// A provider discovered a stronger local name candidate for this session.
    NameCandidateChanged {
        agent_id: Uuid,
        name: String,
        source: LocalAgentNameSource,
    },
}

/// Policy for stopping an agent session
pub(crate) enum StopPolicy {
    /// Send interrupt signal (close PTY master)
    Interrupt,
}
