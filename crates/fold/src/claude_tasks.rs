use std::mem::size_of;

use model::TodoProgress;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TASK_MAX_COUNT: usize = 32;
const TASK_TEXT_MAX_BYTES: usize = 4096;
const TOOL_USE_ID_MAX_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Task {
    id: String,
    subject: String,
    active_form: Option<String>,
    status: TaskStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum PendingAction {
    Create {
        tool_use_id: String,
        subject: String,
        active_form: Option<String>,
    },
    Update {
        tool_use_id: String,
        task_id: String,
        status: TaskStatus,
    },
}

impl PendingAction {
    fn tool_use_id(&self) -> &str {
        match self {
            Self::Create { tool_use_id, .. } | Self::Update { tool_use_id, .. } => tool_use_id,
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Create {
                tool_use_id,
                subject,
                active_form,
            } => {
                tool_use_id.capacity()
                    + subject.capacity()
                    + active_form.as_ref().map_or(0, String::capacity)
            }
            Self::Update {
                tool_use_id,
                task_id,
                ..
            } => tool_use_id.capacity() + task_id.capacity(),
        }
    }
}

/// Bounded state for Claude Code's TaskCreate/TaskUpdate todo vocabulary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskRegistry {
    tasks: Vec<Task>,
    pending: Vec<PendingAction>,
}

impl TaskRegistry {
    pub(crate) fn observe_invocation(&mut self, tool_use_id: &str, name: &str, input: &Value) {
        let tool_use_id = clipped_text(tool_use_id, TOOL_USE_ID_MAX_BYTES);
        let action = match name {
            "TaskCreate" => {
                let Some(subject) = input.get("subject").and_then(Value::as_str) else {
                    return;
                };
                PendingAction::Create {
                    tool_use_id: tool_use_id.clone(),
                    subject: clipped_text(subject, TASK_TEXT_MAX_BYTES),
                    active_form: input
                        .get("activeForm")
                        .and_then(Value::as_str)
                        .map(|value| clipped_text(value, TASK_TEXT_MAX_BYTES)),
                }
            }
            "TaskUpdate" => {
                let Some(task_id) = input.get("taskId").and_then(Value::as_str) else {
                    return;
                };
                let Some(status) = input
                    .get("status")
                    .and_then(Value::as_str)
                    .and_then(TaskStatus::parse)
                else {
                    return;
                };
                PendingAction::Update {
                    tool_use_id: tool_use_id.clone(),
                    task_id: clipped_text(task_id, TOOL_USE_ID_MAX_BYTES),
                    status,
                }
            }
            _ => return,
        };

        if let Some(existing) = self
            .pending
            .iter_mut()
            .find(|pending| pending.tool_use_id() == tool_use_id)
        {
            *existing = action;
        } else {
            if self.pending.len() == TASK_MAX_COUNT {
                self.pending.remove(0);
            }
            self.pending.push(action);
        }
    }

    /// Applies a retained, successful tool result and returns changed progress.
    pub(crate) fn observe_result(
        &mut self,
        tool_use_id: &str,
        result: &Value,
    ) -> Option<TodoProgress> {
        let tool_use_id = clipped_text(tool_use_id, TOOL_USE_ID_MAX_BYTES);
        let index = self
            .pending
            .iter()
            .position(|pending| pending.tool_use_id() == tool_use_id)?;
        let action = self.pending.remove(index);
        if result.get("is_error").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let content = result_text(result)?;

        let changed = match action {
            PendingAction::Create {
                subject,
                active_form,
                ..
            } => {
                let (task_id, result_subject) = created_task(&content)?;
                if clipped_text(result_subject, TASK_TEXT_MAX_BYTES) != subject {
                    return None;
                }
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == task_id) {
                    task.subject = subject;
                    task.active_form = active_form;
                    task.status = TaskStatus::Pending;
                    true
                } else if self.tasks.len() < TASK_MAX_COUNT {
                    self.tasks.push(Task {
                        id: task_id.to_owned(),
                        subject,
                        active_form,
                        status: TaskStatus::Pending,
                    });
                    true
                } else {
                    false
                }
            }
            PendingAction::Update {
                task_id, status, ..
            } => {
                if content != format!("Updated task #{task_id} status") {
                    return None;
                }
                let task = self.tasks.iter_mut().find(|task| task.id == task_id)?;
                task.status = status;
                true
            }
        };
        changed.then(|| self.progress())
    }

    pub(crate) fn clear(&mut self) {
        self.tasks.clear();
        self.pending.clear();
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending.clear();
    }

    pub(crate) fn tip_bytes(&self) -> usize {
        size_of::<Self>()
            + self.tasks.capacity() * size_of::<Task>()
            + self.pending.capacity() * size_of::<PendingAction>()
            + self
                .tasks
                .iter()
                .map(|task| {
                    task.id.capacity()
                        + task.subject.capacity()
                        + task.active_form.as_ref().map_or(0, String::capacity)
                })
                .sum::<usize>()
            + self.pending.iter().map(PendingAction::bytes).sum::<usize>()
    }

    fn progress(&self) -> TodoProgress {
        TodoProgress {
            done: self
                .tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Completed)
                .count(),
            total: self.tasks.len(),
            current: self
                .tasks
                .iter()
                .find(|task| task.status == TaskStatus::InProgress)
                .map(|task| task.active_form.as_ref().unwrap_or(&task.subject).clone()),
        }
    }
}

