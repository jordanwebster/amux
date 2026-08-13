//! Pure row fold for the Codex layer.

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::*;

pub(super) fn observe(layer: &mut CodexLayer, seq: u64, _arrived: DateTime<Utc>, row: &Value) {
    let Some(method) = row.get("type").and_then(Value::as_str) else {
        push_unrecognized(layer, seq, "<missing type>", Some("row has no string type"));
        return;
    };

    // Approval display facts are valid for exactly the immediately following
    // synthesized row. Any intervening row breaks the correlation honestly.
    if method != "amux.codex_approval_required"
        && !matches!(
            method,
            "item/commandExecution/requestApproval"
                | "item/fileChange/requestApproval"
                | "item/permissions/requestApproval"
                | "item/tool/call"
        )
    {
        layer.pending_approval_context = None;
    }

    match method {
        // Six frozen synthesized rows.
        "amux.codex_ready" => fold_ready(layer, seq),
        "amux.codex_gap" => fold_gap(layer, seq, row),
        "amux.codex_reconnect_error" => fold_reconnect_error(layer, seq, row),
        "amux.codex_approval_required" => fold_approval_required(layer, seq, row),
        "amux.codex_approval_resolved" => fold_approval_resolved(layer, row),
        "amux.input_result" => fold_input_result(layer, seq, row),

        // Turn and item lifecycle.
        "turn/started" => fold_turn_started(layer, seq, row),
        "turn/completed" => fold_turn_completed(layer, seq, row),
        "item/started" => fold_item(layer, seq, row, ItemFinality::Open),
        "item/completed" => fold_item(layer, seq, row, ItemFinality::Complete),

        // Streaming item deltas.
        "item/agentMessage/delta" => fold_agent_delta(layer, seq, row),
        "item/reasoning/textDelta" => fold_reasoning_text_delta(layer, seq, row),
        "item/reasoning/summaryTextDelta" => fold_reasoning_summary_delta(layer, seq, row),
        "item/reasoning/summaryPartAdded" => fold_reasoning_summary_part(layer, seq, row),
        "item/commandExecution/outputDelta" | "command/exec/outputDelta" => {
            fold_command_delta(layer, seq, row)
        }
        "item/fileChange/outputDelta" => fold_file_delta(layer, seq, row),
        "item/fileChange/patchUpdated" => fold_file_patch(layer, seq, row),
        "item/plan/delta" => fold_plan_delta(layer, seq, row),

        // Turn-level snapshots.
        "turn/plan/updated" => fold_plan_updated(layer, seq, row),
        "turn/diff/updated" => fold_diff_updated(layer, seq, row),
        "thread/tokenUsage/updated" => layer.latest_usage = Some(token_usage(row)),

        // Approval context rows. They also upsert the proposed work item.
        "item/commandExecution/requestApproval" => fold_command_approval(layer, seq, row),
        "item/fileChange/requestApproval" => fold_file_approval(layer, seq, row),
        "item/permissions/requestApproval" => fold_permissions_approval(layer, seq, row),
        "item/tool/call" => fold_dynamic_tool_approval(layer, seq, row),
        "item/tool/requestUserInput" => fold_unsupported_user_input(layer, seq, row),

        // Errors, boundaries, and notices.
        "error" => fold_error(layer, seq, row, ErrorSeverity::Error),
        "warning" => fold_error(layer, seq, row, ErrorSeverity::Warning),
        "thread/compacted" => {
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::Compacted {
                    turn_id: string(row, "turnId"),
                }),
            );
        }
        "model/rerouted" => {
            push(
                layer,
                seq,
                FeedEntryKind::Error(ErrorEntry {
                    severity: ErrorSeverity::Notice,
                    message: format!(
                        "model rerouted from {} to {}{}",
                        str_or(row, "fromModel", "unknown"),
                        str_or(row, "toModel", "unknown"),
                        string(row, "reason")
                            .map(|reason| format!(" ({reason})"))
                            .unwrap_or_default()
                    ),
                    will_retry: false,
                }),
            );
        }

        // State-only thread facts.
        "thread/status/changed" => fold_thread_status(layer, row),
        "thread/closed" => layer.thread_closed = true,

        // The synthesized resolution/ready rows already own these facts.
        "serverRequest/resolved"
        | "thread/started"
        | "thread/name/updated"
        | "thread/archived"
        | "thread/unarchived" => {}

        // Hooks are not configured by amux V1. If one appears, make the
        // unsupported family visible rather than pretending it was folded.
        "hook/started" | "hook/completed" => push_unrecognized(
            layer,
            seq,
            method,
            Some("Codex hooks are not configured in amux V1"),
        ),

        // Legacy envelopes and every future thread-scoped method degrade
        // visibly through the same mandatory default.
        _ => push_unrecognized(layer, seq, method, None),
    }
}

