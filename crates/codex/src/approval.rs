use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::CommandAction;

/// A JSON-RPC request ID supplied by the app-server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

// ── ApprovalRequest ───────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ApprovalRequest {
    CommandExecution {
        thread_id: String,
        turn_id: String,
        item_id: String,
        request_id: RequestId,
        approval_id: Option<String>,
        command: Vec<String>,
        cwd: Option<PathBuf>,
        reason: Option<String>,
        command_actions: Vec<CommandAction>,
        network_approval: Option<NetworkApprovalContext>,
        additional_permissions: Option<Box<AdditionalPermissionProfile>>,
        skill_metadata: Option<CommandExecutionApprovalSkillMetadata>,
        proposed_execpolicy_amendment: Option<Vec<String>>,
        proposed_network_policy_amendments: Vec<NetworkPolicyAmendment>,
        available_decisions: Vec<CommandExecutionApprovalDecision>,
    },
    FileChange {
        thread_id: String,
        turn_id: String,
        item_id: String,
        request_id: RequestId,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    },
    Permissions {
        thread_id: String,
        turn_id: String,
        item_id: String,
        request_id: RequestId,
        reason: Option<String>,
        permissions: RequestPermissionProfile,
    },
}

impl ApprovalRequest {
    pub fn request_id(&self) -> RequestId {
        match self {
            Self::CommandExecution { request_id, .. }
            | Self::FileChange { request_id, .. }
            | Self::Permissions { request_id, .. } => request_id.clone(),
        }
    }

    pub fn thread_id(&self) -> &str {
        match self {
            Self::CommandExecution { thread_id, .. }
            | Self::FileChange { thread_id, .. }
            | Self::Permissions { thread_id, .. } => thread_id,
        }
    }

    pub(crate) fn turn_id(&self) -> &str {
        match self {
            Self::CommandExecution { turn_id, .. }
            | Self::FileChange { turn_id, .. }
            | Self::Permissions { turn_id, .. } => turn_id,
        }
    }
}

// ── ApprovalResponse ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ApprovalResponse {
    Accept,
    AcceptForSession,
    AcceptWithExecpolicyAmendment {
        execpolicy_amendment: Vec<String>,
    },
    ApplyNetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },
    Decline,
    Cancel,
}

impl ApprovalResponse {
    /// Convert to the wire-format JSON result value.
    /// v2 protocol expects `{"decision": "accept"}` etc.
    pub(crate) fn to_wire_value(&self) -> serde_json::Value {
        match self {
            Self::Accept => serde_json::json!({ "decision": "accept" }),
            Self::AcceptForSession => {
                serde_json::json!({ "decision": "acceptForSession" })
            }
            Self::AcceptWithExecpolicyAmendment {
                execpolicy_amendment,
            } => serde_json::json!({
                "decision": {
                    "acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": execpolicy_amendment
                    }
                }
            }),
            Self::ApplyNetworkPolicyAmendment {
                network_policy_amendment,
            } => serde_json::json!({
                "decision": {
                    "applyNetworkPolicyAmendment": {
                        "network_policy_amendment": network_policy_amendment
                    }
                }
            }),
            Self::Decline => serde_json::json!({ "decision": "decline" }),
            Self::Cancel => serde_json::json!({ "decision": "cancel" }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: NetworkApprovalProtocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkApprovalProtocol {
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "https")]
    Https,
    #[serde(rename = "socks5Tcp")]
    Socks5Tcp,
    #[serde(rename = "socks5Udp")]
    Socks5Udp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalPermissionProfile {
    pub network: Option<AdditionalNetworkPermissions>,
    #[serde(rename = "fileSystem")]
    pub file_system: Option<AdditionalFileSystemPermissions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalNetworkPermissions {
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdditionalFileSystemPermissions {
    pub read: Option<Vec<PathBuf>>,
    pub write: Option<Vec<PathBuf>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionApprovalSkillMetadata {
    pub path_to_skills_md: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandExecutionApprovalDecision {
    Named(CommandExecutionApprovalDecisionKind),
    AcceptWithExecpolicyAmendment {
        #[serde(rename = "acceptWithExecpolicyAmendment")]
        accept_with_execpolicy_amendment: ExecPolicyAmendmentChoice,
    },
    ApplyNetworkPolicyAmendment {
        #[serde(rename = "applyNetworkPolicyAmendment")]
        apply_network_policy_amendment: NetworkPolicyAmendmentChoice,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandExecutionApprovalDecisionKind {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecPolicyAmendmentChoice {
    pub execpolicy_amendment: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendmentChoice {
    pub network_policy_amendment: NetworkPolicyAmendment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendment {
    pub host: String,
    pub action: NetworkPolicyRuleAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NetworkPolicyRuleAction {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionProfile {
    pub network: Option<AdditionalNetworkPermissions>,
    #[serde(rename = "fileSystem")]
    pub file_system: Option<AdditionalFileSystemPermissions>,
}
