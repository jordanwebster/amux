use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::approval::RequestId;
use crate::types::{
    AccountUpdate, CommandExecOutputDelta, DynamicToolCallRequest, FileChangeInfo, HookInfo,
    OutputStream, PlanStep, ThreadInfo, ThreadItem, ThreadStatus, ThreadTokenUsage, Turn,
};

// ── ThreadEvent ──────────────────────────────────────────────────
// Events routed to a specific thread's channel.

#[derive(Debug, Clone)]
pub enum ThreadEvent {
    /// A turn-scoped event.
    Turn(TurnEvent),
}

// ── TurnEvent ────────────────────────────────────────────────────
// All events a consumer can receive from a TurnStream.

#[derive(Debug, Clone)]
pub enum TurnEvent {
    // Item lifecycle
    ItemStarted(ThreadItem),
    ItemCompleted(ThreadItem),

    // Streaming deltas
    AgentMessageDelta {
        item_id: String,
        delta: String,
    },
    CommandOutputDelta {
        item_id: String,
        delta: String,
        stream: OutputStream,
    },
    FileChangeDelta {
        item_id: String,
        delta: String,
    },
    PlanDelta {
        item_id: String,
        delta: String,
    },
    ReasoningSummaryDelta {
        item_id: String,
        delta: String,
        summary_index: u32,
    },
    ReasoningTextDelta {
        item_id: String,
        delta: String,
    },
    ReasoningSummaryPartAdded {
        item_id: String,
        summary_index: u32,
    },

    // Turn-level
    DiffUpdated {
        diff: String,
    },
    PlanUpdated {
        explanation: Option<String>,
        steps: Vec<PlanStep>,
    },
    TokenUsageUpdated(ThreadTokenUsage),
    TurnStarted {
        turn: Turn,
    },
    TurnCompleted {
        turn: Turn,
    },

    // Thread-level
    ThreadStarted {
        thread: ThreadInfo,
    },
    ThreadStatusChanged {
        status: ThreadStatus,
    },
    ThreadNameUpdated {
        name: Option<String>,
    },
    ThreadArchived {
        thread_id: String,
    },
    ThreadUnarchived {
        thread_id: String,
    },
    ThreadClosed {
        thread_id: String,
    },
    ThreadCompacted {
        turn_id: String,
    },
    ModelRerouted {
        turn_id: String,
        from_model: String,
        to_model: String,
        reason: String,
    },
    Warning {
        message: String,
    },
    FileChangePatchUpdated {
        item_id: String,
        changes: Vec<FileChangeInfo>,
    },

    // Hooks
    HookStarted(HookInfo),
    HookCompleted(HookInfo),

    // Approvals (only when no ApprovalHandler is configured)
    ApprovalRequired(crate::approval::ApprovalRequest),
    ApprovalResolved {
        request_id: RequestId,
    },
    ToolCallRequired(DynamicToolCallRequest),
    ServerRequest {
        id: RequestId,
        method: String,
        params: Value,
    },

    // Errors
    Error {
        message: String,
        codex_error_info: Option<String>,
        will_retry: bool,
    },

    // Forward compat
    Unknown {
        method: String,
        params: Value,
    },
}

// ── ServerNotification ───────────────────────────────────────────
// Non-thread-scoped notifications (global channel).

#[derive(Debug, Clone)]
pub enum ServerNotification {
    AccountUpdated(AccountUpdate),
    CommandExecOutputDelta(CommandExecOutputDelta),
    Warning {
        message: String,
        thread_id: Option<String>,
    },
    Unknown {
        method: String,
        params: Value,
    },
}

// ── Parsing helper ───────────────────────────────────────────────