fn fold_ready(layer: &mut CodexLayer, seq: u64) {
    let repeated = layer.ready_count > 0;
    layer.ready_count += 1;
    layer.stale = false;
    layer.gap = false;
    layer.read_only = false;
    layer.accumulators = Accumulators::default();
    layer.pending_approval_context = None;
    if repeated {
        push(layer, seq, FeedEntryKind::Boundary(BoundaryEntry::Ready));
    }
}

fn fold_gap(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let reason = str_or(row, "reason", "unknown").to_string();
    layer.gap = true;
    layer.accumulators = Accumulators::default();
    layer.pending_approval_context = None;
    push(
        layer,
        seq,
        FeedEntryKind::Boundary(BoundaryEntry::Gap { reason }),
    );
}

fn fold_reconnect_error(layer: &mut CodexLayer, seq: u64, row: &Value) {
    layer.gap = false;
    layer.read_only = true;
    let message = row
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Codex reconnect failed")
        .to_string();
    push(
        layer,
        seq,
        FeedEntryKind::Error(ErrorEntry {
            severity: ErrorSeverity::Error,
            message,
            will_retry: true,
        }),
    );
}

fn fold_turn_started(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(turn_id) = row
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        push_unrecognized(layer, seq, "turn/started", Some("missing turn.id"));
        return;
    };
    layer.accumulators = Accumulators::default();
    layer.pending_approval_context = None;
    layer.turn.active_id = Some(turn_id);
    layer.turn.status = ThreadStatus::Active;
    layer.turn.last = None;
}

fn fold_turn_completed(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(turn) = row.get("turn") else {
        push_unrecognized(layer, seq, "turn/completed", Some("missing turn"));
        return;
    };
    let Some(turn_id) = turn.get("id").and_then(Value::as_str).map(str::to_owned) else {
        push_unrecognized(layer, seq, "turn/completed", Some("missing turn.id"));
        return;
    };
    let (status, last) = match turn.get("status").and_then(Value::as_str) {
        Some("completed") => (TurnStatus::Completed, LastTurn::Completed),
        Some("interrupted") => (TurnStatus::Interrupted, LastTurn::Interrupted),
        Some("failed") => (
            TurnStatus::Failed {
                message: turn
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("turn failed")
                    .to_string(),
            },
            LastTurn::Failed,
        ),
        other => {
            push_unrecognized(
                layer,
                seq,
                "turn/completed",
                Some(match other {
                    Some(_) => "unknown turn status",
                    None => "missing turn status",
                }),
            );
            return;
        }
    };
    let kind = FeedEntryKind::Turn(TurnEntry {
        turn_id: turn_id.clone(),
        status,
        token_usage: layer.latest_usage.clone(),
    });
    upsert_turn(layer, seq, &turn_id, kind);

    // A late completion for an older turn must not clear the observed turn.
    if layer.turn.active_id.as_deref() == Some(turn_id.as_str()) {
        layer.turn.active_id = None;
        layer.turn.status = ThreadStatus::Idle;
        layer.turn.last = Some(last);
        layer.accumulators = Accumulators::default();
        layer.pending_approval_context = None;
    }
}

