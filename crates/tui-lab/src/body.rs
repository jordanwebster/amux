//! Item and snapshot bodies in each kind's own message, from one
//! kind-neutral description. The lab authors every row once and encodes it
//! for whichever interpreter the agent is.

use prost::Message as _;
use wire::{
    AgentMessage, ApiError, AskItem, Boundary, ClaudePtyItem, ClaudePtySnapshot, ClaudeSdkItem,
    ClaudeSdkSnapshot, CodexItem, CodexSnapshot, CommandWork, FileChange, FileChangeKind,
    FileChangeWork, Kind, McpToolCall, Prompt, Reasoning, Steer, Task, Text, Thinking, ToolCall,
    ToolClass, Turn, WebSearch, Work, claude_pty_item, claude_sdk_item, codex_item, work,
};

/// One row's body, whatever the kind.
#[derive(Clone, Debug)]
pub enum Body {
    Prompt,
    Steer,
    Message { complete: bool },
    Thinking { complete: bool },
    Tool(ToolCall),
    Turn(Turn),
    Boundary(Boundary),
    AgentMessage(AgentMessage),
    ApiError(ApiError),
    Ask(AskItem),
}

pub fn encode(kind: Kind, body: &Body) -> Vec<u8> {
    match kind {
        Kind::ClaudePty => pty(body).map(|kind| ClaudePtyItem { kind: Some(kind) }.encode_to_vec()),
        Kind::ClaudeSdk => sdk(body).map(|kind| ClaudeSdkItem { kind: Some(kind) }.encode_to_vec()),
        Kind::Codex => codex(body).map(|kind| CodexItem { kind: Some(kind) }.encode_to_vec()),
        Kind::Unspecified => None,
    }
    .unwrap_or_default()
}

fn pty(body: &Body) -> Option<claude_pty_item::Kind> {
    use claude_pty_item::Kind as K;
    Some(match body.clone() {
        Body::Prompt => K::Prompt(Prompt {}),
        Body::Steer => K::Steer(Steer {}),
        Body::Message { complete } => K::Message(Text { complete }),
        Body::Thinking { complete } => K::Thinking(Thinking { complete }),
        Body::Tool(tool) => K::Tool(tool),
        Body::Turn(turn) => K::Turn(turn),
        Body::Boundary(boundary) => K::Boundary(boundary),
        Body::AgentMessage(message) => K::AgentMessage(message),
        Body::ApiError(error) => K::ApiError(error),
        Body::Ask(ask) => K::Ask(ask),
    })
}

fn sdk(body: &Body) -> Option<claude_sdk_item::Kind> {
    use claude_sdk_item::Kind as K;
    Some(match body.clone() {
        Body::Prompt => K::Prompt(Prompt {}),
        Body::Steer => K::Steer(Steer {}),
        Body::Message { complete } => K::Message(Text { complete }),
        Body::Thinking { complete } => K::Thinking(Thinking { complete }),
        Body::Tool(tool) => K::Tool(tool),
        Body::Turn(turn) => K::Turn(turn),
        Body::Boundary(boundary) => K::Boundary(boundary),
        Body::AgentMessage(message) => K::AgentMessage(message),
        Body::ApiError(error) => K::ApiError(error),
        Body::Ask(ask) => K::Ask(ask),
    })
}

fn codex(body: &Body) -> Option<codex_item::Kind> {
    use codex_item::Kind as K;
    Some(match body.clone() {
        Body::Prompt => K::Prompt(Prompt {}),
        Body::Steer => K::Steer(Steer {}),
        Body::Message { complete } => K::Message(Text { complete }),
        Body::Thinking { complete } => K::Reasoning(Reasoning {
            complete,
            summary: Vec::new(),
        }),
        Body::Tool(tool) => K::Work(codex_work(&tool)),
        Body::Turn(turn) => K::Turn(turn),
        Body::Boundary(boundary) => K::Boundary(boundary),
        Body::AgentMessage(message) => K::AgentMessage(message),
        Body::ApiError(error) => K::Error(error),
        Body::Ask(ask) => K::Ask(ask),
    })
}

