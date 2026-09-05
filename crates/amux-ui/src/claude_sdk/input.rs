//! Serializable client intents and their SDK-native payloads.

use amux::AgentId;
pub use amux::claude_io::{PermissionAnswer, PlanAnswer, QuestionAnswer};
use amux::claude_sdk_io::ClaudeSdkV1Input;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "claude_sdk_command", rename_all = "snake_case")]
pub enum ClaudeSdkCommand {
    SendPrompt {
        agent: AgentId,
        text: String,
    },
    AnswerAsk {
        agent: AgentId,
        ask: u64,
        answer: SdkAnswer,
    },
    Interrupt {
        agent: AgentId,
    },
    CyclePermissionMode {
        agent: AgentId,
    },
    SetModel {
        agent: AgentId,
        model: Option<String>,
    },
    RequestContextBreakdown {
        agent: AgentId,
    },
}

impl ClaudeSdkCommand {
    pub(crate) fn agent(&self) -> AgentId {
        match self {
            Self::SendPrompt { agent, .. }
            | Self::AnswerAsk { agent, .. }
            | Self::Interrupt { agent }
            | Self::CyclePermissionMode { agent }
            | Self::SetModel { agent, .. }
            | Self::RequestContextBreakdown { agent } => *agent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "answer", content = "value", rename_all = "snake_case")]
pub enum SdkAnswer {
    Permission(PermissionAnswer),
    Plan(PlanAnswer),
    Question(Vec<QuestionAnswer>),
    Elicitation(ElicitationAnswer),
    Dialog(DialogAnswer),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ElicitationAnswer {
    Accept { content: Value },
    Decline,
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DialogAnswer {
    Choose { option: usize },
    Cancel,
}

/// SDK decisions retain provider JSON, including opaque permission rules and
/// dialog results. Conversion validates them against the provider's types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaudeSdkInput {
    Prompt { text: String },
    Interrupt,
    SetPermissionMode { mode: String },
    SetModel { model: Option<String> },
    RequestContextBreakdown,
    PermissionDecision { request_id: String, decision: Value },
    ElicitationDecision { request_id: String, result: Value },
    DialogDecision { request_id: String, result: Value },
}

impl ClaudeSdkInput {
    pub(crate) fn into_native(self) -> Result<ClaudeSdkV1Input, serde_json::Error> {
        Ok(match self {
            Self::Prompt { text } => ClaudeSdkV1Input::Prompt {
                text,
                image_blocks: Vec::new(),
            },
            Self::Interrupt => ClaudeSdkV1Input::Interrupt,
            Self::SetPermissionMode { mode } => ClaudeSdkV1Input::SetPermissionMode {
                mode: serde_json::from_value(Value::String(mode))?,
            },
            Self::SetModel { model } => ClaudeSdkV1Input::SetModel { model },
            Self::RequestContextBreakdown => ClaudeSdkV1Input::RequestContextBreakdown,
            Self::PermissionDecision {
                request_id,
                decision,
            } => ClaudeSdkV1Input::PermissionDecision {
                request_id,
                decision: serde_json::from_value(decision)?,
            },
            Self::ElicitationDecision { request_id, result } => {
                ClaudeSdkV1Input::ElicitationDecision {
                    request_id,
                    result: serde_json::from_value(result)?,
                }
            }
            Self::DialogDecision { request_id, result } => ClaudeSdkV1Input::DialogDecision {
                request_id,
                result: serde_json::from_value(result)?,
            },
        })
    }
}
