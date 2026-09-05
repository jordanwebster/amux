//! Session-owned choices shared by native clients. Unknown choices stay absent.
pub use amux::codex_io::{ApprovalPolicy, SandboxPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AgentId, AgentKind, ClaudeDriver, Command, Effect, Model, OpId, codex};

pub type ModelId = String;
pub type Effort = String;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFacts {
    pub model: Option<ModelId>,
    pub effort: Option<Effort>,
    pub models: Vec<ModelInfo>,
    pub efforts: Vec<Effort>,
    pub permission: PermissionFacts,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: ModelId,
    pub name: String,
    pub efforts: Vec<Effort>,
    pub default_effort: Option<Effort>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum PermissionFacts {
    #[default]
    Unavailable,
    Claude {
        mode: Option<String>,
    },
    Codex {
        approval: Value,
        sandbox: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "gate", rename_all = "snake_case")]
pub enum SettingsGate {
    Ready,
    PtySettingsUnavailable,
    Unavailable,
    Codex { reason: codex::SendGate },
}
impl SettingsGate {
    pub fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::PtySettingsUnavailable => {
                Some("model, effort and preset changes are unavailable for Claude PTY sessions")
            }
            Self::Unavailable => Some("provider settings unavailable for this agent"),
            Self::Codex { reason } => reason.refusal(),
        }
    }
}
pub fn settings_gate(model: &Model, agent: AgentId) -> SettingsGate {
    match model.agent(agent).map(|card| card.agent.kind) {
        Some(AgentKind::Claude {
            driver: ClaudeDriver::Pty,
        }) => SettingsGate::PtySettingsUnavailable,
        Some(AgentKind::Codex) => match codex::send_gate(model, agent) {
            codex::SendGate::Ready => SettingsGate::Ready,
            reason => SettingsGate::Codex { reason },
        },
        _ => SettingsGate::Unavailable,
    }
}
pub fn facts(model: &Model, agent: AgentId) -> ProviderFacts {
    if let Some(layer) = model.codex(agent) {
        return layer.provider_facts().clone();
    }
    if let Some(layer) = model.claude(agent) {
        return ProviderFacts {
            permission: PermissionFacts::Claude {
                mode: layer.session().permission_mode.clone(),
            },
            ..ProviderFacts::default()
        };
    }
    ProviderFacts::default()
}

impl ProviderFacts {
    pub(crate) fn observe_codex(&mut self, session: &Value) {
        self.model = session
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.effort = session
            .get("effort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.models = session
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                Some(ModelInfo {
                    id: item.get("model")?.as_str()?.to_owned(),
                    name: item.get("displayName")?.as_str()?.to_owned(),
                    efforts: item
                        .get("supportedReasoningEfforts")?
                        .as_array()?
                        .iter()
                        .filter_map(|level| {
                            level.get("reasoningEffort")?.as_str().map(str::to_owned)
                        })
                        .collect(),
                    default_effort: item
                        .get("defaultReasoningEffort")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            })
            .collect();
        self.efforts = self
            .models
            .iter()
            .find(|item| Some(&item.id) == self.model.as_ref())
            .map(|item| item.efforts.clone())
            .unwrap_or_default();
        self.permission = PermissionFacts::Codex {
            approval: session["approvalPolicy"].clone(),
            sandbox: session["sandbox"].clone(),
        };
    }
}

pub(crate) fn update_settings(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
) -> Vec<Effect> {
    let (agent, input) = match &command {
        Command::SetModel { agent, model } => (
            *agent,
            codex::CodexInput::SetModel {
                model: model.clone(),
            },
        ),
        Command::SetEffort { agent, effort } => (
            *agent,
            codex::CodexInput::SetEffort {
                effort: effort.clone(),
            },
        ),
        Command::SetPreset {
            agent,
            approval,
            sandbox,
        } => (
            *agent,
            codex::CodexInput::SetPreset {
                approval: approval.clone(),
                sandbox: sandbox.clone(),
            },
        ),
        _ => unreachable!("settings commands only"),
    };
    if let Some(reason) = settings_gate(model, agent).refusal() {
        return crate::update::refuse(model, op, seq, command, reason);
    }
    let facts = facts(model, agent);
    let invalid = match &input {
        codex::CodexInput::SetModel { model }
            if !facts.models.iter().any(|item| &item.id == model) =>
        {
            Some("model is not offered by this session")
        }
        codex::CodexInput::SetEffort { effort } if !facts.efforts.contains(effort) => {
            Some("effort is not offered by this session's model")
        }
        _ => None,
    };
    if let Some(reason) = invalid {
        return crate::update::refuse(model, op, seq, command, reason);
    }
    codex::update::dispatch_codex_input(
        model,
        op,
        seq,
        command,
        agent,
        input,
        codex::InFlightKind::Settings,
    )
}
