//! Claude-native command reduction. The kernel dispatches the namespaced
//! command here and knows none of the intent, gate, or optimistic state.

use amux::claude_io;

use super::answer::{self, AskAnswer};
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
    let text = answer::normalize_prompt(&text);
    if let Err(refusal) = answer::check_prompt(&text) {
        return refuse(model, op, seq, command, &refusal.to_string());
    }
    let now = model.now();
    with_existing_layer(model, agent, |layer| {
        layer.note_prompt_sent(op, text.clone(), now);
    });
    dispatch_claude_input(
        model,
        op,
        seq,
        command,
        agent,
        claude_io::Intent::Prompt { text },
        false,
    )
}

fn update_answer_ask(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    ask: u64,
    answer: AskAnswer,
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
    if let Err(refusal) = answer::check_answer(&entry.kind, &answer) {
        return refuse(model, op, seq, command, &refusal.to_string());
    }
    let intent = claude_io::Intent::Answer {
        ask_id: entry.session_ask_id.clone(),
        answer: answer.clone(),
    };
    with_existing_layer(model, agent, |layer| {
        layer.note_ask_answered(ask, op, answer);
    });
    dispatch_claude_input(model, op, seq, command, agent, intent, false)
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
        claude_io::Intent::Interrupt,
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
        claude_io::Intent::CyclePermissionMode,
        false,
    )
}

fn dispatch_claude_input(
    model: &mut Model,
    op: OpId,
    seq: u64,
    command: Command,
    agent: amux::AgentId,
    intent: claude_io::Intent,
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
            intent,
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
            let message = error.message();
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
