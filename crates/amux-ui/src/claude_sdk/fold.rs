//! Pure translation of daemon rows into the SDK feed.

use serde_json::Value;

use super::*;
use crate::claude::facts::{inbound_message, invocation};

pub(super) fn observe(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let kind = row["type"].as_str().unwrap_or("<missing type>");
    match kind {
        "assistant" => assistant(layer, seq, row),
        "stream_event" => stream(layer, seq, row),
        "user" => user(layer, seq, row),
        "result" => {
            layer.interrupt_streams();
            let usage = &row["usage"];
            push(
                layer,
                seq,
                FeedEntryKind::Turn(TurnEntry {
                    uuid: id(row, "uuid"),
                    outcome: string(row, "subtype").unwrap_or_else(|| "unknown".into()),
                    is_error: row["is_error"].as_bool().unwrap_or(false),
                    stop_reason: string(row, "stop_reason"),
                    result: string(row, "result"),
                    errors: row["errors"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(64)
                        .filter_map(Value::as_str)
                        .map(clipped)
                        .collect(),
                    usage: TokenUsage {
                        input_tokens: usage["input_tokens"].as_u64(),
                        output_tokens: usage["output_tokens"].as_u64(),
                        cache_read_input_tokens: usage["cache_read_input_tokens"].as_u64(),
                        cache_creation_input_tokens: usage["cache_creation_input_tokens"].as_u64(),
                    },
                    model_usage: bounded_value(&row["modelUsage"]),
                    total_cost_usd: row["total_cost_usd"].as_f64(),
                    duration_ms: row["duration_ms"].as_u64(),
                    duration_api_ms: row["duration_api_ms"].as_u64(),
                    num_turns: row["num_turns"].as_u64(),
                }),
                oversized(row),
            );
        }
        "amux.claude_sdk.ready" => {
            layer.interrupt_streams();
            layer.cursors.clear();
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::Ready {
                    session_id: id(row, "session_id"),
                    resumed: row["resumed"] == true,
                }),
                false,
            );
        }
        "amux.claude_sdk.gap" => {
            layer.interrupt_streams();
            layer.cursors.clear();
            layer.truncated_start = true;
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::Gap {
                    resumed_session_id: id(row, "resumed_session_id"),
                }),
                false,
            );
        }
        "conversation_reset" => {
            layer.interrupt_streams();
            layer.cursors.clear();
            push(
                layer,
                seq,
                FeedEntryKind::Boundary(BoundaryEntry::ConversationReset {
                    conversation_id: id(row, "new_conversation_id"),
                }),
                false,
            );
        }
        "amux.claude_sdk.message" => agent_message(layer, seq, row),
        "system" => system(layer, seq, row),
        "rate_limit_event" | "tool_progress" | "tool_use_summary" | "auth_status" => {
            status(layer, seq, kind, row);
        }
        // These have their own session/ask/write state, separate from content.
        "amux.claude_sdk.session_facts"
        | "amux.claude_sdk.context_breakdown"
        | "amux.claude_sdk.permission_required"
        | "amux.claude_sdk.permission_resolved"
        | "amux.claude_sdk.elicitation_required"
        | "amux.claude_sdk.elicitation_resolved"
        | "amux.claude_sdk.dialog_required"
        | "amux.claude_sdk.dialog_resolved"
        | "amux.attachments" => {}
        "amux.claude_sdk.input_result" => {
            if row["outcome"] != "ok" {
                status(layer, seq, "input_error", row);
            }
        }
        _ => unknown(layer, seq, kind, "row is not recognized"),
    }
}

fn assistant(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let message = &row["message"];
    let Some(message_id) = id(message, "id") else {
        unknown(layer, seq, "assistant", "missing message id");
        return;
    };
    let Some(blocks) = message["content"].as_array() else {
        unknown(layer, seq, "assistant", "missing content blocks");
        return;
    };
    let row_id = id(row, "uuid");
    if row_id.is_some() && layer.entries.iter().any(|e| e.final_row_id == row_id) {
        return;
    }
    let parent = id(row, "parent_tool_use_id");
    let cursor = cursor(layer, &message_id, &parent);
    // Claude emits one final row per block. A whole-message snapshot uses
    // array indices instead, so siblings never overwrite one another.
    let start = if blocks.len() > 1 {
        0
    } else {
        layer.cursors[cursor].next_final_index
    };
    let mut next = start;
    for (offset, block) in blocks.iter().enumerate() {
        let index = if blocks.len() == 1 {
            // A replay tail may start at block 1 or later. Match the retained
            // stream block instead of assigning the absent block 0 to it.
            layer
                .entries
                .iter()
                .filter(|e| e.final_row_id.is_none())
                .filter(|e| block_matches(&e.kind, block))
                .filter_map(|e| e.block.as_ref())
                .filter(|b| {
                    b.message_id == message_id && b.parent_tool_use_id == parent && b.index >= start
                })
                .map(|b| b.index)
                .min()
                .unwrap_or(start)
        } else {
            offset as u64
        };
        next = index + 1;
        let key = BlockId {
            message_id: message_id.clone(),
            parent_tool_use_id: parent.clone(),
            index,
        };
        upsert_block(layer, seq, key, block, Finality::Complete, row_id.clone());
    }
    layer.cursors[cursor].next_final_index = next;
}