fn fold_item(layer: &mut CodexLayer, seq: u64, row: &Value, finality: ItemFinality) {
    let Some(item) = row.get("item") else {
        push_unrecognized(
            layer,
            seq,
            if finality == ItemFinality::Open {
                "item/started"
            } else {
                "item/completed"
            },
            Some("missing item"),
        );
        return;
    };
    let Some(item_id) = item.get("id").and_then(Value::as_str).map(str::to_owned) else {
        push_unrecognized(layer, seq, "item/*", Some("missing item.id"));
        return;
    };
    let item_type = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    match item_type {
        "userMessage" => {
            upsert_item(
                layer,
                seq,
                &item_id,
                FeedEntryKind::Prompt(PromptEntry {
                    item_id: item_id.clone(),
                    source: PromptSource::Protocol,
                    parts: prompt_parts(item.get("content")),
                    finality,
                }),
            );
            set_active(layer, &item_id, ActiveItemKind::Prompt, finality);
        }
        "agentMessage" => {
            let accumulated = layer
                .accumulators
                .message_text
                .entry(item_id.clone())
                .or_default();
            if let Some(completed) = item.get("text").and_then(Value::as_str)
                && (!completed.is_empty() || accumulated.is_empty())
            {
                *accumulated = completed.to_string();
            }
            let text = accumulated.clone();
            let phase = if item.get("phase").and_then(Value::as_str) == Some("final_answer") {
                MessagePhase::FinalAnswer
            } else {
                MessagePhase::Commentary
            };
            upsert_item(
                layer,
                seq,
                &item_id,
                FeedEntryKind::Message(MessageEntry {
                    item_id: item_id.clone(),
                    text,
                    phase,
                    finality,
                }),
            );
            set_active(layer, &item_id, ActiveItemKind::Message, finality);
        }
        "reasoning" => {
            let text = if finality == ItemFinality::Complete {
                strings(item.get("content"))
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                layer
                    .accumulators
                    .reasoning_text
                    .get(&item_id)
                    .cloned()
                    .unwrap_or_default()
            };
            if !text.is_empty() {
                layer
                    .accumulators
                    .reasoning_text
                    .insert(item_id.clone(), text.clone());
            }
            let completed_summary = strings(item.get("summary"));
            if !completed_summary.is_empty() {
                layer
                    .accumulators
                    .reasoning_summary
                    .insert(item_id.clone(), completed_summary);
            }
            let summary = layer
                .accumulators
                .reasoning_summary
                .get(&item_id)
                .cloned()
                .unwrap_or_default();
            upsert_item(
                layer,
                seq,
                &item_id,
                FeedEntryKind::Reasoning(ReasoningEntry {
                    item_id: item_id.clone(),
                    text,
                    summary,
                    finality,
                }),
            );
            set_active(layer, &item_id, ActiveItemKind::Reasoning, finality);
        }
        "commandExecution" => fold_command_item(layer, seq, item, &item_id, finality),
        "fileChange" => fold_file_item(layer, seq, item, &item_id, finality),
        "plan" => fold_plan_item(layer, seq, item, &item_id, finality),
        "mcpToolCall" => fold_mcp_item(layer, seq, item, &item_id, finality),
        "dynamicToolCall" => fold_dynamic_item(layer, seq, item, &item_id, finality),
        "webSearch" => fold_web_item(layer, seq, item, &item_id, finality),
        "contextCompaction" => {
            upsert_item(
                layer,
                seq,
                &item_id,
                FeedEntryKind::Boundary(BoundaryEntry::Compacted {
                    turn_id: string(row, "turnId"),
                }),
            );
            set_active(layer, &item_id, ActiveItemKind::Work, finality);
        }
        other => {
            upsert_item(
                layer,
                seq,
                &item_id,
                FeedEntryKind::Work(WorkEntry {
                    item_id: item_id.clone(),
                    kind: WorkKind::Other {
                        item_type: other.to_string(),
                        raw: item.clone(),
                    },
                    state: work_state(item, finality),
                    stdout_head: String::new(),
                    stderr_head: String::new(),
                    output_truncated: false,
                }),
            );
            set_active(layer, &item_id, ActiveItemKind::Work, finality);
        }
    }
}

fn fold_command_item(
    layer: &mut CodexLayer,
    seq: u64,
    item: &Value,
    item_id: &str,
    finality: ItemFinality,
) {
    let had_deltas = layer
        .accumulators
        .command_stdout
        .get(item_id)
        .is_some_and(|text| !text.is_empty())
        || layer
            .accumulators
            .command_stderr
            .get(item_id)
            .is_some_and(|text| !text.is_empty());
    let mut aggregated_truncated = false;
    if finality == ItemFinality::Complete
        && !had_deltas
        && let Some(output) = item.get("aggregatedOutput").and_then(Value::as_str)
    {
        let target = layer
            .accumulators
            .command_stdout
            .entry(item_id.to_string())
            .or_default();
        append_bounded(target, output, &mut aggregated_truncated);
    }
    let mut entry = existing_work(layer, item_id).unwrap_or_else(|| empty_work(item_id));
    entry.kind = WorkKind::Command {
        command: str_or(item, "command", "").to_string(),
        cwd: string(item, "cwd"),
        exit_code: item
            .get("exitCode")
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok()),
    };
    entry.state = work_state(item, finality);
    entry.stdout_head = layer
        .accumulators
        .command_stdout
        .get(item_id)
        .cloned()
        .unwrap_or_default();
    entry.stderr_head = layer
        .accumulators
        .command_stderr
        .get(item_id)
        .cloned()
        .unwrap_or_default();
    entry.output_truncated |= aggregated_truncated;
    upsert_item(layer, seq, item_id, FeedEntryKind::Work(entry));
    set_active(layer, item_id, ActiveItemKind::Work, finality);
}

fn fold_file_item(
    layer: &mut CodexLayer,
    seq: u64,
    item: &Value,
    item_id: &str,
    finality: ItemFinality,
) {
    let mut entry = existing_work(layer, item_id).unwrap_or_else(|| empty_work(item_id));
    let (patch_head, patch_truncated) = match &entry.kind {
        WorkKind::FileChange {
            patch_head,
            patch_truncated,
            ..
        } => (patch_head.clone(), *patch_truncated),
        _ => (String::new(), false),
    };
    entry.kind = WorkKind::FileChange {
        changes: file_changes(item.get("changes")),
        patch_head,
        patch_truncated,
    };
    entry.state = work_state(item, finality);
    upsert_item(layer, seq, item_id, FeedEntryKind::Work(entry));
    set_active(layer, item_id, ActiveItemKind::Work, finality);
}

