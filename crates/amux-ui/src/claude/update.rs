//! Claude-native command reduction. The kernel dispatches the namespaced
//! command here and knows none of the keystroke, gate, or optimistic state.

use super::{AskState, ClaudeCommand, ClaudeLayer};
use crate::effect::InputPayload;
use crate::model::{AgentLayer, Model};
use crate::msg::{Command, OpError, OpId};
use crate::update::{dispatch_input, refuse};

pub(crate) fn update_command(
    model: &mut Model,
    op: OpId,
    seq: u64,
    native: ClaudeCommand,
) -> Vec<crate::Effect> {
    let command = Command::Claude(native.clone());
    match native {
        ClaudeCommand::SendPrompt { agent, text } => {
            update_send_prompt(model, op, seq, command, agent, text)
        }
        ClaudeCommand::AnswerAsk { agent, ask, answer } => {
            update_answer_ask(model, op, seq, command, agent, ask, answer)
        }
        ClaudeCommand::Interrupt { agent } => update_interrupt(model, op, seq, command, agent),
        ClaudeCommand::CyclePermissionMode { agent } => {
            update_cycle_mode(model, op, seq, command, agent)
        }
    }
}

fn update_send_prompt(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    text: String,
) -> Vec<crate::Effect> {
    if let Some(message) = super::send_gate(model, agent).refusal() {
        return refuse(model, op, seq, command, message);
    }
    let text = super::encoding::normalize_prompt(&text);
    let program = match super::encoding::prompt_program(&text) {
        Ok(program) => program,
        Err(error) => return refuse(model, op, seq, command, &error.to_string()),
    };
    let now = model.now();
    with_existing_layer(model, agent, |layer| {
        layer.note_prompt_sent(op, text, now);
    });
    dispatch_claude_input(model, op, seq, command, agent, program, false)
}

fn update_answer_ask(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    ask: u64,
    answer: super::encoding::AskAnswer,
) -> Vec<crate::Effect> {
    if let Some(message) = super::answer_gate(model, agent) {
        return refuse(model, op, seq, command, message);
    }
    let Some(layer) = model.claude(agent) else {
        return refuse(
            model,
            op,
            seq,
            command,
            "chat input unavailable for this agent",
        );
    };
    let Some(entry) = layer.asks().find(|entry| entry.id == ask) else {
        return refuse(model, op, seq, command, "ask already resolved");
    };
    if layer.ask_head().is_some_and(|head| head.id != ask) {
        return refuse(
            model,
            op,
            seq,
            command,
            "ask is queued behind the current menu — answer the head ask first",
        );
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
    let program = match super::encoding::answer_program(&entry.kind, &answer) {
        Ok(program) => program,
        Err(error) => return refuse(model, op, seq, command, &error.to_string()),
    };
    with_existing_layer(model, agent, |layer| {
        layer.note_ask_answered(ask, op, answer);
    });
    dispatch_claude_input(model, op, seq, command, agent, program, false)
}

fn update_interrupt(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
) -> Vec<crate::Effect> {
    if let Some(message) = super::interrupt_gate(model, agent) {
        return refuse(model, op, seq, command, message);
    }
    dispatch_claude_input(
        model,
        op,
        seq,
        command,
        agent,
        super::encoding::interrupt_program(),
        true,
    )
}

fn update_cycle_mode(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
) -> Vec<crate::Effect> {
    if let Some(message) = super::mode_cycle_gate(model, agent) {
        return refuse(model, op, seq, command, message);
    }
    dispatch_claude_input(
        model,
        op,
        seq,
        command,
        agent,
        super::encoding::mode_cycle_program(),
        false,
    )
}

fn dispatch_claude_input(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    program: Vec<super::encoding::KeyStep>,
    retry_stale: bool,
) -> Vec<crate::Effect> {
    let expected_seq = model.claude(agent).map_or(0, ClaudeLayer::cursor);
    dispatch_input(
        model,
        op,
        seq,
        command,
        agent,
        InputPayload::Claude {
            expected_seq,
            program,
            retry_stale,
        },
    )
}

pub(crate) fn update_failed_command(
    model: &mut Model,
    op: OpId,
    command: &ClaudeCommand,
    error: &OpError,
) {
    match command {
        ClaudeCommand::SendPrompt { agent, .. } => {
            with_existing_layer(model, *agent, |layer| {
                layer.note_prompt_send_failed(op);
            });
        }
        ClaudeCommand::AnswerAsk { agent, ask, .. } => {
            let message = error.message.clone();
            let ask = *ask;
            with_existing_layer(model, *agent, |layer| {
                layer.note_ask_send_failed(ask, op, message);
            });
        }
        ClaudeCommand::Interrupt { .. } | ClaudeCommand::CyclePermissionMode { .. } => {}
    }
}

fn with_existing_layer(
    model: &mut Model,
    agent: amux::AgentId,
    step: impl FnOnce(&mut ClaudeLayer),
) {
    let Some(card) = model.agents.get_mut(&agent) else {
        return;
    };
    let Some(layer) = card.layer.as_mut().and_then(AgentLayer::claude_mut) else {
        return;
    };
    step(layer);
    card.attention = layer.attention();
}
