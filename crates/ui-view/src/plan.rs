//! Plans: agent text that proposes work and waits on the person's decision.
//!
//! Claude writes its plan to a plan file with its ordinary Write tool, then
//! calls ExitPlanMode, whose ask carries the plan. The plan file's Write is
//! the plan until the ExitPlanMode call arrives; from then on that call's
//! row is the plan and the Write draws nothing. Edits to the plan file never
//! draw.
//!
//! Codex has no plan item on the wire yet, nor any plan approval, so a
//! Codex plan is an ordinary message.

use ui_state::{Held, ItemBody, ItemClass, SessionState};

use crate::rows::{AskRow, PlanVerdict, RowKind};

/// A path under Claude's plans directory.
pub(crate) fn is_plan_file(path: &str) -> bool {
    path.contains("/.claude/plans/") || path.starts_with(".claude/plans/")
}

/// Whether an ExitPlanMode call comes after the plan file's Write at
/// `order`, before the next prompt.
fn exit_follows(state: &SessionState, order: u64) -> bool {
    let transcript = state.transcript();
    let Some(head) = transcript.head() else {
        return false;
    };
    if order >= head {
        return false;
    }
    for held in transcript.range(order + 1..=head) {
        if matches!(held.class, ItemClass::Prompt) {
            return false;
        }
        if tool_name(held) == Some("ExitPlanMode") {
            return true;
        }
    }
    false
}

fn tool_name(held: &Held) -> Option<&str> {
    match &held.body {
        ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
        | ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool)) => Some(tool.name.as_str()),
        _ => None,
    }
}

/// The plan file's newest Write before `order`, for an ExitPlanMode call
/// that carries no plan of its own.
pub(crate) fn written_before(state: &SessionState, order: u64) -> String {
    let transcript = state.transcript();
    let Some(oldest) = transcript.oldest_held() else {
        return String::new();
    };
    if order <= oldest {
        return String::new();
    }
    for held in transcript.range(oldest..=order - 1).rev() {
        if matches!(held.class, ItemClass::Prompt) {
            break;
        }
        if let ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
        | ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool)) = &held.body
            && tool.name == "Write"
        {
            let input: serde_json::Value =
                serde_json::from_slice(&tool.input_json).unwrap_or_default();
            let path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if is_plan_file(path) {
                return input
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
            }
        }
    }
    String::new()
}

/// A Claude tool call on the plan file: the plan while it is written, or
/// nothing once ExitPlanMode carries it. None for any other call.
pub(crate) fn plan_file_row(
    state: &SessionState,
    held: &Held,
    name: &str,
    path: &str,
    content: String,
    writing: bool,
) -> Option<RowKind> {
    if !is_plan_file(path) {
        return None;
    }
    if name != "Write" || exit_follows(state, held.item.order) {
        return Some(RowKind::Hidden);
    }
    Some(RowKind::Ask(AskRow::Plan {
        plan: content,
        verdict: PlanVerdict::Open,
        edits_accepted: false,
        note: None,
        writing,
    }))
}
