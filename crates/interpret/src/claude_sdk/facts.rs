//! Stream-JSON lines from headless Claude, read into state and items.

use prost::Message as _;
use serde_json::{Value, json};
use wire::claude_sdk_item::Kind;
use wire::{
    BoundaryKind, DecisionOutcome, FormAsk, HealthState, LinkAsk, PermissionAsk, PlanAsk, SignIn,
    SignInState, TaskState as WireTaskState, ToolServer, ToolServerHealth, ToolServerStatus,
    ToolState, Turn, TurnOutcome, UsageLimits, UsageState, UsageWindow,
};

use super::{AskMeta, AskShape, Request, State, TaskState, Tool, ToolDecisionState, item_body};
use crate::claude_common::{
    PLAN_TOOL, QUESTION_TOOL, TASK_TOOLS, apply_task_tool, compact_json, content_text,
    question_ask, result_images, scope_choices, split_tool_name, text, tool_class,
    without_image_bytes,
};
use crate::{Channel, Effect, Emit, Fact, ItemDraft, ask_item, is_status_tool, status_working_on};

const INTERRUPTED: &str = "[Request interrupted by user";

fn str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The error Claude reports when the API rejects its credential.
const AUTHENTICATION_FAILED: &str = "authentication_failed";

impl State {
    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        if fact.channel != Channel::Stream {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Value>(&fact.payload) else {
            return;
        };
        match text(&line, "type") {
            "system" => self.system(emit, &line),
            "stream_event" => self.stream_event(emit, &line),
            "assistant" => self.assistant(emit, &line),
            "user" => self.user(emit, &line),
            "result" => self.result(emit, &line),
            "control_request" => self.control_request_in(emit, &line),
            "control_response" => self.control_response_in(&line),
            "command_lifecycle" => {
                if text(&line, "state") == "started" {
                    self.taken(text(&line, "command_uuid"));
                }
            }
            "rate_limit_event" => self.rate_limit(&line),
            "auth_status" => {
                self.sign_in = Some(match str_field(&line, "error") {
                    Some(error) => SignIn {
                        state: SignInState::Failed as i32,
                        account: String::new(),
                        message: error,
                    },
                    None if line.get("isAuthenticating").and_then(Value::as_bool) == Some(true) => {
                        SignIn {
                            state: SignInState::SignedOut as i32,
                            account: String::new(),
                            message: text(&line, "output").to_owned(),
                        }
                    }
                    None => SignIn {
                        state: SignInState::SignedIn as i32,
                        ..Default::default()
                    },
                })
            }
            "conversation_reset" => {
                self.session = str_field(&line, "new_conversation_id");
                self.context_tokens = None;
                self.boundary(emit, BoundaryKind::Cleared, String::new());
            }
            // Tool progress, summaries, suggestions and the rest are
            // Claude's own interface; they stay in the facts ring.
            _ => {}
        }
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, code: Option<i32>) {
        self.dismiss_asks(emit);
        let cause = match code {
            Some(code) => format!("exit code {code}"),
            None => "killed by a signal".to_owned(),
        };
        self.boundary(emit, BoundaryKind::Exited, cause);
        self.shared.provider_exited();
        self.exited = true;
        self.stream = None;
    }

    fn open_ask_keys(&self) -> Vec<String> {
        self.shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .collect()
    }

    /// Claude took the message with this uuid: an agent message is
    /// consumed.
    fn taken(&mut self, uuid: &str) {
        if let Some(client) = self.clients.get(uuid)
            && client.message
        {
            let id = client.id.clone();
            self.clients.remove(uuid);
            self.shared.message_consumed(&id);
            self.shared.turn_started();
        }
    }

    // --- system ----------------------------------------------------------

