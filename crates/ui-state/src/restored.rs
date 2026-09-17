//! Renderer-facing entries rebuilt from the canonical entries a chat's store
//! window holds, so every client paints a stored conversation from one
//! translation of the durable body.

pub mod claude {
    use serde_json::Value;

    use crate::attachments::Segment;
    use crate::claude::{FeedEntry, FeedEntryKind, ToolEntry, ToolOutcome, TurnDuration};

    /// The Claude PTY entry a stored canonical entry paints as.
    pub fn feed_entry(id: u64, entry: &crate::StoredClaudeEntry) -> FeedEntry {
        use crate::{
            DurableEntry as _, StoredClaudeBody as ClaudeBody,
            StoredClaudeEntryKind as ClaudeEntryKind,
        };

        let body = entry.body().cloned().unwrap_or_default();
        let text = entry.text().unwrap_or_default().to_string();
        let kind = match entry.entry_kind().unwrap_or_default() {
            ClaudeEntryKind::Prompt => {
                let (source, prompt_id) = match body {
                    ClaudeBody::Prompt { source, prompt_id } => (source, prompt_id),
                    _ => (String::new(), None),
                };
                let source = match source.as_str() {
                    "Typed" => crate::claude::PromptSource::Typed,
                    "Queued" => crate::claude::PromptSource::Queued,
                    "SuggestionAccepted" => crate::claude::PromptSource::SuggestionAccepted,
                    "Human" => crate::claude::PromptSource::Human,
                    "Unstated" | "" => crate::claude::PromptSource::Unstated,
                    label => crate::claude::PromptSource::Other {
                        label: label.to_string(),
                    },
                };
                FeedEntryKind::Prompt(crate::claude::PromptEntry {
                    text: text.clone(),
                    content: vec![Segment::Prose(text)],
                    source,
                    prompt_id,
                })
            }
            ClaudeEntryKind::Message => FeedEntryKind::Message(crate::claude::MessageEntry {
                message_id: String::new(),
                segments: vec![text.clone()],
                content: vec![Segment::Prose(text)],
                finality: match entry.finality() {
                    Some("interrupted") => crate::claude::MessageFinality::Interrupted,
                    Some("abandoned") => crate::claude::MessageFinality::Abandoned,
                    Some("open") | None => crate::claude::MessageFinality::Open,
                    Some(stop_reason) => crate::claude::MessageFinality::Final {
                        stop_reason: stop_reason.to_string(),
                    },
                },
            }),
            ClaudeEntryKind::Thinking => {
                let (duration_ms, redacted) = match body {
                    ClaudeBody::Thinking {
                        duration_ms,
                        redacted,
                    } => (duration_ms, redacted),
                    _ => (None, false),
                };
                FeedEntryKind::Thinking(crate::claude::ThinkingEntry {
                    duration_ms,
                    redacted,
                })
            }
            ClaudeEntryKind::Tool => {
                let tool_use_id = match body {
                    ClaudeBody::Tool { tool_use_id } => tool_use_id,
                    _ => String::new(),
                };
                let input = entry
                    .tool_input()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes.0).ok())
                    .unwrap_or(Value::Null);
                let name = entry.tool_name().map(str::to_string);
                let invocation = name
                    .as_deref()
                    .map(|name| crate::claude::facts::invocation(name, &input))
                    .unwrap_or(crate::claude::facts::ToolInvocation::Other);
                let outcome = entry.tool_outcome().map_or(ToolOutcome::Pending, |bytes| {
                    let block = serde_json::from_slice::<Value>(&bytes.0).unwrap_or(Value::Null);
                    crate::claude::fold::tool_outcome(&Value::Null, &block)
                });
                FeedEntryKind::Tool(ToolEntry {
                    tool_use_id,
                    name,
                    invocation,
                    outcome,
                    message_final: entry.message_final(),
                    group_with_previous: false,
                    message_id: None,
                })
            }
            ClaudeEntryKind::Turn => {
                let (duration, message_count, pending_background_agents) = match body {
                    ClaudeBody::Turn {
                        duration_ms,
                        inferred,
                        message_count,
                        pending_background_agents,
                    } => (
                        if inferred {
                            TurnDuration::SincePrompt { ms: duration_ms }
                        } else {
                            TurnDuration::Measured {
                                ms: duration_ms.max(0) as u64,
                            }
                        },
                        message_count,
                        pending_background_agents,
                    ),
                    _ => (TurnDuration::SincePrompt { ms: 0 }, None, None),
                };
                FeedEntryKind::Turn(crate::claude::TurnEntry {
                    duration,
                    message_count,
                    pending_background_agents,
                })
            }
            ClaudeEntryKind::Compaction => {
                let (trigger, pre_tokens, post_tokens) = match body {
                    ClaudeBody::Compaction {
                        trigger,
                        pre_tokens,
                        post_tokens,
                    } => (trigger, pre_tokens, post_tokens),
                    _ => (None, None, None),
                };
                FeedEntryKind::Compaction(crate::claude::CompactionEntry {
                    trigger,
                    pre_tokens,
                    post_tokens,
                })
            }
            ClaudeEntryKind::CompactSummary => {
                FeedEntryKind::CompactSummary(crate::claude::CompactSummaryEntry { text })
            }
            ClaudeEntryKind::TaskNotification => {
                FeedEntryKind::TaskNotification(crate::claude::TaskNotificationEntry { text })
            }
            ClaudeEntryKind::Interruption => {
                FeedEntryKind::Interruption(crate::claude::InterruptionEntry {
                    kind: crate::claude::InterruptionKind::Turn,
                    interrupted_message_id: None,
                })
            }
            ClaudeEntryKind::AgentMessage => {
                let (id, context, from, kind) = match body {
                    ClaudeBody::AgentMessage {
                        id,
                        context,
                        from,
                        kind,
                    } => (id, context, from, kind),
                    _ => (
                        None,
                        None,
                        "unknown".into(),
                        crate::AgentMessageKind::Unstated,
                    ),
                };
                FeedEntryKind::AgentMessage(crate::claude::AgentMessageEntry {
                    id,
                    context,
                    from,
                    kind,
                    text,
                })
            }
            ClaudeEntryKind::ApiError => {
                let error = match body {
                    ClaudeBody::ApiError { error } => error,
                    _ => None,
                };
                FeedEntryKind::ApiError(crate::claude::ApiErrorEntry {
                    error,
                    text: (!text.is_empty()).then_some(text),
                })
            }
            ClaudeEntryKind::Unrecognized => {
                let (row_type, detail) = match body {
                    ClaudeBody::Unrecognized { row_type, detail } => (row_type, detail),
                    _ => (None, None),
                };
                FeedEntryKind::Unrecognized(crate::claude::UnrecognizedEntry { row_type, detail })
            }
        };
        FeedEntry { id, seq: 0, kind }
    }
}