fn fold_plan_item(
    layer: &mut CodexLayer,
    seq: u64,
    item: &Value,
    item_id: &str,
    finality: ItemFinality,
) {
    let text = item
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| plan_text(layer, item_id))
        .unwrap_or_default();
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.to_string(),
            kind: WorkKind::Plan {
                text,
                explanation: None,
                steps: Vec::new(),
            },
            state: work_state(item, finality),
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
    set_active(layer, item_id, ActiveItemKind::Work, finality);
}

fn fold_mcp_item(
    layer: &mut CodexLayer,
    seq: u64,
    item: &Value,
    item_id: &str,
    finality: ItemFinality,
) {
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.to_string(),
            kind: WorkKind::McpTool {
                server: str_or(item, "server", "").to_string(),
                tool: str_or(item, "tool", "").to_string(),
                arguments: item.get("arguments").cloned().unwrap_or(Value::Null),
                result: item.get("result").filter(|v| !v.is_null()).cloned(),
                error: item.get("error").filter(|v| !v.is_null()).cloned(),
            },
            state: work_state(item, finality),
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
    set_active(layer, item_id, ActiveItemKind::Work, finality);
}

fn fold_dynamic_item(
    layer: &mut CodexLayer,
    seq: u64,
    item: &Value,
    item_id: &str,
    finality: ItemFinality,
) {
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.to_string(),
            kind: WorkKind::DynamicTool {
                tool: str_or(item, "tool", "").to_string(),
                namespace: string(item, "namespace"),
                arguments: item.get("arguments").cloned().unwrap_or(Value::Null),
                success: item.get("success").and_then(Value::as_bool),
            },
            state: work_state(item, finality),
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
    set_active(layer, item_id, ActiveItemKind::Work, finality);
}

fn fold_web_item(
    layer: &mut CodexLayer,
    seq: u64,
    item: &Value,
    item_id: &str,
    finality: ItemFinality,
) {
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.to_string(),
            kind: WorkKind::WebSearch {
                query: str_or(item, "query", "").to_string(),
                action: item.get("action").filter(|v| !v.is_null()).cloned(),
            },
            state: work_state(item, finality),
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
    set_active(layer, item_id, ActiveItemKind::Work, finality);
}

fn fold_agent_delta(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/agentMessage/delta",
            Some("missing itemId"),
        );
        return;
    };
    let delta = str_or(row, "delta", "");
    let text = layer
        .accumulators
        .message_text
        .entry(item_id.to_string())
        .or_default();
    text.push_str(delta);
    let text = text.clone();
    let phase = existing_message_phase(layer, item_id).unwrap_or(MessagePhase::Commentary);
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Message(MessageEntry {
            item_id: item_id.to_string(),
            text,
            phase,
            finality: ItemFinality::Open,
        }),
    );
    set_active(layer, item_id, ActiveItemKind::Message, ItemFinality::Open);
}

fn fold_reasoning_text_delta(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/reasoning/textDelta",
            Some("missing itemId"),
        );
        return;
    };
    layer
        .accumulators
        .reasoning_text
        .entry(item_id.to_string())
        .or_default()
        .push_str(str_or(row, "delta", ""));
    upsert_reasoning_from_accumulators(layer, seq, item_id);
}

fn fold_reasoning_summary_part(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/reasoning/summaryPartAdded",
            Some("missing itemId"),
        );
        return;
    };
    let index = row.get("summaryIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let parts = layer
        .accumulators
        .reasoning_summary
        .entry(item_id.to_string())
        .or_default();
    parts.resize(index + 1, String::new());
    upsert_reasoning_from_accumulators(layer, seq, item_id);
}

fn fold_reasoning_summary_delta(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/reasoning/summaryTextDelta",
            Some("missing itemId"),
        );
        return;
    };
    let index = row.get("summaryIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let parts = layer
        .accumulators
        .reasoning_summary
        .entry(item_id.to_string())
        .or_default();
    parts.resize(index + 1, String::new());
    parts[index].push_str(str_or(row, "delta", ""));
    upsert_reasoning_from_accumulators(layer, seq, item_id);
}

fn upsert_reasoning_from_accumulators(layer: &mut CodexLayer, seq: u64, item_id: &str) {
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Reasoning(ReasoningEntry {
            item_id: item_id.to_string(),
            text: layer
                .accumulators
                .reasoning_text
                .get(item_id)
                .cloned()
                .unwrap_or_default(),
            summary: layer
                .accumulators
                .reasoning_summary
                .get(item_id)
                .cloned()
                .unwrap_or_default(),
            finality: ItemFinality::Open,
        }),
    );
    set_active(
        layer,
        item_id,
        ActiveItemKind::Reasoning,
        ItemFinality::Open,
    );
}

