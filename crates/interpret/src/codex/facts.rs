//! What the app server writes: responses, requests and notifications.

use serde_json::{Value, json};
use wire::{
    AccessGrant, ApiError, CodexAsk, CommandApproval, Decision, DecisionOutcome,
    FileChangeApproval, FormAsk, LinkAsk, McpToolApproval, ModelSwitch, Question, QuestionAsk,
    QuestionOption, ReviewerVerdict, SignIn, SignInState, TaskListStatus, ToolClass, ToolDecision,
    ToolServer, ToolServerHealth, ToolServerStatus, ToolState, Turn, TurnOutcome, UsageLimits,
    UsageState, UsageWindow, Work, codex_ask, codex_item, work,
};

use super::{
    AskMeta, InjectConsumption, Request, State, Streamed, WorkState, ask_key, item_body, work_ask,
    work_complete,
};
use crate::claude_common::{compact_json, text};
use crate::{Channel, Emit, Fact, ItemDraft, ask_item, is_status_tool, status_working_on};

/// Notifications that carry nothing a client draws, or that another fact
/// already covers.
const QUIET: &[&str] = &[
    "thread/status/changed",
    "thread/name/updated",
    "thread/compacted",
    "thread/archived",
    "thread/unarchived",
    "remoteControl/status/changed",
    "serverRequest/resolved",
    "guardianWarning",
    "warning",
    "item/fileChange/outputDelta",
    "item/commandExecution/terminalInteraction",
];

const DEFAULT_DECISIONS: &[&str] = &["accept", "acceptForSession", "decline", "cancel"];