/// Parse a JSON-RPC notification into a `TurnEvent`.
/// The `method` is the notification method name, `params` is the params object.
pub(crate) fn parse_turn_event(method: &str, params: &Value) -> TurnEvent {
    match method {
        "item/started" => parse_param(params, "item")
            .map(TurnEvent::ItemStarted)
            .unwrap_or_else(|| unknown_event(method, params)),
        "item/completed" => parse_param(params, "item")
            .map(TurnEvent::ItemCompleted)
            .unwrap_or_else(|| unknown_event(method, params)),
        "item/agentMessage/delta" => TurnEvent::AgentMessageDelta {
            item_id: str_field(params, "itemId"),
            delta: str_field(params, "delta"),
        },
        "item/commandExecution/outputDelta" | "command/exec/outputDelta" => {
            TurnEvent::CommandOutputDelta {
                item_id: str_field(params, "itemId"),
                delta: str_field(params, "delta"),
                stream: parse_param(params, "stream").unwrap_or(OutputStream::Stdout),
            }
        }
        "item/fileChange/outputDelta" => TurnEvent::FileChangeDelta {
            item_id: str_field(params, "itemId"),
            delta: str_field(params, "delta"),
        },
        "item/fileChange/patchUpdated" => TurnEvent::FileChangePatchUpdated {
            item_id: str_field(params, "itemId"),
            changes: parse_param(params, "changes").unwrap_or_default(),
        },
        "item/plan/delta" => TurnEvent::PlanDelta {
            item_id: str_field(params, "itemId"),
            delta: str_field(params, "delta"),
        },
        "item/reasoning/summaryTextDelta" => TurnEvent::ReasoningSummaryDelta {
            item_id: str_field(params, "itemId"),
            delta: str_field(params, "delta"),
            summary_index: u32_field(params, "summaryIndex"),
        },
        "item/reasoning/textDelta" => TurnEvent::ReasoningTextDelta {
            item_id: str_field(params, "itemId"),
            delta: str_field(params, "delta"),
        },
        "item/reasoning/summaryPartAdded" => TurnEvent::ReasoningSummaryPartAdded {
            item_id: str_field(params, "itemId"),
            summary_index: u32_field(params, "summaryIndex"),
        },
        "turn/diff/updated" => TurnEvent::DiffUpdated {
            diff: str_field(params, "diff"),
        },
        "turn/plan/updated" => TurnEvent::PlanUpdated {
            explanation: optional_str_field(params, "explanation"),
            steps: parse_param(params, "plan").unwrap_or_default(),
        },
        "thread/tokenUsage/updated" => parse_token_usage(params)
            .map(TurnEvent::TokenUsageUpdated)
            .unwrap_or_else(|| unknown_event(method, params)),
        "turn/started" => parse_param(params, "turn")
            .map(|turn| TurnEvent::TurnStarted { turn })
            .unwrap_or_else(|| unknown_event(method, params)),
        "turn/completed" => parse_param(params, "turn")
            .map(|turn| TurnEvent::TurnCompleted { turn })
            .unwrap_or_else(|| unknown_event(method, params)),
        "thread/started" => parse_param(params, "thread")
            .map(|thread| TurnEvent::ThreadStarted { thread })
            .unwrap_or_else(|| unknown_event(method, params)),
        "thread/status/changed" => TurnEvent::ThreadStatusChanged {
            status: parse_param(params, "status").unwrap_or_default(),
        },
        "thread/name/updated" => TurnEvent::ThreadNameUpdated {
            name: optional_str_field(params, "threadName")
                .or_else(|| optional_str_field(params, "name")),
        },
        "thread/archived" => TurnEvent::ThreadArchived {
            thread_id: str_field(params, "threadId"),
        },
        "thread/unarchived" => TurnEvent::ThreadUnarchived {
            thread_id: str_field(params, "threadId"),
        },
        "thread/closed" => TurnEvent::ThreadClosed {
            thread_id: str_field(params, "threadId"),
        },
        "thread/compacted" => TurnEvent::ThreadCompacted {
            turn_id: str_field(params, "turnId"),
        },
        "model/rerouted" => TurnEvent::ModelRerouted {
            turn_id: str_field(params, "turnId"),
            from_model: str_field(params, "fromModel"),
            to_model: str_field(params, "toModel"),
            reason: str_field(params, "reason"),
        },
        "warning" => TurnEvent::Warning {
            message: str_field(params, "message"),
        },
        "hook/started" => parse_param(params, "run")
            .map(TurnEvent::HookStarted)
            .unwrap_or_else(|| unknown_event(method, params)),
        "hook/completed" => parse_param(params, "run")
            .map(TurnEvent::HookCompleted)
            .unwrap_or_else(|| unknown_event(method, params)),
        "serverRequest/resolved" => parse_param(params, "requestId")
            .map(|request_id| TurnEvent::ApprovalResolved { request_id })
            .unwrap_or_else(|| unknown_event(method, params)),
        "error" => {
            // Error details may be nested under an "error" sub-object
            let err_obj = params.get("error").unwrap_or(params);
            TurnEvent::Error {
                message: str_field(err_obj, "message"),
                codex_error_info: optional_str_field(err_obj, "codexErrorInfo"),
                will_retry: bool_field(params, "willRetry"),
            }
        }
        _ => unknown_event(method, params),
    }
}

fn parse_param<T: DeserializeOwned>(params: &Value, key: &str) -> Option<T> {
    params.get(key).and_then(parse_value)
}

fn parse_value<T: DeserializeOwned>(value: &Value) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

fn parse_token_usage(params: &Value) -> Option<ThreadTokenUsage> {
    parse_param(params, "tokenUsage").or_else(|| parse_value(params))
}

fn unknown_event(method: &str, params: &Value) -> TurnEvent {
    TurnEvent::Unknown {
        method: method.to_owned(),
        params: params.clone(),
    }
}

fn str_field(params: &Value, key: &str) -> String {
    optional_str_field(params, key).unwrap_or_default()
}

fn optional_str_field(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn u64_field(params: &Value, key: &str) -> u64 {
    params.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn u32_field(params: &Value, key: &str) -> u32 {
    u64_field(params, key) as u32
}

fn bool_field(params: &Value, key: &str) -> bool {
    params.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}