fn fold_command_delta(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/commandExecution/outputDelta",
            Some("missing itemId"),
        );
        return;
    };
    let stderr = row.get("stream").and_then(Value::as_str) == Some("stderr");
    let mut truncated = false;
    let target = if stderr {
        layer
            .accumulators
            .command_stderr
            .entry(item_id.to_string())
            .or_default()
    } else {
        layer
            .accumulators
            .command_stdout
            .entry(item_id.to_string())
            .or_default()
    };
    append_bounded(target, str_or(row, "delta", ""), &mut truncated);
    let mut entry = existing_work(layer, item_id).unwrap_or_else(|| empty_work(item_id));
    entry.stdout_head = layer
        .accumulators
        .command_stdout
        .get(item_id)
        .cloned()
        .unwrap_or_default();
    entry.stderr_head = layer
        .accumulators
        .command_stderr
        .get(item_id)
        .cloned()
        .unwrap_or_default();
    entry.output_truncated |= truncated;
    entry.state = WorkState::Running;
    upsert_item(layer, seq, item_id, FeedEntryKind::Work(entry));
    set_active(layer, item_id, ActiveItemKind::Work, ItemFinality::Open);
}

fn fold_file_delta(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/fileChange/outputDelta",
            Some("missing itemId"),
        );
        return;
    };
    let mut entry = existing_work(layer, item_id).unwrap_or_else(|| empty_work(item_id));
    let mut kind = match entry.kind {
        WorkKind::FileChange {
            changes,
            patch_head,
            patch_truncated,
        } => WorkKind::FileChange {
            changes,
            patch_head,
            patch_truncated,
        },
        _ => WorkKind::FileChange {
            changes: Vec::new(),
            patch_head: String::new(),
            patch_truncated: false,
        },
    };
    if let WorkKind::FileChange {
        patch_head,
        patch_truncated,
        ..
    } = &mut kind
    {
        append_bounded(patch_head, str_or(row, "delta", ""), patch_truncated);
    }
    entry.kind = kind;
    entry.state = WorkState::Running;
    upsert_item(layer, seq, item_id, FeedEntryKind::Work(entry));
    set_active(layer, item_id, ActiveItemKind::Work, ItemFinality::Open);
}

fn fold_file_patch(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(
            layer,
            seq,
            "item/fileChange/patchUpdated",
            Some("missing itemId"),
        );
        return;
    };
    let mut entry = existing_work(layer, item_id).unwrap_or_else(|| empty_work(item_id));
    let (patch_head, patch_truncated) = match &entry.kind {
        WorkKind::FileChange {
            patch_head,
            patch_truncated,
            ..
        } => (patch_head.clone(), *patch_truncated),
        _ => (String::new(), false),
    };
    entry.kind = WorkKind::FileChange {
        changes: file_changes(row.get("changes")),
        patch_head,
        patch_truncated,
    };
    entry.state = WorkState::Running;
    upsert_item(layer, seq, item_id, FeedEntryKind::Work(entry));
}