fn int(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A sandbox policy as the mode name inputs use.
fn sandbox_mode(policy: &Value) -> Option<String> {
    let kind = match policy {
        Value::String(kind) => kind.as_str(),
        other => other.get("type")?.as_str()?,
    };
    Some(
        match kind {
            "readOnly" => "read-only",
            "workspaceWrite" => "workspace-write",
            "dangerFullAccess" => "danger-full-access",
            other => other,
        }
        .to_owned(),
    )
}

fn tool_state(status: &str) -> ToolState {
    match status {
        "inProgress" => ToolState::Running,
        "completed" => ToolState::Succeeded,
        "failed" => ToolState::Failed,
        "declined" => ToolState::Denied,
        _ => ToolState::Running,
    }
}

/// An offered approval decision as the wire's, and the response it sends.
fn offered_decision(offered: &Value) -> Option<(Decision, String)> {
    let decision = match offered {
        Value::String(name) => match name.as_str() {
            "accept" => Decision::Approve,
            "acceptForSession" => Decision::ApproveSession,
            "decline" => Decision::Deny,
            "cancel" => Decision::Abort,
            _ => return None,
        },
        Value::Object(object) => {
            if object.contains_key("acceptWithExecpolicyAmendment") {
                Decision::ApproveSimilar
            } else if object.contains_key("applyNetworkPolicyAmendment") {
                Decision::ApproveNetwork
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some((decision, json!({ "decision": offered }).to_string()))
}

/// Every string under a `host` key.
fn hosts(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("host", Value::String(host)) => out.push(host.clone()),
                    _ => hosts(value, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| hosts(item, out)),
        _ => {}
    }
}

/// "Reconnecting... 2/5" as attempt and maximum.
fn attempts(message: &str) -> (u32, u32) {
    let Some(tail) = message.rsplit(' ').next() else {
        return (0, 0);
    };
    let mut parts = tail.split('/');
    match (
        parts.next().and_then(|n| n.parse().ok()),
        parts.next().and_then(|n| n.parse().ok()),
    ) {
        (Some(attempt), Some(max)) => (attempt, max),
        _ => (0, 0),
    }
}

/// Codex's typed error kind: the name of its error-info variant.
fn error_kind(info: &Value) -> String {
    match info {
        Value::String(kind) => kind.clone(),
        Value::Object(object) => object.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    }
}

fn unauthorized(error: &Value) -> bool {
    let info = error.get("codexErrorInfo").unwrap_or(&Value::Null);
    info.as_str() == Some("unauthorized")
        || info
            .as_object()
            .and_then(|object| object.values().next())
            .and_then(|inner| int(inner, "httpStatusCode"))
            == Some(401)
}

impl State {
    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        if fact.channel != Channel::Rpc {
            return self.unrecognized(
                emit,
                &format!("{:?}", fact.channel),
                "a fact on a channel Codex does not use",
            );
        }
        let Ok(message) = serde_json::from_slice::<Value>(&fact.payload) else {
            return self.unrecognized(emit, "unparsed", "a line that is not JSON");
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        match (
            message.get("method").and_then(Value::as_str),
            message.get("id"),
        ) {
            (Some(method), Some(id)) => self.server_request(emit, method, id, &params),
            (Some(method), None) => self.notification(emit, method, &params),
            (None, Some(id)) => self.response(emit, id, &message),
            (None, None) => self.unrecognized(emit, "message", "neither a request nor a response"),
        }
    }

    fn local_key(&mut self, prefix: &str) -> String {
        self.next_boundary += 1;
        format!("{prefix}:{}", self.next_boundary)
    }

    fn unrecognized(&mut self, emit: &mut Emit, fact_type: &str, summary: &str) {
        let key = self.local_key("unrecognized");
        self.emit_item(
            emit,
            ItemDraft {
                key,
                body: item_body(codex_item::Kind::Unrecognized(wire::Unrecognized {
                    fact_type: fact_type.to_owned(),
                    summary: summary.to_owned(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn error_item(&mut self, emit: &mut Emit, key: String, error: ApiError) {
        self.emit_item(
            emit,
            ItemDraft {
                key,
                body: item_body(codex_item::Kind::Error(error)),
                complete: true,
                ..Default::default()
            },
        );
    }

    // --- responses -------------------------------------------------------

    fn response(&mut self, emit: &mut Emit, id: &Value, message: &Value) {
        let result = message.get("result").unwrap_or(&Value::Null);
        let error = message.get("error");
        let Some(request) = id.as_str().and_then(|id| self.requests.remove(id)) else {
            // The agent process's own handshake.
            if let Some(thread) = result.get("thread") {
                self.thread_started(emit, thread, Some(result));
            } else if let Some(account) = result.get("account") {
                self.sign_in = Some(match account {
                    Value::Null => SignIn {
                        state: SignInState::SignedOut as i32,
                        ..Default::default()
                    },
                    account => SignIn {
                        state: SignInState::SignedIn as i32,
                        account: opt_text(account, "email")
                            .or_else(|| opt_text(account, "type"))
                            .unwrap_or_default(),
                        message: String::new(),
                    },
                });
            }
            return;
        };
        if let Some(error) = error {
            // A steer that lost the race with the turn's end is not a
            // failure: the prompt waits for the next turn.
            if let Request::Steer { input_id } = &request {
                self.shared.steer_refused(input_id);
                return;
            }
            let key = format!("error:rpc:{}", id.as_str().unwrap_or_default());
            self.error_item(
                emit,
                key,
                ApiError {
                    error_kind: "request".into(),
                    message: text(error, "message").to_owned(),
                    ..Default::default()
                },
            );
            match request {
                Request::Turn { consumes } => {
                    self.shared.reflect_prompt();
                    self.shared.turn_abandoned();
                    // Nothing will consume them now.
                    for envelope in consumes {
                        self.shared.message_consumed(&envelope);
                    }
                }
                Request::Compact => self.shared.turn_abandoned(),
                Request::Steer { .. } => {}
                Request::Inject { envelope_id, .. } => {
                    self.shared.message_consumed(&envelope_id);
                }
                Request::Interrupt => {}
            }
            return;
        }
        match request {
            Request::Turn { consumes } => {
                if self.active_turn.is_none() {
                    self.active_turn = result.get("turn").and_then(|turn| opt_text(turn, "id"));
                }
                for envelope in consumes {
                    self.shared.message_consumed(&envelope);
                }
            }
            Request::Inject {
                envelope_id,
                during_turn,
                turn_over,
            } => {
                if during_turn && self.consumption == InjectConsumption::DrainedMidTurn {
                    if turn_over && !self.shared.is_busy() {
                        // The turn it was sent into ended first: nothing
                        // answers it until a turn starts.
                        self.kick(emit, vec![envelope_id]);
                    } else {
                        // Still running, or a later turn took the recorded
                        // item into its context.
                        self.shared.message_consumed(&envelope_id);
                    }
                }
            }
            Request::Steer { .. } | Request::Interrupt | Request::Compact => {}
        }
    }

    /// The thread, from the handshake's response or `thread/started`.
    fn thread_started(&mut self, emit: &mut Emit, thread: &Value, response: Option<&Value>) {
        let Some(id) = opt_text(thread, "id") else {
            return;
        };
        if let Some(response) = response {
            if let Some(model) = opt_text(response, "model") {
                self.launch_model.get_or_insert(model.clone());
                self.model = Some(model);
            }
            if let Some(policy) = opt_text(response, "approvalPolicy") {
                self.approval = Some(policy);
            }
            if let Some(sandbox) = response.get("sandbox").and_then(sandbox_mode) {
                self.sandbox = Some(sandbox);
            }
            if let Some(effort) = opt_text(response, "reasoningEffort") {
                self.effort = Some(effort);
            }
        }
        if let Some(known) = &self.thread_id {
            // The agent process resumed the thread it had, after a restart
            // or on its own; a thread started anywhere else is a child's.
            if response.is_some() && *known == id {
                self.shared.provider_started();
                self.boundary(emit, wire::BoundaryKind::Resumed, String::new());
                self.release_held(emit);
            }
            return;
        }
        self.thread_id = Some(id);
        if let Some(version) = opt_text(thread, "cliVersion") {
            self.version = Some(version);
        }
        self.shared.provider_started();
        let kind = if opt_text(thread, "forkedFromId").is_some() {
            wire::BoundaryKind::Forked
        } else if thread
            .get("turns")
            .and_then(Value::as_array)
            .is_some_and(|turns| !turns.is_empty())
        {
            wire::BoundaryKind::Resumed
        } else {
            wire::BoundaryKind::Started
        };
        self.boundary(emit, kind, String::new());
        self.release_held(emit);
    }

    // --- requests from the server ----------------------------------------

    fn server_request(&mut self, emit: &mut Emit, method: &str, id: &Value, params: &Value) {
        let key = ask_key(id);
        let item_id = text(params, "itemId").to_owned();
        let (item_key, body, decisions) = match method {
            "item/commandExecution/requestApproval" => {
                if !self.works.contains_key(&item_id) {
                    let at_ms = int(params, "startedAtMs").unwrap_or(self.shared.now_ms());
                    self.works.insert(
                        item_id.clone(),
                        WorkState {
                            at_ms,
                            work: Some(Work {
                                of: Some(work::Of::Command(wire::CommandWork {
                                    command: text(params, "command").to_owned(),
                                    cwd: text(params, "cwd").to_owned(),
                                    ..Default::default()
                                })),
                                state: ToolState::Pending as i32,
                                class: ToolClass::Consequential as i32,
                                ..Default::default()
                            }),
                            text: String::new(),
                            turn: text(params, "turnId").to_owned(),
                        },
                    );
                    self.emit_work(emit, &item_id);
                }
                let mut network_hosts = Vec::new();
                if let Some(context) = params.get("networkApprovalContext") {
                    hosts(context, &mut network_hosts);
                }
                let mut offered = params
                    .get("availableDecisions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_DECISIONS.iter().map(|d| json!(d)).collect());
                // Codex honours decline even when its offer leaves it out, and
                // a person must always be able to refuse a command.
                if !offered.contains(&json!("decline")) {
                    let at = offered
                        .iter()
                        .position(|decision| *decision == json!("cancel"))
                        .unwrap_or(offered.len());
                    offered.insert(at, json!("decline"));
                }
                for decision in &offered {
                    if decision.get("applyNetworkPolicyAmendment").is_some() {
                        hosts(decision, &mut network_hosts);
                    }
                }
                network_hosts.dedup();
                (
                    item_id,
                    codex_ask::Body::Command(CommandApproval {
                        command: text(params, "command").to_owned(),
                        cwd: text(params, "cwd").to_owned(),
                        reason: text(params, "reason").to_owned(),
                        allow_prefix: strings(params.get("proposedExecpolicyAmendment")),
                        network_hosts,
                    }),
                    offered,
                )
            }
            "item/fileChange/requestApproval" => {
                let changes = match self
                    .works
                    .get(&item_id)
                    .and_then(|state| state.work.as_ref())
                    .and_then(|work| work.of.as_ref())
                {
                    Some(work::Of::FileChange(change)) => change.changes.clone(),
                    _ => Vec::new(),
                };
                (
                    item_id,
                    codex_ask::Body::FileChange(FileChangeApproval {
                        reason: text(params, "reason").to_owned(),
                        grant_root: text(params, "grantRoot").to_owned(),
                        changes,
                    }),
                    DEFAULT_DECISIONS.iter().map(|d| json!(d)).collect(),
                )
            }
            "item/permissions/requestApproval" => {
                let permissions = params.get("permissions").unwrap_or(&Value::Null);
                let files = permissions.get("fileSystem").unwrap_or(&Value::Null);
                let network = permissions.get("network").unwrap_or(&Value::Null);
                let mut network_hosts = Vec::new();
                hosts(network, &mut network_hosts);
                (
                    String::new(),
                    codex_ask::Body::Access(AccessGrant {
                        reason: text(params, "reason").to_owned(),
                        read: strings(files.get("read")),
                        write: strings(files.get("write")),
                        network: network
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        network_hosts,
                    }),
                    Vec::new(),
                )
            }
            "item/tool/requestUserInput" => {
                let mut shapes = Vec::new();
                let questions = params
                    .get("questions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|question| {
                        let options = question
                            .get("options")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default();
                        shapes.push((
                            text(question, "id").to_owned(),
                            options
                                .iter()
                                .map(|option| text(option, "label").to_owned())
                                .collect(),
                        ));
                        Question {
                            header: text(question, "header").to_owned(),
                            question: text(question, "question").to_owned(),
                            multi_select: false,
                            options: options
                                .iter()
                                .map(|option| QuestionOption {
                                    label: text(option, "label").to_owned(),
                                    description: text(option, "description").to_owned(),
                                    preview: String::new(),
                                    recommended: text(option, "label")
                                        .trim_end()
                                        .ends_with("(Recommended)"),
                                })
                                .collect(),
                            allow_other: question
                                .get("isOther")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            secret: question
                                .get("isSecret")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        }
                    })
                    .collect();
                return self.open(
                    emit,
                    CodexAsk {
                        key: key.clone(),
                        item_key: String::new(),
                        body: Some(codex_ask::Body::Question(QuestionAsk { questions })),
                        decisions: Vec::new(),
                    },
                    AskMeta {
                        id: key,
                        method: method.to_owned(),
                        responses: Vec::new(),
                        questions: shapes,
                        at_ms: 0,
                    },
                );
            }
            "mcpServer/elicitation/request" => {
                return self.elicitation(emit, key, method, params);
            }
            "item/tool/call" => {
                // Tools amux offers Codex are served by amux's tool server;
                // a client-side tool is nothing this agent hosts.
                return self.respond(
                    emit,
                    &key,
                    json!({
                        "success": false,
                        "contentItems": [{
                            "type": "inputText",
                            "text": "This client hosts no dynamic tools.",
                        }],
                    }),
                );
            }
            _ => {
                let id = serde_json::from_str::<Value>(&key).unwrap_or(Value::Null);
                emit.effect(crate::Effect::ProviderWrite(
                    serde_json::to_vec(&json!({
                        "id": id,
                        "error": { "code": -32601, "message": format!("amux does not handle {method}") },
                    }))
                    .expect("json"),
                ));
                return self.unrecognized(emit, method, "a request amux cannot answer");
            }
        };
        let (decisions, responses) = decisions
            .iter()
            .filter_map(offered_decision)
            .map(|(decision, response)| (decision as i32, response))
            .unzip();
        self.open(
            emit,
            CodexAsk {
                key: key.clone(),
                item_key,
                body: Some(body),
                decisions,
            },
            AskMeta {
                id: key,
                method: method.to_owned(),
                responses,
                questions: Vec::new(),
                at_ms: 0,
            },
        );
    }

    /// Opens an ask; one that is the work gets its own item, which the ask
    /// points at.
    fn open(&mut self, emit: &mut Emit, mut ask: CodexAsk, mut meta: AskMeta) {
        meta.at_ms = self.shared.now_ms();
        if work_ask(&ask).is_some() {
            ask.item_key = ask_item::key(&ask.key);
            self.emit_ask(emit, &ask, meta.at_ms, None);
        }
        self.asks.insert(ask.key.clone(), meta);
        self.shared.open_ask(ask);
    }

    /// The tool-server call running now, which an elicitation belongs to.
    fn running_tool_call(&self) -> Option<(String, String, String)> {
        self.works
            .iter()
            .filter_map(|(key, state)| match state.work.as_ref() {
                Some(Work {
                    of: Some(work::Of::Mcp(call)),
                    state: running,
                    ..
                }) if *running == ToolState::Running as i32 => {
                    Some((state.at_ms, key, call.server.clone(), call.tool.clone()))
                }
                _ => None,
            })
            .max_by_key(|(at_ms, ..)| *at_ms)
            .map(|(_, key, server, tool)| (key.clone(), server, tool))
    }

    fn elicitation(&mut self, emit: &mut Emit, key: String, method: &str, params: &Value) {
        let meta = params.get("_meta").unwrap_or(&Value::Null);
        let server = text(params, "serverName").to_owned();
        let message = text(params, "message").to_owned();
        let (item_key, call_server, tool) = self.running_tool_call().unwrap_or_default();
        let (body, offered): (_, Vec<(Decision, Value)>) = if text(meta, "codex_approval_kind")
            == "mcp_tool_call"
        {
            let mut offered = vec![(
                Decision::Approve,
                json!({ "action": "accept", "content": {} }),
            )];
            if strings(meta.get("persist"))
                .iter()
                .any(|scope| scope == "session")
            {
                offered.push((
                    Decision::ApproveSession,
                    json!({ "action": "accept", "content": {}, "_meta": { "persist": "session" } }),
                ));
            }
            offered.push((
                Decision::Deny,
                json!({ "action": "decline", "content": null }),
            ));
            offered.push((
                Decision::Abort,
                json!({ "action": "cancel", "content": null }),
            ));
            (
                codex_ask::Body::McpTool(McpToolApproval {
                    server: if call_server.is_empty() {
                        server
                    } else {
                        call_server
                    },
                    tool,
                    arguments_json: compact_json(meta.get("tool_params").unwrap_or(&Value::Null))
                        .into_bytes(),
                }),
                offered,
            )
        } else if text(params, "mode") == "url" {
            (
                codex_ask::Body::McpLink(LinkAsk {
                    server,
                    message,
                    url: text(params, "url").to_owned(),
                }),
                Vec::new(),
            )
        } else {
            (
                codex_ask::Body::McpForm(FormAsk {
                    server,
                    message,
                    schema_json: compact_json(
                        params.get("requestedSchema").unwrap_or(&Value::Null),
                    )
                    .into_bytes(),
                }),
                Vec::new(),
            )
        };
        let (decisions, responses) = offered
            .into_iter()
            .map(|(decision, response)| (decision as i32, response.to_string()))
            .unzip();
        self.open(
            emit,
            CodexAsk {
                key: key.clone(),
                item_key,
                body: Some(body),
                decisions,
            },
            AskMeta {
                id: key,
                method: method.to_owned(),
                responses,
                questions: Vec::new(),
                at_ms: 0,
            },
        );
    }

    /// An ask the server resolved without an answer from here: answered
    /// elsewhere, or withdrawn when its turn ended.
    fn ask_resolved(&mut self, emit: &mut Emit, key: &str) {
        let Some(ask) = self.shared.close_ask(key) else {
            return;
        };
        let meta = self.asks.remove(key);
        self.dismiss(emit, &ask, meta);
    }

    // --- notifications ---------------------------------------------------

    fn notification(&mut self, emit: &mut Emit, method: &str, params: &Value) {
        // A child thread's activity on the same server is its own; the
        // collaboration item reports it here.
        if let (Some(ours), Some(theirs)) = (
            self.thread_id.as_deref(),
            params.get("threadId").and_then(Value::as_str),
        ) && ours != theirs
        {
            return;
        }
        match method {
            "thread/started" => {
                if let Some(thread) = params.get("thread") {
                    self.thread_started(emit, thread, None);
                }
            }
            "turn/started" => {
                self.shared.turn_started();
                self.active_turn = params
                    .get("turn")
                    .and_then(|turn| opt_text(turn, "id"))
                    .or(self.active_turn.take());
                if std::mem::take(&mut self.interrupt_pending)
                    && let Some(turn) = self.active_turn.clone()
                {
                    self.request(
                        emit,
                        "turn/interrupt",
                        json!({ "threadId": self.thread(), "turnId": turn }),
                        Request::Interrupt,
                    );
                }
            }
            "turn/completed" => {
                let turn = params.get("turn").cloned().unwrap_or(Value::Null);
                self.turn_completed(emit, &turn);
            }
            "item/started" => self.item_event(emit, params, false),
            "item/completed" => self.item_event(emit, params, true),
            "item/agentMessage/delta" | "item/plan/delta" | "item/reasoning/textDelta" => {
                let key = text(params, "itemId").to_owned();
                self.extend(emit, &key, text(params, "delta"));
            }
            "item/commandExecution/outputDelta" => {
                let key = text(params, "itemId").to_owned();
                let delta = text(params, "delta");
                if let Some(state) = self.works.get_mut(&key) {
                    state.text.push_str(delta);
                }
                self.extend(emit, &key, delta);
            }
            "item/reasoning/summaryPartAdded" | "item/reasoning/summaryTextDelta" => {
                let key = text(params, "itemId").to_owned();
                let index = int(params, "summaryIndex").unwrap_or(0) as usize;
                let delta = text(params, "delta");
                if let Some(Streamed::Reasoning(summary)) = self.streamed.get_mut(&key) {
                    if summary.len() <= index {
                        summary.resize(index + 1, String::new());
                    }
                    summary[index].push_str(delta);
                    if !delta.is_empty() {
                        self.emit_reasoning(emit, &key, None, false);
                    }
                }
            }
            "turn/diff/updated" => {
                let key = format!("diff:{}", text(params, "turnId"));
                let diff = (key.clone(), text(params, "diff").to_owned());
                if self.last_diff.as_ref() == Some(&diff) {
                    return;
                }
                self.last_diff = Some(diff);
                self.emit_item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(codex_item::Kind::TurnDiff(wire::TurnDiff {
                            patch: text(params, "diff").to_owned(),
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            "turn/plan/updated" => {
                self.plan = Some(
                    params
                        .get("plan")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|step| {
                            let status = match text(step, "status") {
                                "completed" => TaskListStatus::Completed,
                                "inProgress" => TaskListStatus::InProgress,
                                _ => TaskListStatus::Pending,
                            };
                            let subject = opt_text(step, "step")
                                .or_else(|| opt_text(step, "text"))
                                .unwrap_or_default();
                            (subject, status as i32)
                        })
                        .collect(),
                );
            }
            "thread/tokenUsage/updated" => {
                let usage = params.get("tokenUsage").unwrap_or(&Value::Null);
                if let Some(total) = usage.get("last").and_then(|last| int(last, "totalTokens")) {
                    self.context_tokens = Some(total as u64);
                }
                if let Some(window) = int(usage, "modelContextWindow") {
                    self.context_window = Some(window as u64);
                }
            }
            "account/rateLimits/updated" => {
                self.usage = Some(usage_limits(
                    params.get("rateLimits").unwrap_or(&Value::Null),
                ));
            }
            "account/updated" => {
                self.sign_in = Some(match opt_text(params, "authMode") {
                    None => SignIn {
                        state: SignInState::SignedOut as i32,
                        ..Default::default()
                    },
                    Some(mode) => SignIn {
                        state: SignInState::SignedIn as i32,
                        account: match opt_text(params, "planType") {
                            Some(plan) => format!("{mode} {plan}"),
                            None => mode,
                        },
                        message: String::new(),
                    },
                });
            }
            "account/login/completed" => {
                if params.get("success").and_then(Value::as_bool) == Some(false) {
                    self.sign_in = Some(SignIn {
                        state: SignInState::Failed as i32,
                        account: String::new(),
                        message: text(params, "error").to_owned(),
                    });
                }
            }
            "mcpServer/startupStatus/updated" => self.server_status(emit, params),
            "model/rerouted" => {
                let to = text(params, "toModel").to_owned();
                let key = self.local_key("reroute");
                self.emit_item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(codex_item::Kind::Reroute(ModelSwitch {
                            from: text(params, "fromModel").to_owned(),
                            to: to.clone(),
                            reason: text(params, "reason").to_owned(),
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
                self.model = Some(to);
            }
            "error" => self.api_error(emit, params),
            "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => {
                self.review(emit, params, method.ends_with("completed"))
            }
            "thread/settings/updated" => {
                let settings = params.get("threadSettings").unwrap_or(&Value::Null);
                if let Some(model) = opt_text(settings, "model") {
                    self.model = Some(model);
                }
                if let Some(policy) = opt_text(settings, "approvalPolicy") {
                    self.approval = Some(policy);
                }
                if let Some(sandbox) = settings.get("sandboxPolicy").and_then(sandbox_mode) {
                    self.sandbox = Some(sandbox);
                }
                if let Some(effort) = settings.get("effort") {
                    self.effort = effort.as_str().map(str::to_owned);
                }
            }
            method if QUIET.contains(&method) => {
                if method == "serverRequest/resolved"
                    && let Some(id) = params.get("requestId")
                {
                    self.ask_resolved(emit, &ask_key(id));
                }
            }
            other => self.unrecognized(emit, other, "a notification amux does not read"),
        }
    }

    fn server_status(&mut self, emit: &mut Emit, params: &Value) {
        let name = text(params, "name").to_owned();
        let status = match text(params, "status") {
            "starting" => ToolServerStatus::Starting,
            "ready" => ToolServerStatus::Ready,
            "failed" | "cancelled" => ToolServerStatus::Failed,
            "needsAuth" | "notLoggedIn" => ToolServerStatus::NeedsAuth,
            _ => ToolServerStatus::Unspecified,
        };
        let error = opt_text(params, "error")
            .or_else(|| opt_text(params, "failureReason"))
            .unwrap_or_default();
        let health = self.servers.get_or_insert_with(ToolServerHealth::default);
        let entry = ToolServer {
            name: name.clone(),
            status: status as i32,
            error: error.clone(),
        };
        match health.servers.iter_mut().find(|server| server.name == name) {
            Some(server) => *server = entry,
            None => health.servers.push(entry),
        }
        let failed = health.servers.iter().any(|server| {
            server.status == ToolServerStatus::Failed as i32
                || server.status == ToolServerStatus::NeedsAuth as i32
        });
        health.state = if failed {
            wire::HealthState::Degraded
        } else {
            wire::HealthState::Healthy
        } as i32;
        if matches!(
            status,
            ToolServerStatus::Failed | ToolServerStatus::NeedsAuth
        ) {
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("mcp:{name}"),
                    body: item_body(codex_item::Kind::McpStartup(wire::McpStartup {
                        server: name,
                        status: status as i32,
                        error,
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
        }
    }

    fn api_error(&mut self, emit: &mut Emit, params: &Value) {
        let error = params.get("error").unwrap_or(&Value::Null);
        let will_retry = params
            .get("willRetry")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let headline = text(error, "message");
        let (attempt, max_attempts) = if will_retry {
            attempts(headline)
        } else {
            (0, 0)
        };
        let message = opt_text(error, "additionalDetails").unwrap_or_else(|| headline.to_owned());
        if unauthorized(error) {
            self.sign_in = Some(SignIn {
                state: SignInState::Failed as i32,
                account: String::new(),
                message: message.clone(),
            });
        }
        let turn = text(params, "turnId").to_owned();
        self.error_item(
            emit,
            format!("error:{turn}"),
            ApiError {
                error_kind: error_kind(error.get("codexErrorInfo").unwrap_or(&Value::Null)),
                message,
                will_retry,
                attempt,
                max_attempts,
                retry_at_ms: None,
            },
        );
        self.final_error = (!will_retry).then_some(turn);
    }

    fn review(&mut self, emit: &mut Emit, params: &Value, completed: bool) {
        let review = params.get("review").unwrap_or(&Value::Null);
        let target = text(params, "targetItemId").to_owned();
        let decision = text(review, "status").to_owned();
        self.emit_item(
            emit,
            ItemDraft {
                key: format!("review:{}", text(params, "reviewId")),
                body: item_body(codex_item::Kind::Verdict(ReviewerVerdict {
                    decision: decision.clone(),
                    risk: text(review, "riskLevel").to_owned(),
                    rationale: text(review, "rationale").to_owned(),
                    item_key: target.clone(),
                })),
                at_ms: int(params, "startedAtMs"),
                complete: true,
                ..Default::default()
            },
        );
        if !completed {
            return;
        }
        let outcome = match decision.as_str() {
            "approved" => DecisionOutcome::AutoApproved,
            "denied" | "rejected" | "declined" => DecisionOutcome::Denied,
            _ => return,
        };
        let verdict = ToolDecision {
            outcome: outcome as i32,
            scope: "reviewer".into(),
            ..Default::default()
        };
        if self.works.contains_key(&target) {
            self.decide(emit, &target, verdict);
        } else {
            self.reviewed.insert(target, verdict);
        }
    }

    // --- items -----------------------------------------------------------

    fn item_event(&mut self, emit: &mut Emit, params: &Value, completed: bool) {
        let item = params.get("item").unwrap_or(&Value::Null);
        let id = text(item, "id").to_owned();
        let turn = text(params, "turnId").to_owned();
        let at_ms = int(params, "startedAtMs");
        let ended_at_ms = int(params, "completedAtMs");
        match text(item, "type") {
            "userMessage" => {
                if completed {
                    self.user_message(emit, &id, item);
                }
            }
            "agentMessage" | "plan" => {
                let kind = self.streamed.get(&id).cloned().unwrap_or(
                    if text(item, "phase") == "commentary" {
                        Streamed::WorkingNote
                    } else {
                        Streamed::Message
                    },
                );
                self.streamed.insert(id.clone(), kind.clone());
                let complete = |complete| wire::Text { complete };
                let body = match kind {
                    Streamed::WorkingNote => codex_item::Kind::WorkingNote(complete(completed)),
                    _ => codex_item::Kind::Message(complete(completed)),
                };
                let text = match self.shared.open_item(&id) {
                    Some(open) if !completed => open.text.clone(),
                    _ => text(item, "text").to_owned(),
                };
                self.emit_item(
                    emit,
                    ItemDraft {
                        key: id.clone(),
                        text,
                        body: item_body(body),
                        at_ms,
                        complete: completed,
                        ..Default::default()
                    },
                );
                if completed && kind == Streamed::Message {
                    self.shared.note_message(&id);
                }
            }
            "reasoning" => {
                let summary = strings(item.get("summary"));
                let entry = self
                    .streamed
                    .entry(id.clone())
                    .or_insert_with(|| Streamed::Reasoning(Vec::new()));
                if completed || !summary.is_empty() {
                    *entry = Streamed::Reasoning(summary);
                }
                let content = strings(item.get("content")).join("\n");
                self.emit_reasoning(emit, &id, completed.then_some(content), completed);
            }
            "contextCompaction" => {
                if completed {
                    self.boundary(emit, wire::BoundaryKind::Compacted, String::new());
                }
            }
            "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "dynamicToolCall"
            | "webSearch"
            | "imageView"
            | "imageGeneration"
            | "collabAgentToolCall" => {
                self.work_item(emit, &id, item, &turn, at_ms, ended_at_ms, completed)
            }
            other => {
                let other = other.to_owned();
                self.emit_item(
                    emit,
                    ItemDraft {
                        key: id,
                        body: item_body(codex_item::Kind::Unrecognized(wire::Unrecognized {
                            fact_type: other,
                            summary: "an item amux does not read".into(),
                        })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                );
            }
        }
    }

    fn emit_reasoning(
        &mut self,
        emit: &mut Emit,
        key: &str,
        full_text: Option<String>,
        complete: bool,
    ) {
        let Some(Streamed::Reasoning(summary)) = self.streamed.get(key).cloned() else {
            return;
        };
        let text = full_text.unwrap_or_else(|| {
            self.shared
                .open_item(key)
                .map(|open| open.text.clone())
                .unwrap_or_default()
        });
        self.emit_item(
            emit,
            ItemDraft {
                key: key.to_owned(),
                text,
                body: item_body(codex_item::Kind::Reasoning(wire::Reasoning {
                    complete,
                    summary,
                })),
                complete,
                ..Default::default()
            },
        );
    }

    /// A prompt as Codex reflects it. One this interpreter sent is already
    /// an item; one typed into an attached terminal becomes one.
    fn user_message(&mut self, emit: &mut Emit, id: &str, item: &Value) {
        if self.shared.reflect_prompt().is_some() {
            return;
        }
        if let Some(entry) = self.shared.steer_reflected(|_| true) {
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("steer:{}", crate::serde_pb::to_hex(&entry.input_id)),
                    text: entry.text,
                    attachments: entry.attachments,
                    input_id: entry.input_id,
                    body: item_body(codex_item::Kind::Steer(wire::Steer {})),
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        let text = item
            .get("content")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter(|part| text(part, "type") == "text")
                    .map(|part| text(part, "text"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        self.emit_item(
            emit,
            ItemDraft {
                key: id.to_owned(),
                text,
                body: item_body(codex_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn work_item(
        &mut self,
        emit: &mut Emit,
        id: &str,
        item: &Value,
        turn: &str,
        at_ms: Option<i64>,
        ended_at_ms: Option<i64>,
        completed: bool,
    ) {
        let kind = text(item, "type");
        if kind == "mcpToolCall" && is_status_tool(text(item, "server"), text(item, "tool")) {
            let arguments = compact_json(item.get("arguments").unwrap_or(&Value::Null));
            if let Some(working_on) = status_working_on(arguments.as_bytes()) {
                self.shared.set_working_on(working_on);
            }
            return;
        }
        let prior = self.works.get(id).cloned();
        let mut state = match text(item, "status") {
            "" => ToolState::Succeeded,
            status => tool_state(status),
        };
        if !completed && state != ToolState::Running {
            state = ToolState::Running;
        }
        let (of, class) = match kind {
            "commandExecution" => {
                let actions = item
                    .get("commandActions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let kinds = actions
                    .iter()
                    .map(|action| text(action, "type").to_owned())
                    .collect::<Vec<_>>();
                let exploring = !kinds.is_empty()
                    && kinds
                        .iter()
                        .all(|kind| matches!(kind.as_str(), "read" | "search" | "listFiles"));
                let exit_code = int(item, "exitCode").map(|code| code as i32);
                if completed && state == ToolState::Succeeded && exit_code.is_some_and(|c| c != 0) {
                    state = ToolState::Failed;
                }
                (
                    work::Of::Command(wire::CommandWork {
                        command: text(item, "command").to_owned(),
                        cwd: text(item, "cwd").to_owned(),
                        exit_code,
                        action: kinds.join(","),
                        background: false,
                    }),
                    if exploring {
                        ToolClass::Exploration
                    } else {
                        ToolClass::Consequential
                    },
                )
            }
            "fileChange" => (
                work::Of::FileChange(wire::FileChangeWork {
                    changes: item
                        .get("changes")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|change| {
                            let kind = change.get("kind").unwrap_or(&Value::Null);
                            wire::FileChange {
                                path: text(change, "path").to_owned(),
                                kind: match text(kind, "type") {
                                    "add" => wire::FileChangeKind::Add,
                                    "delete" => wire::FileChangeKind::Delete,
                                    "update" => wire::FileChangeKind::Update,
                                    _ => wire::FileChangeKind::Unspecified,
                                } as i32,
                                move_to: text(kind, "move_path").to_owned(),
                                patch: text(change, "diff").to_owned(),
                            }
                        })
                        .collect(),
                }),
                ToolClass::Consequential,
            ),
            "mcpToolCall" | "dynamicToolCall" => {
                let dynamic = kind == "dynamicToolCall";
                if dynamic && completed && item.get("success") == Some(&Value::Bool(false)) {
                    state = ToolState::Failed;
                }
                let error = match item.get("error") {
                    Some(Value::String(error)) => error.clone(),
                    Some(error @ Value::Object(_)) => text(error, "message").to_owned(),
                    _ => String::new(),
                };
                (
                    work::Of::Mcp(wire::McpToolCall {
                        server: if dynamic {
                            text(item, "namespace").to_owned()
                        } else {
                            text(item, "server").to_owned()
                        },
                        tool: text(item, "tool").to_owned(),
                        arguments_json: compact_json(item.get("arguments").unwrap_or(&Value::Null))
                            .into_bytes(),
                        result_json: compact_json(
                            item.get(if dynamic { "contentItems" } else { "result" })
                                .unwrap_or(&Value::Null),
                        )
                        .into_bytes(),
                        error,
                    }),
                    if item.get("readOnlyHint") == Some(&Value::Bool(true)) {
                        ToolClass::Exploration
                    } else {
                        ToolClass::Consequential
                    },
                )
            }
            "webSearch" => (
                work::Of::WebSearch(wire::WebSearch {
                    query: opt_text(item, "query")
                        .or_else(|| {
                            item.get("action")
                                .and_then(|action| opt_text(action, "query"))
                        })
                        .unwrap_or_default(),
                }),
                ToolClass::Exploration,
            ),
            "imageView" | "imageGeneration" => (
                work::Of::Image(wire::ImageWork {
                    generated: kind == "imageGeneration",
                    path: opt_text(item, "path")
                        .or_else(|| opt_text(item, "savedPath"))
                        .unwrap_or_default(),
                }),
                ToolClass::Exploration,
            ),
            _ => (
                work::Of::Collab(wire::CollabWork {
                    tool: text(item, "tool").to_owned(),
                    thread_ids: strings(item.get("receiverThreadIds")),
                    prompt: text(item, "prompt").to_owned(),
                }),
                ToolClass::Consequential,
            ),
        };
        let prior_work = prior.as_ref().and_then(|prior| prior.work.clone());
        let decision = prior_work
            .as_ref()
            .and_then(|work| work.decision.clone())
            .or_else(|| self.reviewed.remove(id));
        let background = prior_work
            .as_ref()
            .and_then(|work| match &work.of {
                Some(work::Of::Command(command)) => Some(command.background),
                _ => None,
            })
            .unwrap_or(false);
        let of = match of {
            work::Of::Command(mut command) => {
                command.background = background;
                work::Of::Command(command)
            }
            of => of,
        };
        let started = prior.as_ref().map(|prior| prior.at_ms);
        let text = match (kind, completed) {
            ("commandExecution", true) => opt_text(item, "aggregatedOutput")
                .or_else(|| prior.as_ref().map(|prior| prior.text.clone()))
                .unwrap_or_default(),
            _ => prior.as_ref().map(|p| p.text.clone()).unwrap_or_default(),
        };
        let at_ms = started.or(at_ms).unwrap_or(self.shared.now_ms());
        let ended_at_ms = if completed {
            ended_at_ms
                .or_else(|| int(item, "durationMs").map(|duration| at_ms + duration))
                .or(Some(self.shared.now_ms()))
        } else {
            None
        };
        self.works.insert(
            id.to_owned(),
            WorkState {
                at_ms,
                work: Some(Work {
                    of: Some(of),
                    state: state as i32,
                    class: class as i32,
                    decision,
                    ended_at_ms,
                }),
                text,
                turn: prior.map_or_else(|| turn.to_owned(), |prior| prior.turn),
            },
        );
        if completed && let Some(background) = &mut self.background {
            background.remove(id);
        }
        self.emit_work(emit, id);
    }

    // --- turn end and exit -----------------------------------------------

    fn turn_completed(&mut self, emit: &mut Emit, turn: &Value) {
        let status = text(turn, "status");
        let outcome = match status {
            "completed" => TurnOutcome::Completed,
            "interrupted" => TurnOutcome::Interrupted,
            "failed" => TurnOutcome::Failed,
            _ => TurnOutcome::Unspecified,
        };
        let turn_id = text(turn, "id").to_owned();
        if let Some(error) = turn.get("error").filter(|error| !error.is_null())
            && self.final_error.as_deref() != Some(turn_id.as_str())
        {
            let message = opt_text(error, "additionalDetails")
                .unwrap_or_else(|| text(error, "message").into());
            self.error_item(
                emit,
                format!("error:{turn_id}"),
                ApiError {
                    error_kind: error_kind(error.get("codexErrorInfo").unwrap_or(&Value::Null)),
                    message,
                    ..Default::default()
                },
            );
        }
        self.final_error = None;
        for ask in self.shared.close_all_asks() {
            let meta = self.asks.remove(&ask.key);
            self.dismiss(emit, &ask, meta);
        }
        self.settle_open(emit, outcome == TurnOutcome::Completed);
        // A steer lives in the turn it was sent into. One Codex has not
        // answered yet reached it after the turn ended and will be refused
        // (Codex answers a steer it took before announcing the turn's end),
        // so it waits in the queue again.
        let unanswered = self
            .requests
            .values()
            .filter_map(|request| match request {
                Request::Steer { input_id } => Some(input_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for input_id in &unanswered {
            self.shared.steer_refused(input_id);
        }
        self.shared.steers_lost();
        for request in self.requests.values_mut() {
            if let Request::Inject { turn_over, .. } = request {
                *turn_over = true;
            }
        }
        let at_ms = self.shared.now_ms();
        let duration = int(turn, "durationMs");
        if let Some(ended) = self.shared.turn_ended(emit) {
            let started_at_ms = duration.map_or(ended.started_at_ms, |duration| at_ms - duration);
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("turn:{}", ended.id),
                    body: item_body(codex_item::Kind::Turn(Turn {
                        turn_id: ended.id,
                        outcome: outcome as i32,
                        started_at_ms,
                        cost_usd: None,
                    })),
                    at_ms: Some(at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        self.active_turn = None;
        self.interrupt_pending = false;
        self.created
            .retain(|key| self.shared.open_item(key).is_some());
        if !self.created.contains(&self.newest) {
            self.newest.clear();
        }
        if self.consumption == InjectConsumption::ParkedUntilNextTurn {
            self.turn_over_parked(emit);
        }
    }

    /// At turn end nothing streams any more. A command still running
    /// carries on in the background when the turn completed, else it was
    /// cut short; any other open item is finished as it stands.
    fn settle_open(&mut self, emit: &mut Emit, completed: bool) {
        let open = self
            .works
            .iter()
            .filter(|(_, state)| state.work.as_ref().is_some_and(|work| !work_complete(work)))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in open {
            let Some(work) = self
                .works
                .get_mut(&key)
                .and_then(|state| state.work.as_mut())
            else {
                continue;
            };
            match (&mut work.of, completed) {
                (Some(work::Of::Command(command)), true)
                    if work.state == ToolState::Running as i32 =>
                {
                    if command.background {
                        continue;
                    }
                    command.background = true;
                    self.background.get_or_insert_default().insert(key.clone());
                }
                _ => work.state = ToolState::Cancelled as i32,
            }
            self.emit_work(emit, &key);
        }
        self.works.retain(|key, state| {
            state.work.as_ref().is_some_and(|work| !work_complete(work))
                || self
                    .background
                    .as_ref()
                    .is_some_and(|set| set.contains(key))
        });
        for (key, kind) in std::mem::take(&mut self.streamed) {
            let Some(open) = self.shared.open_item(&key).cloned() else {
                continue;
            };
            let body = match kind {
                Streamed::Message => codex_item::Kind::Message(wire::Text { complete: true }),
                Streamed::WorkingNote => {
                    codex_item::Kind::WorkingNote(wire::Text { complete: true })
                }
                Streamed::Reasoning(summary) => codex_item::Kind::Reasoning(wire::Reasoning {
                    complete: true,
                    summary,
                }),
            };
            self.shared.item(
                emit,
                ItemDraft {
                    key,
                    text: open.text,
                    body: item_body(body),
                    at_ms: Some(open.at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, cause: String) {
        for ask in self.shared.close_all_asks() {
            if let Some(meta) = self.asks.remove(&ask.key) {
                self.emit_ask(emit, &ask, meta.at_ms, Some(ask_item::dismissed()));
            }
        }
        self.settle_open(emit, false);
        self.shared.provider_exited();
        self.active_turn = None;
        self.boundary(emit, wire::BoundaryKind::Exited, cause);
    }
}

fn usage_limits(limits: &Value) -> UsageLimits {
    let mut windows = Vec::new();
    for slot in ["primary", "secondary"] {
        let Some(window) = limits.get(slot).filter(|window| !window.is_null()) else {
            continue;
        };
        let minutes = int(window, "windowDurationMins").unwrap_or(0);
        windows.push(UsageWindow {
            name: match minutes {
                0 => slot.to_owned(),
                m if m % 1440 == 0 => format!("{}d", m / 1440),
                m if m % 60 == 0 => format!("{}h", m / 60),
                m => format!("{m}m"),
            },
            used_percent: window
                .get("usedPercent")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            resets_at_ms: int(window, "resetsAt").map(|at| at * 1000),
        });
    }
    let blocked = limits
        .get("rateLimitReachedType")
        .is_some_and(|reached| !reached.is_null());
    let near = windows.iter().any(|window| window.used_percent >= 80.0);
    let credits = limits.get("credits").filter(|credits| !credits.is_null());
    UsageLimits {
        state: if blocked {
            UsageState::Blocked
        } else if near {
            UsageState::NearLimit
        } else {
            UsageState::Ok
        } as i32,
        windows,
        credits: credits.and_then(|credits| {
            if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
                Some("unlimited".to_owned())
            } else {
                opt_text(credits, "balance")
            }
        }),
    }
}
