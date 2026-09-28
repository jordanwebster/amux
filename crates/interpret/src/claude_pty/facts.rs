//! Transcript rows, hook payloads and the launch fact, read into state and
//! items. The module docs in `mod.rs` state the inference rules.

use serde_json::Value;
use wire::claude_pty_item::Kind;
use wire::{
    Ask, BoundaryKind, DecisionOutcome, PermissionAsk, PlanAsk, ScopeChoice, ToolState, Turn,
    TurnOutcome,
};

use super::{
    AskMeta, AskShape, Decision, PendingMessage, PermissionMenus, Running, Slash, State, Subagent,
    Tool, item_body,
};
use crate::claude_common::{
    PLAN_TOOL, QUESTION_TOOL, TASK_TOOLS, apply_task_tool, compact_json, content_text,
    question_ask, result_images, same_json, scope_choices, split_tool_name, text, timestamp_ms,
    tool_class, without_image_bytes,
};
use crate::{Channel, Emit, Fact, ItemDraft, ask_item, is_status_tool, status_working_on};

/// How terminal Claude words a call the person refused, on the tool result.
const REJECTED: &str = "The user doesn't want to proceed with this tool use.";
const REJECTED_NOTE: &str = "the user said:\n";
const INTERRUPTED: &str = "[Request interrupted by user";
/// The tool that starts a subagent.
const AGENT_TOOL: &str = "Agent";
/// The Notification hook's types for a tool server's form and link, which
/// Claude shows in its own terminal (2.1.283).
const ELICITATION_FORM: &str = "elicitation_dialog";
const ELICITATION_LINK: &str = "elicitation_url_dialog";
/// What an unanswerable ask says, for a form and for a link.
const UNANSWERABLE_FORM: &str = "Claude is showing a form from a tool server this build can't read. Attach to Claude's terminal to answer it, or stop the agent.";
const TRUST_QUESTION: &str = "Do you trust the files in this folder?";
pub(super) const TRUST_YES: &str = "Trust this folder";
const TRUST_NO: &str = "Exit";
const UNANSWERABLE_LINK: &str = "Claude is showing a link from a tool server this build can't read. Attach to Claude's terminal to answer it, or stop the agent.";

/// The agent id of a subagent Claude launched in the background, from the
/// Agent call's immediate result.
fn launched_in_background(result: &Value) -> Option<&str> {
    if result.get("isAsync").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    result.get("agentId").and_then(Value::as_str)
}

/// The text between `<tag>` and `</tag>`, trimmed.
fn between<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// The messages from another session a user row shows the model: the body
/// of each `<cross-session-message …>` element (Claude 2.1.240), or the
/// text between the peer preamble and its trust note (2.1.282).
fn peer_message_bodies(text: &str) -> Vec<String> {
    const OPEN: &str = "<cross-session-message";
    const CLOSE: &str = "</cross-session-message>";
    const PREAMBLE: &str = "Another Claude session sent a message:\n";
    const TRUST_NOTE: &str = "\n\nThis came from another Claude session";
    let mut bodies = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let inner = &after[tag_end + 1..];
        let Some(close) = inner.find(CLOSE) else {
            break;
        };
        bodies.push(inner[..close].trim().to_owned());
        rest = &inner[close + CLOSE.len()..];
    }
    if bodies.is_empty()
        && let Some(message) = text.trim_start().strip_prefix(PREAMBLE)
    {
        let end = message.find(TRUST_NOTE).unwrap_or(message.len());
        bodies.push(message[..end].trim().to_owned());
    }
    bodies
}

