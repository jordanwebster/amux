use crate::agent;
use crate::protocol::Route;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Wire-format agent DTO used in protocol command responses.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Agent {
    pub id: Uuid,
    pub host_id: Uuid,
    pub name: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
    pub route: Route,
    pub agent_type: String,
    pub structured_protocol: Option<String>,
    pub readonly: bool,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl Agent {
    pub fn is_remote(&self) -> bool {
        self.route.peek().is_some()
    }
}

impl From<agent::Agent> for Agent {
    fn from(agent: agent::Agent) -> Self {
        Self {
            id: agent.id,
            host_id: agent.host_id,
            name: agent.name,
            command: agent.command,
            working_dir: agent.working_dir,
            route: agent.route,
            agent_type: agent.agent_type,
            structured_protocol: agent.structured_protocol,
            readonly: agent.readonly,
            args: agent.args,
            created_at: agent.created_at,
        }
    }
}
