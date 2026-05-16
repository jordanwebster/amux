//! TranscriptTailer - Tails a Claude transcript file and passes through raw JSON.
//!
//! This module watches a Claude Code transcript JSONL file and writes each
//! line as an opaque `serde_json::Value` to the structured output buffer.
//! amux does not interpret transcript semantics — that is the client's job.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::agents::MultiplexStructuredBuffer;

// ============================================================================
// TranscriptTailer
// ============================================================================

/// Tails a Claude transcript file and writes raw JSON entries to a buffer.
pub(in crate::agents) struct TranscriptTailer {
    path: PathBuf,
    buffer: Arc<MultiplexStructuredBuffer>,
    shutdown_tx: watch::Sender<bool>,
}

impl TranscriptTailer {
    /// Create a new TranscriptTailer for the given transcript path.
    pub(in crate::agents) fn new(path: PathBuf, buffer: Arc<MultiplexStructuredBuffer>) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            path,
            buffer,
            shutdown_tx,
        }
    }

    /// Start tailing the transcript file in a background task.
    pub(in crate::agents) fn start(&self) -> JoinHandle<()> {
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
    pub(in crate::agents) fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

async fn tail_transcript(
    path: PathBuf,
    buffer: Arc<MultiplexStructuredBuffer>,
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

    // Catchup: read existing content
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }

        let trimmed = line.trim();
        if !trimmed.is_empty()
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        {
            buffer.write(value).await;
        }
    }

    // Catchup→live transition: emit a synthetic marker so subscribers waiting
    // for the transcript to reach a known state (e.g. fork coordination) can
    // know the existing content has been fully drained. The marker lives in the
    // broadcast buffer like any other entry, so new subscribers see it in their
    // replay in position, and a relink-driven clear just emits another one.
    buffer
        .write(serde_json::json!({ "type": "amux.transcript_ready" }))
        .await;

    // Live tail
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
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
            {
                buffer.write(value).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::MultiplexStructuredBuffer;

    fn parse_line(line: &str) -> Vec<serde_json::Value> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return vec![];
        }
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => vec![value],
            Err(_) => vec![],
        }
    }

    #[test]
    fn passthrough_user_message() {
        let line = r#"{"type":"user","message":{"content":"hello"},"uuid":"u1","timestamp":"2025-01-15T12:00:00Z"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "user");
        assert_eq!(events[0]["uuid"], "u1");
        assert_eq!(events[0]["message"]["content"], "hello");
    }

    #[test]
    fn passthrough_assistant_message_with_tool_use() {
        let line = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"reading"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"a.rs"}}]},"uuid":"a1","timestamp":"2025-01-15T12:00:01Z"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "assistant");
        assert_eq!(events[0]["message"]["content"][1]["name"], "Read");
    }

    #[test]
    fn passthrough_system_event() {
        let line = r#"{"type":"system","subtype":"turn_duration","duration_ms":1530}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["subtype"], "turn_duration");
        assert_eq!(events[0]["duration_ms"], 1530);
    }

    #[test]
    fn passthrough_progress_event() {
        let line = r#"{"type":"progress","data":{"type":"agent_progress","agentId":"a1"},"timestamp":"2025-01-15T12:00:06Z"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "progress");
        assert_eq!(events[0]["data"]["agentId"], "a1");
    }

    #[test]
    fn passthrough_agent_name() {
        let line =
            r#"{"type":"agent-name","agentName":"merry-forging-lantern","sessionId":"sess_1"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "agent-name");
        assert_eq!(events[0]["agentName"], "merry-forging-lantern");
    }

    #[test]
    fn empty_and_invalid_lines_skipped() {
        assert!(parse_line("").is_empty());
        assert!(parse_line("   ").is_empty());
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn passthrough_preserves_all_fields() {
        let line = r#"{"type":"user","message":{"content":"hello"},"uuid":"u1","timestamp":"2025-01-15T12:00:00Z","cwd":"/tmp","gitBranch":"main","slug":"my-slug"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["cwd"], "/tmp");
        assert_eq!(events[0]["gitBranch"], "main");
        assert_eq!(events[0]["slug"], "my-slug");
    }

    #[tokio::test]
    async fn tailer_writes_lines_to_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        tokio::fs::write(
            &path,
            concat!(
                "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"t1\"}\n",
                "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"uuid\":\"a1\",\"timestamp\":\"t2\"}\n",
            ),
        )
        .await
        .unwrap();

        let buffer = Arc::new(MultiplexStructuredBuffer::new(100));
        let tailer = TranscriptTailer::new(path, buffer.clone());
        let handle = tailer.start();

        // Two transcript lines + one trailing transcript_ready marker.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if buffer.current_seq().await == 3 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (mut reader, seq) = buffer.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 3);

        let first = reader.read().await.unwrap();
        assert_eq!(first.payload["type"], "user");
        let second = reader.read().await.unwrap();
        assert_eq!(second.payload["type"], "assistant");
        let third = reader.read().await.unwrap();
        assert_eq!(third.payload["type"], "amux.transcript_ready");

        tailer.stop();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn tailer_emits_transcript_ready_for_empty_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        tokio::fs::write(&path, "").await.unwrap();

        let buffer = Arc::new(MultiplexStructuredBuffer::new(100));
        let tailer = TranscriptTailer::new(path, buffer.clone());
        let handle = tailer.start();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if buffer.current_seq().await == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (mut reader, seq) = buffer.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 1);
        let entry = reader.read().await.unwrap();
        assert_eq!(entry.payload["type"], "amux.transcript_ready");
        assert_eq!(entry.seq, 1);

        tailer.stop();
        let _ = handle.await;
    }
}
