//! SDK writes are gated by the same session condition as the visible phase.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AskState, ClaudeSdkCommand, ClaudeSdkInput, ClaudeSdkLayer, SdkPhase, SendGate};
use crate::model::AgentLayer;
use crate::update::{dispatch_input, refuse};
use crate::{Command, Effect, InputPayload, Model, OpError, OpId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InFlightInput {
    pub op: OpId,
    pub command: ClaudeSdkCommand,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptEcho {
    pub op: OpId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputFailure {
    pub op: OpId,
    pub command: ClaudeSdkCommand,
    pub message: String,
}

pub(crate) fn update_command(
    model: &mut Model,
    op: OpId,
    seq: u64,
    native: ClaudeSdkCommand,
) -> Vec<Effect> {
    let agent = native.agent();
    let command = Command::ClaudeSdk(native.clone());
    let gate = super::send_gate(model, agent);
    let allowed = match &native {
        ClaudeSdkCommand::SendPrompt { .. } => gate == SendGate::Ready,
        ClaudeSdkCommand::AnswerAsk { .. } => gate == SendGate::NeedsYou,
        ClaudeSdkCommand::Interrupt { .. } => {
            matches!(gate, SendGate::Working | SendGate::NeedsYou)
        }
        ClaudeSdkCommand::CyclePermissionMode { .. }
        | ClaudeSdkCommand::SetModel { .. }
        | ClaudeSdkCommand::SetEffort { .. }
        | ClaudeSdkCommand::SetPermissionMode { .. }
        | ClaudeSdkCommand::RequestContextBreakdown { .. } => matches!(
            gate,
            SendGate::Ready | SendGate::Working | SendGate::NeedsYou
        ),
    };
    if !allowed {
        return refuse(
            model,
            op,
            seq,
            command,
            gate.refusal()
                .unwrap_or("command is unavailable in this session state"),
        );
    }
    let input = match &native {
        ClaudeSdkCommand::SendPrompt { text, .. } => {
            if text.trim().is_empty() {
                return refuse(model, op, seq, command, "prompt must not be empty");
            }
            ClaudeSdkInput::Prompt { text: text.clone() }
        }
        ClaudeSdkCommand::AnswerAsk { ask, answer, .. } => {
            let layer = model.claude_sdk(agent).expect("gate requires layer");
            let Some(entry) = layer.asks().find(|entry| entry.id == *ask) else {
                return refuse(model, op, seq, command, "ask already resolved");
            };
            if layer.ask_head().is_some_and(|head| head.id != *ask) {
                return refuse(model, op, seq, command, "answer the head ask first");
            }
            if matches!(entry.state, AskState::AnsweredOptimistic { .. }) {
                return refuse(
                    model,
                    op,
                    seq,
                    command,
                    "answer already in flight — awaiting confirmation",
                );
            }
            match super::answer::encode(entry, answer) {
                Ok(input) => input,
                Err(message) => return refuse(model, op, seq, command, &message),
            }
        }
        ClaudeSdkCommand::Interrupt { .. } => {
            if !matches!(
                super::phase(model, agent),
                SdkPhase::Working | SdkPhase::NeedsYou { .. }
            ) {
                return refuse(model, op, seq, command, "nothing to interrupt");
            }
            ClaudeSdkInput::Interrupt
        }
        ClaudeSdkCommand::CyclePermissionMode { .. } => {
            let mode = match model
                .claude_sdk(agent)
                .and_then(|l| l.session.permission_mode.as_deref())
            {
                Some("default") => "acceptEdits",
                Some("acceptEdits") => "plan",
                Some("plan" | "bypassPermissions" | "dontAsk" | "auto") => "default",
                _ => return refuse(model, op, seq, command, "permission mode is unknown"),
            };
            ClaudeSdkInput::SetPermissionMode { mode: mode.into() }
        }
        ClaudeSdkCommand::SetModel {
            model: selection, ..
        } => {
            if selection.as_ref().is_some_and(|s| s.trim().is_empty()) {
                return refuse(model, op, seq, command, "model must not be empty");
            }
            ClaudeSdkInput::SetModel {
                model: selection.clone(),
            }
        }
        ClaudeSdkCommand::SetEffort { effort, .. } => ClaudeSdkInput::SetEffort {
            effort: effort.clone(),
        },
        ClaudeSdkCommand::SetPermissionMode { mode, .. } => {
            ClaudeSdkInput::SetPermissionMode { mode: mode.clone() }
        }
        ClaudeSdkCommand::RequestContextBreakdown { .. } => ClaudeSdkInput::RequestContextBreakdown,
    };
    if input.clone().into_native().is_err() {
        return refuse(model, op, seq, command, "invalid Claude SDK input value");
    }
    with_layer(model, agent, |layer| {
        layer.last_input_failure = None;
        layer.in_flight = Some(InFlightInput {
            op,
            command: native.clone(),
        });
        match &native {
            ClaudeSdkCommand::SendPrompt { text, .. } => {
                layer.echo = Some(PromptEcho {
                    op,
                    text: text.clone(),
                })
            }
            ClaudeSdkCommand::AnswerAsk { ask, answer, .. } => {
                if let Some(entry) = layer.asks.iter_mut().find(|entry| entry.id == *ask) {
                    entry.state = AskState::AnsweredOptimistic {
                        op,
                        answer: answer.clone(),
                    };
                }
            }
            _ => {}
        }
    });
    dispatch_input(
        model,
        op,
        seq,
        command,
        agent,
        InputPayload::ClaudeSdk { payload: input },
    )
}

pub(crate) fn update_failed_command(
    model: &mut Model,
    op: OpId,
    command: &ClaudeSdkCommand,
    error: &OpError,
) {
    with_layer(model, command.agent(), |layer| {
        layer.fail_input(op, error.message())
    });
}

fn with_layer(model: &mut Model, agent: amux::AgentId, step: impl FnOnce(&mut ClaudeSdkLayer)) {
    let stream = model.streams.get(&agent).map(|s| &s.phase);
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    let Some(AgentLayer::ClaudeSdk(layer)) = card.layer.as_mut() else {
        return;
    };
    step(layer);
    card.attention = layer.attention(stream);
}

impl ClaudeSdkLayer {
    pub fn in_flight_input(&self) -> Option<&InFlightInput> {
        self.in_flight.as_ref()
    }
    pub fn pending_echo(&self) -> Option<&PromptEcho> {
        self.echo.as_ref()
    }
    pub fn last_input_failure(&self) -> Option<&InputFailure> {
        self.last_input_failure.as_ref()
    }

    fn fail_input(&mut self, op: OpId, message: String) {
        let Some(input) = self.in_flight.take_if(|input| input.op == op) else {
            return;
        };
        if self.echo.as_ref().is_some_and(|echo| echo.op == op) {
            self.echo = None;
        }
        for ask in &mut self.asks {
            if matches!(&ask.state, AskState::AnsweredOptimistic { op: pending, .. } if *pending == op)
            {
                ask.state = AskState::SendFailed {
                    message: message.clone(),
                };
            }
        }
        self.last_input_failure = Some(InputFailure {
            op,
            command: input.command,
            message,
        });
    }

    pub(super) fn observe_input(&mut self, row: &Value) {
        // Authoritative rows can beat the RPC acknowledgement. Once accepted,
        // a late transport failure must not restore a prompt or answer draft.
        if let Some(input) = &self.in_flight {
            let resolved = match &input.command {
                ClaudeSdkCommand::SendPrompt { .. } => {
                    row["type"] == "user"
                        && row["parent_tool_use_id"].is_null()
                        && row["uuid"]
                            .as_str()
                            .and_then(|id| uuid::Uuid::parse_str(id).ok())
                            == Some(input.op.0)
                }
                ClaudeSdkCommand::AnswerAsk { ask, .. } => self.asks.iter().any(|entry| {
                    entry.id == *ask
                        && row["request_id"] == entry.request_id
                        && row["type"] == format!("amux.claude_sdk.{}_resolved", entry.channel())
                }),
                _ => false,
            };
            if resolved {
                self.in_flight = None;
            }
        }
        match row["type"].as_str().unwrap_or("") {
            "amux.claude_sdk.input_result" => {
                let Some(input) = &self.in_flight else {
                    return;
                };
                if row["input_id"]
                    != serde_json::to_value(input.op.0.as_bytes().as_slice()).expect("byte array")
                {
                    return;
                }
                if let Some(outcome) = row["outcome"].as_str() {
                    if outcome == "ok" {
                        self.in_flight = None;
                    } else {
                        self.fail_input(input.op, outcome.to_owned());
                    }
                }
            }
            "user"
                if row["parent_tool_use_id"].is_null()
                    && self.echo.as_ref().is_some_and(|echo| {
                        row["uuid"]
                            .as_str()
                            .and_then(|id| uuid::Uuid::parse_str(id).ok())
                            == Some(echo.op.0)
                    }) =>
            {
                self.echo = None;
            }
            _ => {}
        }
    }

    pub(super) fn clear_inputs(&mut self, message: &str) {
        if let Some(input) = &self.in_flight {
            self.fail_input(input.op, message.into());
        }
        self.echo = None;
        // Transport acceptance is not ask resolution. A lost stream permits
        // retry only after the replay proves the request is still pending.
        for ask in &mut self.asks {
            if matches!(ask.state, AskState::AnsweredOptimistic { .. }) {
                ask.state = AskState::SendFailed {
                    message: message.into(),
                };
            }
        }
    }
}
