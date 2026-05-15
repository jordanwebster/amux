use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::Route;

/// Wire-format agent DTO used in protocol command responses.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: Uuid,
    pub host_id: Uuid,
    pub name: Option<String>,
    pub command: String,
    pub working_dir: PathBuf,
    pub agent_type: String,
    #[serde(default)]
    pub io_protocols: Vec<String>,
    pub readonly: bool,
    pub args: Vec<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AgentEntry {
    pub agent: Agent,
    pub route: Route,
}

impl AgentEntry {
    pub fn is_remote(&self) -> bool {
        self.route.peek().is_some()
    }
}