impl State {
    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        let Ok(value) = serde_json::from_slice::<Value>(&fact.payload) else {
            return;
        };
        match fact.channel {
            Channel::Transcript => self.row(emit, &value),
            Channel::Hook => self.hook(emit, &value),
            Channel::Agent => self.agent_fact(emit, &value),
            Channel::Stream | Channel::Rpc | Channel::Tools => {}
        }
    }

    fn agent_fact(&mut self, emit: &mut Emit, value: &Value) {
        match text(value, "type") {
            "launch" => {}
            // Waiting on its trust dialog, Claude takes no prompt; it
            // starts its session once trusted, and that start says so.
            "ready" if self.trust_ask().is_some() => return,
            "ready" => return self.shared.provider_started(),
            "trust_dialog" => return self.trust_dialog(emit),
            _ => return,
        }
        let provider = &mut self.provider;
        let version = text(value, "version");
        provider.version = (!version.is_empty()).then(|| version.to_owned());
        provider.keymap = text(value, "keymap").to_owned();
        provider.permission_menus = value
            .get("permission_menus")
            .cloned()
            .and_then(|menus| serde_json::from_value(menus).ok())
            .unwrap_or_default();
        provider.relaunched = provider.launches > 0;
        provider.launches += 1;
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, cause: String) {
        self.running.clear();
        self.close_all_unknown(emit);
        self.boundary(emit, BoundaryKind::Exited, cause);
        self.shared.provider_exited();
        self.local_turn = false;
    }

    // --- hooks -----------------------------------------------------------

    fn hook(&mut self, emit: &mut Emit, hook: &Value) {
        if let Some(mode) = hook.get("permission_mode").and_then(Value::as_str) {
            self.provider.permission_mode = Some(mode.to_owned());
        }
        let event = text(hook, "hook_event_name");
        let agent = text(hook, "agent_id");
        if !agent.is_empty()
            && matches!(
                event,
                "PreToolUse" | "PermissionRequest" | "PostToolUse" | "PostToolUseFailure"
            )
        {
            return self.subagent_hook(emit, agent, event, hook);
        }
        match event {
            "SessionStart" => self.session_start(emit, hook),
            "SessionEnd" => self.close_all_unknown(emit),
            "UserPromptSubmit" => self.shared.provider_started(),
            "PreToolUse" => self.pre_tool_use(hook),
            "PermissionRequest" => self.permission_request(hook),
            "PostToolUse" | "PostToolUseFailure" => self.call_ended(text(hook, "tool_use_id")),
            "Notification" => self.notification(emit, hook),
            "Stop" => {
                if let Some(tasks) = hook.get("background_tasks").and_then(Value::as_array) {
                    self.background = Some(tasks.len() as u32);
                }
                self.running.clear();
                self.close_all_unknown(emit);
            }
            _ => {}
        }
    }

    fn session_start(&mut self, emit: &mut Emit, hook: &Value) {
        self.shared.provider_started();
        self.running.clear();
        // Claude starts its session only once its folder is trusted: No
        // exits.
        self.close_trust_answered_elsewhere(emit);
        self.close_all_unknown(emit);
        let session = text(hook, "session_id");
        if !session.is_empty() {
            self.provider.session = Some(session.to_owned());
        }
        let model = text(hook, "model");
        if !model.is_empty() {
            self.provider.model = Some(model.to_owned());
        }
        let path = text(hook, "transcript_path");
        if !path.is_empty() && self.provider.transcript.as_deref() != Some(path) {
            self.provider.transcript = Some(path.to_owned());
            emit.effect(crate::Effect::FollowTranscript {
                path: path.to_owned(),
            });
        }
        let source = text(hook, "source");
        let kind = if std::mem::take(&mut self.provider.relaunched) {
            BoundaryKind::Restarted
        } else {
            match source {
                "resume" => BoundaryKind::Resumed,
                "clear" => BoundaryKind::Cleared,
                "compact" => BoundaryKind::Compacted,
                _ => BoundaryKind::Started,
            }
        };
        if matches!(kind, BoundaryKind::Cleared | BoundaryKind::Compacted) {
            self.context_tokens = None;
            self.end_local_turn();
        }
        if kind == BoundaryKind::Cleared {
            self.shared.steers_lost();
            if self.tasks.is_some() {
                self.tasks = Some(Vec::new());
            }
        }
        self.boundary(emit, kind, String::new());
    }

    /// A call inside a subagent: a step on its Agent row, and an ask it
    /// raises or answers.
    fn subagent_hook(&mut self, emit: &mut Emit, agent: &str, event: &str, hook: &Value) {
        let row = self.agent_row(agent);
        let name = text(hook, "tool_name");
        match event {
            "PermissionRequest" => self.permission_request(hook),
            "PreToolUse" => {
                if let Some(subagent) = row
                    .as_ref()
                    .and_then(|id| self.tools.get_mut(id))
                    .and_then(|tool| tool.subagent.as_mut())
                {
                    subagent.tool_count += 1;
                    subagent.last_tool = split_tool_name(name).1;
                }
            }
            _ => {
                let input = hook.get("tool_input").cloned().unwrap_or(Value::Null);
                let answered = self
                    .asks
                    .iter()
                    .filter(|(_, meta)| meta.agent.as_deref() == Some(agent))
                    .filter(|(_, meta)| meta.tool_name == name)
                    .min_by_key(|(_, meta)| (!same_json(&meta.input, &input), meta.seq))
                    .map(|(key, _)| key.clone());
                if let Some(key) = answered
                    && self.shared.asks().get(&key).is_some()
                {
                    self.close(emit, &key, Decision::elsewhere(DecisionOutcome::Allowed));
                }
            }
        }
        if let Some(id) = row {
            self.emit_tool(emit, &id);
        }
    }

    /// The Agent call a subagent belongs to. A background subagent is known
    /// by the id its launch returned; a foreground one's calls come while its
    /// Agent call runs, so the newest running Agent call not yet claimed.
    fn agent_row(&mut self, agent: &str) -> Option<String> {
        if let Some(id) = self.agents.get(agent) {
            return self.tools.contains_key(id).then(|| id.clone());
        }
        let claimed = self.agents.values().cloned().collect::<Vec<_>>();
        let id = self
            .tools
            .iter()
            .filter(|(id, tool)| tool.subagent.is_some() && !tool.finished && !claimed.contains(id))
            .max_by_key(|(_, tool)| tool.seq)
            .map(|(id, _)| id.clone())?;
        self.agents.insert(agent.to_owned(), id.clone());
        Some(id)
    }

    /// The Agent call returned before its subagent finished: the row stays
    /// running until the task notification.
    fn agent_launched(&mut self, id: &str, result: &Value) -> bool {
        let Some(agent) = launched_in_background(result) else {
            return false;
        };
        let Some(tool) = self.tools.get_mut(id) else {
            return false;
        };
        if tool.subagent.is_none() {
            return false;
        }
        tool.awaiting_notification = true;
        tool.background = true;
        self.agents.insert(agent.to_owned(), id.to_owned());
        true
    }

    fn permission_request(&mut self, hook: &Value) {
        let name = text(hook, "tool_name");
        let input = hook.get("tool_input").cloned().unwrap_or(Value::Null);
        let (server, tool) = split_tool_name(name);
        let (body, shape) = match tool.as_str() {
            QUESTION_TOOL if server.is_empty() => {
                let (mut question, questions) = question_ask(&input);
                // Claude draws a question with previews side by side, with
                // no row for a typed answer, so none is offered there.
                for (question, shape) in question.questions.iter_mut().zip(&questions) {
                    question.allow_other = !shape.previews;
                }
                (
                    wire::ask::Body::Question(question),
                    AskShape::Question { questions },
                )
            }
            PLAN_TOOL if server.is_empty() => (
                wire::ask::Body::Plan(PlanAsk {
                    plan: text(&input, "plan").to_owned(),
                    offers_auto_accept: true,
                }),
                AskShape::Plan,
            ),
            _ => permission_ask(
                &server,
                &tool,
                &input,
                hook,
                &self.provider.permission_menus,
            ),
        };
        let seq = self.next_seq();
        self.next_ask += 1;
        let key = format!("ask:{}", self.next_ask);
        let agent = Some(text(hook, "agent_id").to_owned()).filter(|agent| !agent.is_empty());
        let (bound, item_key) = match &agent {
            // Shown on the subagent's Agent row, which takes no decision.
            Some(agent) => (None, self.agent_row(agent)),
            None => {
                let bound = self.tool_for_ask(name, &input);
                (bound.clone(), bound)
            }
        };
        self.asks.insert(
            key.clone(),
            AskMeta {
                seq,
                tool_name: name.to_owned(),
                input: input.to_string(),
                shape,
                bound,
                closed: None,
                agent,
            },
        );
        self.shared.open_ask(Ask {
            key,
            item_key: item_key.unwrap_or_default(),
            body: Some(body),
            opened_at_ms: self.shared.now_ms(),
        });
    }

    /// Claude's first screen asks whether its folder is trusted, and
    /// exits on No. It is asked here as a question with those two answers.
    fn trust_dialog(&mut self, emit: &mut Emit) {
        if self
            .asks
            .values()
            .any(|meta| matches!(meta.shape, AskShape::Trust))
        {
            return;
        }
        let seq = self.next_seq();
        self.next_ask += 1;
        let key = format!("ask:{}", self.next_ask);
        self.asks.insert(
            key.clone(),
            AskMeta {
                seq,
                tool_name: String::new(),
                input: String::new(),
                shape: AskShape::Trust,
                bound: None,
                closed: None,
                agent: None,
            },
        );
        let option = |label: &str, description: &str| wire::QuestionOption {
            label: label.to_owned(),
            description: description.to_owned(),
            ..Default::default()
        };
        let ask = Ask {
            item_key: ask_item::key(&key),
            key,
            body: Some(wire::ask::Body::Question(wire::QuestionAsk {
                questions: vec![wire::Question {
                    header: "Folder".to_owned(),
                    question: TRUST_QUESTION.to_owned(),
                    multi_select: false,
                    options: vec![
                        option(TRUST_YES, "Claude can read, edit and run files here"),
                        option(TRUST_NO, "Claude exits without starting"),
                    ],
                    allow_other: false,
                    secret: false,
                }],
            })),
            opened_at_ms: self.shared.now_ms(),
        };
        self.shared.open_ask(ask.clone());
        self.emit_ask_item(emit, &ask, None);
    }

    /// A tool server's form or link on Claude's screen. No hook answers it,
    /// so the ask can only be answered in the terminal or ended by stopping.
    fn notification(&mut self, emit: &mut Emit, hook: &Value) {
        let reason = match text(hook, "notification_type") {
            ELICITATION_FORM => UNANSWERABLE_FORM,
            ELICITATION_LINK => UNANSWERABLE_LINK,
            _ => return,
        };
        // A tool server asks while one of its calls runs; that call's
        // result ends the dialog. A call only a PreToolUse hook announced
        // has its row on the way: the ask's item waits for that row, so it
        // lands below the call and the rows written before it.
        let landed = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.finished && !tool.awaiting_notification)
            .max_by_key(|(_, tool)| (!tool.server.is_empty(), tool.seq))
            .map(|(id, _)| id.clone());
        let announced = self.running.last().map(|running| running.id.clone());
        let awaits_row = landed.is_none() && announced.is_some();
        let call = landed.or(announced);
        let seq = self.next_seq();
        self.next_ask += 1;
        let key = format!("ask:{}", self.next_ask);
        self.asks.insert(
            key.clone(),
            AskMeta {
                seq,
                tool_name: String::new(),
                input: String::new(),
                shape: AskShape::Unanswerable { call },
                bound: None,
                closed: None,
                agent: None,
            },
        );
        let ask = Ask {
            item_key: if awaits_row {
                String::new()
            } else {
                ask_item::key(&key)
            },
            key,
            body: Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
                reason: reason.to_owned(),
            })),
            opened_at_ms: self.shared.now_ms(),
        };
        self.shared.open_ask(ask.clone());
        if !awaits_row {
            self.emit_ask_item(emit, &ask, None);
        }
    }

    /// A call starts. Its row follows within a second or so, and by
    /// seconds while a background task runs; until then the activity line
    /// names it from here. The model is working, whether or not the row
    /// of the prompt that began the turn landed yet.
    fn pre_tool_use(&mut self, hook: &Value) {
        self.shared.turn_started();
        let (server, name) = split_tool_name(text(hook, "tool_name"));
        // Drawn elsewhere, or as its subagent's work.
        if is_status_tool(&server, &name)
            || (server.is_empty() && (TASK_TOOLS.contains(&name.as_str()) || name == AGENT_TOOL))
        {
            return;
        }
        self.running.push(Running {
            id: text(hook, "tool_use_id").to_owned(),
            name,
            since_ms: self.shared.now_ms(),
        });
    }

    /// A call's PostToolUse hook or its result row: it no longer runs.
    fn call_ended(&mut self, id: &str) {
        self.running.retain(|running| running.id != id);
    }

    // --- tools -----------------------------------------------------------

    /// A call's tool_use row: the call opens, running.
    fn tool_seen(
        &mut self,
        emit: &mut Emit,
        id: &str,
        name: &str,
        input: &Value,
        at_ms: Option<i64>,
        message_id: &str,
    ) {
        if id.is_empty() {
            return;
        }
        let (server, tool_name) = split_tool_name(name);
        if !self.tools.contains_key(id) {
            let hidden = is_status_tool(&server, &tool_name)
                || (server.is_empty() && TASK_TOOLS.contains(&tool_name.as_str()));
            if is_status_tool(&server, &tool_name)
                && let Some(working_on) = status_working_on(input.to_string().as_bytes())
            {
                self.shared.set_working_on(working_on);
            }
            let seq = self.next_seq();
            let class = tool_class(&server, &tool_name);
            let subagent = (server.is_empty() && tool_name == AGENT_TOOL).then(Subagent::default);
            self.tools.insert(
                id.to_owned(),
                Tool {
                    seq,
                    at_ms: at_ms.unwrap_or_else(|| self.shared.now_ms()),
                    name: tool_name,
                    server,
                    input: input.to_string(),
                    state: ToolState::Running as i32,
                    outcome_text: String::new(),
                    outcome_json: String::new(),
                    class: class as i32,
                    background: input
                        .get("run_in_background")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    decision: None,
                    ended_at_ms: None,
                    message_id: message_id.to_owned(),
                    finished: false,
                    hidden,
                    subagent,
                    awaiting_notification: false,
                    images: Vec::new(),
                    emitted: Vec::new(),
                },
            );
        }
        self.emit_tool(emit, id);
        self.emit_unanswerable_items_for(emit, id);
    }

    fn tool_result(&mut self, emit: &mut Emit, block: &Value, row: &Value, at_ms: Option<i64>) {
        let id = text(block, "tool_use_id").to_owned();
        let output = content_text(block.get("content").unwrap_or(&Value::Null));
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let rejected = is_error && output.starts_with(REJECTED);
        let result = row.get("toolUseResult").cloned().unwrap_or(Value::Null);
        self.call_ended(&id);
        self.close_unanswerable_for_tool(emit, &id);
        // The launch metadata of a background subagent is for the model.
        if self.agent_launched(&id, &result) {
            self.close_for_tool(emit, &id, DecisionOutcome::Allowed);
            return self.emit_tool(emit, &id);
        }
        let Some(tool) = self.tools.get_mut(&id) else {
            return;
        };
        let outcome = if rejected {
            DecisionOutcome::Denied
        } else {
            DecisionOutcome::Allowed
        };
        let note = match output.find(REJECTED_NOTE) {
            Some(at) if rejected => output[at + REJECTED_NOTE.len()..].trim().to_owned(),
            _ => String::new(),
        };
        // An ask a fact closed without saying how: the result says.
        if let Some(decision) = &mut tool.decision
            && decision.outcome == DecisionOutcome::Unknown as i32
        {
            *decision = Decision::elsewhere(outcome);
            decision.note = note.clone();
        }
        let denied = tool
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == DecisionOutcome::Denied as i32);
        tool.finished = true;
        tool.state = if rejected
            && (denied
                || self
                    .asks
                    .values()
                    .any(|meta| meta.bound.as_deref() == Some(id.as_str())))
        {
            ToolState::Denied as i32
        } else if rejected {
            ToolState::Cancelled as i32
        } else if is_error {
            ToolState::Failed as i32
        } else {
            ToolState::Succeeded as i32
        };
        tool.outcome_text = output.clone();
        let images = result_images(block.get("content").unwrap_or(&Value::Null));
        if !images.is_empty() {
            tool.images = Vec::new();
            for (image, write) in images {
                tool.images.push(image);
                emit.effect(write);
            }
        }
        if !result.is_null() {
            tool.outcome_json = compact_json(&without_image_bytes(&result));
        }
        if tool.ended_at_ms.is_none() {
            tool.ended_at_ms = at_ms.or(Some(self.shared.now_ms()));
        }
        for key in self.open_ask_keys() {
            if self.asks.get(&key).and_then(|meta| meta.bound.as_deref()) == Some(id.as_str()) {
                let mut decision = Decision::elsewhere(outcome);
                decision.note = note.clone();
                self.close(emit, &key, decision);
            }
        }
        self.task_tool(&id, &result);
        self.emit_tool(emit, &id);
    }

    fn task_tool(&mut self, id: &str, result: &Value) {
        let Some(tool) = self.tools.get(id) else {
            return;
        };
        if !tool.server.is_empty() {
            return;
        }
        let input = serde_json::from_str::<Value>(&tool.input).unwrap_or(Value::Null);
        let name = tool.name.clone();
        apply_task_tool(&mut self.tasks, &name, &input, result);
    }

    // --- rows ------------------------------------------------------------

    fn row(&mut self, emit: &mut Emit, row: &Value) {
        if self.provider.version.is_none()
            && let Some(version) = row.get("version").and_then(Value::as_str)
        {
            self.provider.version = Some(version.to_owned());
        }
        if self.provider.session.is_none()
            && let Some(session) = row.get("sessionId").and_then(Value::as_str)
        {
            self.provider.session = Some(session.to_owned());
        }
        let at_ms = timestamp_ms(row);
        match text(row, "type") {
            "user" => {
                self.shared.provider_started();
                self.user_row(emit, row, at_ms);
            }
            "assistant" => {
                self.shared.provider_started();
                self.assistant_row(emit, row, at_ms);
            }
            "system" => self.system_row(emit, row, at_ms),
            "permission-mode" => {
                if let Some(mode) = row.get("permissionMode").and_then(Value::as_str) {
                    self.provider.permission_mode = Some(mode.to_owned());
                }
            }
            "attachment" => {
                let attachment = row.get("attachment").unwrap_or(&Value::Null);
                if text(attachment, "type") == "queued_command"
                    && matches!(text(attachment, "commandMode"), "" | "prompt")
                {
                    let prompt = text(attachment, "prompt").to_owned();
                    self.joined_prompt(emit, text(row, "uuid").to_owned(), prompt, at_ms);
                }
            }
            // Everything else is bookkeeping for Claude's own interface:
            // titles, modes, file history, queue operations, reminders.
            _ => {}
        }
    }

    fn user_row(&mut self, emit: &mut Emit, row: &Value, at_ms: Option<i64>) {
        let uuid = text(row, "uuid").to_owned();
        let content = row.pointer("/message/content").unwrap_or(&Value::Null);
        let whole = content_text(content);
        if !peer_message_bodies(&whole).is_empty() {
            return self.reflected_messages(&whole);
        }
        if row.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
            self.shared.item(
                emit,
                ItemDraft {
                    key: uuid,
                    text: whole,
                    body: item_body(Kind::CompactSummary(wire::CompactSummary {})),
                    at_ms,
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
            return;
        }
        // Claude's own message to the model, never a person's prompt.
        if row.pointer("/origin/kind").and_then(Value::as_str) == Some("task-notification")
            || text(row, "promptSource") == "system"
        {
            return self.task_notification(emit, &whole, at_ms);
        }
        if let Value::Array(blocks) = content {
            let mut results = false;
            for block in blocks {
                if text(block, "type") == "tool_result" {
                    results = true;
                    self.tool_result(emit, block, row, at_ms);
                }
            }
            if results {
                return;
            }
        }
        let trimmed = whole.trim_start();
        if trimmed.starts_with(INTERRUPTED) {
            return self.interrupted(emit, uuid, at_ms);
        }
        if trimmed.starts_with("<local-command-caveat>") {
            return;
        }
        if trimmed.starts_with("<command-name>") {
            let command = between(trimmed, "command-name").unwrap_or_default();
            let args = between(trimmed, "command-args").unwrap_or_default();
            let same = self
                .slash
                .as_ref()
                .is_some_and(|slash| slash.command == command && slash.output.is_none());
            if !same {
                self.slash_command(emit, uuid, command.to_owned(), args.to_owned(), at_ms);
            }
            return;
        }
        if trimmed.starts_with("<local-command-stdout>")
            || trimmed.starts_with("<local-command-stderr>")
        {
            let output = between(trimmed, "local-command-stdout")
                .or_else(|| between(trimmed, "local-command-stderr"))
                .unwrap_or_default()
                .to_owned();
            if let Some(slash) = &mut self.slash {
                slash.output = Some(output);
                self.emit_slash(emit);
            }
            self.end_local_turn();
            return;
        }
        // A slash command as typed: Claude records it without a prompt
        // origin before it runs.
        if trimmed.starts_with('/') && row.get("origin").is_none() {
            let (command, args) = trimmed
                .split_once(char::is_whitespace)
                .unwrap_or((trimmed, ""));
            return self.slash_command(
                emit,
                uuid,
                command.to_owned(),
                args.trim().to_owned(),
                at_ms,
            );
        }
        self.prompt_row(emit, uuid, whole, at_ms);
    }

    fn slash_command(
        &mut self,
        emit: &mut Emit,
        key: String,
        command: String,
        args: String,
        at_ms: Option<i64>,
    ) {
        let input_id = if self.shared.awaiting_reflection().is_empty() {
            Vec::new()
        } else {
            self.local_turn = true;
            self.shared.reflect_prompt().unwrap_or_default()
        };
        self.slash = Some(Slash {
            key,
            command,
            args,
            input_id,
            at_ms: at_ms.unwrap_or_else(|| self.shared.now_ms()),
            output: None,
        });
        self.emit_slash(emit);
    }

    fn end_local_turn(&mut self) {
        if std::mem::take(&mut self.local_turn) {
            self.shared.turn_abandoned();
        }
    }

    /// A person's prompt, typed in the terminal or submitted through amux.
    /// A prompt sent into a turn that had no tool boundary left lands here
    /// too, as a turn of its own after that turn ended: an ordinary prompt.
    fn prompt_row(&mut self, emit: &mut Emit, key: String, text: String, at_ms: Option<i64>) {
        // A prompt amux submitted went in with no ask open, so its
        // reflection proves nothing about an ask open now: the row can land
        // after the hook that opened one, since rows are read by polling.
        // Any other prompt means the call under the ask is over.
        if self.shared.awaiting_reflection().is_empty() {
            self.close_all_unknown(emit);
        }
        // The submission began a turn, but another may have run and ended
        // since: a background task's notification answered first.
        self.shared.turn_started();
        let input_id = match self.shared.reflect_prompt() {
            Some(input_id) => input_id,
            None => self
                .steered_entry(&text)
                .map(|entry| entry.input_id)
                .unwrap_or_default(),
        };
        self.local_turn = false;
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text,
                input_id,
                body: item_body(Kind::Prompt(wire::Prompt {})),
                at_ms,
                complete: true,
                ..Default::default()
            },
        );
    }

    /// A prompt typed while a turn ran that Claude folded into that turn at
    /// a tool boundary: a queued-command attachment, with no user row. Its
    /// item is the prompt, marked steered; one sent through amux carries
    /// its input id.
    fn joined_prompt(&mut self, emit: &mut Emit, key: String, text: String, at_ms: Option<i64>) {
        let entry = self.steered_entry(&text);
        let (input_id, attachments) = match entry {
            Some(entry) => (entry.input_id, entry.attachments),
            None => (self.folded_prompt(&text).unwrap_or_default(), Vec::new()),
        };
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text,
                attachments,
                input_id,
                body: item_body(Kind::Steer(wire::Steer {})),
                at_ms,
                complete: true,
            },
        );
    }

    /// A prompt amux typed while Claude's own turn ran, as when a
    /// background task's notification began one first: Claude folds it
    /// into that turn instead of reflecting it as a prompt row. It is
    /// reflected here when its text is the oldest awaiting reflection, so
    /// later reflections keep their own input ids.
    fn folded_prompt(&mut self, text: &str) -> Option<Vec<u8>> {
        let oldest = self.shared.awaiting_reflection().first()?;
        let prompt = self.submitted.iter().find(|prompt| &prompt.id == oldest)?;
        if prompt.text.trim() != text.trim() {
            return None;
        }
        self.shared.reflect_prompt()
    }

    /// The steered queue entry a reflection with this text stands for.
    /// Claude takes its own queue in order and echoes no id, so the entry
    /// with the same text wins and the oldest one otherwise.
    fn steered_entry(&mut self, text: &str) -> Option<wire::QueuedInput> {
        let text = text.trim();
        self.shared
            .steer_reflected(|entry| entry.text.trim() == text)
            .or_else(|| self.shared.steer_reflected(|_| true))
    }

    /// Agent messages that arrived on the messaging socket, as Claude
    /// showed them to the model: the consumption point.
    fn reflected_messages(&mut self, text: &str) {
        // Claude answers a message from its socket in a turn of its own.
        self.shared.turn_started();
        for body in peer_message_bodies(text) {
            if let Some(at) = self
                .messages
                .iter()
                .position(|message| message.text.trim() == body)
            {
                let PendingMessage { id, .. } = self.messages.remove(at);
                self.shared.message_consumed(&id);
            }
        }
    }

    /// A background task reported to the model. For a subagent it names the
    /// Agent call that launched it and carries its answer; the model answers
    /// the notification in a turn of its own.
    fn task_notification(&mut self, emit: &mut Emit, text: &str, at_ms: Option<i64>) {
        self.close_all_unknown(emit);
        self.local_turn = false;
        self.shared.turn_started();
        let Some(id) = between(text, "tool-use-id") else {
            return;
        };
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if !tool.awaiting_notification || tool.finished {
            return;
        }
        let result = text
            .find("<result>")
            .and_then(|start| {
                let body = &text[start + "<result>".len()..];
                body.rfind("</result>").map(|end| body[..end].trim())
            })
            .unwrap_or_default();
        tool.finished = true;
        tool.state = match between(text, "status") {
            Some("completed") => ToolState::Succeeded,
            Some("failed") => ToolState::Failed,
            _ => ToolState::Cancelled,
        } as i32;
        tool.outcome_text = result.to_owned();
        tool.ended_at_ms = at_ms.or(Some(self.shared.now_ms()));
        let id = id.to_owned();
        self.emit_tool(emit, &id);
    }

    fn interrupted(&mut self, emit: &mut Emit, key: String, at_ms: Option<i64>) {
        self.running.clear();
        self.close_all_unknown(emit);
        // Claude hands prompts still in its own queue back to its composer.
        self.shared.steers_lost();
        // A subagent in the background outlives the interrupted turn.
        let running = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.finished && !tool.awaiting_notification)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in running {
            if let Some(tool) = self.tools.get_mut(&id) {
                tool.finished = true;
                tool.state = ToolState::Cancelled as i32;
            }
            self.emit_tool(emit, &id);
        }
        self.shared.item(
            emit,
            ItemDraft {
                key,
                body: item_body(Kind::Interruption(wire::Interruption {})),
                at_ms,
                complete: true,
                ..Default::default()
            },
        );
        self.end_turn(emit, TurnOutcome::Interrupted, at_ms, None);
    }

    fn end_turn(
        &mut self,
        emit: &mut Emit,
        outcome: TurnOutcome,
        at_ms: Option<i64>,
        duration_ms: Option<i64>,
    ) {
        self.local_turn = false;
        self.running.clear();
        let Some(turn) = self.shared.turn_ended(emit) else {
            return;
        };
        let at_ms = at_ms.unwrap_or_else(|| self.shared.now_ms());
        let started_at_ms = duration_ms.map_or(turn.started_at_ms, |duration| at_ms - duration);
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("turn:{}", turn.id),
                body: item_body(Kind::Turn(Turn {
                    turn_id: turn.id,
                    outcome: outcome as i32,
                    started_at_ms,
                    cost_usd: None,
                })),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
        // A finished turn's calls and asks cannot change any more, but for
        // subagents still running in the background.
        self.tools
            .retain(|_, tool| tool.awaiting_notification && !tool.finished);
        let tools = &self.tools;
        self.agents.retain(|_, id| tools.contains_key(id));
        let open = self.open_ask_keys();
        self.asks.retain(|key, _| open.contains(key));
    }

    fn assistant_row(&mut self, emit: &mut Emit, row: &Value, at_ms: Option<i64>) {
        self.local_turn = false;
        // The model is answering, whether or not a prompt row said so.
        self.shared.turn_started();
        let uuid = text(row, "uuid").to_owned();
        let message = row.get("message").unwrap_or(&Value::Null);
        let message_id = text(message, "id").to_owned();
        if let Some(model) = message.get("model").and_then(Value::as_str)
            && model != "<synthetic>"
        {
            self.provider.model = Some(model.to_owned());
        }
        if let Some(usage) = message.get("usage") {
            let tokens = [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
            ]
            .iter()
            .filter_map(|field| usage.get(*field).and_then(Value::as_u64))
            .sum::<u64>();
            if tokens > 0 {
                self.context_tokens = Some(tokens);
            }
        }
        // A row from a later message proves an asked call is over.
        for key in self.open_ask_keys() {
            let later = self
                .asks
                .get(&key)
                .and_then(|meta| meta.bound.as_ref())
                .and_then(|bound| self.tools.get(bound))
                .is_some_and(|tool| !message_id.is_empty() && tool.message_id != message_id);
            if later {
                self.close(emit, &key, Decision::unknown());
            }
        }
        // The model writes again only once the dialog's call returned.
        for (key, call) in self.unanswerable_asks() {
            let later = match call.map(|call| self.tools.get(&call)) {
                Some(Some(tool)) => !message_id.is_empty() && tool.message_id != message_id,
                // The call's own row has not landed yet: this may be it.
                Some(None) => false,
                None => true,
            };
            if later {
                self.close_ask_item(emit, &key, ask_item::dismissed());
            }
        }
        if row.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
            let message_text = content_text(message.get("content").unwrap_or(&Value::Null));
            self.shared.item(
                emit,
                ItemDraft {
                    key: uuid,
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: text(row, "error").to_owned(),
                        message: message_text,
                        ..Default::default()
                    })),
                    at_ms,
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        let blocks = match message.get("content") {
            Some(Value::Array(blocks)) => blocks.clone(),
            Some(Value::String(text)) => {
                vec![serde_json::json!({ "type": "text", "text": text })]
            }
            _ => Vec::new(),
        };
        let several = blocks.len() > 1;
        for (index, block) in blocks.iter().enumerate() {
            let key = if several && index > 0 {
                format!("{uuid}#{index}")
            } else {
                uuid.clone()
            };
            match text(block, "type") {
                "text" => {
                    self.shared.item(
                        emit,
                        ItemDraft {
                            key: key.clone(),
                            text: text(block, "text").to_owned(),
                            body: item_body(Kind::Message(wire::Text { complete: true })),
                            at_ms,
                            complete: true,
                            ..Default::default()
                        },
                    );
                    self.shared.note_message(&key);
                }
                "thinking" | "redacted_thinking" => self.shared.item(
                    emit,
                    ItemDraft {
                        key,
                        text: text(block, "thinking").to_owned(),
                        body: item_body(Kind::Thinking(wire::Thinking { complete: true })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                ),
                "tool_use" => {
                    let id = text(block, "id").to_owned();
                    let name = text(block, "name").to_owned();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    self.tool_seen(emit, &id, &name, &input, at_ms, &message_id);
                    let drawn = self.tools.get(&id).is_some_and(|tool| !tool.hidden);
                    if drawn && let Some(ask) = self.ask_for_tool(&name, &input) {
                        self.bind(emit, &ask, &id);
                    }
                }
                other => self.shared.item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(Kind::Unrecognized(wire::Unrecognized {
                            fact_type: format!("assistant/{other}"),
                            summary: String::new(),
                        })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                ),
            }
        }
    }

    fn system_row(&mut self, emit: &mut Emit, row: &Value, at_ms: Option<i64>) {
        let uuid = text(row, "uuid").to_owned();
        match text(row, "subtype") {
            "turn_duration" => {
                self.close_all_unknown(emit);
                let duration = row.get("durationMs").and_then(Value::as_i64);
                self.end_turn(emit, TurnOutcome::Completed, at_ms, duration);
            }
            "compact_boundary" => {
                let metadata = row.get("compactMetadata").unwrap_or(&Value::Null);
                let after = metadata.get("postTokens").and_then(Value::as_u64);
                if after.is_some() {
                    self.context_tokens = after;
                }
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: uuid,
                        body: item_body(Kind::Compaction(wire::Compaction {
                            tokens_before: metadata.get("preTokens").and_then(Value::as_u64),
                            tokens_after: after,
                            automatic: text(metadata, "trigger") == "auto",
                        })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "local_command" => {
                let content = text(row, "content").to_owned();
                let synthetic = serde_json::json!({
                    "type": "user",
                    "uuid": uuid,
                    "timestamp": row.get("timestamp"),
                    "message": { "content": content },
                });
                self.user_row(emit, &synthetic, at_ms);
            }
            _ => {}
        }
    }
}

/// A permission ask offering the scoped entries the terminal's menu holds
/// and the agent's keymap can type: one per suggestion, or all of them as
/// one where Claude folds them.
fn permission_ask(
    server: &str,
    tool: &str,
    input: &Value,
    hook: &Value,
    menus: &PermissionMenus,
) -> (wire::ask::Body, AskShape) {
    let suggestions = hook
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let count = suggestions.len() as u32;
    // A menu the keymap does not know offers no scope: its Yes and its
    // deny are still typed.
    let scopes = if menus.per_suggestion.contains(&count) {
        scope_choices(&suggestions)
    } else if menus.folded.contains(&count) {
        vec![folded(scope_choices(&suggestions))]
    } else {
        Vec::new()
    };
    let shape = AskShape::Permission {
        scopes: scopes
            .iter()
            .map(|scope| scope.destination.clone())
            .collect(),
        suggestions: count,
    };
    (
        wire::ask::Body::Permission(PermissionAsk {
            tool_name: tool.to_owned(),
            input_json: input.to_string().into_bytes(),
            scopes,
            reason: String::new(),
            description: String::new(),
            deny_stops: true,
            deny_can_stop: false,
            server: server.to_owned(),
        }),
        shape,
    )
}

/// Every suggestion as the one entry a folded menu offers: choosing it
/// applies them all, where the first one says.
fn folded(choices: Vec<ScopeChoice>) -> ScopeChoice {
    let mut all = ScopeChoice::default();
    for (at, choice) in choices.into_iter().enumerate() {
        if at == 0 {
            all.destination = choice.destination;
        }
        all.rules.extend(choice.rules);
        all.directories.extend(choice.directories);
        if all.mode.is_empty() {
            all.mode = choice.mode;
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::peer_message_bodies;

    #[test]
    fn peer_messages_are_read_in_both_wordings() {
        assert_eq!(
            peer_message_bodies(
                "Another Claude session sent a message:\n<cross-session-message from=\"amux\">\nhi\n</cross-session-message>\n\nThis came from another Claude session."
            ),
            ["hi"]
        );
        assert_eq!(
            peer_message_bodies(
                "Another Claude session sent a message:\nReply BRAVO.\n\nThis came from another Claude session — not typed by your user."
            ),
            ["Reply BRAVO."]
        );
        assert!(peer_message_bodies("an ordinary prompt").is_empty());
    }
}