fn stream(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let event = &row["event"];
    let parent = id(row, "parent_tool_use_id");
    let kind = event["type"].as_str().unwrap_or("<missing event type>");
    if kind == "message_start" {
        let Some(message_id) = id(&event["message"], "id") else {
            unknown(layer, seq, kind, "missing message id");
            return;
        };
        // A parent and each subagent have independent streaming channels.
        for old in &mut layer.cursors {
            if old.parent_tool_use_id == parent {
                old.streaming = false;
            }
        }
        let index = cursor(layer, &message_id, &parent);
        layer.cursors[index].streaming = true;
        let key = BlockId {
            message_id,
            parent_tool_use_id: parent,
            index: 0,
        };
        if !layer.entries.iter().any(|e| e.block.as_ref() == Some(&key)) {
            upsert_block(
                layer,
                seq,
                key,
                &serde_json::json!({"type":"text","text":""}),
                Finality::Streaming,
                None,
            );
            layer.cursors[index].placeholder_entry_id = layer.entries.back().map(|e| e.id);
        }
        return;
    }
    let Some(cursor) = layer
        .cursors
        .iter()
        .rposition(|c| c.streaming && c.parent_tool_use_id == parent)
    else {
        unknown(layer, seq, kind, "stream start is unavailable");
        return;
    };
    let message_id = layer.cursors[cursor].message_id.clone();
    match kind {
        "content_block_start" => {
            let Some(index) = event["index"].as_u64() else {
                unknown(layer, seq, kind, "missing block index");
                return;
            };
            if let Some(placeholder) = layer.cursors[cursor].placeholder_entry_id.take()
                && let Some(entry) = layer
                    .entries
                    .iter_mut()
                    .find(|e| e.id == placeholder && e.final_row_id.is_none())
                && let Some(block) = &mut entry.block
            {
                block.index = index;
            }
            upsert_block(
                layer,
                seq,
                BlockId {
                    message_id,
                    parent_tool_use_id: parent,
                    index,
                },
                &event["content_block"],
                Finality::Streaming,
                None,
            );
        }
        "content_block_delta" | "content_block_stop" => {
            let Some(index) = event["index"].as_u64() else {
                unknown(layer, seq, kind, "missing block index");
                return;
            };
            let key = BlockId {
                message_id,
                parent_tool_use_id: parent,
                index,
            };
            let Some(entry) = layer
                .entries
                .iter_mut()
                .find(|e| e.block.as_ref() == Some(&key))
            else {
                unknown(layer, seq, kind, "block start is unavailable");
                return;
            };
            let Some(finality) = finality_mut(&mut entry.kind) else {
                return;
            };
            if matches!(finality, Finality::Complete | Finality::Interrupted) {
                return;
            }
            if kind == "content_block_stop" {
                *finality = Finality::Stopped;
                return;
            }
            let delta = &event["delta"];
            let delta_type = delta["type"].as_str().unwrap_or("");
            let target = match (&mut entry.kind, delta_type) {
                (FeedEntryKind::Message(m), "text_delta") => Some((&mut m.text, "text")),
                (FeedEntryKind::Thinking(t), "thinking_delta") => Some((&mut t.text, "thinking")),
                (FeedEntryKind::Tool(t), "input_json_delta") => {
                    Some((&mut t.input_json, "partial_json"))
                }
                (FeedEntryKind::Thinking(_), "signature_delta") => return,
                _ => None,
            };
            if let Some((text, field)) = target
                && let Some(part) = delta[field].as_str()
            {
                entry.content_truncated |= append(text, part);
            } else {
                unknown(layer, seq, kind, "unrecognized or mismatched block delta");
            }
        }
        "message_stop" => {
            for entry in &mut layer.entries {
                if entry
                    .block
                    .as_ref()
                    .is_some_and(|b| b.message_id == message_id && b.parent_tool_use_id == parent)
                    && let Some(finality) = finality_mut(&mut entry.kind)
                    && *finality == Finality::Streaming
                {
                    *finality = Finality::Stopped;
                }
            }
            // Keep the cursor to ignore late deltas against completed blocks.
        }
        "message_delta" => {}
        _ => unknown(layer, seq, kind, "unrecognized stream event"),
    }
}

