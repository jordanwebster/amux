//! What Claude in a terminal and Claude through the SDK share: the tool
//! vocabulary, the question and scope shapes, the task list the task tools
//! keep, and the one-line renderings goldens use.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    Ask, Attachment, BlobRef, DecisionOutcome, Question, QuestionAsk, QuestionOption, ScopeChoice,
    TaskList, TaskListEntry, TaskListStatus, ToolCall, ToolClass, ToolState, attachment,
};

use crate::Effect;
use crate::claude_pty::QuestionShape;

/// Built-in tools that only look: views fold runs of them together.
pub(crate) const EXPLORATION: &[&str] = &[
    "Read",
    "Grep",
    "Glob",
    "LS",
    "WebFetch",
    "WebSearch",
    "ToolSearch",
    "NotebookRead",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
];

/// Tools whose calls are drawn as the task list, never as rows.
pub(crate) const TASK_TOOLS: &[&str] = &[
    "TaskCreate",
    "TaskUpdate",
    "TaskGet",
    "TaskList",
    "TodoWrite",
];

pub(crate) const QUESTION_TOOL: &str = "AskUserQuestion";
pub(crate) const PLAN_TOOL: &str = "ExitPlanMode";

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub(crate) fn timestamp_ms(row: &Value) -> Option<i64> {
    let stamp = row.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|at| at.timestamp_millis())
}

pub(crate) fn compact_json(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        value.to_string()
    }
}

/// A content value as text: a string, or its text blocks joined.
pub(crate) fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block.get("text").and_then(Value::as_str),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The images in a tool result's content blocks, each as the attachment a
/// tool row carries and the blob write that stores its bytes.
pub(crate) fn result_images(content: &Value) -> Vec<(Attachment, Effect)> {
    use base64::Engine as _;
    use sha2::Digest as _;

    let Value::Array(blocks) = content else {
        return Vec::new();
    };
    blocks
        .iter()
        .filter(|block| text(block, "type") == "image")
        .filter_map(|block| {
            let source = block.get("source")?;
            if text(source, "type") != "base64" {
                return None;
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text(source, "data"))
                .ok()?;
            let hash = sha2::Sha256::digest(&bytes).to_vec();
            let image = Attachment {
                of: Some(attachment::Of::Image(BlobRef {
                    hash: hash.clone(),
                    name: String::new(),
                    mime: text(source, "media_type").to_owned(),
                    size: bytes.len() as u64,
                })),
            };
            Some((image, Effect::WriteBlob { hash, bytes }))
        })
        .collect()
}

/// A tool's structured result without the base64 copy of an image it
/// read; the bytes live in the blob its row references.
pub(crate) fn without_image_bytes(result: &Value) -> Value {
    let mut result = result.clone();
    if let Some(file) = result.get_mut("file").and_then(Value::as_object_mut)
        && file
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|mime| mime.starts_with("image/"))
    {
        file.remove("base64");
    }
    result
}

pub(crate) fn task_status(status: &str) -> Option<i32> {
    Some(match status {
        "pending" => TaskListStatus::Pending as i32,
        "in_progress" => TaskListStatus::InProgress as i32,
        "completed" => TaskListStatus::Completed as i32,
        _ => return None,
    })
}

/// `mcp__server__tool` as (server, tool); a built-in tool has no server.
pub(crate) fn split_tool_name(name: &str) -> (String, String) {
    if let Some(rest) = name.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return (server.to_owned(), tool.to_owned());
    }
    (String::new(), name.to_owned())
}

pub(crate) fn same_json(stored: &str, value: &Value) -> bool {
    serde_json::from_str::<Value>(stored).is_ok_and(|stored| stored == *value)
}

/// A task list entry as the task tools maintain it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Task {
    pub id: String,
    pub subject: String,
    pub status: i32,
    pub active_form: String,
}

pub(crate) fn or_dash(text: &str) -> &str {
    if text.is_empty() { "-" } else { text }
}

