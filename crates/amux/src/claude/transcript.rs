//! TranscriptTailer - Tails a Claude transcript file and parses entries.
//!
//! This module watches a Claude Code transcript JSONL file and converts
//! transcript entries into structured output events.

use super::structured_log_source::StructuredLogSource;
use super::types::{
    AgentStructuredOutput, AssistantContentBlock, ClaudeStructuredOutput, CompactMetadata,
    MessageUsage, PostToolUse, PreToolUse,
};
use crate::buffer::MultiplexStructuredBuffer;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;

// ============================================================================
// Transcript Entry Types
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum TranscriptEntry {
    #[serde(rename = "user")]
    User {
        message: UserMessageRaw,
        uuid: Option<String>,
        timestamp: Option<String>,
        cwd: Option<String>,
        #[serde(rename = "gitBranch")]
        git_branch: Option<String>,
        #[serde(rename = "parentUuid")]
        parent_uuid: Option<String>,
        #[serde(rename = "promptId")]
        prompt_id: Option<String>,
        #[serde(rename = "permissionMode")]
        permission_mode: Option<String>,
        #[serde(rename = "toolUseResult")]
        tool_use_result: Option<Value>,
        #[allow(dead_code)]
        #[serde(rename = "sourceToolAssistantUUID")]
        source_tool_assistant_uuid: Option<String>,
        slug: Option<String>,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: AssistantMessageRaw,
        uuid: Option<String>,
        timestamp: Option<String>,
        cwd: Option<String>,
        #[serde(rename = "gitBranch")]
        git_branch: Option<String>,
        #[serde(rename = "requestId")]
        request_id: Option<String>,
        slug: Option<String>,
    },
    #[serde(rename = "system")]
    System {
        subtype: Option<String>,
        uuid: Option<String>,
        timestamp: Option<String>,
        slug: Option<String>,
        #[serde(flatten)]
        fields: HashMap<String, Value>,
    },
    #[serde(rename = "queue-operation")]
    QueueOperation {},
    #[serde(rename = "progress")]
    Progress {
        data: Option<Value>,
        #[serde(rename = "uuid")]
        _uuid: Option<String>,
        timestamp: Option<String>,
        #[serde(rename = "toolUseID")]
        tool_use_id: Option<String>,
        #[serde(rename = "parentToolUseID")]
        parent_tool_use_id: Option<String>,
        cwd: Option<String>,
        #[serde(rename = "gitBranch")]
        git_branch: Option<String>,
    },
    #[serde(rename = "agent-name")]
    AgentName {
        #[serde(rename = "agentName")]
        agent_name: String,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
    },
    #[serde(rename = "custom-title")]
    CustomTitle {
        #[serde(rename = "customTitle")]
        custom_title: String,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
    },
    #[serde(rename = "last-prompt")]
    LastPrompt {
        #[serde(rename = "lastPrompt")]
        last_prompt: String,
        #[serde(rename = "sessionId")]
        session_id: Option<String>,
    },
    #[serde(rename = "file-history-snapshot")]
    FileHistorySnapshot {},
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct UserMessageRaw {
    #[serde(deserialize_with = "deserialize_user_content")]
    content: UserContent,
}

#[derive(Debug)]
enum UserContent {
    Text(String),
    ToolResults(Vec<ToolResultBlock>),
}

#[derive(Debug, Clone, Deserialize)]
struct ToolResultBlock {
    #[serde(rename = "tool_use_id", alias = "toolUseID")]
    tool_use_id: String,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    is_error: bool,
}

#[derive(Debug, Deserialize)]
struct AssistantMessageRaw {
    #[serde(default)]
    content: Vec<RawContentBlock>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default, rename = "id")]
    message_id: Option<String>,
    #[serde(default)]
    usage: Option<MessageUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking,
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

fn deserialize_user_content<'de, D>(deserializer: D) -> Result<UserContent, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(text) => Ok(UserContent::Text(text)),
        Value::Array(items) => {
            let mut blocks = Vec::with_capacity(items.len());
            for item in items {
                blocks.push(
                    serde_json::from_value::<ToolResultBlock>(item)
                        .map_err(serde::de::Error::custom)?,
                );
            }
            Ok(UserContent::ToolResults(blocks))
        }
        other => Err(serde::de::Error::custom(format!(
            "unsupported user content shape: {other}"
        ))),
    }
}

