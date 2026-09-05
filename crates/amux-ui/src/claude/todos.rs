//! Session task lists from Claude's native tool blocks, shared by PTY and SDK folds.
use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{TaskList, TodoState};

/// Tool identities outlive their results so repeated message blocks cannot
/// restore an older list. Like the row dedupe window, this memory is bounded.
const RETAINED: usize = super::SEEN_ROWS_RETAINED;
const PENDING: usize = super::OPEN_TOOLS_RETAINED;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeTodos {
    current: Option<TaskList>,
    writes: VecDeque<Write>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Write {
    id: String,
    pending: Option<TaskList>,
}

pub enum Disposition {
    /// The caller retains the ordinary tool row, including unknown shapes.
    Other,
    /// Session bookkeeping; no transcript entry is needed.
    Absorbed,
    /// A recognized write failed; the caller retains its error in the feed.
    Failed,
}

impl ClaudeTodos {
    pub fn current(&self) -> Option<&TaskList> {
        self.current.as_ref()
    }

    /// Consume a native `tool_use` or `tool_result` content block. The caller
    /// owns row ordering, epoch reset, ask correlation and feed presentation.
    pub fn observe(&mut self, block: &Value) -> Disposition {
        match block["type"].as_str() {
            Some("tool_use") if block["name"] == "TodoWrite" => {
                let Some(id) = block["id"].as_str().filter(|id| !id.is_empty()) else {
                    return Disposition::Other;
                };
                if self.writes.iter().any(|write| write.id == id) {
                    return Disposition::Absorbed;
                }
                let Some(list) = task_list(&block["input"]) else {
                    return Disposition::Other;
                };
                self.writes.push_back(Write {
                    id: id.to_owned(),
                    pending: Some(list),
                });
                if self.writes.len() > RETAINED {
                    self.writes.pop_front();
                }
                if self
                    .writes
                    .iter()
                    .filter(|write| write.pending.is_some())
                    .count()
                    > PENDING
                    && let Some(index) =
                        self.writes.iter().position(|write| write.pending.is_some())
                {
                    // Forget an unresolved obligation honestly: its eventual
                    // result falls back to an ordinary orphan tool row.
                    self.writes.remove(index);
                }
                Disposition::Absorbed
            }
            Some("tool_result") => {
                let Some(write) = self
                    .writes
                    .iter_mut()
                    .find(|write| block["tool_use_id"].as_str() == Some(write.id.as_str()))
                else {
                    return Disposition::Other;
                };
                let Some(list) = write.pending.take() else {
                    return Disposition::Absorbed;
                };
                if block["is_error"].as_bool() == Some(true) {
                    Disposition::Failed
                } else {
                    self.current = Some(list);
                    Disposition::Absorbed
                }
            }
            _ => Disposition::Other,
        }
    }
}

fn task_list(input: &Value) -> Option<TaskList> {
    let mut items = Vec::new();
    let mut current = None;
    let mut done = 0;
    for todo in input["todos"].as_array()? {
        let text = todo["content"].as_str()?;
        let state = match todo["status"].as_str()? {
            "pending" => TodoState::Pending,
            "in_progress" => TodoState::InProgress,
            "completed" => TodoState::Completed,
            _ => return None,
        };
        if state == TodoState::Completed {
            done += 1;
        }
        if state == TodoState::InProgress && current.is_none() {
            current = Some(todo["activeForm"].as_str().unwrap_or(text).to_owned());
        }
        items.push((text.to_owned(), state));
    }
    Some(TaskList {
        done,
        total: items.len(),
        current,
        items,
    })
}