/// A Claude-named tool call as the unit of Codex work closest to it.
pub fn codex_work(tool: &ToolCall) -> Work {
    let input: serde_json::Value = serde_json::from_slice(&tool.input_json).unwrap_or_default();
    let field = |name: &str| {
        input
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let command = |command: String, action: &str| {
        work::Of::Command(CommandWork {
            command,
            cwd: String::new(),
            exit_code: tool.exit_code,
            action: action.into(),
            background: tool.background,
        })
    };
    let of = match tool.name.as_str() {
        "Bash" => command(field("command"), ""),
        "Read" => command(format!("cat {}", field("file_path")), "read"),
        "Grep" => command(format!("rg {}", field("pattern")), "search"),
        "Glob" | "LS" => command(format!("ls {}", field("pattern")), "list_files"),
        "WebSearch" => work::Of::WebSearch(WebSearch {
            query: field("query"),
        }),
        "Edit" | "Write" | "MultiEdit" => {
            let old = field("old_string");
            let new = if tool.name == "Write" {
                field("content")
            } else {
                field("new_string")
            };
            let path = field("file_path");
            work::Of::FileChange(FileChangeWork {
                changes: vec![FileChange {
                    patch: unified(&path, &old, &new),
                    path,
                    kind: if tool.name == "Write" {
                        FileChangeKind::Add as i32
                    } else {
                        FileChangeKind::Update as i32
                    },
                    move_to: String::new(),
                }],
            })
        }
        name => work::Of::Mcp(McpToolCall {
            server: tool.server.clone(),
            tool: name.into(),
            arguments_json: tool.input_json.clone(),
            result_json: tool.outcome_json.clone(),
            error: String::new(),
        }),
    };
    Work {
        of: Some(of),
        state: tool.state,
        class: tool.class,
        decision: tool.decision.clone(),
        ended_at_ms: tool.ended_at_ms,
    }
}

/// A one-hunk unified diff replacing `old` with `new`.
pub fn unified(path: &str, old: &str, new: &str) -> String {
    let old: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.lines().collect()
    };
    let new: Vec<&str> = new.lines().collect();
    let mut out = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -1,{} +1,{} @@\n",
        old.len(),
        new.len()
    );
    for line in old {
        out.push_str(&format!("-{line}\n"));
    }
    for line in new {
        out.push_str(&format!("+{line}\n"));
    }
    out
}

/// Whether a Claude tool name is exploration, which the views gather into
/// runs.
pub fn class_of(name: &str) -> ToolClass {
    match name {
        "Read" | "Grep" | "Glob" | "LS" | "WebFetch" | "WebSearch" | "ToolSearch" => {
            ToolClass::Exploration
        }
        _ => ToolClass::Consequential,
    }
}

/// The per-kind snapshot fields the lab keeps, whatever the kind.
#[derive(Clone, Debug, Default)]
pub struct SnapshotFacts {
    pub claude_asks: Vec<wire::Ask>,
    pub codex_asks: Vec<wire::CodexAsk>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    pub context: Option<wire::ContextMeter>,
    pub usage: Option<wire::UsageLimits>,
    pub tasks: Option<wire::TaskList>,
    pub sign_in: Option<wire::SignIn>,
    pub background: Option<u32>,
    pub servers: Option<wire::ToolServerHealth>,
    pub active_tasks: Vec<Task>,
    pub running_turn: bool,
}

pub fn encode_snapshot(kind: Kind, facts: &SnapshotFacts) -> Vec<u8> {
    let background = facts.background.map(|running| wire::BackgroundProcesses {
        known: true,
        running,
    });
    let sign_in = Some(facts.sign_in.clone().unwrap_or(wire::SignIn {
        state: wire::SignInState::SignedIn as i32,
        account: "you@example.com".into(),
        message: String::new(),
    }));
    let usage = Some(facts.usage.clone().unwrap_or(wire::UsageLimits {
        state: wire::UsageState::Ok as i32,
        ..Default::default()
    }));
    let servers = Some(facts.servers.clone().unwrap_or(wire::ToolServerHealth {
        state: wire::HealthState::Healthy as i32,
        servers: Vec::new(),
    }));
    let tasks = Some(facts.tasks.clone().unwrap_or(wire::TaskList {
        known: true,
        entries: Vec::new(),
    }));
    match kind {
        Kind::ClaudePty => ClaudePtySnapshot {
            asks: facts.claude_asks.clone(),
            tasks,
            context: facts.context.clone(),
            model: facts.model.clone(),
            permission_mode: facts.mode.clone(),
            provider_session: Some("lab-session".into()),
            usage,
            servers,
            sign_in,
            background_processes: background,
            running_calls: Vec::new(),
        }
        .encode_to_vec(),
        Kind::ClaudeSdk => ClaudeSdkSnapshot {
            asks: facts.claude_asks.clone(),
            tasks,
            context: facts.context.clone(),
            model: facts.model.clone(),
            effort: facts.effort.clone(),
            permission_mode: facts.mode.clone(),
            active_tasks: facts.active_tasks.clone(),
            usage,
            servers,
            sign_in,
            background_processes: background,
            provider_session: Some("lab-session".into()),
            models: offered_models(),
            commands: Vec::new(),
        }
        .encode_to_vec(),
        Kind::Codex => CodexSnapshot {
            asks: facts.codex_asks.clone(),
            context: facts.context.clone(),
            model: facts.model.clone(),
            approval_policy: facts.mode.clone(),
            sandbox: Some("workspace-write".into()),
            active_turn: facts.running_turn.then(|| "turn".into()),
            servers,
            usage,
            sign_in,
            background_processes: background,
            plan: tasks,
            effort: facts.effort.clone(),
            thread_id: Some("lab-thread".into()),
            models: Vec::new(),
            commands: Vec::new(),
        }
        .encode_to_vec(),
        Kind::Unspecified => Vec::new(),
    }
}

fn offered_models() -> Vec<wire::OfferedModel> {
    ["opus", "sonnet", "haiku"]
        .into_iter()
        .map(|name| wire::OfferedModel {
            value: name.into(),
            display_name: name.into(),
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            ..Default::default()
        })
        .collect()
}
