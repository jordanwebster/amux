use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanOutcome {
    Approved,
    Rejected,
}

/// The Cargo workspace containing the `amux` package.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("amux package must live at <workspace>/crates/amux")
        .to_path_buf()
}

/// Resolve user-facing capture paths consistently, independent of Cargo's
/// package test working directory.
pub fn workspace_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root().join(path)
    }
}

/// The parsed `message.content` blocks of a row — empty when the row has no
/// block array. The one row-walking seam shared by every block probe.
pub fn message_blocks(row: &Value) -> &[Value] {
    row.pointer("/message/content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Read a named tool's id from parsed assistant content blocks. Object key
/// order is deliberately irrelevant.
pub fn tool_use_id<'a>(row: &'a Value, tool_name: &str) -> Option<&'a str> {
    if row.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    message_blocks(row)
        .iter()
        .find(|block| {
            block.get("type").and_then(Value::as_str) == Some("tool_use")
                && block.get("name").and_then(Value::as_str) == Some(tool_name)
        })?
        .get("id")?
        .as_str()
}

/// Classify the typed result of an ExitPlanMode menu answer without relying
/// on a preceding assistant row. Claude may not flush that row while the menu
/// is blocked, so the hook is the readiness fact and this result is the first
/// reliable correlation id after input.
pub fn plan_resolution(row: &Value) -> Option<(&str, PlanOutcome)> {
    if row.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let result = message_blocks(row)
        .iter()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))?;
    let id = result.get("tool_use_id")?.as_str()?;

    let rejected = result.get("is_error").and_then(Value::as_bool) == Some(true)
        && row.get("toolDenialKind").and_then(Value::as_str) == Some("user-rejected")
        && row
            .get("userFeedback")
            .and_then(Value::as_str)
            .is_some_and(|feedback| !feedback.is_empty());
    if rejected {
        return Some((id, PlanOutcome::Rejected));
    }

    let approved = result.get("is_error").and_then(Value::as_bool) != Some(true)
        && row
            .get("toolUseResult")
            .and_then(Value::as_object)
            .is_some_and(|sidecar| {
                ["filePath", "isAgent", "plan"]
                    .iter()
                    .all(|field| sidecar.contains_key(*field))
            });
    approved.then_some((id, PlanOutcome::Approved))
}

pub fn is_exit_plan_request(row: &Value) -> bool {
    row.get("type").and_then(Value::as_str) == Some("hook.permission_request")
        && row.get("tool_name").and_then(Value::as_str) == Some("ExitPlanMode")
        && row.get("tool_input").and_then(Value::as_object).is_some()
}
