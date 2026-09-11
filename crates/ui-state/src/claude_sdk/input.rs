//! Serializable client intents and their SDK-native payloads.

use model::AgentId;
pub use model::{ClaudeSdkInput, PermissionAnswer, PlanAnswer, QuestionAnswer};
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