fn block_matches(kind: &FeedEntryKind, block: &Value) -> bool {
    match kind {
        FeedEntryKind::Message(_) => block["type"] == "text",
        FeedEntryKind::Thinking(_) => matches!(
            block["type"].as_str(),
            Some("thinking" | "redacted_thinking")
        ),
        FeedEntryKind::Tool(tool) => block["id"].as_str() == Some(tool.tool_use_id.as_str()),
        _ => false,
    }
}

fn upsert_block(
    layer: &mut ClaudeSdkLayer,
    seq: u64,
    key: BlockId,
    block: &Value,
    finality: Finality,
    row_id: Option<String>,
) {
    let existing = layer
        .entries
        .iter()
        .position(|e| e.block.as_ref() == Some(&key));
    if finality != Finality::Complete
        && existing.is_some_and(|i| {
            matches!(
                &layer.entries[i].kind,
                FeedEntryKind::Message(MessageEntry {
                    finality: Finality::Complete,
                    ..
                }) | FeedEntryKind::Thinking(ThinkingEntry {
                    finality: Finality::Complete,
                    ..
                }) | FeedEntryKind::Tool(ToolEntry {
                    finality: Finality::Complete,
                    ..
                })
            )
        })
    {
        return;
    }
    let kind = match block["type"].as_str() {
        Some("text") if block["text"].is_string() => FeedEntryKind::Message(MessageEntry {
            text: string(block, "text").unwrap_or_default(),
            finality,
        }),
        Some("thinking" | "redacted_thinking") => FeedEntryKind::Thinking(ThinkingEntry {
            text: string(block, "thinking").unwrap_or_default(),
            redacted: block["type"] == "redacted_thinking",
            finality,
        }),
        Some("tool_use" | "server_tool_use") => {
            let (Some(tool_use_id), Some(name)) = (id(block, "id"), id(block, "name")) else {
                unknown(layer, seq, "assistant.tool_use", "missing tool id or name");
                return;
            };
            let input = bounded_value(&block["input"]);
            FeedEntryKind::Tool(ToolEntry {
                tool_use_id,
                name: name.clone(),
                invocation: invocation(&name, input.as_ref().unwrap_or(&Value::Null)),
                input,
                input_json: String::new(),
                finality,
                result: existing.and_then(|i| match &layer.entries[i].kind {
                    FeedEntryKind::Tool(t) => t.result.clone(),
                    _ => None,
                }),
            })
        }
        _ => {
            unknown(
                layer,
                seq,
                "assistant.content",
                "unrecognized content block",
            );
            return;
        }
    };
    if let Some(index) = existing {
        let entry = &mut layer.entries[index];
        entry.kind = kind;
        entry.content_truncated = oversized(block);
        entry.final_row_id = row_id;
    } else {
        push(layer, seq, kind, oversized(block));
        let entry = layer.entries.back_mut().expect("just pushed");
        entry.block = Some(key);
        entry.final_row_id = row_id;
    }
}

fn cursor(layer: &mut ClaudeSdkLayer, message_id: &str, parent: &Option<String>) -> usize {
    if let Some(index) = layer
        .cursors
        .iter()
        .position(|c| c.message_id == message_id && &c.parent_tool_use_id == parent)
    {
        return index;
    }
    if layer.cursors.len() == FEED_RETAINED {
        layer.cursors.pop_front();
    }
    layer.cursors.push_back(MessageCursor {
        message_id: message_id.into(),
        parent_tool_use_id: parent.clone(),
        next_final_index: 0,
        streaming: false,
        placeholder_entry_id: None,
    });
    layer.cursors.len() - 1
}

