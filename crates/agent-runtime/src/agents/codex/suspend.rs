use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::agents::AgentParent;
use crate::suspend::SuspendedAgent;

pub(super) struct CodexSuspendRecord {
    pub agent_id: Uuid,
    pub name: Option<String>,
    pub working_dir: PathBuf,
    pub model: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_policy: Option<String>,
    pub thread_id: String,
    pub daemon_mode: Option<String>,
    pub created_at: DateTime<Utc>,
    pub parent: Option<AgentParent>,
}

impl From<CodexSuspendRecord> for SuspendedAgent {
    fn from(record: CodexSuspendRecord) -> Self {
        Self::Codex {
            agent_id: record.agent_id,
            name: record.name,
            working_dir: record.working_dir,
            model: record.model,
            approval_policy: record.approval_policy,
            sandbox_policy: record.sandbox_policy,
            thread_id: record.thread_id,
            daemon_mode: record.daemon_mode,
            created_at: record.created_at,
            parent: record.parent,
            working_on: None,
        }
    }
}