pub(crate) fn describe_tool(tool: &ToolCall) -> String {
    let mut text = format!(
        "{}{} {}",
        if tool.server.is_empty() {
            String::new()
        } else {
            format!("{}·", tool.server)
        },
        tool.name,
        ToolState::try_from(tool.state).map_or("?", |state| state.as_str_name())
    );
    if tool.class == wire::ToolClass::Exploration as i32 {
        text.push_str(" exploration");
    }
    if tool.background {
        text.push_str(" background");
    }
    if let Some(decision) = &tool.decision {
        text.push_str(&format!(
            " decision={}{}{}{}",
            DecisionOutcome::try_from(decision.outcome)
                .map_or("?", |outcome| outcome.as_str_name()),
            if decision.scope.is_empty() {
                String::new()
            } else {
                format!(" scope={}", decision.scope)
            },
            if decision.note.is_empty() {
                String::new()
            } else {
                format!(" note={}", Value::String(decision.note.clone()))
            },
            if decision.elsewhere { " elsewhere" } else { "" }
        ));
    }
    if let Some(ended) = tool.ended_at_ms {
        text.push_str(&format!(" ended={ended}"));
    }
    for attachment in &tool.attachments {
        if let Some(attachment::Of::Image(image)) = &attachment.of {
            text.push_str(&format!(
                " image={}:{}:{}",
                image.mime,
                image.size,
                crate::to_hex(&image.hash[..image.hash.len().min(4)])
            ));
        }
    }
    let input = String::from_utf8_lossy(&tool.input_json);
    text.push_str(&format!(" input={}", clip(&input, 60)));
    if !tool.outcome_text.is_empty() {
        text.push_str(&format!(" out={}", clip(&tool.outcome_text, 60)));
    }
    text
}

pub(crate) fn clip(text: &str, chars: usize) -> String {
    let clipped = match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    };
    Value::String(clipped).to_string()
}