fn user(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let uuid = id(row, "uuid");
    if uuid.is_some() && layer.entries.iter().any(|e| e.final_row_id == uuid) {
        return;
    }
    let content = &row["message"]["content"];
    if !content.is_string() && !content.is_array() {
        unknown(layer, seq, "user", "missing message content");
        return;
    }
    let mut text = String::new();
    let mut images = 0;
    let mut truncated = false;
    if let Some(value) = content.as_str() {
        truncated |= append(&mut text, value);
    }
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => {
                if !text.is_empty() {
                    truncated |= append(&mut text, "\n");
                }
                if let Some(value) = block["text"].as_str() {
                    truncated |= append(&mut text, value);
                }
            }
            Some("image") => images += 1,
            Some("tool_result") => tool_result(layer, seq, row, block),
            _ => unknown(layer, seq, "user.content", "unrecognized content block"),
        }
    }
    if text.is_empty() && images == 0 {
        return;
    }
    let kind = if let Some(message) = inbound_message(&text) {
        FeedEntryKind::AgentMessage(AgentMessageEntry {
            id: message.id,
            context: message.context,
            from: message.from,
            kind: message.kind,
            text: message.text,
            delivery: None,
        })
    } else if text == "[Request interrupted by user]" {
        FeedEntryKind::Status(StatusEntry {
            status: text,
            details: None,
        })
    } else {
        FeedEntryKind::Prompt(PromptEntry {
            uuid: uuid.clone(),
            text,
            image_count: images,
            synthetic: row["isSynthetic"] == true,
            replay: row["isReplay"] == true,
        })
    };
    push(layer, seq, kind, truncated);
    layer.entries.back_mut().expect("just pushed").final_row_id = uuid;
}

fn tool_result(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value, block: &Value) {
    let Some(tool_id) = id(block, "tool_use_id") else {
        unknown(layer, seq, "user.tool_result", "missing tool id");
        return;
    };
    let parent = id(row, "parent_tool_use_id");
    let content = &block["content"];
    let mut text = String::new();
    let mut truncated = false;
    if let Some(value) = content.as_str() {
        truncated |= append(&mut text, value);
    }
    for value in content.as_array().into_iter().flatten() {
        if let Some(part) = value["text"].as_str() {
            if !text.is_empty() {
                truncated |= append(&mut text, "\n");
            }
            truncated |= append(&mut text, part);
        }
    }
    let details = bounded_value(&row["tool_use_result"]);
    truncated |= oversized(&row["tool_use_result"]);
    let result = ToolResult {
        text,
        is_error: block["is_error"] == true,
        details,
    };
    if let Some(entry) = layer.entries.iter_mut().rev().find(|entry| {
        matches!(&entry.kind, FeedEntryKind::Tool(t) if t.tool_use_id == tool_id)
            && entry
                .block
                .as_ref()
                .is_none_or(|b| b.parent_tool_use_id == parent)
    }) {
        if let FeedEntryKind::Tool(tool) = &mut entry.kind {
            tool.result = Some(result);
        }
        entry.content_truncated |= truncated;
    } else {
        // A tail may start at the result. Preserve it without inventing an invocation.
        push(
            layer,
            seq,
            FeedEntryKind::Tool(ToolEntry {
                tool_use_id: tool_id,
                name: String::new(),
                invocation: ToolInvocation::Other,
                input: None,
                input_json: String::new(),
                finality: Finality::Complete,
                result: Some(result),
            }),
            truncated,
        );
    }
}

fn system(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    match row["subtype"].as_str().unwrap_or("<missing subtype>") {
        "compact_boundary" => {
            let metadata = &row["compact_metadata"];
            push(
                layer,
                seq,
                FeedEntryKind::Compaction(CompactionEntry {
                    trigger: string(metadata, "trigger"),
                    pre_tokens: metadata["pre_tokens"].as_u64(),
                    post_tokens: metadata["post_tokens"].as_u64(),
                }),
                false,
            );
        }
        "task_started" | "task_progress" | "task_updated" | "task_notification" => {
            task(layer, seq, row)
        }
        "background_tasks_changed" => {
            if let Some(tasks) = row["tasks"].as_array() {
                for row in tasks {
                    task(layer, seq, row);
                }
            } else {
                unknown(
                    layer,
                    seq,
                    "system.background_tasks_changed",
                    "missing task list",
                );
            }
        }
        "status" => status(layer, seq, row["status"].as_str().unwrap_or("ready"), row),
        "init" | "thinking_tokens" => {}
        subtype => unknown(
            layer,
            seq,
            &format!("system.{subtype}"),
            "unrecognized system row",
        ),
    }
}