pub mod claude_sdk {
    use crate::claude_sdk::{FeedEntry, FeedEntryKind, Finality};

    /// The Claude SDK entry a stored canonical entry paints as.
    pub fn feed_entry(id: u64, entry: &crate::StoredClaudeSdkEntry) -> FeedEntry {
        use crate::{
            DurableEntry as _, StoredClaudeSdkBody as ClaudeSdkBody,
            StoredClaudeSdkEntryKind as ClaudeSdkEntryKind,
        };

        let body = entry.body().cloned().unwrap_or_default();
        let text = entry.text().unwrap_or_default().to_string();
        let kind = match entry.entry_kind().unwrap_or_default() {
            ClaudeSdkEntryKind::Prompt => {
                let (uuid, image_count, synthetic, replay) = match body {
                    ClaudeSdkBody::Prompt {
                        uuid,
                        image_count,
                        synthetic,
                        replay,
                    } => (uuid, image_count, synthetic, replay),
                    _ => (None, 0, false, false),
                };
                FeedEntryKind::Prompt(crate::claude_sdk::PromptEntry {
                    uuid,
                    text,
                    image_count,
                    synthetic,
                    replay,
                })
            }
            ClaudeSdkEntryKind::Message => {
                FeedEntryKind::Message(crate::claude_sdk::MessageEntry {
                    text,
                    finality: stored_finality(entry.finality()),
                })
            }
            ClaudeSdkEntryKind::Thinking => {
                FeedEntryKind::Thinking(crate::claude_sdk::ThinkingEntry {
                    text,
                    redacted: matches!(body, ClaudeSdkBody::Thinking { redacted: true }),
                    finality: stored_finality(entry.finality()),
                })
            }
            ClaudeSdkEntryKind::Tool => {
                let tool_use_id = match body {
                    ClaudeSdkBody::Tool {
                        tool_use_id,
                        parent_tool_use_id,
                    } => {
                        let _ = parent_tool_use_id;
                        tool_use_id
                    }
                    _ => String::new(),
                };
                let name = entry.tool_name().unwrap_or_default().to_string();
                let input = entry
                    .tool_input()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes.0).ok())
                    .unwrap_or(serde_json::Value::Null);
                let result = entry.tool_outcome().and_then(|bytes| {
                    let value = serde_json::from_slice::<serde_json::Value>(&bytes.0).ok()?;
                    let details = value
                        .get("details")
                        .cloned()
                        .filter(|value| !value.is_null());
                    Some(crate::claude_sdk::ToolResult {
                        text: value
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        is_error: value
                            .get("is_error")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        edit: details.as_ref().and_then(crate::claude::facts::landed_edit),
                        details,
                    })
                });
                FeedEntryKind::Tool(crate::claude_sdk::ToolEntry {
                    tool_use_id,
                    name: name.clone(),
                    invocation: crate::claude::facts::invocation(&name, &input),
                    input_json: serde_json::to_string(&input).unwrap_or_default(),
                    input: Some(input),
                    finality: stored_finality(entry.finality()),
                    result,
                    group_with_previous: false,
                })
            }
            ClaudeSdkEntryKind::Task => {
                let task_id = match body {
                    ClaudeSdkBody::Task { task_id } => task_id,
                    _ => String::new(),
                };
                let usage = entry.task_usage().and_then(|bytes| {
                    serde_json::from_slice::<crate::claude_sdk::TaskUsage>(&bytes.0).ok()
                });
                FeedEntryKind::Task(crate::claude_sdk::TaskEntry {
                    task_id,
                    tool_use_id: entry.task_tool_use_id().map(str::to_string),
                    description: entry.task_description().unwrap_or_default().to_string(),
                    subagent_type: entry.task_subagent().map(str::to_string),
                    state: entry.task_state().cloned().unwrap_or_default(),
                    last_tool: entry.task_last_tool().map(str::to_string),
                    summary: entry.task_summary().map(str::to_string),
                    usage,
                })
            }
            ClaudeSdkEntryKind::Turn => {
                let (uuid, outcome, is_error, stop_reason) = match body {
                    ClaudeSdkBody::Turn {
                        uuid,
                        outcome,
                        is_error,
                        stop_reason,
                    } => (uuid, outcome, is_error, stop_reason),
                    _ => (None, "unknown".into(), false, None),
                };
                let raw = entry
                    .tool_outcome()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes.0).ok())
                    .unwrap_or(serde_json::Value::Null);
                FeedEntryKind::Turn(crate::claude_sdk::TurnEntry {
                    uuid,
                    outcome,
                    is_error,
                    stop_reason,
                    result: (!text.is_empty()).then_some(text),
                    errors: raw
                        .get("errors")
                        .and_then(serde_json::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    usage: serde_json::from_value(raw.get("usage").cloned().unwrap_or_default())
                        .unwrap_or_default(),
                    model_usage: raw.get("modelUsage").cloned(),
                    total_cost_usd: raw
                        .get("total_cost_usd")
                        .and_then(serde_json::Value::as_f64),
                    duration_ms: raw.get("duration_ms").and_then(serde_json::Value::as_u64),
                    duration_api_ms: raw
                        .get("duration_api_ms")
                        .and_then(serde_json::Value::as_u64),
                    num_turns: raw.get("num_turns").and_then(serde_json::Value::as_u64),
                })
            }
            ClaudeSdkEntryKind::Compaction => {
                let (trigger, pre_tokens, post_tokens) = match body {
                    ClaudeSdkBody::Compaction {
                        trigger,
                        pre_tokens,
                        post_tokens,
                    } => (trigger, pre_tokens, post_tokens),
                    _ => (None, None, None),
                };
                FeedEntryKind::Compaction(crate::claude_sdk::CompactionEntry {
                    trigger,
                    pre_tokens,
                    post_tokens,
                })
            }
            ClaudeSdkEntryKind::AgentMessage => {
                let (id, context, from, message_kind, delivery) = match body {
                    ClaudeSdkBody::AgentMessage {
                        id,
                        context,
                        from,
                        kind,
                        delivery,
                    } => (id, context, from, kind, delivery),
                    _ => (
                        None,
                        None,
                        "unknown".into(),
                        crate::AgentMessageKind::Unstated,
                        None,
                    ),
                };
                FeedEntryKind::AgentMessage(crate::claude_sdk::AgentMessageEntry {
                    id,
                    context,
                    from,
                    kind: message_kind,
                    text,
                    delivery,
                })
            }
            ClaudeSdkEntryKind::Status => {
                let status = match body {
                    ClaudeSdkBody::Status { status } => status,
                    _ => text,
                };
                FeedEntryKind::Status(crate::claude_sdk::StatusEntry {
                    status,
                    details: None,
                })
            }
            ClaudeSdkEntryKind::Boundary => {
                let boundary = match body {
                    ClaudeSdkBody::Boundary {
                        boundary,
                        session_id,
                    } if boundary == "gap" => crate::claude_sdk::BoundaryEntry::Gap {
                        resumed_session_id: session_id,
                    },
                    ClaudeSdkBody::Boundary {
                        boundary,
                        session_id,
                    } if boundary == "conversation_reset" => {
                        crate::claude_sdk::BoundaryEntry::ConversationReset {
                            conversation_id: session_id,
                        }
                    }
                    ClaudeSdkBody::Boundary {
                        boundary,
                        session_id,
                    } => crate::claude_sdk::BoundaryEntry::Ready {
                        session_id,
                        resumed: boundary == "resumed",
                    },
                    _ => crate::claude_sdk::BoundaryEntry::Ready {
                        session_id: None,
                        resumed: false,
                    },
                };
                FeedEntryKind::Boundary(boundary)
            }
            ClaudeSdkEntryKind::Unrecognized | ClaudeSdkEntryKind::ApiError => {
                let (row_type, detail) = match body {
                    ClaudeSdkBody::Unrecognized { row_type, detail } => (row_type, detail),
                    _ => ("unrecognized".into(), text),
                };
                FeedEntryKind::Unrecognized(crate::claude_sdk::UnrecognizedEntry {
                    row_type,
                    detail,
                })
            }
        };
        let parent_tool_use_id = match entry.body() {
            Some(ClaudeSdkBody::Tool {
                parent_tool_use_id, ..
            }) => parent_tool_use_id.clone(),
            _ => None,
        };
        FeedEntry::restored(id, kind, parent_tool_use_id, false)
    }

    fn stored_finality(value: Option<&str>) -> Finality {
        match value {
            Some("complete") => Finality::Complete,
            Some("interrupted") => Finality::Interrupted,
            Some("stopped") => Finality::Stopped,
            _ => Finality::Streaming,
        }
    }
}