/// The question form AskUserQuestion's input describes, and each
/// question's shape.
pub(crate) fn question_ask(input: &Value) -> (QuestionAsk, Vec<QuestionShape>) {
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let shapes = questions
        .iter()
        .map(|question| QuestionShape {
            options: question
                .get("options")
                .and_then(Value::as_array)
                .map_or(0, |options| options.len() as u32),
            multi_select: question
                .get("multiSelect")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
        .collect();
    let questions = questions
        .iter()
        .map(|question| Question {
            header: text(question, "header").to_owned(),
            question: text(question, "question").to_owned(),
            multi_select: question
                .get("multiSelect")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            options: question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .map(|option| QuestionOption {
                            label: text(option, "label").to_owned(),
                            description: text(option, "description").to_owned(),
                            preview: text(option, "preview").to_owned(),
                            recommended: false,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // Terminal Claude's form always offers a typed answer.
            allow_other: true,
            secret: false,
        })
        .collect();
    (QuestionAsk { questions }, shapes)
}

/// Every scope choice the provider offered, in order.
pub(crate) fn scope_choices(suggestions: &[Value]) -> Vec<ScopeChoice> {
    let strings = |value: &Value, key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    suggestions
        .iter()
        .enumerate()
        .map(|(index, suggestion)| ScopeChoice {
            index: index as u32,
            destination: text(suggestion, "destination").to_owned(),
            rules: suggestion
                .get("rules")
                .and_then(Value::as_array)
                .map(|rules| {
                    rules
                        .iter()
                        .map(
                            |rule| match rule.get("ruleContent").and_then(Value::as_str) {
                                Some(content) => format!("{}({content})", text(rule, "toolName")),
                                None => text(rule, "toolName").to_owned(),
                            },
                        )
                        .collect()
                })
                .unwrap_or_default(),
            directories: strings(suggestion, "directories"),
            mode: text(suggestion, "mode").to_owned(),
            label: String::new(),
        })
        .collect()
}

/// A task tool's call and result applied to the task list. Upserts by
/// task id, so two reports of the same result change nothing twice.
pub(crate) fn apply_task_tool(
    tasks: &mut Option<Vec<Task>>,
    name: &str,
    input: &Value,
    result: &Value,
) {
    if !TASK_TOOLS.contains(&name) {
        return;
    }
    let tasks = tasks.get_or_insert_with(Vec::new);
    match name {
        "TaskCreate" => {
            let Some(task_id) = result
                .pointer("/task/id")
                .and_then(Value::as_str)
                .map(str::to_owned)
            else {
                return;
            };
            if tasks.iter().any(|task| task.id == task_id) {
                return;
            }
            tasks.push(Task {
                id: task_id,
                subject: text(input, "subject").to_owned(),
                status: TaskListStatus::Pending as i32,
                active_form: text(input, "activeForm").to_owned(),
            });
        }
        "TaskUpdate" => {
            if result.get("success").and_then(Value::as_bool) == Some(false) {
                return;
            }
            let task_id = text(input, "taskId");
            if text(input, "status") == "deleted" {
                tasks.retain(|task| task.id != task_id);
                return;
            }
            let Some(task) = tasks.iter_mut().find(|task| task.id == task_id) else {
                return;
            };
            if let Some(status) = task_status(text(input, "status")) {
                task.status = status;
            }
            if let Some(subject) = input.get("subject").and_then(Value::as_str) {
                task.subject = subject.to_owned();
            }
            if let Some(active) = input.get("activeForm").and_then(Value::as_str) {
                task.active_form = active.to_owned();
            }
        }
        "TodoWrite" => {
            let Some(todos) = input.get("todos").and_then(Value::as_array) else {
                return;
            };
            *tasks = todos
                .iter()
                .enumerate()
                .map(|(index, todo)| Task {
                    id: (index + 1).to_string(),
                    subject: text(todo, "content").to_owned(),
                    status: task_status(text(todo, "status"))
                        .unwrap_or(TaskListStatus::Unspecified as i32),
                    active_form: text(todo, "activeForm").to_owned(),
                })
                .collect();
        }
        _ => {}
    }
}

/// The snapshot's task list: unknown until a task tool ran.
pub(crate) fn task_list(tasks: &Option<Vec<Task>>) -> TaskList {
    match tasks {
        None => crate::unknown::task_list(),
        Some(tasks) => TaskList {
            known: true,
            entries: tasks
                .iter()
                .map(|task| TaskListEntry {
                    id: task.id.clone(),
                    subject: task.subject.clone(),
                    status: task.status,
                    active_form: task.active_form.clone(),
                })
                .collect(),
        },
    }
}

/// How a call is drawn: reads, searches and fetches fold into runs.
pub(crate) fn tool_class(server: &str, name: &str) -> ToolClass {
    if server.is_empty() && EXPLORATION.contains(&name) {
        ToolClass::Exploration
    } else {
        ToolClass::Consequential
    }
}

/// Open asks as goldens show them.
pub(crate) fn describe_asks(asks: &[Ask]) -> String {
    asks.iter()
        .map(|ask| {
            let body = match &ask.body {
                Some(wire::ask::Body::Permission(permission)) => format!(
                    "permission:{}{}{}{}",
                    permission.tool_name,
                    if permission.server.is_empty() {
                        String::new()
                    } else {
                        format!("@{}", permission.server)
                    },
                    permission
                        .scopes
                        .iter()
                        .map(|scope| format!(
                            "/{}{}{}",
                            scope.destination,
                            if scope.mode.is_empty() {
                                String::new()
                            } else {
                                format!(" mode={}", scope.mode)
                            },
                            scope
                                .rules
                                .iter()
                                .chain(&scope.directories)
                                .map(|rule| format!(" {rule}"))
                                .collect::<String>()
                        ))
                        .collect::<String>(),
                    [
                        (!permission.reason.is_empty()).then(|| format!(
                            " reason={}",
                            Value::String(permission.reason.clone())
                        )),
                        (!permission.description.is_empty()).then(|| format!(
                            " description={}",
                            Value::String(permission.description.clone())
                        )),
                        permission
                            .deny_can_stop
                            .then(|| " deny-can-stop".to_owned()),
                        permission.deny_stops.then(|| " deny-stops".to_owned()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<String>()
                ),
                Some(wire::ask::Body::Question(question)) => format!(
                    "question:{}",
                    question
                        .questions
                        .iter()
                        .map(|question| format!(
                            "{}{}{}",
                            question.options.len(),
                            if question.multi_select { "m" } else { "" },
                            if question.allow_other { "+other" } else { "" }
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Some(wire::ask::Body::Plan(plan)) => format!(
                    "plan:{}chars{}",
                    plan.plan.chars().count(),
                    if plan.offers_auto_accept {
                        " auto-accept"
                    } else {
                        ""
                    }
                ),
                Some(wire::ask::Body::Form(form)) => format!(
                    "form:{} {}",
                    form.server,
                    Value::String(form.message.clone())
                ),
                Some(wire::ask::Body::Link(link)) => format!(
                    "link:{} {} {}",
                    link.server,
                    Value::String(link.message.clone()),
                    link.url
                ),
                Some(wire::ask::Body::Unanswerable(unanswerable)) => {
                    format!("unanswerable:{}", unanswerable.reason)
                }
                None => "none".into(),
            };
            format!(
                "{}->{} {}",
                ask.key,
                if ask.item_key.is_empty() {
                    "-"
                } else {
                    &ask.item_key
                },
                body
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A task list as goldens show it.
pub(crate) fn describe_tasks(tasks: &TaskList) -> String {
    if !tasks.known {
        return "?".into();
    }
    format!(
        "[{}]",
        tasks
            .entries
            .iter()
            .map(|entry| format!(
                "{}:{}:{}",
                entry.id,
                TaskListStatus::try_from(entry.status).map_or("?", |status| status.as_str_name()),
                Value::String(entry.subject.clone())
            ))
            .collect::<Vec<_>>()
            .join(",")
    )
}
