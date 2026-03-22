//! TranscriptTailer - Tails a Claude transcript file and parses entries.
//!
//! This module watches a Claude Code transcript JSONL file and parses
//! user/assistant messages into StructuredOutput entries.

use super::types::{AgentStructuredOutput, ClaudeStructuredOutput};
use crate::buffer::SequencedStructuredBuffer;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;

// ============================================================================
// Transcript Entry Types (serde-based parsing)
// ============================================================================

/// A single entry in the Claude transcript JSONL file.
/// Uses serde's internally tagged enum to dispatch on the "type" field.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum TranscriptEntry {
    #[serde(rename = "user")]
    User {
        message: UserMessage,
        uuid: Option<String>,
        timestamp: Option<String>,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: AssistantMessage,
        uuid: Option<String>,
        timestamp: Option<String>,
    },
    /// Catch-all for other entry types (summary, file-history-snapshot, etc.)
    #[serde(other)]
    Other,
}

/// User message content - simple string.
#[derive(Debug, Deserialize)]
struct UserMessage {
    content: String,
}

/// Assistant message content - array of content blocks.
#[derive(Debug, Deserialize)]
struct AssistantMessage {
    content: Vec<ContentBlock>,
}

/// A single content block within an assistant message.
/// Uses serde's internally tagged enum to dispatch on the "type" field.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    /// Catch-all for other content types (thinking, tool_use, tool_result, etc.)
    #[serde(other)]
    Other,
}

impl TranscriptEntry {
    /// Convert transcript entry to StructuredOutput if applicable.
    fn into_structured_output(self) -> Option<AgentStructuredOutput> {
        match self {
            TranscriptEntry::User {
                message,
                uuid,
                timestamp,
            } => Some(AgentStructuredOutput::Claude(
                ClaudeStructuredOutput::UserMessage {
                    content: message.content,
                    timestamp: timestamp.unwrap_or_default(),
                    uuid: uuid.unwrap_or_default(),
                },
            )),
            TranscriptEntry::Assistant {
                message,
                uuid,
                timestamp,
            } => {
                let text = message.extract_text()?;
                Some(AgentStructuredOutput::Claude(
                    ClaudeStructuredOutput::AssistantMessage {
                        content: text,
                        timestamp: timestamp.unwrap_or_default(),
                        uuid: uuid.unwrap_or_default(),
                    },
                ))
            }
            TranscriptEntry::Other => None,
        }
    }
}

impl AssistantMessage {
    /// Extract all text content from the message, joining multiple text blocks.
    fn extract_text(&self) -> Option<String> {
        let texts: Vec<&str> = self
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect();

        if texts.is_empty() {
            None
        } else {
            Some(texts.join(""))
        }
    }
}

// ============================================================================
// TranscriptTailer
// ============================================================================

/// Tails a Claude transcript file and writes parsed entries to a buffer.
pub struct TranscriptTailer {
    path: PathBuf,
    buffer: Arc<SequencedStructuredBuffer>,
    shutdown_tx: watch::Sender<bool>,
}

impl TranscriptTailer {
    /// Create a new TranscriptTailer for the given transcript path.
    pub fn new(path: PathBuf, buffer: Arc<SequencedStructuredBuffer>) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            path,
            buffer,
            shutdown_tx,
        }
    }

    /// Start tailing the transcript file in a background task.
    ///
    /// Returns a JoinHandle for the tailing task.
    pub fn start(&self) -> JoinHandle<()> {
        let path = self.path.clone();
        let buffer = self.buffer.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            if let Err(e) = tail_transcript(path, buffer, &mut shutdown_rx).await {
                tracing::warn!(error = %e, "transcript tailer error");
            }
        })
    }

    /// Signal the tailer to stop.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

/// Internal function to tail the transcript file.
async fn tail_transcript(
    path: PathBuf,
    buffer: Arc<SequencedStructuredBuffer>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> std::io::Result<()> {
    // Wait for file to exist
    while !path.exists() {
        if *shutdown_rx.borrow() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let file = File::open(&path).await?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();

    // Read existing content first (replay)
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break; // End of existing content
        }

        if let Some(entry) = parse_transcript_line(&line) {
            buffer.write(entry).await;
        }
    }

    // Now tail for new content
    loop {
        if *shutdown_rx.borrow() {
            return Ok(());
        }

        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;

        if bytes_read == 0 {
            // No new data, wait and retry
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    return Ok(());
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    // Re-check file position in case it was truncated
                    let current_pos = reader.stream_position().await?;
                    let metadata = tokio::fs::metadata(&path).await?;
                    if metadata.len() < current_pos {
                        // File was truncated, seek to beginning
                        reader.seek(std::io::SeekFrom::Start(0)).await?;
                    }
                }
            }
        } else if let Some(entry) = parse_transcript_line(&line) {
            buffer.write(entry).await;
        }
    }
}

/// Parse a single line from the transcript JSONL file.
fn parse_transcript_line(line: &str) -> Option<AgentStructuredOutput> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    serde_json::from_str::<TranscriptEntry>(line)
        .ok()
        .and_then(TranscriptEntry::into_structured_output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_message() {
        let line = r#"{"type":"user","message":{"role":"user","content":"Hello world"},"uuid":"abc123","timestamp":"2025-01-15T12:00:00Z"}"#;
        let entry = parse_transcript_line(line).unwrap();

        let AgentStructuredOutput::Claude(inner) = entry;
        match inner {
            ClaudeStructuredOutput::UserMessage {
                content,
                timestamp,
                uuid,
            } => {
                assert_eq!(content, "Hello world");
                assert_eq!(timestamp, "2025-01-15T12:00:00Z");
                assert_eq!(uuid, "abc123");
            }
            _ => panic!("Expected UserMessage"),
        }
    }

    #[test]
    fn test_parse_assistant_message() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi there!"}]},"uuid":"def456","timestamp":"2025-01-15T12:00:01Z"}"#;
        let entry = parse_transcript_line(line).unwrap();

        let AgentStructuredOutput::Claude(inner) = entry;
        match inner {
            ClaudeStructuredOutput::AssistantMessage {
                content,
                timestamp,
                uuid,
            } => {
                assert_eq!(content, "Hi there!");
                assert_eq!(timestamp, "2025-01-15T12:00:01Z");
                assert_eq!(uuid, "def456");
            }
            _ => panic!("Expected AssistantMessage"),
        }
    }

    #[test]
    fn test_parse_assistant_with_thinking() {
        // Assistant messages with thinking blocks should only extract text
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think..."},{"type":"text","text":"Here is my answer"}]},"uuid":"ghi789","timestamp":"2025-01-15T12:00:02Z"}"#;
        let entry = parse_transcript_line(line).unwrap();

        let AgentStructuredOutput::Claude(inner) = entry;
        match inner {
            ClaudeStructuredOutput::AssistantMessage { content, .. } => {
                assert_eq!(content, "Here is my answer");
            }
            _ => panic!("Expected AssistantMessage"),
        }
    }

    #[test]
    fn test_parse_summary_ignored() {
        let line = r#"{"type":"summary","summary":"Test summary","leafUuid":"abc"}"#;
        assert!(parse_transcript_line(line).is_none());
    }

    #[test]
    fn test_parse_empty_line() {
        assert!(parse_transcript_line("").is_none());
        assert!(parse_transcript_line("   ").is_none());
    }

    #[test]
    fn test_parse_invalid_json() {
        assert!(parse_transcript_line("not json").is_none());
    }
}