fn fold_plan_delta(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(item_id) = row.get("itemId").and_then(Value::as_str) else {
        push_unrecognized(layer, seq, "item/plan/delta", Some("missing itemId"));
        return;
    };
    let mut text = plan_text(layer, item_id).unwrap_or_default();
    text.push_str(str_or(row, "delta", ""));
    upsert_item(
        layer,
        seq,
        item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.to_string(),
            kind: WorkKind::Plan {
                text,
                explanation: None,
                steps: Vec::new(),
            },
            state: WorkState::Running,
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
    set_active(layer, item_id, ActiveItemKind::Work, ItemFinality::Open);
}

fn fold_plan_updated(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = format!(
        "turn-plan:{}",
        layer.turn.active_id.as_deref().unwrap_or("unknown")
    );
    let steps = row
        .get("plan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|step| PlanStep {
            step: str_or(step, "step", "").to_string(),
            status: str_or(step, "status", "pending").to_string(),
        })
        .collect();
    upsert_item(
        layer,
        seq,
        &item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.clone(),
            kind: WorkKind::Plan {
                text: String::new(),
                explanation: string(row, "explanation"),
                steps,
            },
            state: WorkState::Running,
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
}

fn fold_diff_updated(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = format!(
        "turn-diff:{}",
        row.get("turnId")
            .and_then(Value::as_str)
            .or(layer.turn.active_id.as_deref())
            .unwrap_or("unknown")
    );
    let mut patch_head = String::new();
    let mut patch_truncated = false;
    append_bounded(
        &mut patch_head,
        str_or(row, "diff", ""),
        &mut patch_truncated,
    );
    upsert_item(
        layer,
        seq,
        &item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.clone(),
            kind: WorkKind::FileChange {
                changes: Vec::new(),
                patch_head,
                patch_truncated,
            },
            state: WorkState::Running,
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
}

fn fold_command_approval(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = str_or(row, "itemId", "unknown-command").to_string();
    let context = AskContext::Command {
        item_id: item_id.clone(),
        command: str_or(row, "command", "").to_string(),
        cwd: string(row, "cwd"),
        reason: string(row, "reason"),
    };
    layer.pending_approval_context = Some(context);
    let mut entry = existing_work(layer, &item_id).unwrap_or_else(|| empty_work(&item_id));
    entry.kind = WorkKind::Command {
        command: str_or(row, "command", "").to_string(),
        cwd: string(row, "cwd"),
        exit_code: None,
    };
    entry.state = WorkState::Proposed;
    upsert_item(layer, seq, &item_id, FeedEntryKind::Work(entry));
}

fn fold_file_approval(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = str_or(row, "itemId", "unknown-file-change").to_string();
    let changes = existing_work(layer, &item_id)
        .and_then(|entry| match entry.kind {
            WorkKind::FileChange { changes, .. } => Some(changes),
            _ => None,
        })
        .unwrap_or_default();
    layer.pending_approval_context = Some(AskContext::FileChange {
        item_id: item_id.clone(),
        reason: string(row, "reason"),
        changes: changes.clone(),
    });
    let mut entry = existing_work(layer, &item_id).unwrap_or_else(|| empty_work(&item_id));
    entry.kind = WorkKind::FileChange {
        changes,
        patch_head: String::new(),
        patch_truncated: false,
    };
    entry.state = WorkState::Proposed;
    upsert_item(layer, seq, &item_id, FeedEntryKind::Work(entry));
}

fn fold_permissions_approval(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = str_or(row, "itemId", "unknown-permissions").to_string();
    layer.pending_approval_context = Some(AskContext::Permissions {
        item_id: item_id.clone(),
        reason: string(row, "reason"),
        permissions: row.get("permissions").cloned().unwrap_or(Value::Null),
    });
    upsert_item(
        layer,
        seq,
        &item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.clone(),
            kind: WorkKind::Other {
                item_type: "permissions".to_string(),
                raw: row.clone(),
            },
            state: WorkState::Proposed,
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
}

fn fold_dynamic_tool_approval(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = row
        .get("callId")
        .or_else(|| row.get("itemId"))
        .and_then(Value::as_str)
        .unwrap_or("unknown-dynamic-tool")
        .to_string();
    let context = AskContext::DynamicTool {
        item_id: item_id.clone(),
        tool: str_or(row, "tool", "").to_string(),
        namespace: string(row, "namespace"),
        arguments: row.get("arguments").cloned().unwrap_or(Value::Null),
    };
    layer.pending_approval_context = Some(context);
    upsert_item(
        layer,
        seq,
        &item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.clone(),
            kind: WorkKind::DynamicTool {
                tool: str_or(row, "tool", "").to_string(),
                namespace: string(row, "namespace"),
                arguments: row.get("arguments").cloned().unwrap_or(Value::Null),
                success: None,
            },
            state: WorkState::Proposed,
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
}

fn fold_approval_required(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let Some(context) = layer.pending_approval_context.take() else {
        push(
            layer,
            seq,
            FeedEntryKind::Error(ErrorEntry {
                severity: ErrorSeverity::Warning,
                message: "Codex approval arrived without its preceding request context".to_string(),
                will_retry: false,
            }),
        );
        return;
    };
    let request_id = row.get("request_id").cloned().unwrap_or(Value::Null);
    if !request_id.is_string() && !request_id.is_i64() && !request_id.is_u64() {
        push(
            layer,
            seq,
            FeedEntryKind::Error(ErrorEntry {
                severity: ErrorSeverity::Warning,
                message: "Codex approval has a non string/integer request id".to_string(),
                will_retry: false,
            }),
        );
        return;
    }
    let available = row
        .get("availableDecisions")
        .cloned()
        .unwrap_or(Value::Null);
    let actions = available
        .as_array()
        .into_iter()
        .flatten()
        .cloned()
        .map(|wire| AskAction {
            decision: wire.as_str().and_then(CodexDecision::from_wire),
            wire,
        })
        .collect();
    layer.asks.retain(|ask| ask.request_id != request_id);
    layer.asks.push_back(Ask {
        seq,
        request_id: request_id.clone(),
        context: context.clone(),
        available_decisions: available,
        actions,
    });
    if layer.asks.len() > ASKS_RETAINED {
        layer.asks.pop_front();
        layer.truncated_start = true;
    }
    set_work_state(
        layer,
        context.item_id(),
        WorkState::AwaitingApproval { request_id },
    );
}

fn fold_approval_resolved(layer: &mut CodexLayer, row: &Value) {
    let request_id = row.get("request_id").cloned().unwrap_or(Value::Null);
    let resolution = ApprovalResolution::from_wire(str_or(row, "reason", "unknown"));
    let Some(index) = layer
        .asks
        .iter()
        .position(|ask| ask.request_id == request_id)
    else {
        return;
    };
    let ask = layer.asks.remove(index).expect("ask index came from queue");
    let local_decision = layer.inputs.iter().find_map(|input| match &input.kind {
        InFlightKind::Answer {
            request_id: pending,
            decision,
        } if *pending == request_id => Some(*decision),
        _ => None,
    });
    set_work_state(
        layer,
        ask.context.item_id(),
        if resolution.proceeded() {
            if matches!(
                local_decision,
                Some(CodexDecision::Decline | CodexDecision::Cancel)
            ) {
                WorkState::Denied
            } else {
                WorkState::Running
            }
        } else {
            WorkState::Abandoned { reason: resolution }
        },
    );
}

fn fold_unsupported_user_input(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let item_id = str_or(row, "itemId", "unsupported-user-input").to_string();
    upsert_item(
        layer,
        seq,
        &item_id,
        FeedEntryKind::Work(WorkEntry {
            item_id: item_id.clone(),
            kind: WorkKind::UnsupportedUserInput {
                questions: row.get("questions").cloned().unwrap_or(Value::Null),
            },
            state: WorkState::BlockedUnsupported,
            stdout_head: String::new(),
            stderr_head: String::new(),
            output_truncated: false,
        }),
    );
    if !layer.accumulators.unsupported.contains(&item_id) {
        layer.accumulators.unsupported.push_back(item_id);
    }
}

fn fold_input_result(layer: &mut CodexLayer, seq: u64, row: &Value) {
    let input_id = row.get("input_id").and_then(Value::as_array).map(|bytes| {
        bytes
            .iter()
            .filter_map(Value::as_u64)
            .filter_map(|byte| u8::try_from(byte).ok())
            .collect::<Vec<_>>()
    });
    let matched = input_id.as_ref().and_then(|input_id| {
        layer
            .inputs
            .iter()
            .find(|input| &input.input_id == input_id)
            .cloned()
    });
    if let Some(input_id) = &input_id {
        layer.inputs.retain(|input| &input.input_id != input_id);
    }
    let error = row
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| {
            (row.get("result").and_then(Value::as_str) == Some("error"))
                .then_some("Codex input failed")
        });
    if let Some(message) = error {
        push(
            layer,
            seq,
            FeedEntryKind::Error(ErrorEntry {
                severity: ErrorSeverity::Error,
                message: message.to_string(),
                will_retry: false,
            }),
        );
    } else if let Some(InFlightInput {
        op,
        kind: InFlightKind::Steer { text, .. },
        ..
    }) = matched
    {
        push(
            layer,
            seq,
            FeedEntryKind::Prompt(PromptEntry {
                item_id: format!("steer:{}", op.0),
                source: PromptSource::SteerEcho,
                parts: vec![PromptPart::Text { text }],
                finality: ItemFinality::Complete,
            }),
        );
    }
}

fn fold_error(layer: &mut CodexLayer, seq: u64, row: &Value, severity: ErrorSeverity) {
    let body = row.get("error").unwrap_or(row);
    push(
        layer,
        seq,
        FeedEntryKind::Error(ErrorEntry {
            severity,
            message: body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Codex reported an error")
                .to_string(),
            will_retry: row
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
    );
}

fn fold_thread_status(layer: &mut CodexLayer, row: &Value) {
    layer.turn.status = match row.pointer("/status/type").and_then(Value::as_str) {
        Some("active") => ThreadStatus::Active,
        Some("idle") => ThreadStatus::Idle,
        _ => ThreadStatus::Unknown,
    };
}

fn token_usage(row: &Value) -> TokenUsage {
    let usage = row.pointer("/tokenUsage/total").unwrap_or(&Value::Null);
    TokenUsage {
        input_tokens: usage.get("inputTokens").and_then(Value::as_u64),
        cached_input_tokens: usage.get("cachedInputTokens").and_then(Value::as_u64),
        output_tokens: usage.get("outputTokens").and_then(Value::as_u64),
        reasoning_output_tokens: usage.get("reasoningOutputTokens").and_then(Value::as_u64),
        total_tokens: usage.get("totalTokens").and_then(Value::as_u64),
        model_context_window: row
            .pointer("/tokenUsage/modelContextWindow")
            .and_then(Value::as_u64),
    }
}

fn work_state(item: &Value, finality: ItemFinality) -> WorkState {
    if finality == ItemFinality::Open {
        return WorkState::Running;
    }
    WorkState::Done {
        outcome: match item.get("status").and_then(Value::as_str) {
            Some("completed") => WorkOutcome::Succeeded,
            Some("failed") => WorkOutcome::Failed,
            Some("declined") => WorkOutcome::Declined,
            _ => WorkOutcome::Unknown,
        },
    }
}

fn empty_work(item_id: &str) -> WorkEntry {
    WorkEntry {
        item_id: item_id.to_string(),
        kind: WorkKind::Other {
            item_type: "unknown".to_string(),
            raw: Value::Null,
        },
        state: WorkState::Running,
        stdout_head: String::new(),
        stderr_head: String::new(),
        output_truncated: false,
    }
}

fn prompt_parts(value: Option<&Value>) -> Vec<PromptPart> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text") => PromptPart::Text {
                text: str_or(part, "text", "").to_string(),
            },
            Some("image") => PromptPart::Image {
                url: string(part, "url"),
            },
            Some("localImage") => PromptPart::LocalImage {
                path: string(part, "path"),
            },
            kind => PromptPart::Other {
                item_type: kind.map(str::to_owned),
                raw: part.clone(),
            },
        })
        .collect()
}

fn file_changes(value: Option<&Value>) -> Vec<FileChange> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|change| FileChange {
            path: str_or(change, "path", "").to_string(),
            status: change
                .pointer("/kind/type")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| string(change, "status")),
            diff: string(change, "diff"),
        })
        .collect()
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .as_str()
                .or_else(|| value.get("text").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .collect()
}