    fn system(&mut self, emit: &mut Emit, line: &Value) {
        let uuid = text(line, "uuid").to_owned();
        match text(line, "subtype") {
            "init" => self.init(emit, line),
            "status" => {
                if let Some(mode) = str_field(line, "permissionMode") {
                    self.permission_mode = Some(mode);
                }
            }
            "compact_boundary" => {
                let metadata = line.get("compact_metadata").unwrap_or(&Value::Null);
                let after = metadata.get("post_tokens").and_then(Value::as_u64);
                if after.is_some() {
                    self.context_tokens = after;
                }
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: uuid,
                        body: item_body(Kind::Compaction(wire::Compaction {
                            tokens_before: metadata.get("pre_tokens").and_then(Value::as_u64),
                            tokens_after: after,
                            automatic: text(metadata, "trigger") == "auto",
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
                self.boundary(emit, BoundaryKind::Compacted, String::new());
            }
            "api_retry" => {
                if text(line, "error") == AUTHENTICATION_FAILED {
                    self.sign_in_failed(text(line, "error"));
                }
                let attempt = line.get("attempt").and_then(Value::as_u64).unwrap_or(0) as u32;
                let max_attempts =
                    line.get("max_retries").and_then(Value::as_u64).unwrap_or(0) as u32;
                let delay = line.get("retry_delay_ms").and_then(Value::as_i64);
                let status = line
                    .get("error_status")
                    .and_then(Value::as_u64)
                    .map(|status| format!(" ({status})"))
                    .unwrap_or_default();
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: uuid,
                        body: item_body(Kind::ApiError(wire::ApiError {
                            error_kind: text(line, "error").to_owned(),
                            message: format!("{}{status}", text(line, "error")),
                            will_retry: true,
                            attempt,
                            max_attempts,
                            retry_at_ms: delay.map(|delay| self.shared.now_ms() + delay),
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "model_refusal_fallback" => {
                let from = text(line, "original_model").to_owned();
                let to = text(line, "fallback_model").to_owned();
                self.model = Some(to.clone());
                let reason = str_field(line, "api_refusal_explanation")
                    .or_else(|| str_field(line, "api_refusal_category"))
                    .unwrap_or_else(|| text(line, "content").to_owned());
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: uuid,
                        body: item_body(Kind::ModelSwitch(wire::ModelSwitch { from, to, reason })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "model_refusal_no_fallback" => self.shared.item(
                emit,
                ItemDraft {
                    key: uuid,
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: "refusal".into(),
                        message: str_field(line, "api_refusal_explanation")
                            .unwrap_or_else(|| text(line, "content").to_owned()),
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            ),
            "local_command_output" => self.slash_output(emit, uuid, text(line, "content")),
            "task_started" | "task_updated" | "task_progress" | "task_notification" => {
                self.task_event(emit, line)
            }
            "background_tasks_changed" => {
                if let Some(tasks) = line.get("tasks").and_then(Value::as_array) {
                    self.background = Some(tasks.len() as u32);
                }
            }
            "permission_denied" => {
                let id = text(line, "tool_use_id").to_owned();
                let note = str_field(line, "decision_reason")
                    .unwrap_or_else(|| text(line, "message").to_owned());
                self.decide(
                    emit,
                    &id,
                    ToolDecisionState {
                        outcome: DecisionOutcome::Denied as i32,
                        scope: String::new(),
                        note,
                    },
                );
            }
            "elicitation_complete" => {
                let elicitation = text(line, "elicitation_id").to_owned();
                for key in self.open_ask_keys() {
                    let matches = self.shared.asks().get(&key).is_some_and(|ask| {
                        matches!(&ask.body, Some(wire::ask::Body::Link(link)) if link.url.contains(&elicitation))
                    }) || key == elicitation;
                    if matches && let Some(ask) = self.shared.close_ask(&key) {
                        self.asks.remove(&key);
                        self.emit_ask(
                            emit,
                            &ask,
                            Some(ask_item::outcome(wire::AskOutcome::Answered)),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn init(&mut self, emit: &mut Emit, line: &Value) {
        self.shared.provider_started();
        let session = str_field(line, "session_id");
        let previous = self.session.clone();
        self.session = session.clone().or(previous.clone());
        if let Some(version) = str_field(line, "claude_code_version") {
            self.version = Some(version);
        }
        if let Some(model) = str_field(line, "model") {
            self.model = Some(model);
        }
        if let Some(mode) = str_field(line, "permissionMode") {
            self.permission_mode = Some(mode);
        }
        if let Some(effort) = line.get("effort") {
            self.effort = effort.as_str().map(str::to_owned);
        }
        if let Some(servers) = line.get("mcp_servers").and_then(Value::as_array) {
            self.servers = Some(server_health(servers));
        }
        // Claude repeats its init at every turn; only a new process or a
        // new session is a boundary.
        let kind = if self.inits == 0 {
            Some(if self.incarnation > 1 {
                BoundaryKind::Resumed
            } else {
                BoundaryKind::Started
            })
        } else if std::mem::take(&mut self.exited) {
            Some(BoundaryKind::Restarted)
        } else if session.is_some() && session != previous {
            Some(BoundaryKind::Forked)
        } else {
            None
        };
        self.inits += 1;
        let Some(kind) = kind else {
            return;
        };
        self.boundary(emit, kind, String::new());
    }

    fn rate_limit(&mut self, line: &Value) {
        let info = line.get("rate_limit_info").unwrap_or(&Value::Null);
        let state = match text(info, "status") {
            "allowed" => UsageState::Ok,
            "allowed_warning" => UsageState::NearLimit,
            "rejected" => UsageState::Blocked,
            _ => UsageState::Unknown,
        };
        let windows = info
            .get("unifiedWindows")
            .and_then(Value::as_object)
            .map(|windows| {
                windows
                    .iter()
                    .map(|(name, window)| UsageWindow {
                        name: name.clone(),
                        used_percent: window
                            .get("utilization")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0)
                            * 100.0,
                        resets_at_ms: window
                            .get("resetsAt")
                            .and_then(Value::as_i64)
                            .map(|at| at * 1000),
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.usage = Some(UsageLimits {
            state: state as i32,
            windows,
            credits: None,
        });
    }

    fn task_event(&mut self, emit: &mut Emit, line: &Value) {
        let id = text(line, "task_id").to_owned();
        if id.is_empty() {
            return;
        }
        let now = self.shared.now_ms();
        let task = self.active_tasks.entry(id.clone()).or_insert(TaskState {
            at_ms: now,
            description: String::new(),
            state: WireTaskState::Running as i32,
            tool_count: 0,
            last_tool: String::new(),
            tool_key: String::new(),
            tokens: 0,
        });
        if let Some(description) = str_field(line, "description") {
            task.description = description;
        }
        if let Some(tool) = str_field(line, "tool_use_id") {
            task.tool_key = tool;
        }
        if let Some(last) = str_field(line, "last_tool_name") {
            task.last_tool = last;
        }
        if let Some(usage) = line.get("usage") {
            if let Some(tools) = usage.get("tool_uses").and_then(Value::as_u64) {
                task.tool_count = tools as u32;
            }
            if let Some(tokens) = usage.get("total_tokens").and_then(Value::as_u64) {
                task.tokens = tokens;
            }
        }
        let status = line
            .pointer("/patch/status")
            .or_else(|| line.get("status"))
            .and_then(Value::as_str);
        if let Some(status) = status {
            task.state = match status {
                "completed" => WireTaskState::Completed,
                "failed" => WireTaskState::Failed,
                "killed" | "stopped" | "cancelled" => WireTaskState::Stopped,
                _ => WireTaskState::Running,
            } as i32;
        }
        let wire = task.to_wire(&id);
        let at_ms = task.at_ms;
        let summary = str_field(line, "summary");
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("task:{id}"),
                text: summary.unwrap_or_default(),
                body: item_body(Kind::Task(wire)),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
        // A finished task's summary stays on its item; only running ones
        // are drawn in the strip.
    }

    // --- streams ---------------------------------------------------------

    fn stream_event(&mut self, emit: &mut Emit, line: &Value) {
        let event = line.get("event").unwrap_or(&Value::Null);
        match text(event, "type") {
            "message_start" => {
                let message = event.get("message").unwrap_or(&Value::Null);
                self.stream = str_field(message, "id");
                self.stream_tools.clear();
                self.usage_seen(message.get("usage"));
                self.shared.turn_started();
            }
            "content_block_start" => {
                let Some(message) = self.stream.clone() else {
                    return;
                };
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                let block = event.get("content_block").unwrap_or(&Value::Null);
                let key = format!("{message}:{index}");
                match text(block, "type") {
                    "text" => self.open_block(emit, key, text(block, "text"), false),
                    "thinking" => self.open_block(emit, key, text(block, "thinking"), true),
                    "tool_use" => {
                        let id = text(block, "id").to_owned();
                        self.stream_tools.insert(index, id.clone());
                        self.tool_seen(emit, &id, text(block, "name"), None, line);
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let Some(message) = self.stream.clone() else {
                    return;
                };
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = event.get("delta").unwrap_or(&Value::Null);
                let key = format!("{message}:{index}");
                let appended = match text(delta, "type") {
                    "text_delta" => text(delta, "text"),
                    "thinking_delta" => text(delta, "thinking"),
                    _ => return,
                };
                self.shared.append(emit, &key, appended);
            }
            "content_block_stop" => {
                let Some(message) = self.stream.clone() else {
                    return;
                };
                let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
                let key = format!("{message}:{index}");
                if let Some(open) = self.shared.open_item(&key).cloned() {
                    let thinking = matches!(
                        wire::ClaudeSdkItem::decode(open.body.as_slice()).map(|item| item.kind),
                        Ok(Some(Kind::Thinking(_)))
                    );
                    self.close_block(emit, key, open.text, thinking);
                }
            }
            "message_delta" => self.usage_seen(event.get("usage")),
            "message_stop" => self.stream = None,
            _ => {}
        }
    }

    fn open_block(&mut self, emit: &mut Emit, key: String, text: &str, thinking: bool) {
        let body = if thinking {
            item_body(Kind::Thinking(wire::Thinking { complete: false }))
        } else {
            item_body(Kind::Message(wire::Text { complete: false }))
        };
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text: text.to_owned(),
                body,
                ..Default::default()
            },
        );
    }

    fn close_block(&mut self, emit: &mut Emit, key: String, text: String, thinking: bool) {
        let body = if thinking {
            item_body(Kind::Thinking(wire::Thinking { complete: true }))
        } else {
            item_body(Kind::Message(wire::Text { complete: true }))
        };
        self.shared.item(
            emit,
            ItemDraft {
                key: key.clone(),
                text,
                body,
                complete: true,
                ..Default::default()
            },
        );
        if !thinking {
            self.shared.note_message(&key);
        }
    }

    fn usage_seen(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else {
            return;
        };
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

    /// Claude rejected the credential: the sign-in problem the strip
    /// shows until Claude reports an account again.
    fn sign_in_failed(&mut self, message: &str) {
        let account = self
            .sign_in
            .as_ref()
            .map(|sign_in| sign_in.account.clone())
            .unwrap_or_default();
        self.sign_in = Some(SignIn {
            state: SignInState::Failed as i32,
            account,
            message: message.to_owned(),
        });
    }

    // --- whole messages --------------------------------------------------

    fn assistant(&mut self, emit: &mut Emit, line: &Value) {
        self.shared.turn_started();
        let message = line.get("message").unwrap_or(&Value::Null);
        let message_id = text(message, "id").to_owned();
        if let Some(model) = str_field(message, "model")
            && model != "<synthetic>"
        {
            self.model = Some(model);
        }
        self.usage_seen(message.get("usage"));
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(error) = str_field(line, "error") {
            let message = content_text(message.get("content").unwrap_or(&Value::Null));
            if error == AUTHENTICATION_FAILED {
                self.sign_in_failed(&message);
            }
            self.shared.item(
                emit,
                ItemDraft {
                    key: text(line, "uuid").to_owned(),
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: error,
                        message,
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        let base = self.blocks.get(&message_id).copied().unwrap_or(0);
        self.blocks
            .insert(message_id.clone(), base + blocks.len() as u32);
        for (offset, block) in blocks.iter().enumerate() {
            let key = format!("{message_id}:{}", base + offset as u32);
            match text(block, "type") {
                "text" => self.close_block(emit, key, text(block, "text").to_owned(), false),
                "thinking" | "redacted_thinking" => {
                    self.close_block(emit, key, text(block, "thinking").to_owned(), true)
                }
                "tool_use" => {
                    let id = text(block, "id").to_owned();
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    self.tool_seen(emit, &id, text(block, "name"), Some(&input), line);
                }
                other => self.shared.item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(Kind::Unrecognized(wire::Unrecognized {
                            fact_type: format!("assistant/{other}"),
                            summary: String::new(),
                        })),
                        complete: true,
                        ..Default::default()
                    },
                ),
            }
        }
    }

    fn tool_seen(
        &mut self,
        emit: &mut Emit,
        id: &str,
        name: &str,
        input: Option<&Value>,
        line: &Value,
    ) {
        if id.is_empty() {
            return;
        }
        let (server, tool_name) = split_tool_name(name);
        let status = is_status_tool(&server, &tool_name);
        if status
            && let Some(input) = input
            && let Some(working_on) = status_working_on(input.to_string().as_bytes())
        {
            self.shared.set_working_on(working_on);
        }
        let now = self.shared.now_ms();
        let tool = self.tools.entry(id.to_owned()).or_insert_with(|| Tool {
            at_ms: now,
            class: tool_class(&server, &tool_name) as i32,
            hidden: status || (server.is_empty() && TASK_TOOLS.contains(&tool_name.as_str())),
            name: tool_name,
            server,
            input: String::new(),
            state: ToolState::Running as i32,
            outcome_text: String::new(),
            outcome_json: String::new(),
            background: false,
            decision: None,
            ended_at_ms: None,
            parent_key: text(line, "parent_tool_use_id").to_owned(),
            images: Vec::new(),
            emitted: Vec::new(),
        });
        if let Some(input) = input {
            tool.input = input.to_string();
            tool.background = input
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
        self.emit_tool(emit, id);
    }

    fn user(&mut self, emit: &mut Emit, line: &Value) {
        let uuid = text(line, "uuid").to_owned();
        let content = line.pointer("/message/content").unwrap_or(&Value::Null);
        let whole = content_text(content);
        if line.get("isReplay").and_then(Value::as_bool) == Some(true) {
            if let Some(output) = local_output(&whole) {
                return self.slash_output(emit, uuid, &output);
            }
            self.steer_replayed(emit, &uuid);
            return self.taken(&uuid);
        }
        if let Value::Array(blocks) = content {
            for block in blocks {
                if text(block, "type") == "tool_result" {
                    self.tool_result(emit, block, line);
                }
            }
        }
        // The compaction summary Claude writes back is its own; the SDK's
        // compaction row carries the counts.
        if whole.trim_start().starts_with(INTERRUPTED) {
            self.interrupted = true;
        }
    }

    /// Claude replays a prompt sent into the running turn when it takes it:
    /// before the turn's result it joined that turn at a tool boundary and
    /// its item is marked steered; after it, no boundary was left and it
    /// opened a turn of its own as an ordinary prompt.
    fn steer_replayed(&mut self, emit: &mut Emit, uuid: &str) {
        let Some(client) = self.clients.get(uuid).filter(|client| !client.message) else {
            return;
        };
        let id = client.id.clone();
        let Some(entry) = self.shared.steer_reflected(|entry| entry.input_id == id) else {
            return;
        };
        self.clients.remove(uuid);
        let body = if self.shared.is_busy() {
            Kind::Steer(wire::Steer {})
        } else {
            self.shared.turn_started();
            Kind::Prompt(wire::Prompt {})
        };
        self.shared.item(
            emit,
            ItemDraft {
                key: uuid.to_owned(),
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(body),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn tool_result(&mut self, emit: &mut Emit, block: &Value, line: &Value) {
        let id = text(block, "tool_use_id").to_owned();
        let output = content_text(block.get("content").unwrap_or(&Value::Null));
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let result = line
            .get("tool_use_result")
            .or_else(|| line.get("toolUseResult"));
        let now = self.shared.now_ms();
        let Some(tool) = self.tools.get_mut(&id) else {
            return;
        };
        let denied = tool
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == DecisionOutcome::Denied as i32);
        tool.state = if denied {
            ToolState::Denied
        } else if is_error {
            ToolState::Failed
        } else {
            ToolState::Succeeded
        } as i32;
        tool.outcome_text = output;
        let images = result_images(block.get("content").unwrap_or(&Value::Null));
        if !images.is_empty() {
            tool.images = Vec::new();
            for (image, write) in images {
                tool.images.push(image);
                emit.effect(write);
            }
        }
        if let Some(result) = result {
            tool.outcome_json = compact_json(&without_image_bytes(result));
        }
        tool.ended_at_ms.get_or_insert(now);
        if tool.server.is_empty() && TASK_TOOLS.contains(&tool.name.as_str()) {
            let input = serde_json::from_str::<Value>(&tool.input).unwrap_or(Value::Null);
            let name = tool.name.clone();
            apply_task_tool(
                &mut self.tasks,
                &name,
                &input,
                result.unwrap_or(&Value::Null),
            );
        }
        self.emit_tool(emit, &id);
    }

    fn slash_output(&mut self, emit: &mut Emit, key: String, output: &str) {
        let output = local_output(output).unwrap_or_else(|| output.to_owned());
        let (command, args) = match &self.slash {
            Some(slash) => {
                let (command, args) = slash.split_once(char::is_whitespace).unwrap_or((slash, ""));
                (command.to_owned(), args.trim().to_owned())
            }
            None => (String::new(), String::new()),
        };
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text: output,
                body: item_body(Kind::Slash(wire::SlashOutput { command, args })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn result(&mut self, emit: &mut Emit, line: &Value) {
        self.dismiss_asks(emit);
        if let Some(window) = line
            .get("modelUsage")
            .and_then(Value::as_object)
            .and_then(|models| {
                let model = self.model.as_deref().unwrap_or_default();
                models
                    .get(model)
                    .or_else(|| models.values().next())
                    .and_then(|usage| usage.get("contextWindow"))
                    .and_then(Value::as_u64)
            })
        {
            self.context_window = Some(window);
        }
        let subtype = text(line, "subtype");
        let aborted = text(line, "terminal_reason").starts_with("aborted");
        // An API failure ends the turn with subtype success and is_error;
        // the assistant row before it already carried the error.
        let api_failure = line.get("is_error").and_then(Value::as_bool) == Some(true);
        let outcome = if std::mem::take(&mut self.interrupted) || aborted {
            TurnOutcome::Interrupted
        } else if subtype == "success" && !api_failure {
            TurnOutcome::Completed
        } else {
            TurnOutcome::Failed
        };
        if outcome == TurnOutcome::Failed && subtype != "success" {
            let errors = line
                .get("errors")
                .and_then(Value::as_array)
                .map(|errors| {
                    errors
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            self.shared.item(
                emit,
                ItemDraft {
                    key: format!("{}:error", text(line, "uuid")),
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: subtype.to_owned(),
                        message: errors,
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        let Some(turn) = self.shared.turn_ended(emit) else {
            return;
        };
        let at_ms = self.shared.now_ms();
        let started_at_ms = line
            .get("duration_ms")
            .and_then(Value::as_i64)
            .map_or(turn.started_at_ms, |duration| at_ms - duration);
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("turn:{}", turn.id),
                body: item_body(Kind::Turn(Turn {
                    turn_id: turn.id,
                    outcome: outcome as i32,
                    started_at_ms,
                    cost_usd: line.get("total_cost_usd").and_then(Value::as_f64),
                })),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
        self.tools.clear();
        self.blocks.clear();
    }

    // --- control ---------------------------------------------------------

    fn control_request_in(&mut self, emit: &mut Emit, line: &Value) {
        let request_id = text(line, "request_id").to_owned();
        let request = line.get("request").unwrap_or(&Value::Null);
        match text(request, "subtype") {
            "can_use_tool" => self.can_use_tool(emit, &request_id, request),
            "elicitation" => {
                let server = text(request, "mcp_server_name").to_owned();
                let message = text(request, "message").to_owned();
                let (body, shape) = if text(request, "mode") == "url" {
                    (
                        wire::ask::Body::Link(LinkAsk {
                            server,
                            message,
                            url: text(request, "url").to_owned(),
                        }),
                        AskShape::Link,
                    )
                } else {
                    (
                        wire::ask::Body::Form(FormAsk {
                            server,
                            message,
                            schema_json: request
                                .get("requested_schema")
                                .map(|schema| schema.to_string().into_bytes())
                                .unwrap_or_default(),
                        }),
                        AskShape::Form,
                    )
                };
                self.asks.insert(
                    request_id.clone(),
                    AskMeta {
                        tool_use_id: String::new(),
                        input: String::new(),
                        suggestions: Vec::new(),
                        shape,
                    },
                );
                let ask = wire::Ask {
                    item_key: ask_item::key(&request_id),
                    key: request_id,
                    body: Some(body),
                    opened_at_ms: self.shared.now_ms(),
                };
                self.emit_ask(emit, &ask, None);
                self.shared.open_ask(ask);
            }
            other => emit.effect(Effect::ProviderWrite(
                serde_json::to_vec(&json!({
                    "type": "control_response",
                    "response": {
                        "subtype": "error",
                        "request_id": request_id,
                        "error": format!("amux does not handle {other}"),
                    },
                }))
                .expect("json"),
            )),
        }
    }

    fn can_use_tool(&mut self, emit: &mut Emit, request_id: &str, request: &Value) {
        let name = text(request, "tool_name");
        let input = request.get("input").cloned().unwrap_or(Value::Null);
        let tool_use_id = text(request, "tool_use_id").to_owned();
        // The call's row may not have been written yet: the request names
        // it, so the ask always has an item to point at.
        self.tool_seen(emit, &tool_use_id, name, Some(&input), request);
        let (server, tool) = split_tool_name(name);
        let suggestions = request
            .get("permission_suggestions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (body, shape) = match tool.as_str() {
            QUESTION_TOOL if server.is_empty() => {
                let (question, _) = question_ask(&input);
                let questions = question
                    .questions
                    .iter()
                    .map(|question| {
                        (
                            question.question.clone(),
                            question
                                .options
                                .iter()
                                .map(|option| option.label.clone())
                                .collect(),
                            question.multi_select,
                        )
                    })
                    .collect();
                (
                    wire::ask::Body::Question(question),
                    AskShape::Question(questions),
                )
            }
            PLAN_TOOL if server.is_empty() => (
                wire::ask::Body::Plan(PlanAsk {
                    plan: text(&input, "plan").to_owned(),
                    offers_auto_accept: true,
                }),
                AskShape::Plan,
            ),
            _ => {
                let reason = match request.get("decision_reason") {
                    Some(Value::String(reason)) => reason.clone(),
                    Some(reason @ Value::Object(_)) => str_field(reason, "reason")
                        .or_else(|| str_field(reason, "type"))
                        .unwrap_or_default(),
                    _ => str_field(request, "blocked_path")
                        .map(|path| format!("outside the allowed directories: {path}"))
                        .unwrap_or_default(),
                };
                (
                    wire::ask::Body::Permission(PermissionAsk {
                        tool_name: tool.clone(),
                        input_json: input.to_string().into_bytes(),
                        scopes: scope_choices(&suggestions),
                        reason,
                        description: str_field(request, "description")
                            .or_else(|| str_field(request, "title"))
                            .unwrap_or_default(),
                        deny_stops: false,
                        deny_can_stop: true,
                        server: server.clone(),
                    }),
                    AskShape::Permission,
                )
            }
        };
        self.asks.insert(
            request_id.to_owned(),
            AskMeta {
                tool_use_id: tool_use_id.clone(),
                input: input.to_string(),
                suggestions: suggestions.iter().map(Value::to_string).collect(),
                shape,
            },
        );
        let drawn = self
            .tools
            .get(&tool_use_id)
            .is_some_and(|tool| !tool.hidden);
        self.shared.open_ask(wire::Ask {
            key: request_id.to_owned(),
            item_key: if drawn { tool_use_id } else { String::new() },
            body: Some(body),
            opened_at_ms: self.shared.now_ms(),
        });
    }

    /// Responses to requests: the interpreter's own, matched by id, and
    /// the agent process's initialize, mcp_status and get_context_usage,
    /// recognised by their shape.
    fn control_response_in(&mut self, line: &Value) {
        let response = line.get("response").unwrap_or(&Value::Null);
        let request_id = text(response, "request_id");
        let ok = text(response, "subtype") == "success";
        let body = response.get("response").unwrap_or(&Value::Null);
        match self.requests.remove(request_id) {
            Some(Request::Model(model)) if ok => self.model = model.or(self.model.take()),
            Some(Request::Mode(mode)) if ok => self.permission_mode = Some(mode),
            Some(_) => {}
            None => {
                // Claude reports its init only once the first message
                // arrives, so the answer to initialize is what says it
                // takes input; without it a queued first prompt would wait
                // for an init that only a prompt can bring.
                if ok && body.get("commands").is_some() {
                    self.shared.provider_started();
                }
                if let Some(account) = body.get("account") {
                    self.sign_in = Some(SignIn {
                        state: SignInState::SignedIn as i32,
                        account: str_field(account, "email")
                            .or_else(|| str_field(account, "subscriptionType"))
                            .unwrap_or_default(),
                        message: String::new(),
                    });
                }
                if let Some(servers) = body.get("mcpServers").and_then(Value::as_array) {
                    self.servers = Some(server_health(servers));
                }
                if let Some(total) = body.get("totalTokens").and_then(Value::as_u64) {
                    self.context_tokens = Some(total);
                    self.context_window = body
                        .get("maxTokens")
                        .and_then(Value::as_u64)
                        .or(self.context_window);
                    self.context_breakdown = body
                        .get("categories")
                        .and_then(Value::as_array)
                        .map(|categories| {
                            categories
                                .iter()
                                .map(|category| {
                                    (
                                        text(category, "name").to_owned(),
                                        category.get("tokens").and_then(Value::as_u64).unwrap_or(0),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                }
            }
        }
    }
}

/// A local command's output as Claude replays it.
fn local_output(text: &str) -> Option<String> {
    let text = text.trim();
    for tag in ["local-command-stdout", "local-command-stderr"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(rest) = text.strip_prefix(&open) {
            return Some(
                rest.split(&close)
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            );
        }
    }
    None
}

fn server_health(servers: &[Value]) -> ToolServerHealth {
    let servers = servers
        .iter()
        .map(|server| ToolServer {
            name: text(server, "name").to_owned(),
            status: match text(server, "status") {
                "connected" => ToolServerStatus::Ready,
                "pending" => ToolServerStatus::Starting,
                "needs-auth" => ToolServerStatus::NeedsAuth,
                "failed" => ToolServerStatus::Failed,
                _ => ToolServerStatus::Unspecified,
            } as i32,
            error: text(server, "error").to_owned(),
        })
        .collect::<Vec<_>>();
    // A server waiting for sign-in is the person's choice, not a failure.
    let degraded = servers
        .iter()
        .any(|server| server.status == ToolServerStatus::Failed as i32);
    ToolServerHealth {
        state: if degraded {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        } as i32,
        servers,
    }
}