fn task(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let Some(task_id) = id(row, "task_id") else {
        unknown(layer, seq, "system.task", "missing task id");
        return;
    };
    let index = layer
        .entries
        .iter()
        .position(|e| matches!(&e.kind, FeedEntryKind::Task(t) if t.task_id == task_id));
    let index = index.unwrap_or_else(|| {
        push(
            layer,
            seq,
            FeedEntryKind::Task(TaskEntry {
                task_id,
                description: String::new(),
                subagent_type: None,
                state: TaskState::Running,
                last_tool: None,
                summary: None,
                usage: None,
            }),
            false,
        );
        layer.entries.len() - 1
    });
    let entry = &mut layer.entries[index];
    entry.content_truncated |= oversized(row);
    let FeedEntryKind::Task(task) = &mut entry.kind else {
        unreachable!()
    };
    let fields = if row["subtype"] == "task_updated" {
        &row["patch"]
    } else {
        row
    };
    if let Some(description) = string(fields, "description") {
        task.description = description;
    }
    if let Some(subagent) = string(fields, "subagent_type") {
        task.subagent_type = Some(subagent);
    }
    if let Some(tool) = string(fields, "last_tool_name").or_else(|| string(fields, "last_tool")) {
        task.last_tool = Some(tool);
    }
    if let Some(summary) = string(fields, "summary") {
        task.summary = Some(summary);
    }
    if let Some(state) = string(fields, "status") {
        task.state = match state.as_str() {
            "running" | "in_progress" | "pending" => TaskState::Running,
            "completed" => TaskState::Completed,
            "failed" => TaskState::Failed,
            "stopped" | "killed" => TaskState::Stopped,
            _ => TaskState::Unknown(state),
        };
    }
    if let Some(usage) = fields.get("usage").filter(|v| v.is_object()) {
        task.usage = Some(TaskUsage {
            total_tokens: usage["total_tokens"].as_u64(),
            tool_uses: usage["tool_uses"].as_u64(),
            duration_ms: usage["duration_ms"].as_u64(),
        });
    }
}

fn agent_message(layer: &mut ClaudeSdkLayer, seq: u64, row: &Value) {
    let envelope = &row["envelope"];
    let Some(text) = string(envelope, "text") else {
        unknown(
            layer,
            seq,
            "amux.claude_sdk.message",
            "missing envelope text",
        );
        return;
    };
    let message_id = id(envelope, "id");
    if message_id.is_some()
        && layer.entries.iter().any(|entry| {
            matches!(&entry.kind,
        FeedEntryKind::AgentMessage(m) if m.id == message_id)
        })
    {
        return;
    }
    let from = &envelope["from"];
    let sender = if from["type"] == "human" {
        "human".into()
    } else {
        string(from, "name")
            .or_else(|| id(from, "agent_id"))
            .or_else(|| from.as_str().map(clipped))
            .unwrap_or_else(|| "unknown".into())
    };
    let message_kind = string(envelope, "kind");
    push(
        layer,
        seq,
        FeedEntryKind::AgentMessage(AgentMessageEntry {
            id: message_id,
            context: id(envelope, "context"),
            from: sender,
            kind: AgentMessageKind::read(message_kind.as_deref()),
            text,
            delivery: string(row, "delivery"),
        }),
        oversized(row),
    );
}

fn status(layer: &mut ClaudeSdkLayer, seq: u64, name: &str, row: &Value) {
    push(
        layer,
        seq,
        FeedEntryKind::Status(StatusEntry {
            status: clipped(name),
            details: bounded_value(row),
        }),
        oversized(row),
    );
}

fn unknown(layer: &mut ClaudeSdkLayer, seq: u64, kind: &str, detail: &str) {
    push(
        layer,
        seq,
        FeedEntryKind::Unrecognized(UnrecognizedEntry {
            row_type: clipped(kind),
            detail: clipped(detail),
        }),
        kind.len() > CONTENT_BYTES_RETAINED || detail.len() > CONTENT_BYTES_RETAINED,
    );
}

fn push(layer: &mut ClaudeSdkLayer, seq: u64, kind: FeedEntryKind, content_truncated: bool) {
    if layer.entries.len() == FEED_RETAINED {
        layer.entries.pop_front();
        layer.evicted += 1;
    }
    layer.entries.push_back(FeedEntry {
        id: layer.next_entry_id,
        seq,
        kind,
        block: None,
        content_truncated,
        final_row_id: None,
    });
    layer.next_entry_id += 1;
}

fn id(value: &Value, field: &str) -> Option<String> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= ID_BYTES_RETAINED)
        .map(str::to_owned)
}

fn string(value: &Value, field: &str) -> Option<String> {
    value[field].as_str().map(clipped)
}

fn clipped(text: &str) -> String {
    let mut out = String::new();
    append(&mut out, text);
    out
}

fn append(out: &mut String, text: &str) -> bool {
    let mut take = CONTENT_BYTES_RETAINED
        .saturating_sub(out.len())
        .min(text.len());
    while !text.is_char_boundary(take) {
        take -= 1;
    }
    out.push_str(&text[..take]);
    take < text.len()
}

fn oversized(value: &Value) -> bool {
    value.to_string().len() > CONTENT_BYTES_RETAINED
}

fn bounded_value(value: &Value) -> Option<Value> {
    (!value.is_null() && !oversized(value)).then(|| value.clone())
}