fn existing_work(layer: &CodexLayer, item_id: &str) -> Option<WorkEntry> {
    let id = *layer.item_entries.get(item_id)?;
    match entry_kind(layer, id)? {
        FeedEntryKind::Work(entry) => Some(entry.clone()),
        _ => None,
    }
}

fn existing_message_phase(layer: &CodexLayer, item_id: &str) -> Option<MessagePhase> {
    let id = *layer.item_entries.get(item_id)?;
    match entry_kind(layer, id)? {
        FeedEntryKind::Message(entry) => Some(entry.phase),
        _ => None,
    }
}

fn plan_text(layer: &CodexLayer, item_id: &str) -> Option<String> {
    let work = existing_work(layer, item_id)?;
    match work.kind {
        WorkKind::Plan { text, .. } => Some(text),
        _ => None,
    }
}

fn set_work_state(layer: &mut CodexLayer, item_id: &str, state: WorkState) {
    let Some(id) = layer.item_entries.get(item_id).copied() else {
        return;
    };
    if let Some(FeedEntryKind::Work(entry)) = entry_kind_mut(layer, id) {
        entry.state = state;
    }
}

fn set_active(layer: &mut CodexLayer, item_id: &str, kind: ActiveItemKind, finality: ItemFinality) {
    layer
        .accumulators
        .active_items
        .retain(|item| item.item_id != item_id);
    if finality == ItemFinality::Open {
        layer.accumulators.active_items.push_back(ActiveItem {
            item_id: item_id.to_string(),
            kind,
        });
    }
}

