//! Transcript rows, hook payloads and the launch fact, read into state and
//! items. The module docs in `mod.rs` state the inference rules.

use serde_json::Value;
use wire::claude_pty_item::Kind;
use wire::{
    Ask, BoundaryKind, DecisionOutcome, PermissionAsk, PlanAsk, ToolState, Turn, TurnOutcome,
};

use super::{AskMeta, AskShape, Decision, PendingMessage, Slash, State, Tool, item_body};
use crate::claude_common::{
    PLAN_TOOL, QUESTION_TOOL, TASK_TOOLS, apply_task_tool, compact_json, content_text,
    question_ask, scope_choices, split_tool_name, text, timestamp_ms, tool_class,
};
use crate::{Channel, Emit, Fact, ItemDraft, is_status_tool, status_working_on};

/// How terminal Claude words a call the person refused, on the tool result.
const REJECTED: &str = "The user doesn't want to proceed with this tool use.";
const REJECTED_NOTE: &str = "the user said:\n";
const INTERRUPTED: &str = "[Request interrupted by user";

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
            Channel::Agent => self.agent_fact(&value),
            Channel::Stream | Channel::Rpc | Channel::Tools => {}
        }
    }

    fn agent_fact(&mut self, value: &Value) {
        if text(value, "type") != "launch" {
            return;
        }
        let provider = &mut self.provider;
        let version = text(value, "version");
        provider.version = (!version.is_empty()).then(|| version.to_owned());
        provider.keymap = text(value, "keymap").to_owned();
        provider.relaunched = provider.launches > 0;
        provider.launches += 1;
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, code: Option<i32>) {
        self.close_all_unknown(emit);
        let cause = match code {
            Some(code) => format!("exit code {code}"),
            None => "killed by a signal".to_owned(),
        };
        self.boundary(emit, BoundaryKind::Exited, cause);
        self.shared.provider_exited();
        self.local_turn = false;
    }

    // --- hooks -----------------------------------------------------------

    fn hook(&mut self, emit: &mut Emit, hook: &Value) {
        if let Some(mode) = hook.get("permission_mode").and_then(Value::as_str) {
            self.provider.permission_mode = Some(mode.to_owned());
        }
        match text(hook, "hook_event_name") {
            "SessionStart" => self.session_start(emit, hook),
            "SessionEnd" => self.close_all_unknown(emit),
            "UserPromptSubmit" => self.shared.provider_started(),
            "PreToolUse" => {
                let id = text(hook, "tool_use_id");
                let input = hook.get("tool_input").cloned().unwrap_or(Value::Null);
                self.tool_seen(emit, id, text(hook, "tool_name"), &input, None, None);
            }
            "PermissionRequest" => self.permission_request(hook),
            "PostToolUse" => self.post_tool_use(emit, hook, false),
            "PostToolUseFailure" => self.post_tool_use(emit, hook, true),
            "Stop" => {
                if let Some(tasks) = hook.get("background_tasks").and_then(Value::as_array) {
                    self.background = Some(tasks.len() as u32);
                }
                self.close_all_unknown(emit);
            }
            _ => {}
        }
    }

    fn session_start(&mut self, emit: &mut Emit, hook: &Value) {
        self.shared.provider_started();
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
        if kind == BoundaryKind::Cleared && self.tasks.is_some() {
            self.tasks = Some(Vec::new());
        }
        self.boundary(emit, kind, String::new());
    }

    fn permission_request(&mut self, hook: &Value) {
        let name = text(hook, "tool_name");
        let input = hook.get("tool_input").cloned().unwrap_or(Value::Null);
        let (server, tool) = split_tool_name(name);
        let (body, shape) = match tool.as_str() {
            QUESTION_TOOL if server.is_empty() => {
                let (question, questions) = question_ask(&input);
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
            _ => permission_ask(&server, &tool, &input, hook),
        };
        let seq = self.next_seq();
        self.next_ask += 1;
        let key = format!("ask:{}", self.next_ask);
        let bound = self.tool_for_ask(name, &input);
        self.asks.insert(
            key.clone(),
            AskMeta {
                seq,
                tool_name: name.to_owned(),
                input: input.to_string(),
                shape,
                bound: bound.clone(),
                closed: None,
            },
        );
        self.shared.open_ask(Ask {
            key,
            item_key: bound.unwrap_or_default(),
            body: Some(body),
            opened_at_ms: self.shared.now_ms(),
        });
    }

    fn post_tool_use(&mut self, emit: &mut Emit, hook: &Value, failed: bool) {
        let id = text(hook, "tool_use_id").to_owned();
        let name = text(hook, "tool_name");
        let input = hook.get("tool_input").cloned().unwrap_or(Value::Null);
        self.tool_seen(emit, &id, name, &input, None, None);
        // An ask this call answered that never learned its call: the hook
        // names both.
        let drawn = self.tools.get(&id).is_some_and(|tool| !tool.hidden);
        if drawn
            && let Some(ask) = self.ask_for_tool(name, &input)
            && self.shared.asks().get(&ask).is_some()
        {
            self.bind(emit, &ask, &id);
        }
        let response = hook.get("tool_response").cloned().unwrap_or(Value::Null);
        if let Some(tool) = self.tools.get_mut(&id) {
            if !tool.finished {
                tool.finished = true;
                tool.state = if failed {
                    ToolState::Failed as i32
                } else {
                    ToolState::Succeeded as i32
                };
                if failed {
                    tool.outcome_text = text(hook, "error").to_owned();
                }
            }
            if tool.outcome_json.is_empty() {
                tool.outcome_json = compact_json(&response);
            }
            if tool.ended_at_ms.is_none() {
                tool.ended_at_ms = Some(match hook.get("duration_ms").and_then(Value::as_i64) {
                    Some(duration) => tool.at_ms + duration,
                    None => self.shared.now_ms(),
                });
            }
        }
        self.task_tool(&id, &response);
        self.close_for_tool(emit, &id, DecisionOutcome::Allowed);
        self.emit_tool(emit, &id);
    }

    // --- tools -----------------------------------------------------------

    /// A call seen from a hook or its row. The first report creates it; a
    /// later one fills in what it adds. `row` carries the row's timestamp
    /// and message id.
    fn tool_seen(
        &mut self,
        emit: &mut Emit,
        id: &str,
        name: &str,
        input: &Value,
        at_ms: Option<i64>,
        message_id: Option<&str>,
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
                    message_id: None,
                    finished: false,
                    hidden,
                    emitted: Vec::new(),
                },
            );
        }
        if let Some(message_id) = message_id
            && let Some(tool) = self.tools.get_mut(id)
        {
            tool.message_id = Some(message_id.to_owned());
        }
        self.emit_tool(emit, id);
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
        let Some(tool) = self.tools.get_mut(&id) else {
            return;
        };
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
        if !result.is_null() {
            tool.outcome_json = compact_json(&result);
        }
        if tool.ended_at_ms.is_none() {
            tool.ended_at_ms = at_ms.or(Some(self.shared.now_ms()));
        }
        let outcome = if rejected {
            DecisionOutcome::Denied
        } else {
            DecisionOutcome::Allowed
        };
        for key in self.open_ask_keys() {
            if self.asks.get(&key).and_then(|meta| meta.bound.as_deref()) == Some(id.as_str()) {
                let mut decision = Decision::elsewhere(outcome);
                if rejected && let Some(at) = output.find(REJECTED_NOTE) {
                    decision.note = output[at + REJECTED_NOTE.len()..].trim().to_owned();
                }
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
        // A subagent's own steps; its call and answer are the parent's rows.
        if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
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
    fn prompt_row(&mut self, emit: &mut Emit, key: String, text: String, at_ms: Option<i64>) {
        self.close_all_unknown(emit);
        let input_id = match self.shared.reflect_prompt() {
            Some(input_id) => input_id,
            None => {
                self.shared.turn_started();
                Vec::new()
            }
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

    fn interrupted(&mut self, emit: &mut Emit, key: String, at_ms: Option<i64>) {
        self.close_all_unknown(emit);
        let running = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.finished)
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
        // A finished turn's calls and asks cannot change any more.
        self.tools.clear();
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
                .and_then(|tool| tool.message_id.as_deref())
                .is_some_and(|asked| !message_id.is_empty() && asked != message_id);
            if later {
                self.close(emit, &key, Decision::unknown());
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
                    self.tool_seen(emit, &id, &name, &input, at_ms, Some(&message_id));
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
            "api_error" => {
                let error = row.get("error").unwrap_or(&Value::Null);
                let attempt = row.get("retryAttempt").and_then(Value::as_u64).unwrap_or(0) as u32;
                let max_attempts =
                    row.get("maxRetries").and_then(Value::as_u64).unwrap_or(0) as u32;
                let kind = error
                    .pointer("/error/type")
                    .or_else(|| error.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let message = error
                    .pointer("/error/message")
                    .or_else(|| error.get("message"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| text(row, "content").to_owned());
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: uuid,
                        body: item_body(Kind::ApiError(wire::ApiError {
                            error_kind: kind.to_owned(),
                            message,
                            will_retry: attempt < max_attempts,
                            attempt,
                            max_attempts,
                            retry_at_ms: row
                                .get("retryInMs")
                                .and_then(Value::as_f64)
                                .zip(at_ms)
                                .map(|(wait, at)| at + wait as i64),
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

fn permission_ask(
    server: &str,
    tool: &str,
    input: &Value,
    hook: &Value,
) -> (wire::ask::Body, AskShape) {
    let suggestions = hook
        .get("permission_suggestions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let scopes = scope_choices(&suggestions);
    let shape = AskShape::Permission {
        scopes: scopes
            .iter()
            .map(|scope| scope.destination.clone())
            .collect(),
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