impl TaskStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

fn result_text(result: &Value) -> Option<String> {
    if let Some(content) = result.get("content").and_then(Value::as_str) {
        return Some(content.to_owned());
    }
    let mut text = String::new();
    for block in result.get("content")?.as_array()? {
        let part = block.get("text").and_then(Value::as_str)?;
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(part);
    }
    Some(text)
}

fn created_task(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("Task #")?;
    let (task_id, subject) = rest.split_once(" created successfully: ")?;
    (!task_id.is_empty()
        && task_id.bytes().all(|byte| byte.is_ascii_digit())
        && !subject.is_empty())
    .then_some((task_id, subject))
}

fn clipped_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl crate::private::Sealed for TaskStatus {
    fn assert_fields_are_postcard_safe() {}
}
impl crate::PostcardSafe for TaskStatus {}

impl crate::private::Sealed for Task {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<TaskStatus>();
    }
}
impl crate::PostcardSafe for Task {}

impl crate::private::Sealed for PendingAction {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<String>();
        crate::assert_postcard_safe::<TaskStatus>();
    }
}
impl crate::PostcardSafe for PendingAction {}

impl crate::private::Sealed for TaskRegistry {
    fn assert_fields_are_postcard_safe() {
        crate::assert_postcard_safe::<Vec<Task>>();
        crate::assert_postcard_safe::<Vec<PendingAction>>();
    }
}
impl crate::PostcardSafe for TaskRegistry {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn result(id: &str, content: &str) -> Value {
        json!({"tool_use_id":id,"type":"tool_result","content":content})
    }

    #[test]
    fn successful_creates_and_updates_derive_progress_in_creation_order() {
        let mut tasks = TaskRegistry::default();
        tasks.observe_invocation(
            "create-1",
            "TaskCreate",
            &json!({"subject":"first","description":"one","activeForm":"doing first"}),
        );
        assert_eq!(
            tasks.observe_result(
                "create-1",
                &result("create-1", "Task #1 created successfully: first")
            ),
            Some(TodoProgress {
                done: 0,
                total: 1,
                current: None,
            })
        );
        tasks.observe_invocation(
            "create-2",
            "TaskCreate",
            &json!({"subject":"second","description":"two"}),
        );
        tasks.observe_result(
            "create-2",
            &result("create-2", "Task #2 created successfully: second"),
        );
        tasks.observe_invocation(
            "update-2",
            "TaskUpdate",
            &json!({"taskId":"2","status":"in_progress"}),
        );
        let progress = tasks
            .observe_result("update-2", &result("update-2", "Updated task #2 status"))
            .unwrap();
        assert_eq!(progress.current.as_deref(), Some("second"));

        tasks.observe_invocation(
            "update-1",
            "TaskUpdate",
            &json!({"taskId":"1","status":"in_progress"}),
        );
        let progress = tasks
            .observe_result("update-1", &result("update-1", "Updated task #1 status"))
            .unwrap();
        assert_eq!(progress.current.as_deref(), Some("doing first"));

        tasks.observe_invocation(
            "complete-1",
            "TaskUpdate",
            &json!({"taskId":"1","status":"completed"}),
        );
        let progress = tasks
            .observe_result(
                "complete-1",
                &result("complete-1", "Updated task #1 status"),
            )
            .unwrap();
        assert_eq!(progress.done, 1);
        assert_eq!(progress.total, 2);
        assert_eq!(progress.current.as_deref(), Some("second"));
    }

    #[test]
    fn failed_or_mismatched_results_do_not_change_progress() {
        let mut tasks = TaskRegistry::default();
        tasks.observe_invocation(
            "denied",
            "TaskCreate",
            &json!({"subject":"no","activeForm":"not creating"}),
        );
        assert!(
            tasks
                .observe_result(
                    "denied",
                    &json!({"type":"tool_result","content":"Task #1 created successfully: no","is_error":true})
                )
                .is_none()
        );
        assert_eq!(tasks.progress().total, 0);
    }

    #[test]
    fn registry_caps_tasks_and_clips_utf8_text() {
        let mut tasks = TaskRegistry::default();
        let long = format!("{}é", "x".repeat(TASK_TEXT_MAX_BYTES - 1));
        for index in 0..TASK_MAX_COUNT + 1 {
            let tool = format!("create-{index}");
            tasks.observe_invocation(
                &tool,
                "TaskCreate",
                &json!({"subject":long,"activeForm":long}),
            );
            tasks.observe_result(
                &tool,
                &result(
                    &tool,
                    &format!("Task #{} created successfully: {long}", index + 1),
                ),
            );
        }
        assert_eq!(tasks.progress().total, TASK_MAX_COUNT);
        assert!(tasks.tasks.iter().all(|task| task.subject.len() <= 4096));
        assert!(tasks.tip_bytes() < crate::TIP_MAX_BYTES);
    }
}