fn append_bounded(target: &mut String, delta: &str, truncated: &mut bool) {
    if target.len() >= OUTPUT_HEAD_MAX {
        *truncated |= !delta.is_empty();
        return;
    }
    let remaining = OUTPUT_HEAD_MAX - target.len();
    if delta.len() <= remaining {
        target.push_str(delta);
        return;
    }
    let mut end = remaining;
    while end > 0 && !delta.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&delta[..end]);
    *truncated = true;
}

fn upsert_item(layer: &mut CodexLayer, seq: u64, item_id: &str, kind: FeedEntryKind) -> u64 {
    if let Some(id) = layer.item_entries.get(item_id).copied()
        && let Some(entry) = layer.entries.iter_mut().find(|entry| entry.id == id)
    {
        entry.kind = kind;
        return id;
    }
    let id = push(layer, seq, kind);
    layer.item_entries.insert(item_id.to_string(), id);
    id
}

fn upsert_turn(layer: &mut CodexLayer, seq: u64, turn_id: &str, kind: FeedEntryKind) -> u64 {
    if let Some(id) = layer.turn_entries.get(turn_id).copied()
        && let Some(entry) = layer.entries.iter_mut().find(|entry| entry.id == id)
    {
        entry.kind = kind;
        return id;
    }
    let id = push(layer, seq, kind);
    layer.turn_entries.insert(turn_id.to_string(), id);
    id
}

fn push(layer: &mut CodexLayer, seq: u64, kind: FeedEntryKind) -> u64 {
    let id = layer.next_entry_id;
    layer.next_entry_id += 1;
    layer.entries.push_back(FeedEntry { id, seq, kind });
    while layer.entries.len() > FEED_RETAINED {
        if let Some(evicted) = layer.entries.pop_front() {
            layer.item_entries.retain(|_, entry| *entry != evicted.id);
            layer.turn_entries.retain(|_, entry| *entry != evicted.id);
            layer.evicted += 1;
        }
    }
    id
}

fn push_unrecognized(layer: &mut CodexLayer, seq: u64, method: &str, detail: Option<&str>) {
    push(
        layer,
        seq,
        FeedEntryKind::Unrecognized(UnrecognizedEntry {
            method: method.to_string(),
            detail: detail.map(str::to_owned),
        }),
    );
}

fn entry_kind(layer: &CodexLayer, id: u64) -> Option<&FeedEntryKind> {
    layer
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .map(|entry| &entry.kind)
}

fn entry_kind_mut(layer: &mut CodexLayer, id: u64) -> Option<&mut FeedEntryKind> {
    layer
        .entries
        .iter_mut()
        .find(|entry| entry.id == id)
        .map(|entry| &mut entry.kind)
}

fn string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn str_or<'a>(value: &'a Value, key: &str, default: &'a str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or(default)
}
