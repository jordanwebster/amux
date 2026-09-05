//! One explicit, client-owned held draft per agent, delivered by the native write path.

use amux::AgentId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{FinishedOp, PendingOp};
use crate::update::{push_finished, refuse};
use crate::{AgentLayer, Command, DraftAttachment, Effect, Model, OpError, OpId, OpOutcome};

/// Canonical prompt text (including inline elements) and its artifact payloads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Draft {
    pub segments: Vec<DraftSegment>,
    pub attachments: Vec<DraftAttachment>,
}

/// Inline draft content. A selected command remains typed through queue and replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "segment", rename_all = "snake_case")]
pub enum DraftSegment {
    Text { text: String },
    CommandToken { name: String },
}

impl Draft {
    pub fn plain(text: impl Into<String>, attachments: Vec<DraftAttachment>) -> Self {
        Self {
            segments: vec![DraftSegment::Text { text: text.into() }],
            attachments,
        }
    }

    /// Human-readable draft preview, never used to dispatch a command token.
    pub fn text(&self) -> String {
        self.segments
            .iter()
            .map(|segment| match segment {
                DraftSegment::Text { text } => text.clone(),
                DraftSegment::CommandToken { name } => format!("/{name}"),
            })
            .collect()
    }

    pub fn command(&self) -> Result<Option<(String, String)>, &'static str> {
        let mut name = None;
        let mut args = String::new();
        for (index, segment) in self.segments.iter().enumerate() {
            match segment {
                DraftSegment::Text { text } => args.push_str(text),
                DraftSegment::CommandToken { name: value } if index == 0 && !value.is_empty() => {
                    name = Some(value.clone());
                }
                DraftSegment::CommandToken { .. } => {
                    return Err("a command token must be first and unique");
                }
            }
        }
        Ok(name.map(|name| (name, args)))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum QueueCommand {
    Hold { agent: AgentId, draft: Draft },
    Replace { agent: AgentId, draft: Draft },
    Cancel { agent: AgentId },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum QueueDelivery {
    Held,
    Sending { op: OpId },
    Failed { error: OpError },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueuedMessage {
    pub draft: Draft,
    pub held_at: DateTime<Utc>,
    pub delivery: QueueDelivery,
    op: OpId,
    after_seq: u64,
    eligible: bool,
    retry: bool,
}

/// Holding is deliberate: only a live working session accepts a new queue.
pub fn can_hold(model: &Model, agent: AgentId) -> bool {
    model.is_connected()
        && match model.agent(agent).and_then(|card| card.layer.as_ref()) {
            Some(AgentLayer::Claude(_)) => {
                crate::claude::send_gate(model, agent) == crate::claude::SendGate::Working
            }
            Some(AgentLayer::Codex(_)) => {
                crate::codex::send_gate(model, agent) == crate::codex::SendGate::ActiveTurn
            }
            _ => false,
        }
}

fn ready(model: &Model, agent: AgentId) -> bool {
    model.is_connected()
        && match model.agent(agent).and_then(|card| card.layer.as_ref()) {
            Some(AgentLayer::Claude(_)) => {
                crate::claude::send_gate(model, agent) == crate::claude::SendGate::Ready
            }
            Some(AgentLayer::Codex(_)) => {
                crate::codex::send_gate(model, agent) == crate::codex::SendGate::Ready
            }
            _ => false,
        }
}

fn cursor(model: &Model, agent: AgentId) -> u64 {
    match model.agent(agent).and_then(|card| card.layer.as_ref()) {
        Some(AgentLayer::Claude(layer)) => layer.cursor(),
        Some(AgentLayer::Codex(layer)) => layer.entries().map(|entry| entry.seq).max().unwrap_or(0),
        _ => 0,
    }
}

fn turn_end(model: &Model, agent: AgentId) -> u64 {
    match model.agent(agent).and_then(|card| card.layer.as_ref()) {
        Some(AgentLayer::Claude(layer)) => layer
            .entries()
            .filter(|entry| matches!(entry.kind, crate::claude::FeedEntryKind::Turn(_)))
            .map(|entry| entry.seq)
            .max()
            .unwrap_or(0),
        Some(AgentLayer::Codex(layer)) => layer
            .entries()
            .filter(|entry| matches!(entry.kind, crate::codex::FeedEntryKind::Turn(_)))
            .map(|entry| entry.seq)
            .max()
            .unwrap_or(0),
        _ => 0,
    }
}

pub(crate) fn update_command(
    model: &mut Model,
    op: OpId,
    seq: u64,
    mut command: QueueCommand,
) -> Vec<Effect> {
    if let QueueCommand::Hold { draft, .. } | QueueCommand::Replace { draft, .. } = &mut command {
        for attachment in &mut draft.attachments {
            attachment.bytes = None;
        }
    }
    let agent = match &command {
        QueueCommand::Hold { agent, .. }
        | QueueCommand::Replace { agent, .. }
        | QueueCommand::Cancel { agent } => *agent,
    };
    let existing = model.queued(agent);
    let refusal = match &command {
        _ if existing.is_some_and(|q| matches!(q.delivery, QueueDelivery::Sending { .. })) => {
            Some("queued message is being sent")
        }
        QueueCommand::Hold { .. } if existing.is_some() => {
            Some("a message is already queued — replace or cancel it")
        }
        QueueCommand::Hold { .. } if !can_hold(model, agent) => {
            Some("queue requires a live working session")
        }
        QueueCommand::Replace { .. } | QueueCommand::Cancel { .. } if existing.is_none() => {
            Some("no queued message")
        }
        QueueCommand::Hold { draft, .. } | QueueCommand::Replace { draft, .. }
            if draft.text().trim().is_empty() =>
        {
            Some("queued message is empty")
        }
        _ => None,
    };
    if let Some(message) = refusal {
        return refuse(model, op, seq, Command::Queue(command), message);
    }
    let previous = remove(model, agent);
    match &command {
        QueueCommand::Cancel { .. } => {
            push_finished(
                model,
                FinishedOp {
                    op,
                    seq,
                    command: Command::Queue(command),
                    outcome: OpOutcome::QueueCancelled {
                        draft: previous.expect("validated queue").draft,
                    },
                },
            );
        }
        QueueCommand::Hold { draft, .. } | QueueCommand::Replace { draft, .. } => {
            model.queues.insert(
                agent,
                QueuedMessage {
                    draft: draft.clone(),
                    held_at: model.now().unwrap_or_else(|| {
                        model.agent(agent).expect("validated agent").last_activity
                    }),
                    delivery: QueueDelivery::Held,
                    op,
                    after_seq: previous
                        .as_ref()
                        .map_or_else(|| cursor(model, agent), |q| q.after_seq),
                    eligible: previous.as_ref().is_some_and(|q| q.eligible),
                    retry: false,
                },
            );
            // The operation stays pending while held. Its final outcome is delivery,
            // replacement or cancellation; no asynchronous acknowledgement is invented.
            model.pending_ops.insert(
                op,
                PendingOp {
                    op,
                    seq,
                    command: Command::Queue(command),
                },
            );
        }
    }
    Vec::new()
}

pub(crate) fn remove(model: &mut Model, agent: AgentId) -> Option<QueuedMessage> {
    let queue = model.queues.remove(&agent)?;
    if let Some(pending) = model.pending_ops.remove(&queue.op) {
        push_finished(
            model,
            FinishedOp {
                op: pending.op,
                seq: pending.seq,
                command: pending.command,
                outcome: OpOutcome::QueueRemoved,
            },
        );
    }
    Some(queue)
}

pub(crate) fn reopened(model: &mut Model, agent: AgentId) {
    if let Some(queue) = model.queues.get_mut(&agent) {
        queue.retry = true;
    }
}

pub(crate) fn observe_result(model: &mut Model, pending: &PendingOp, outcome: &OpOutcome) -> bool {
    let agent = model
        .queues
        .iter()
        .find_map(|(agent, q)| (q.op == pending.op).then_some(*agent));
    let Some(agent) = agent else {
        return false;
    };
    if let OpOutcome::Error { error } = outcome {
        model.queues.get_mut(&agent).expect("queue found").delivery = QueueDelivery::Failed {
            error: error.clone(),
        };
        model.pending_ops.insert(pending.op, pending.clone());
        return true;
    }
    model.queues.remove(&agent);
    false
}

pub(crate) fn deliver_ready(model: &mut Model) -> Vec<Effect> {
    let agents: Vec<_> = model.queues.keys().copied().collect();
    let mut effects = Vec::new();
    for agent in agents {
        let end = turn_end(model, agent);
        let is_ready = ready(model, agent);
        let queue = model.queues.get_mut(&agent).expect("collected queue");
        queue.eligible |= end > queue.after_seq;
        if !queue.eligible
            || !is_ready
            || matches!(queue.delivery, QueueDelivery::Sending { .. })
            || (matches!(queue.delivery, QueueDelivery::Failed { .. }) && !queue.retry)
        {
            continue;
        }
        let op = queue.op;
        let draft = queue.draft.clone();
        queue.delivery = QueueDelivery::Sending { op };
        queue.retry = false;
        let seq = model.pending_ops.get(&op).expect("held operation").seq;
        let sent = update_draft(model, op, seq, agent, draft);
        if sent.is_empty() {
            // Native validation can refuse a draft even when the session gate is ready.
            if let Some(index) = model
                .finished_ops
                .iter()
                .position(|finished| finished.op == op)
            {
                let finished = model.finished_ops.remove(index);
                if let OpOutcome::Error { error } = finished.outcome {
                    model
                        .queues
                        .get_mut(&agent)
                        .expect("queue retained")
                        .delivery = QueueDelivery::Failed { error };
                }
            }
        }
        effects.extend(sent);
    }
    effects
}

/// Immediate and queued drafts share native gates and token validation.
pub(crate) fn update_draft(
    model: &mut Model,
    op: OpId,
    seq: u64,
    agent: AgentId,
    draft: Draft,
) -> Vec<Effect> {
    let mut state_draft = draft.clone();
    for attachment in &mut state_draft.attachments {
        attachment.bytes = None;
    }
    let command = Command::Send {
        agent,
        draft: state_draft,
    };
    let selected = match draft.command() {
        Ok(selected) => selected,
        Err(reason) => return refuse(model, op, seq, command, reason),
    };
    if let Some((name, args)) = selected {
        if !draft.attachments.is_empty() {
            return refuse(
                model,
                op,
                seq,
                command,
                "command attachments are unavailable",
            );
        }
        if model.codex(agent).is_none() {
            return refuse(
                model,
                op,
                seq,
                command,
                "provider commands are unavailable for this agent",
            );
        }
        if let crate::codex::WritePermission::Refused(reason) =
            crate::codex::write_permission(model, agent, crate::codex::WriteAction::Prompt)
        {
            return refuse(model, op, seq, command, reason);
        }
        if !crate::provider::facts(model, agent)
            .commands
            .iter()
            .any(|item| item.name == name && !item.terminal_only)
        {
            return refuse(
                model,
                op,
                seq,
                command,
                "command is not offered by this session",
            );
        }
        return crate::codex::update::dispatch_codex_input(
            model,
            op,
            seq,
            command,
            agent,
            crate::codex::CodexInput::Command { name, args },
            crate::codex::InFlightKind::Prompt,
        );
    }
    let text = draft.text();
    if !draft.attachments.is_empty() {
        crate::update::update_attachment_prompt(model, op, seq, agent, text, draft.attachments)
    } else if model.claude(agent).is_some() {
        crate::claude::update::update_command(
            model,
            op,
            seq,
            crate::ClaudeCommand::SendPrompt { agent, text },
        )
    } else {
        crate::codex::update::update_command(
            model,
            op,
            seq,
            crate::CodexCommand::Prompt { agent, text },
        )
    }
}