/// Common context fields present on most transcript entries.
struct EntryContext {
    timestamp: String,
    cwd: Option<String>,
    git_branch: Option<String>,
    slug: Option<String>,
}

// ============================================================================
// Transcript Parser
// ============================================================================

#[derive(Default)]
struct TranscriptParser {
    pending_tools: HashMap<String, (String, Value)>,
}

impl TranscriptParser {
    fn new() -> Self {
        Self::default()
    }

    fn parse_line(&mut self, line: &str) -> Vec<AgentStructuredOutput> {
        let line = line.trim();
        if line.is_empty() {
            return Vec::new();
        }

        serde_json::from_str::<TranscriptEntry>(line)
            .ok()
            .map(|entry| {
                self.parse_entry(entry)
                    .into_iter()
                    .map(AgentStructuredOutput::Claude)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_value(&mut self, value: Value) -> Vec<ClaudeStructuredOutput> {
        serde_json::from_value::<TranscriptEntry>(value)
            .ok()
            .map(|entry| self.parse_entry(entry))
            .unwrap_or_default()
    }

    fn parse_entry(&mut self, entry: TranscriptEntry) -> Vec<ClaudeStructuredOutput> {
        match entry {
            TranscriptEntry::User {
                message,
                uuid,
                timestamp,
                cwd,
                git_branch,
                parent_uuid,
                prompt_id,
                permission_mode,
                tool_use_result,
                source_tool_assistant_uuid: _,
                slug,
            } => match message.content {
                UserContent::Text(content) => vec![ClaudeStructuredOutput::UserMessage {
                    content,
                    uuid: uuid.unwrap_or_default(),
                    timestamp: timestamp.unwrap_or_default(),
                    cwd,
                    git_branch,
                    parent_uuid,
                    prompt_id,
                    permission_mode,
                    slug,
                }],
                UserContent::ToolResults(results) => {
                    let ctx = EntryContext {
                        timestamp: timestamp.unwrap_or_default(),
                        cwd,
                        git_branch,
                        slug,
                    };
                    results
                        .into_iter()
                        .filter_map(|result| {
                            self.parse_tool_result(
                                result,
                                tool_use_result.clone(),
                                EntryContext {
                                    timestamp: ctx.timestamp.clone(),
                                    cwd: ctx.cwd.clone(),
                                    git_branch: ctx.git_branch.clone(),
                                    slug: ctx.slug.clone(),
                                },
                            )
                        })
                        .collect()
                }
            },
            TranscriptEntry::Assistant {
                message,
                uuid,
                timestamp,
                cwd,
                git_branch,
                request_id,
                slug,
            } => self.parse_assistant(
                message,
                uuid.unwrap_or_default(),
                EntryContext {
                    timestamp: timestamp.unwrap_or_default(),
                    cwd,
                    git_branch,
                    slug,
                },
                request_id,
            ),
            TranscriptEntry::System {
                subtype,
                uuid,
                timestamp,
                slug,
                fields,
            } => self.parse_system(subtype, uuid, timestamp, slug, fields),
            TranscriptEntry::Progress {
                data,
                _uuid: _,
                timestamp,
                tool_use_id,
                parent_tool_use_id,
                cwd,
                git_branch,
            } => self.parse_progress(
                data,
                timestamp,
                tool_use_id,
                parent_tool_use_id,
                cwd,
                git_branch,
            ),
            TranscriptEntry::AgentName {
                agent_name,
                session_id,
            } => vec![ClaudeStructuredOutput::AgentName {
                agent_name,
                session_id,
            }],
            TranscriptEntry::CustomTitle {
                custom_title,
                session_id,
            } => {
                vec![ClaudeStructuredOutput::CustomTitle {
                    custom_title,
                    session_id,
                }]
            }
            TranscriptEntry::LastPrompt {
                last_prompt,
                session_id,
            } => {
                vec![ClaudeStructuredOutput::LastPrompt {
                    last_prompt,
                    session_id,
                }]
            }
            TranscriptEntry::FileHistorySnapshot {} => Vec::new(),
            TranscriptEntry::QueueOperation {} => Vec::new(),
            TranscriptEntry::Other => Vec::new(),
        }
    }

    fn parse_assistant(
        &mut self,
        message: AssistantMessageRaw,
        uuid: String,
        ctx: EntryContext,
        request_id: Option<String>,
    ) -> Vec<ClaudeStructuredOutput> {
        let mut content = Vec::new();
        let mut tool_events = Vec::new();

        for block in message.content {
            match block {
                RawContentBlock::Text { text } => {
                    content.push(AssistantContentBlock::Text { text });
                }
                RawContentBlock::Thinking {
                    thinking,
                    signature,
                } => {
                    content.push(AssistantContentBlock::Thinking {
                        thinking,
                        signature: signature.unwrap_or_default(),
                    });
                }
                RawContentBlock::RedactedThinking => {
                    content.push(AssistantContentBlock::RedactedThinking);
                }
                RawContentBlock::ToolUse { id, name, input } => {
                    self.pending_tools
                        .insert(id.clone(), (name.clone(), input.clone()));
                    if let Some(tool) = build_pre_tool_use(&name, &input) {
                        tool_events.push(ClaudeStructuredOutput::PreToolUseEvent {
                            tool_use_id: id,
                            tool,
                            timestamp: ctx.timestamp.clone(),
                            cwd: ctx.cwd.clone(),
                            git_branch: ctx.git_branch.clone(),
                        });
                    }
                }
                RawContentBlock::Other => content.push(AssistantContentBlock::Other),
            }
        }

        let mut events = vec![ClaudeStructuredOutput::AssistantMessage {
            content,
            uuid,
            timestamp: ctx.timestamp,
            cwd: ctx.cwd,
            git_branch: ctx.git_branch,
            model: message.model,
            stop_reason: message.stop_reason,
            message_id: message.message_id,
            request_id,
            usage: message.usage,
            slug: ctx.slug,
        }];
        events.extend(tool_events);
        events
    }

    fn parse_tool_result(
        &mut self,
        block: ToolResultBlock,
        tool_use_result: Option<Value>,
        ctx: EntryContext,
    ) -> Option<ClaudeStructuredOutput> {
        let tool_result = tool_use_result
            .or(block.content.clone())
            .unwrap_or(Value::Null);
        let pending = self.pending_tools.remove(&block.tool_use_id);

        // Rejection: toolUseResult is a string (e.g. "User rejected tool use").
        // This is distinct from tool execution errors where toolUseResult is
        // a structured object but is_error is true.
        if tool_result.is_string() {
            return Some(ClaudeStructuredOutput::ToolUseRejected {
                tool_use_id: block.tool_use_id,
                error: error_message_from_value(&tool_result),
                timestamp: ctx.timestamp,
                cwd: ctx.cwd,
                git_branch: ctx.git_branch,
                slug: ctx.slug,
            });
        }

        let (tool_name, tool_input) = pending?;
        let tool = build_post_tool_use(&tool_name, &tool_input, &tool_result)?;

        Some(ClaudeStructuredOutput::PostToolUseEvent {
            tool_use_id: block.tool_use_id,
            tool,
            is_error: block.is_error,
            timestamp: ctx.timestamp,
            cwd: ctx.cwd,
            git_branch: ctx.git_branch,
            slug: ctx.slug,
        })
    }

    fn parse_system(
        &mut self,
        subtype: Option<String>,
        uuid: Option<String>,
        timestamp: Option<String>,
        slug: Option<String>,
        fields: HashMap<String, Value>,
    ) -> Vec<ClaudeStructuredOutput> {
        let subtype = subtype.unwrap_or_else(|| "unknown".to_string());
        match subtype.as_str() {
            "turn_duration" => {
                let Some(duration_ms) = fields_u64(&fields, &["duration_ms", "durationMs"]) else {
                    return Vec::new();
                };
                vec![ClaudeStructuredOutput::TurnDuration {
                    duration_ms,
                    message_count: fields_u64(&fields, &["message_count", "messageCount"]),
                    timestamp,
                    uuid,
                    slug,
                }]
            }
            "api_error" => vec![ClaudeStructuredOutput::ApiError {
                error: fields.get("error").cloned().unwrap_or(Value::Null),
                retry_in_ms: fields_f64(&fields, &["retry_in_ms", "retryInMs"]),
                retry_attempt: fields_u64(&fields, &["retry_attempt", "retryAttempt"]),
                max_retries: fields_u64(&fields, &["max_retries", "maxRetries"]),
                timestamp,
                uuid,
                slug,
            }],
            "compact_boundary" => vec![ClaudeStructuredOutput::CompactBoundary {
                compact_metadata: fields
                    .get("compact_metadata")
                    .or_else(|| fields.get("compactMetadata"))
                    .cloned()
                    .and_then(|value| serde_json::from_value::<CompactMetadata>(value).ok()),
                timestamp,
                uuid,
                slug,
            }],
            "local_command" => vec![ClaudeStructuredOutput::LocalCommand {
                content: fields
                    .get("content")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                timestamp,
                uuid,
                slug,
            }],
            // Internal hook machinery — not meaningful as structured output
            "stop_hook_summary" => Vec::new(),
            _ => vec![ClaudeStructuredOutput::SystemEvent {
                subtype,
                payload: fields,
                timestamp,
                uuid,
                slug,
            }],
        }
    }

    fn parse_progress(
        &mut self,
        data: Option<Value>,
        timestamp: Option<String>,
        tool_use_id: Option<String>,
        parent_tool_use_id: Option<String>,
        cwd: Option<String>,
        git_branch: Option<String>,
    ) -> Vec<ClaudeStructuredOutput> {
        let Some(data) = data else {
            return Vec::new();
        };
        let Some(data_type) = value_str(&data, &["type"]) else {
            return vec![ClaudeStructuredOutput::ProgressEvent {
                data,
                tool_use_id,
                parent_tool_use_id,
                timestamp,
                cwd,
                git_branch,
            }];
        };

        match data_type.as_str() {
            "hook_progress" => Vec::new(),
            "waiting_for_task" => vec![ClaudeStructuredOutput::WaitingForTask {
                task_description: value_str(&data, &["task_description", "taskDescription"]),
                task_type: value_str(&data, &["task_type", "taskType"]),
                tool_use_id: tool_use_id
                    .or_else(|| value_str(&data, &["toolUseID", "tool_use_id"])),
                timestamp,
            }],
            "agent_progress" => vec![ClaudeStructuredOutput::AgentProgress {
                agent_id: value_str(&data, &["agent_id", "agentId"]).unwrap_or_default(),
                prompt: value_str(&data, &["prompt"]),
                events: self.parse_progress_message(
                    &data,
                    timestamp.clone(),
                    cwd.clone(),
                    git_branch.clone(),
                ),
                parent_tool_use_id: parent_tool_use_id
                    .or_else(|| value_str(&data, &["parentToolUseID", "parent_tool_use_id"])),
                timestamp,
                cwd,
                git_branch,
            }],
            _ => vec![ClaudeStructuredOutput::ProgressEvent {
                data,
                tool_use_id,
                parent_tool_use_id,
                timestamp,
                cwd,
                git_branch,
            }],
        }
    }

    fn parse_progress_message(
        &mut self,
        data: &Value,
        timestamp: Option<String>,
        cwd: Option<String>,
        git_branch: Option<String>,
    ) -> Vec<ClaudeStructuredOutput> {
        if let Some(message) = data.get("message") {
            if message.get("type").is_some() {
                return self.parse_value(message.clone());
            }
            if let Some(role) = message.get("role").and_then(Value::as_str) {
                let mut entry = Map::new();
                entry.insert("type".to_string(), Value::String(role.to_string()));
                entry.insert("message".to_string(), message.clone());
                if let Some(uuid) = data.get("uuid").cloned() {
                    entry.insert("uuid".to_string(), uuid);
                }
                if let Some(timestamp) = timestamp {
                    entry.insert("timestamp".to_string(), Value::String(timestamp));
                }
                if let Some(cwd) = cwd {
                    entry.insert("cwd".to_string(), Value::String(cwd));
                }
                if let Some(git_branch) = git_branch {
                    entry.insert("gitBranch".to_string(), Value::String(git_branch));
                }
                if let Some(request_id) = data.get("requestId").cloned() {
                    entry.insert("requestId".to_string(), request_id);
                }
                return self.parse_value(Value::Object(entry));
            }
        }

        if let Some(role) = data.get("role").and_then(Value::as_str) {
            let mut entry = Map::new();
            entry.insert("type".to_string(), Value::String(role.to_string()));
            entry.insert("message".to_string(), data.clone());
            if let Some(timestamp) = timestamp {
                entry.insert("timestamp".to_string(), Value::String(timestamp));
            }
            if let Some(cwd) = cwd {
                entry.insert("cwd".to_string(), Value::String(cwd));
            }
            if let Some(git_branch) = git_branch {
                entry.insert("gitBranch".to_string(), Value::String(git_branch));
            }
            return self.parse_value(Value::Object(entry));
        }

        Vec::new()
    }
}

fn build_pre_tool_use(tool_name: &str, tool_input: &Value) -> Option<PreToolUse> {
    serde_json::from_value(json!({
        "tool_name": tool_name,
        "tool_input": tool_input,
    }))
    .ok()
}

fn build_post_tool_use(
    tool_name: &str,
    tool_input: &Value,
    tool_result: &Value,
) -> Option<PostToolUse> {
    serde_json::from_value(json!({
        "tool_name": tool_name,
        "tool_input": tool_input,
        "tool_response": tool_result,
    }))
    .ok()
}

fn error_message_from_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other if !other.is_null() => other.to_string(),
        _ => "Tool use rejected".to_string(),
    }
}

fn fields_u64(fields: &HashMap<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| fields.get(*name))
        .and_then(Value::as_u64)
}

