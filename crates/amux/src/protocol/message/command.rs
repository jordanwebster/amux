use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{DebugFormat, HookProvider, ProtocolError, ShutdownReason};
use crate::protocol::agent::Agent;

/// CLI-only commands that must not be accepted from remote peers.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    ListAgents,
    ListAgentsResult {
        agents: Vec<Agent>,
    },
    ResolveAgent {
        identifier: String,
    },
    ResolveAgentResult {
        agent: Option<Agent>,
    },
    Shutdown,
    ShutdownNotification {
        reason: ShutdownReason,
    },
    Debug {
        verbose: bool,
        format: DebugFormat,
    },
    DebugResult {
        dump: String,
    },
    ConnectToServer {
        address: String,
    },
    ConnectToServerResult {
        error: Option<ProtocolError>,
    },
    HandleHook {
        agent_id: Uuid,
        provider: HookProvider,
        #[serde(with = "serde_bytes")]
        payload: Vec<u8>,
        external: bool,
    },
    HandleHookResult {
        error: Option<ProtocolError>,
    },
    Suspend,
    SuspendResult {
        suspended_count: u64,
        error: Option<ProtocolError>,
    },
    Resume,
    ResumeResult {
        resumed_count: u64,
        failed_count: u64,
        error: Option<ProtocolError>,
    },
    #[serde(other)]
    Unknown,
}

impl Command {
    /// Short label for this variant, for use in logs and error messages
    pub fn type_label(&self) -> &'static str {
        match self {
            Command::ListAgents => "ListAgents",
            Command::ListAgentsResult { .. } => "ListAgentsResult",
            Command::ResolveAgent { .. } => "ResolveAgent",
            Command::ResolveAgentResult { .. } => "ResolveAgentResult",
            Command::Shutdown => "Shutdown",
            Command::ShutdownNotification { .. } => "ShutdownNotification",
            Command::Debug { .. } => "Debug",
            Command::DebugResult { .. } => "DebugResult",
            Command::ConnectToServer { .. } => "ConnectToServer",
            Command::ConnectToServerResult { .. } => "ConnectToServerResult",
            Command::HandleHook { .. } => "HandleHook",
            Command::HandleHookResult { .. } => "HandleHookResult",
            Command::Suspend => "Suspend",
            Command::SuspendResult { .. } => "SuspendResult",
            Command::Resume => "Resume",
            Command::ResumeResult { .. } => "ResumeResult",
            Command::Unknown => "Unknown",
        }
    }
}
