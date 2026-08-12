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

/// Read a named tool's id from parsed assistant content blocks. Object key
/// order is deliberately irrelevant.
pub fn tool_use_id<'a>(row: &'a Value, tool_name: &str) -> Option<&'a str> {
    if row.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    row.pointer("/message/content")?
        .as_array()?
        .iter()
        .find(|block| {
            block.get("type").and_then(Value::as_str) == Some("tool_use")
                && block.get("name").and_then(Value::as_str) == Some(tool_name)
        })?
        .get("id")?
        .as_str()
}

/// The id carried by the first parsed `tool_result` block in a user row.
pub fn tool_result_id(row: &Value) -> Option<&str> {
    if row.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    row.pointer("/message/content")?
        .as_array()?
        .iter()
        .find(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))?
        .get("tool_use_id")?
        .as_str()
}

/// Classify the typed result of an ExitPlanMode menu answer without relying
/// on a preceding assistant row. Claude may not flush that row while the menu
/// is blocked, so the hook is the readiness fact and this result is the first
/// reliable correlation id after input.
pub fn plan_resolution(row: &Value) -> Option<(&str, PlanOutcome)> {
    let id = tool_result_id(row)?;
    let result = row
        .pointer("/message/content")?
        .as_array()?
        .iter()
        .find(|block| {
            block.get("type").and_then(Value::as_str) == Some("tool_result")
                && block.get("tool_use_id").and_then(Value::as_str) == Some(id)
        })?;

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

/// Offline equivalent of the live row waiter: find the first row at or after
/// `from_index` with the requested plan resolution shape.
pub fn find_plan_resolution(
    rows: &[Value],
    from_index: usize,
    outcome: PlanOutcome,
) -> Option<(usize, &str)> {
    rows.iter()
        .enumerate()
        .skip(from_index)
        .find_map(|(index, row)| {
            let (id, observed) = plan_resolution(row)?;
            (observed == outcome).then_some((index + 1, id))
        })
}
