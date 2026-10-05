//! What amux sends to Codex's server: requests, the one notification, and
//! answers to the server's requests.
//!
//! An optional field amux leaves out when it has nothing is skipped when
//! `None`, so the bytes written are the bytes amux has always written.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::items::{ToolOutputContent, UserInput};
use crate::macros::{method_enum, string_enum, tagged_enum};
use crate::server::{CommandDecision, PermissionProfile};
use crate::thread::{
    ApprovalsReviewer, AskForApproval, CollaborationMode, ReasoningEffort, SandboxMode,
    SandboxPolicy,
};

method_enum! {
    pub enum ClientRequest {
        "initialize" => Initialize(InitializeParams),
        "thread/start" => ThreadStart(ThreadStartParams),
        "thread/resume" => ThreadResume(ThreadResumeParams),
        "thread/name/set" => ThreadSetName(ThreadSetNameParams),
        "thread/list" => ThreadList(ThreadListParams),
        "thread/compact/start" => ThreadCompactStart(ThreadIdParams),
        "thread/inject_items" => ThreadInjectItems(InjectItemsParams),
        "thread/backgroundTerminals/list" => BackgroundTerminalsList(BackgroundTerminalsListParams),
        "turn/start" => TurnStart(TurnStartParams),
        "turn/steer" => TurnSteer(TurnSteerParams),
        "turn/interrupt" => TurnInterrupt(TurnInterruptParams),
        "model/list" => ModelList(ModelListParams),
        "skills/list" => SkillsList(SkillsListParams),
        "account/read" => AccountRead(AccountReadParams),
    }
}

method_enum! {
    pub enum ClientNotification {
        "initialized" => Initialized(()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    pub version: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_api: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,
    /// Tools amux defines for the thread, as JSON schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSetNameParams {
    pub thread_id: String,
    pub name: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Every filter is written, null when unset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub sort_key: Option<String>,
    #[serde(default)]
    pub model_providers: Option<Vec<String>>,
    #[serde(default)]
    pub source_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub search_term: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIdParams {
    pub thread_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Items recorded into the thread without starting a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectItemsParams {
    pub thread_id: String,
    pub items: Vec<InjectedItem>,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    /// An item in the model's own conversation format.
    pub enum InjectedItem {
        "message" => Message(InjectedMessage),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InjectedMessage {
    pub role: String,
    pub content: Vec<InjectedContent>,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    pub enum InjectedContent {
        "input_text" => InputText(crate::items::TextContent),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalsListParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    /// amux's id for the prompt; Codex echoes it as the user message's
    /// `clientId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    pub expected_turn_id: String,
    pub input: Vec<UserInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwds: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// amux's answer to one of the server's requests. The shapes do not
/// overlap, so a written answer reads back as the variant it was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClientResponse {
    CommandApproval(CommandApprovalResponse),
    PermissionsApproval(PermissionsApprovalResponse),
    ToolCall(ToolCallResponse),
    Elicitation(ElicitationResponse),
    UserInput(UserInputResponse),
}

/// The answer to a command or file change approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandApprovalResponse {
    pub decision: CommandDecision,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionsApprovalResponse {
    pub permissions: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<GrantScope>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum GrantScope {
        Turn = "turn",
        Session = "session",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResponse {
    pub content_items: Vec<ToolOutputContent>,
    pub success: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationResponse {
    pub action: ElicitationAction,
    /// The form's answers; null unless accepted.
    pub content: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Extra>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum ElicitationAction {
        Accept = "accept",
        Decline = "decline",
        Cancel = "cancel",
    }
}

/// Answers keyed by question id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInputResponse {
    pub answers: std::collections::BTreeMap<String, Answer>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub answers: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}
