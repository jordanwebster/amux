//! Codex-native command reduction.

use serde_json::{Value, json};

use super::{
    CodexCommand, CodexDecision, CodexInput, CodexLayer, InFlightInput, InFlightKind, SendGate,
};
use crate::effect::InputPayload;
use crate::model::{AgentLayer, Model};
use crate::msg::{Command, OpError, OpId};
use crate::update::{dispatch_input, refuse};

pub(crate) fn update_command(
    model: &mut Model,
    op: OpId,
    seq: u64,
    native: CodexCommand,
) -> Vec<crate::Effect> {
    let command = Command::Codex(native.clone());
    match native {
        CodexCommand::Prompt { agent, text } => {
            let gate = super::send_gate(model, agent);
            if !gate.allows_prompt() {
                return refuse_gate(model, op, seq, command, gate);
            }
            let input = prompt_input(&text);
            dispatch_codex_input(
                model,
                op,
                seq,
                command,
                agent,
                CodexInput::UserTurn { input },
                InFlightKind::Prompt,
            )
        }
        CodexCommand::Steer { agent, text } => {
            let gate = super::send_gate(model, agent);
            if !gate.allows_steer() {
                return refuse_action_gate(
                    model,
                    op,
                    seq,
                    command,
                    gate,
                    "cannot steer without an active turn",
                );
            }
            let Some(turn_id) = model
                .codex(agent)
                .and_then(|layer| layer.active_turn_id())
                .map(str::to_owned)
            else {
                return refuse(
                    model,
                    op,
                    seq,
                    command,
                    "cannot steer without an active turn",
                );
            };
            let input = prompt_input(&text);
            dispatch_codex_input(
                model,
                op,
                seq,
                command,
                agent,
                CodexInput::Steer {
                    turn_id: turn_id.clone(),
                    input,
                },
                InFlightKind::Steer { turn_id, text },
            )
        }
        CodexCommand::Answer {
            agent,
            request_id,
            decision,
        } => update_answer(model, op, seq, command, agent, request_id, decision),
        CodexCommand::Interrupt { agent } => {
            let gate = super::send_gate(model, agent);
            if !gate.allows_interrupt() {
                return refuse_action_gate(
                    model,
                    op,
                    seq,
                    command,
                    gate,
                    "cannot interrupt without an active turn",
                );
            }
            let Some(turn_id) = model
                .codex(agent)
                .and_then(|layer| layer.active_turn_id())
                .map(str::to_owned)
            else {
                return refuse(
                    model,
                    op,
                    seq,
                    command,
                    "cannot interrupt without an observed active turn",
                );
            };
            dispatch_codex_input(
                model,
                op,
                seq,
                command,
                agent,
                CodexInput::Interrupt {
                    turn_id: turn_id.clone(),
                },
                InFlightKind::Interrupt { turn_id },
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn update_answer(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    request_id: Value,
    decision: CodexDecision,
) -> Vec<crate::Effect> {
    let gate = super::send_gate(model, agent);
    if !gate.allows_answer() {
        return refuse_action_gate(
            model,
            op,
            seq,
            command,
            gate,
            "cannot answer without a blocking Codex request",
        );
    }
    let Some(layer) = model.codex(agent) else {
        return refuse_gate(model, op, seq, command, SendGate::Unavailable);
    };
    let Some(ask) = layer.asks().find(|ask| ask.request_id == request_id) else {
        return refuse(model, op, seq, command, "ask already resolved");
    };
    if !ask
        .actions
        .iter()
        .any(|action| action.decision == Some(decision))
    {
        return refuse(
            model,
            op,
            seq,
            command,
            "decision is not offered by this Codex request",
        );
    }
    if layer.in_flight_inputs().any(|input| {
        matches!(&input.kind, InFlightKind::Answer { request_id: pending, .. } if *pending == request_id)
    }) {
        return refuse(model, op, seq, command, "answer already in flight");
    }
    let request_bytes = serde_json::to_vec(&request_id).expect("JSON values always serialize");
    dispatch_codex_input(
        model,
        op,
        seq,
        command,
        agent,
        CodexInput::ApprovalDecision {
            request_id: request_bytes,
            decision: decision.wire_value().to_string(),
        },
        InFlightKind::Answer {
            request_id,
            decision,
        },
    )
}

/// Refuse with the gate's own words, so a refusal and the rendered gate can
/// never state different reasons for the same fact.
fn refuse_gate(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    gate: SendGate,
) -> Vec<crate::Effect> {
    let message = gate.refusal().expect("a non-ready gate states its refusal");
    refuse(model, op, seq, command, message)
}

fn refuse_action_gate(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    gate: SendGate,
    ready_message: &'static str,
) -> Vec<crate::Effect> {
    match gate.refusal() {
        Some(message) => refuse(model, op, seq, command, message),
        None => refuse(model, op, seq, command, ready_message),
    }
}

fn dispatch_codex_input(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    input: CodexInput,
    kind: InFlightKind,
) -> Vec<crate::Effect> {
    with_existing_layer(model, agent, |layer| {
        layer.note_input(InFlightInput {
            op,
            input_id: op.0.as_bytes().to_vec(),
            kind,
        });
    });
    dispatch_input(
        model,
        op,
        seq,
        command,
        agent,
        InputPayload::Codex { payload: input },
    )
}

fn prompt_input(text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!([{ "type": "text", "text": text }]))
        .expect("prompt input is always serializable")
}

pub(crate) fn update_failed_command(
    model: &mut Model,
    op: OpId,
    command: &CodexCommand,
    _error: &OpError,
) {
    let agent = match command {
        CodexCommand::Prompt { agent, .. }
        | CodexCommand::Steer { agent, .. }
        | CodexCommand::Answer { agent, .. }
        | CodexCommand::Interrupt { agent } => *agent,
    };
    with_existing_layer(model, agent, |layer| {
        layer.note_input_send_failed(op);
    });
}

fn with_existing_layer(
    model: &mut Model,
    agent: amux::AgentId,
    step: impl FnOnce(&mut CodexLayer),
) {
    let stream_phase = model.streams.get(&agent).map(|stream| &stream.phase);
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    let Some(layer) = card.layer.as_mut().and_then(AgentLayer::codex_mut) else {
        return;
    };
    step(layer);
    card.attention = super::projected_attention(layer, stream_phase);
}