fn fields_f64(fields: &HashMap<String, Value>, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| fields.get(*name))
        .and_then(Value::as_f64)
}

fn value_str(value: &Value, names: &[&str]) -> Option<String> {
    let Value::Object(object) = value else {
        return None;
    };
    names
        .iter()
        .find_map(|name| object.get(*name))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

// ============================================================================
// TranscriptTailer
// ============================================================================

/// Tails a Claude transcript file and writes parsed entries to a buffer.
pub struct TranscriptTailer {
    path: PathBuf,
    buffer: Arc<MultiplexStructuredBuffer>,
    shutdown_tx: watch::Sender<bool>,
    log_source: StructuredLogSource,
}

impl TranscriptTailer {
    /// Create a new TranscriptTailer for the given transcript path.
    pub fn new(
        path: PathBuf,
        buffer: Arc<MultiplexStructuredBuffer>,
        log_source: StructuredLogSource,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            path,
            buffer,
            shutdown_tx,
            log_source,
        }
    }

    /// Start tailing the transcript file in a background task.
    pub fn start(&self) -> JoinHandle<()> {
        let path = self.path.clone();
        let buffer = self.buffer.clone();
        let log_source = self.log_source.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            if let Err(e) =
                tail_transcript(path, buffer, log_source.clone(), &mut shutdown_rx).await
            {
                log_source.mark_failed(std::io::Error::new(e.kind(), e.to_string()));
                tracing::warn!(error = %e, "transcript tailer error");
            }
        })
    }

    /// Signal the tailer to stop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