pub mod codex {
    use serde_json::Value;

    use crate::codex::{
        BoundaryEntry, ErrorSeverity, FeedEntry, FeedEntryKind, ItemFinality, McpStartupEntry,
        McpStartupStatus, MessagePhase, PromptEntry, PromptPart, PromptSource, TurnStatus,
        WorkEntry, WorkKind, WorkOutcome, WorkState,
    };

    /// The Codex entry a stored canonical entry paints as.
    pub fn feed_entry(id: u64, entry: &crate::StoredCodexEntry) -> FeedEntry {
        use crate::{
            DurableEntry as _, StoredCodexBody as CodexBody, StoredCodexEntryKind as CodexEntryKind,
        };

        let body = entry.body().cloned().unwrap_or_default();
        let text = entry.text().unwrap_or_default().to_string();
        let finality = if entry.finality().or_else(|| entry.state()) == Some("open") {
            ItemFinality::Open
        } else {
            ItemFinality::Complete
        };
        let kind = match entry.entry_kind().unwrap_or_default() {
            CodexEntryKind::Prompt => {
                let (item_id, source) = match body {
                    CodexBody::Item { item_id, .. } => (item_id, PromptSource::Protocol),
                    CodexBody::Steer { input_id } => (input_id, PromptSource::SteerEcho),
                    _ => (String::new(), PromptSource::Protocol),
                };
                FeedEntryKind::Prompt(PromptEntry {
                    item_id,
                    source,
                    parts: vec![PromptPart::Text { text: text.clone() }],
                    content: vec![crate::attachments::Segment::Prose(text)],
                    finality,
                })
            }
            CodexEntryKind::Message => {
                let (item_id, item_type) = match body {
                    CodexBody::Item { item_id, item_type } => (item_id, item_type),
                    _ => (String::new(), String::new()),
                };
                let phase = entry
                    .details()
                    .and_then(|details| serde_json::from_slice::<Value>(&details.0).ok())
                    .and_then(|details| {
                        details
                            .get("phase")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .filter(|phase| phase == "commentary")
                    .map_or_else(
                        || {
                            if item_type.contains("agentMessage/delta") {
                                MessagePhase::Commentary
                            } else {
                                MessagePhase::FinalAnswer
                            }
                        },
                        |_| MessagePhase::Commentary,
                    );
                FeedEntryKind::Message(crate::codex::MessageEntry {
                    item_id,
                    text: text.clone(),
                    content: vec![crate::attachments::Segment::Prose(text)],
                    phase,
                    finality,
                })
            }
            CodexEntryKind::Reasoning => {
                let (item_id, item_type) = match body {
                    CodexBody::Item { item_id, item_type } => (item_id, item_type),
                    _ => (String::new(), String::new()),
                };
                let mut summary: Vec<String> = entry
                    .details()
                    .and_then(|details| serde_json::from_slice::<Value>(&details.0).ok())
                    .and_then(|details| details.get("summary").cloned())
                    .and_then(|summary| serde_json::from_value(summary).ok())
                    .unwrap_or_default();
                let reasoning_text = if item_type.contains("reasoning/summary") {
                    if summary.is_empty() && !text.is_empty() {
                        summary.push(text.clone());
                    }
                    String::new()
                } else {
                    text
                };
                FeedEntryKind::Reasoning(crate::codex::ReasoningEntry {
                    item_id,
                    text: reasoning_text,
                    summary,
                    finality,
                })
            }
            CodexEntryKind::Work => {
                if let Some(restored) = restored_codex_work(entry) {
                    restored.kind
                } else {
                    let item_id = match body {
                        CodexBody::Item { item_id, .. } => item_id,
                        _ => String::new(),
                    };
                    FeedEntryKind::Work(WorkEntry {
                        item_id,
                        kind: WorkKind::Other {
                            item_type: "work".into(),
                            raw: serde_json::Value::Null,
                        },
                        state: WorkState::Done {
                            outcome: WorkOutcome::Unknown,
                        },
                        stdout_head: text,
                        stderr_head: String::new(),
                        output_truncated: entry.is_clipped(),
                    })
                }
            }
            CodexEntryKind::McpStartup => {
                let mut servers = std::collections::BTreeMap::new();
                if !text.is_empty() {
                    servers.insert(
                        text,
                        crate::codex::McpServerStartup {
                            status: McpStartupStatus::Ready,
                            error: None,
                            failure_reason: None,
                        },
                    );
                }
                FeedEntryKind::McpStartup(McpStartupEntry { servers })
            }
            CodexEntryKind::AgentMessage => {
                let (id, context, from, message_kind, delivery) = match body {
                    CodexBody::AgentMessage {
                        id,
                        context,
                        from,
                        kind,
                        delivery,
                    } => (id, context, from, kind, delivery),
                    _ => (
                        None,
                        None,
                        "unknown".into(),
                        crate::AgentMessageKind::Unstated,
                        None,
                    ),
                };
                FeedEntryKind::AgentMessage(crate::codex::AgentMessageEntry {
                    id,
                    context,
                    from,
                    kind: message_kind,
                    text,
                    delivery,
                })
            }
            CodexEntryKind::Turn => {
                let (turn_id, status, token_usage) = match body {
                    CodexBody::Turn {
                        turn_id,
                        status,
                        token_usage,
                    } => (turn_id, status, token_usage),
                    _ => (String::new(), "completed".into(), None),
                };
                let status = match status.as_str() {
                    "interrupted" => TurnStatus::Interrupted,
                    "failed" => TurnStatus::Failed { message: text },
                    _ => TurnStatus::Completed,
                };
                FeedEntryKind::Turn(crate::codex::TurnEntry {
                    turn_id,
                    status,
                    token_usage,
                })
            }
            CodexEntryKind::Boundary => {
                let boundary = match body {
                    CodexBody::Boundary { kind } if kind == "resumed" => BoundaryEntry::Resumed,
                    CodexBody::Boundary { kind } if kind == "ready" => BoundaryEntry::Ready,
                    CodexBody::Boundary { kind } if kind == "compacted" => {
                        BoundaryEntry::Compacted {
                            turn_id: (!text.is_empty()).then_some(text),
                        }
                    }
                    CodexBody::Boundary { .. } => BoundaryEntry::Gap { reason: text },
                    _ => BoundaryEntry::Gap { reason: text },
                };
                FeedEntryKind::Boundary(boundary)
            }
            CodexEntryKind::Error => {
                let (severity, will_retry) = match body {
                    CodexBody::Error {
                        severity,
                        will_retry,
                    } => (
                        match severity.as_str() {
                            "warning" => ErrorSeverity::Warning,
                            "notice" => ErrorSeverity::Notice,
                            _ => ErrorSeverity::Error,
                        },
                        will_retry,
                    ),
                    _ => (ErrorSeverity::Error, false),
                };
                FeedEntryKind::Error(crate::codex::ErrorEntry {
                    severity,
                    message: text,
                    will_retry,
                })
            }
            CodexEntryKind::Unrecognized => {
                let method = match body {
                    CodexBody::Unrecognized { method } => method,
                    _ => "unrecognized".into(),
                };
                FeedEntryKind::Unrecognized(crate::codex::UnrecognizedEntry {
                    method,
                    detail: (!text.is_empty()).then_some(text),
                })
            }
        };
        FeedEntry { id, seq: 0, kind }
    }

    fn restored_codex_work(entry: &crate::StoredCodexEntry) -> Option<FeedEntry> {
        use crate::DurableEntry as _;

        let details = entry.details()?;
        let value: serde_json::Value = serde_json::from_slice(&details.0).ok()?;
        let event = if value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| {
                matches!(
                    kind,
                    "commandExecution"
                        | "fileChange"
                        | "mcpToolCall"
                        | "dynamicToolCall"
                        | "webSearch"
                        | "plan"
                )
            }) {
            serde_json::json!({"type":"item/completed", "item":value})
        } else {
            value
        };
        let mut observation = crate::codex::Observation::default();
        observation.observe(1, &event, |text| {
            vec![crate::attachments::Segment::Prose(text.to_string())]
        });
        let mut work = observation.work().last().cloned()?;
        if let WorkKind::FileChange {
            patch_head,
            patch_truncated,
            ..
        } = &mut work.kind
        {
            *patch_head = entry.text().unwrap_or_default().to_string();
            *patch_truncated = entry.is_clipped();
        }
        if matches!(work.kind, WorkKind::Command { .. })
            && let Some(output) = entry.text()
        {
            work.stdout_head = output.to_string();
            work.output_truncated = entry.is_clipped();
        }
        work.state = match entry.state() {
            Some("awaiting_approval") => WorkState::AwaitingApproval {
                request_id: serde_json::Value::Null,
            },
            Some("running" | "open") => WorkState::Running,
            Some("denied") => WorkState::Denied,
            Some("blocked_unsupported") => WorkState::BlockedUnsupported,
            Some("proposed") => WorkState::Proposed,
            _ => work.state.clone(),
        };
        Some(FeedEntry {
            id: 0,
            seq: 0,
            kind: FeedEntryKind::Work(work),
        })
    }
}
