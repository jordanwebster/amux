//! TranscriptTailer - Tails a Claude transcript file and passes through raw JSON.
//!
//! This module watches a Claude Code transcript JSONL file and writes each
//! line as an opaque `serde_json::Value` to the structured output buffer.
//! amux does not interpret transcript semantics — that is the client's job.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::agents::StructuredLogSource;
use crate::agents::claude::ClaudeVersionCache;

// ============================================================================
// TranscriptTailer
// ============================================================================

/// Tails a Claude transcript file and writes raw JSON entries to a buffer.
pub(super) struct TranscriptTailer {
    path: PathBuf,
    source: StructuredLogSource,
    claude_version_cache: ClaudeVersionCache,
    delivery_ready: Option<Arc<AtomicBool>>,
    shutdown_tx: watch::Sender<bool>,
}

impl TranscriptTailer {
    /// Create a new TranscriptTailer for the given transcript path.
    pub(super) fn new(
        path: PathBuf,
        source: StructuredLogSource,
        claude_version_cache: ClaudeVersionCache,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            path,
            source,
            claude_version_cache,
            delivery_ready: None,
            shutdown_tx,
        }
    }

    /// Create a tailer that marks a managed session ready once its transcript
    /// has reached the live-tail boundary.
    pub(super) fn with_delivery_ready(
        path: PathBuf,
        source: StructuredLogSource,
        delivery_ready: Arc<AtomicBool>,
        claude_version_cache: ClaudeVersionCache,
    ) -> Self {
        let mut tailer = Self::new(path, source, claude_version_cache);
        tailer.delivery_ready = Some(delivery_ready);
        tailer
    }

    /// Start tailing the transcript file in a background task.
    pub(super) fn start(&self) -> JoinHandle<()> {
        let path = self.path.clone();
        let source = self.source.clone();
        let claude_version_cache = self.claude_version_cache.clone();
        let delivery_ready = self.delivery_ready.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            if let Err(e) = tail_transcript(
                path,
                source,
                delivery_ready,
                claude_version_cache,
                &mut shutdown_rx,
            )
            .await
            {
                tracing::warn!(error = %e, "transcript tailer error");
            }
        })
    }

    /// Signal the tailer to stop.
    pub(super) fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

async fn tail_transcript(
    path: PathBuf,
    source: StructuredLogSource,
    delivery_ready: Option<Arc<AtomicBool>>,
    claude_version_cache: ClaudeVersionCache,
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
            claude_version_cache.observe_transcript_row(&value);
            source.write(value).await;
        }
    }

    // Catchup→live transition: emit a synthetic marker so subscribers waiting
    // for the transcript to reach a known state (e.g. fork coordination) can
    // know the existing content has been fully drained. The marker lives in the
    // broadcast buffer like any other entry, so new subscribers see it in their
    // replay in position, and a relink-driven clear just emits another one.
    source
        .write(serde_json::json!({ "type": "amux.transcript_ready" }))
        .await;
    if let Some(delivery_ready) = delivery_ready {
        delivery_ready.store(true, Ordering::Release);
    }

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
                claude_version_cache.observe_transcript_row(&value);
                source.write(value).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::StructuredLogSource;

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

        let source = StructuredLogSource::new(100);
        let tailer = TranscriptTailer::new(path, source.clone(), ClaudeVersionCache::default());
        let handle = tailer.start();

        // Two transcript lines + one trailing transcript_ready marker.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if source.current_seq().await == 3 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (mut reader, seq) = source.subscribe_with_query(None).await.unwrap();
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

        let source = StructuredLogSource::new(100);
        let tailer = TranscriptTailer::new(path, source.clone(), ClaudeVersionCache::default());
        let handle = tailer.start();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if source.current_seq().await == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (mut reader, seq) = source.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 1);
        let entry = reader.read().await.unwrap();
        assert_eq!(entry.payload["type"], "amux.transcript_ready");
        assert_eq!(entry.seq, 1);

        tailer.stop();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn transcript_reported_version_supersedes_the_cached_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        tokio::fs::write(
            &path,
            "{\"type\":\"user\",\"version\":\"2.1.224\",\"message\":{\"content\":\"hello\"}}\n",
        )
        .await
        .unwrap();
        let cache = ClaudeVersionCache::default();
        cache.observe_transcript_row(&serde_json::json!({"version": "2.1.223"}));
        let source = StructuredLogSource::new(100);
        let tailer = TranscriptTailer::new(path, source.clone(), cache.clone());
        let handle = tailer.start();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while source.current_seq().await != 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(cache.current().as_deref(), Some("2.1.224"));
        tailer.stop();
        let _ = handle.await;
    }
}