async fn tail_transcript(
    path: PathBuf,
    buffer: Arc<MultiplexStructuredBuffer>,
    log_source: StructuredLogSource,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> std::io::Result<()> {
    while !path.exists() {
        if *shutdown_rx.borrow() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let file = File::open(&path).await?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut parser = TranscriptParser::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }

        for entry in parser.parse_line(&line) {
            buffer.write(entry).await;
        }
    }

    log_source.mark_linked().await;

    loop {
        if *shutdown_rx.borrow() {
            return Ok(());
        }

        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;

        if bytes_read == 0 {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    let current_pos = reader.stream_position().await?;
                    let metadata = tokio::fs::metadata(&path).await?;
                    if metadata.len() < current_pos {
                        reader.seek(std::io::SeekFrom::Start(0)).await?;
                    }
                }
            }
        } else {
            for entry in parser.parse_line(&line) {
                buffer.write(entry).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_claude(parser: &mut TranscriptParser, line: &str) -> Vec<ClaudeStructuredOutput> {
        parser
            .parse_line(line)
            .into_iter()
            .map(|entry| {
                let AgentStructuredOutput::Claude(inner) = entry;
                inner
            })
            .collect()
    }

    #[test]
    fn parse_text_user_message_with_context_fields() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"user","message":{"role":"user","content":"Hello world"},"uuid":"abc123","timestamp":"2025-01-15T12:00:00Z","cwd":"/tmp/project","gitBranch":"main","parentUuid":"parent-1","promptId":"prompt-1","permissionMode":"bypassPermissions"}"#;

        assert_eq!(
            parse_claude(&mut parser, line),
            vec![ClaudeStructuredOutput::UserMessage {
                content: "Hello world".to_string(),
                uuid: "abc123".to_string(),
                timestamp: "2025-01-15T12:00:00Z".to_string(),
                cwd: Some("/tmp/project".to_string()),
                git_branch: Some("main".to_string()),
                parent_uuid: Some("parent-1".to_string()),
                prompt_id: Some("prompt-1".to_string()),
                permission_mode: Some("bypassPermissions".to_string()),
                slug: None,
            }]
        );
    }

    #[test]
    fn parse_assistant_message_with_tool_use_and_thinking() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"assistant","message":{"role":"assistant","id":"msg_123","model":"claude-sonnet-4-6","stop_reason":"tool_use","usage":{"input_tokens":10,"output_tokens":20},"content":[{"type":"thinking","thinking":"","signature":"sig_1"},{"type":"text","text":"I'll read the file."},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"README.md"}}]},"uuid":"def456","timestamp":"2025-01-15T12:00:01Z","cwd":"/tmp/project","gitBranch":"feature/parser","requestId":"req_123"}"#;

        let events = parse_claude(&mut parser, line);
        assert_eq!(events.len(), 2);

        match &events[0] {
            ClaudeStructuredOutput::AssistantMessage {
                content,
                uuid,
                timestamp,
                cwd,
                git_branch,
                model,
                stop_reason,
                message_id,
                request_id,
                usage,
                slug,
            } => {
                assert_eq!(uuid, "def456");
                assert_eq!(timestamp, "2025-01-15T12:00:01Z");
                assert_eq!(cwd.as_deref(), Some("/tmp/project"));
                assert_eq!(git_branch.as_deref(), Some("feature/parser"));
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
                assert_eq!(stop_reason.as_deref(), Some("tool_use"));
                assert_eq!(message_id.as_deref(), Some("msg_123"));
                assert_eq!(request_id.as_deref(), Some("req_123"));
                assert_eq!(
                    *content,
                    vec![
                        AssistantContentBlock::Thinking {
                            thinking: String::new(),
                            signature: "sig_1".to_string(),
                        },
                        AssistantContentBlock::Text {
                            text: "I'll read the file.".to_string(),
                        },
                    ]
                );
                assert_eq!(
                    usage.as_ref().and_then(|usage| usage.output_tokens),
                    Some(20)
                );
                assert_eq!(*slug, None);
            }
            other => panic!("expected AssistantMessage, got {other:?}"),
        }

        match &events[1] {
            ClaudeStructuredOutput::PreToolUseEvent {
                tool_use_id,
                tool,
                timestamp,
                cwd,
                git_branch,
            } => {
                assert_eq!(tool_use_id, "toolu_1");
                assert_eq!(timestamp, "2025-01-15T12:00:01Z");
                assert_eq!(cwd.as_deref(), Some("/tmp/project"));
                assert_eq!(git_branch.as_deref(), Some("feature/parser"));
                assert!(matches!(tool, PreToolUse::Read { .. }));
            }
            other => panic!("expected PreToolUseEvent, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_result_success_from_user_message() {
        let mut parser = TranscriptParser::new();
        let pre = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_read","name":"Read","input":{"file_path":"README.md"}}]},"uuid":"a1","timestamp":"2025-01-15T12:00:01Z","cwd":"/tmp/project","gitBranch":"main"}"#;
        let post = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_read","content":{"filePath":"README.md","content":"hello\n","numLines":1,"startLine":1,"totalLines":1},"is_error":false}]},"toolUseResult":{"type":"text","file":{"filePath":"README.md","content":"hello\n","numLines":1,"startLine":1,"totalLines":1}},"uuid":"u2","timestamp":"2025-01-15T12:00:02Z","cwd":"/tmp/project","gitBranch":"main"}"#;

        parse_claude(&mut parser, pre);
        let events = parse_claude(&mut parser, post);

        assert_eq!(events.len(), 1);
        match &events[0] {
            ClaudeStructuredOutput::PostToolUseEvent {
                tool_use_id,
                tool,
                is_error,
                timestamp,
                cwd,
                git_branch,
                slug,
            } => {
                assert_eq!(tool_use_id, "toolu_read");
                assert!(!is_error);
                assert_eq!(timestamp, "2025-01-15T12:00:02Z");
                assert_eq!(cwd.as_deref(), Some("/tmp/project"));
                assert_eq!(git_branch.as_deref(), Some("main"));
                assert_eq!(*slug, None);
                assert!(matches!(tool, PostToolUse::Read { .. }));
            }
            other => panic!("expected PostToolUseEvent, got {other:?}"),
        }
    }

    #[test]
    fn parse_tool_rejection_from_user_message() {
        let mut parser = TranscriptParser::new();
        let pre = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_edit","name":"Edit","input":{"file_path":"README.md","old_string":"hello","new_string":"bye"}}]},"uuid":"a1","timestamp":"2025-01-15T12:00:01Z"}"#;
        let rejected = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_edit","is_error":true,"content":"User rejected tool use"}]},"toolUseResult":"User rejected tool use","uuid":"u3","timestamp":"2025-01-15T12:00:03Z","cwd":"/tmp/project","gitBranch":"main"}"#;

        parse_claude(&mut parser, pre);
        let events = parse_claude(&mut parser, rejected);

        assert_eq!(
            events,
            vec![ClaudeStructuredOutput::ToolUseRejected {
                tool_use_id: "toolu_edit".to_string(),
                error: "User rejected tool use".to_string(),
                timestamp: "2025-01-15T12:00:03Z".to_string(),
                cwd: Some("/tmp/project".to_string()),
                git_branch: Some("main".to_string()),
                slug: None,
            }]
        );
    }

    #[test]
    fn parse_turn_duration_system_event() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"system","subtype":"turn_duration","duration_ms":1530,"message_count":7,"uuid":"sys1","timestamp":"2025-01-15T12:00:04Z"}"#;

        assert_eq!(
            parse_claude(&mut parser, line),
            vec![ClaudeStructuredOutput::TurnDuration {
                duration_ms: 1530,
                message_count: Some(7),
                timestamp: Some("2025-01-15T12:00:04Z".to_string()),
                uuid: Some("sys1".to_string()),
                slug: None,
            }]
        );
    }

    #[test]
    fn parse_api_error_system_event() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"system","subtype":"api_error","error":{"message":"rate limited"},"retry_in_ms":250.0,"retry_attempt":2,"max_retries":5,"uuid":"sys2","timestamp":"2025-01-15T12:00:05Z"}"#;

        assert_eq!(
            parse_claude(&mut parser, line),
            vec![ClaudeStructuredOutput::ApiError {
                error: json!({"message":"rate limited"}),
                retry_in_ms: Some(250.0),
                retry_attempt: Some(2),
                max_retries: Some(5),
                timestamp: Some("2025-01-15T12:00:05Z".to_string()),
                uuid: Some("sys2".to_string()),
                slug: None,
            }]
        );
    }

    #[test]
    fn parse_agent_progress_with_nested_events() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"progress","timestamp":"2025-01-15T12:00:06Z","cwd":"/tmp/project","gitBranch":"main","parentToolUseID":"toolu_parent","data":{"type":"agent_progress","agentId":"agent_1","prompt":"find rust files","message":{"role":"assistant","id":"msg_sub","model":"claude-haiku","stop_reason":"tool_use","content":[{"type":"text","text":"Searching now."},{"type":"tool_use","id":"toolu_sub_read","name":"Glob","input":{"pattern":"**/*.rs"}}]}}}"#;

        let events = parse_claude(&mut parser, line);
        assert_eq!(events.len(), 1);

        match &events[0] {
            ClaudeStructuredOutput::AgentProgress {
                agent_id,
                prompt,
                events,
                parent_tool_use_id,
                timestamp,
                cwd,
                git_branch,
            } => {
                assert_eq!(agent_id, "agent_1");
                assert_eq!(prompt.as_deref(), Some("find rust files"));
                assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_parent"));
                assert_eq!(timestamp.as_deref(), Some("2025-01-15T12:00:06Z"));
                assert_eq!(cwd.as_deref(), Some("/tmp/project"));
                assert_eq!(git_branch.as_deref(), Some("main"));
                assert_eq!(events.len(), 2);
                assert!(matches!(
                    &events[0],
                    ClaudeStructuredOutput::AssistantMessage { .. }
                ));
                assert!(matches!(
                    &events[1],
                    ClaudeStructuredOutput::PreToolUseEvent { .. }
                ));
            }
            other => panic!("expected AgentProgress, got {other:?}"),
        }
    }

    #[test]
    fn parse_session_metadata_entries() {
        let mut parser = TranscriptParser::new();

        assert_eq!(
            parse_claude(
                &mut parser,
                r#"{"type":"agent-name","agentName":"merry-forging-lantern","sessionId":"sess_1"}"#
            ),
            vec![ClaudeStructuredOutput::AgentName {
                agent_name: "merry-forging-lantern".to_string(),
                session_id: Some("sess_1".to_string()),
            }]
        );
        assert_eq!(
            parse_claude(
                &mut parser,
                r#"{"type":"custom-title","customTitle":"Transcript Parser Work","sessionId":"sess_1"}"#
            ),
            vec![ClaudeStructuredOutput::CustomTitle {
                custom_title: "Transcript Parser Work".to_string(),
                session_id: Some("sess_1".to_string()),
            }]
        );
        assert_eq!(
            parse_claude(
                &mut parser,
                r#"{"type":"last-prompt","lastPrompt":"finish the parser","sessionId":"sess_1"}"#
            ),
            vec![ClaudeStructuredOutput::LastPrompt {
                last_prompt: "finish the parser".to_string(),
                session_id: Some("sess_1".to_string()),
            }]
        );
    }

    #[test]
    fn parse_stop_hook_summary_is_skipped() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"system","subtype":"stop_hook_summary","hookCount":2,"hookInfos":[{"command":"amux hooks claude stop"}],"hookErrors":[],"preventedContinuation":false,"stopReason":"","hasOutput":false,"timestamp":"2025-01-15T12:00:10Z","uuid":"shook1"}"#;
        assert!(parse_claude(&mut parser, line).is_empty());
    }

    #[test]
    fn parse_local_command() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"system","subtype":"local_command","content":"<command-name>/rename</command-name>","timestamp":"2025-01-15T12:00:11Z","uuid":"lc1"}"#;
        assert_eq!(
            parse_claude(&mut parser, line),
            vec![ClaudeStructuredOutput::LocalCommand {
                content: Some("<command-name>/rename</command-name>".to_string()),
                timestamp: Some("2025-01-15T12:00:11Z".to_string()),
                uuid: Some("lc1".to_string()),
                slug: None,
            }]
        );
    }

    #[test]
    fn parse_slug_propagated_on_user_and_assistant() {
        let mut parser = TranscriptParser::new();

        let user = r#"{"type":"user","message":{"role":"user","content":"hello"},"uuid":"u1","timestamp":"2025-01-15T12:00:00Z","slug":"merry-forging-lantern"}"#;
        let events = parse_claude(&mut parser, user);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ClaudeStructuredOutput::UserMessage { slug, .. } => {
                assert_eq!(slug.as_deref(), Some("merry-forging-lantern"));
            }
            other => panic!("expected UserMessage, got {other:?}"),
        }

        let assistant = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]},"uuid":"a1","timestamp":"2025-01-15T12:00:01Z","slug":"merry-forging-lantern"}"#;
        let events = parse_claude(&mut parser, assistant);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ClaudeStructuredOutput::AssistantMessage { slug, .. } => {
                assert_eq!(slug.as_deref(), Some("merry-forging-lantern"));
            }
            other => panic!("expected AssistantMessage, got {other:?}"),
        }
    }

    #[test]
    fn parse_slug_propagated_on_system_events() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"system","subtype":"turn_duration","duration_ms":500,"uuid":"sys1","timestamp":"2025-01-15T12:00:04Z","slug":"merry-forging-lantern"}"#;
        let events = parse_claude(&mut parser, line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ClaudeStructuredOutput::TurnDuration { slug, .. } => {
                assert_eq!(slug.as_deref(), Some("merry-forging-lantern"));
            }
            other => panic!("expected TurnDuration, got {other:?}"),
        }
    }

    #[test]
    fn parse_queue_operation_is_skipped() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2025-01-15T12:00:00Z","sessionId":"sess_1","content":"hello"}"#;
        assert!(parse_claude(&mut parser, line).is_empty());
    }

    #[test]
    fn parse_file_history_snapshot_is_skipped() {
        let mut parser = TranscriptParser::new();
        let line = r#"{"type":"file-history-snapshot","messageId":"abc","snapshot":{"messageId":"abc","trackedFileBackups":{},"timestamp":"2025-01-15T12:00:00Z"},"isSnapshotUpdate":false}"#;
        assert!(parse_claude(&mut parser, line).is_empty());
    }

    #[test]
    fn parse_invalid_or_empty_lines_as_no_events() {
        let mut parser = TranscriptParser::new();
        assert!(parse_claude(&mut parser, "").is_empty());
        assert!(parse_claude(&mut parser, "   ").is_empty());
        assert!(parse_claude(&mut parser, "not json").is_empty());
    }
}
