//! Composite type owning a structured log buffer and its transcript tailer.
//!
//! `StructuredLogSource` manages the lifecycle of transcript tailing and
//! provides clean reset semantics when Claude Code's session changes
//! (e.g. via `/clear`, `/compact`, `/fork`). On re-link, the old tailer
//! is stopped, the buffer is cleared, and a new tailer begins writing
//! to the same buffer — keeping existing subscribers connected.

use super::transcript::TranscriptTailer;
use super::types::AgentStructuredOutput;
use crate::buffer::{MultiplexStructuredBuffer, MultiplexStructuredReader};
use crate::error::AmuxError;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

/// Maximum number of structured log entries to keep
const MAX_LOG_ENTRIES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum LinkState {
    Unlinked,
    Linking,
    Linked,
    Failed(String),
    Closed,
}

struct StructuredLogSourceInner {
    buffer: Arc<MultiplexStructuredBuffer>,
    tailer: Mutex<Option<(TranscriptTailer, JoinHandle<()>)>>,
    current_path: Mutex<Option<PathBuf>>,
    pending_writes: Mutex<Vec<AgentStructuredOutput>>,
    link_state_tx: watch::Sender<LinkState>,
}

/// Owns a structured log buffer and an optional transcript tailer.
#[derive(Clone)]
pub struct StructuredLogSource {
    inner: Arc<StructuredLogSourceInner>,
}

impl StructuredLogSource {
    /// Create a new source with an empty buffer.
    pub fn new() -> Self {
        let (link_state_tx, _) = watch::channel(LinkState::Unlinked);
        Self {
            inner: Arc::new(StructuredLogSourceInner {
                buffer: Arc::new(MultiplexStructuredBuffer::new(MAX_LOG_ENTRIES)),
                tailer: Mutex::new(None),
                current_path: Mutex::new(None),
                pending_writes: Mutex::new(Vec::new()),
                link_state_tx,
            }),
        }
    }

    /// Link a transcript file to this source.
    ///
    /// On first call, starts tailing the file. On subsequent calls (session
    /// change), stops the old tailer, clears the buffer, and starts tailing
    /// the new path. Existing subscribers remain connected and receive entries
    /// from the new transcript.
    pub async fn link_transcript(&self, path: PathBuf) {
        let current_state = self.inner.link_state_tx.borrow().clone();
        {
            let current_path = self.inner.current_path.lock().await;
            if current_path.as_ref() == Some(&path)
                && matches!(current_state, LinkState::Linking | LinkState::Linked)
            {
                return;
            }
        }

        let old_tailer = {
            let mut guard = self.inner.tailer.lock().await;
            guard.take()
        };
        if let Some((old_tailer, handle)) = old_tailer {
            old_tailer.stop();
            let _ = handle.await;
            self.inner.buffer.clear().await;
        }

        {
            let mut current_path = self.inner.current_path.lock().await;
            *current_path = Some(path.clone());
        }
        self.inner.pending_writes.lock().await.clear();
        self.inner.link_state_tx.send_replace(LinkState::Linking);

        let tailer = TranscriptTailer::new(path, self.inner.buffer.clone(), self.clone());
        let handle = tailer.start();
        let mut guard = self.inner.tailer.lock().await;
        *guard = Some((tailer, handle));
    }

    pub async fn wait_until_linked(&self) -> Result<(), AmuxError> {
        let mut link_state_rx = self.inner.link_state_tx.subscribe();
        loop {
            match &*link_state_rx.borrow() {
                LinkState::Linked => return Ok(()),
                LinkState::Failed(error) => {
                    return Err(AmuxError::ServerError(format!(
                        "structured transcript link failed: {error}"
                    )));
                }
                LinkState::Closed => {
                    return Err(AmuxError::ServerError(
                        "structured transcript source closed".to_string(),
                    ));
                }
                LinkState::Unlinked | LinkState::Linking => {}
            }
            if link_state_rx.changed().await.is_err() {
                return Err(AmuxError::ServerError(
                    "structured transcript source closed".to_string(),
                ));
            }
        }
    }

    /// Subscribe to the structured log buffer immediately.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.inner.buffer.subscribe().await
    }

    /// Subscribe to the structured log buffer immediately and return the matching seq.
    pub async fn subscribe_with_current_seq(&self) -> Option<(MultiplexStructuredReader, u64)> {
        self.inner.buffer.subscribe_with_current_seq().await
    }

    /// Write a structured output entry directly (e.g. permission requests).
    pub async fn write(&self, entry: AgentStructuredOutput) {
        if self.is_linked() {
            self.inner.buffer.write(entry).await;
        } else {
            let mut pending_writes = self.inner.pending_writes.lock().await;
            if self.is_linked() {
                drop(pending_writes);
                self.inner.buffer.write(entry).await;
            } else {
                pending_writes.push(entry);
            }
        }
    }

    /// Return the current sequence number.
    pub async fn current_seq(&self) -> u64 {
        self.inner.buffer.current_seq().await
    }

    pub fn is_linked(&self) -> bool {
        matches!(&*self.inner.link_state_tx.borrow(), LinkState::Linked)
    }

    /// Access the underlying buffer (needed by child waiter task).
    pub fn buffer(&self) -> &Arc<MultiplexStructuredBuffer> {
        &self.inner.buffer
    }

    pub async fn mark_linked(&self) {
        loop {
            let pending = {
                let mut pending_writes = self.inner.pending_writes.lock().await;
                if pending_writes.is_empty() {
                    self.inner.link_state_tx.send_replace(LinkState::Linked);
                    return;
                }
                std::mem::take(&mut *pending_writes)
            };
            for entry in pending {
                self.inner.buffer.write(entry).await;
            }
        }
    }

    pub fn mark_failed(&self, error: std::io::Error) {
        self.inner
            .link_state_tx
            .send_replace(LinkState::Failed(error.to_string()));
    }

    /// Stop the tailer and close the buffer.
    pub async fn close(&self) {
        let tailer = {
            let mut guard = self.inner.tailer.lock().await;
            guard.take()
        };
        if let Some((tailer, handle)) = tailer {
            tailer.stop();
            let _ = handle.await;
        }
        self.inner.link_state_tx.send_replace(LinkState::Closed);
        self.inner.buffer.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::ClaudeStructuredOutput;
    use tempfile::tempdir;

    #[tokio::test]
    async fn subscribe_waits_for_replay_and_flushes_pending_entries_in_order() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let log_source = StructuredLogSource::new();

        log_source.link_transcript(transcript_path.clone()).await;
        log_source
            .write(AgentStructuredOutput::Claude(
                ClaudeStructuredOutput::AgentStopped {
                    cwd: Some(dir.path().display().to_string()),
                    stop_hook_active: Some(false),
                },
            ))
            .await;

        let ready_task = tokio::spawn({
            let log_source = log_source.clone();
            async move {
                log_source.wait_until_linked().await.unwrap();
                log_source.subscribe_with_current_seq().await.unwrap()
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !ready_task.is_finished(),
            "readiness should wait until the transcript is linked"
        );

        tokio::fs::write(
            &transcript_path,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        let (mut reader, seq) = tokio::time::timeout(std::time::Duration::from_secs(2), ready_task)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(seq, 2);
        assert_eq!(
            reader.read().await.unwrap().data,
            AgentStructuredOutput::Claude(ClaudeStructuredOutput::UserMessage {
                content: "hello".to_string(),
                uuid: "u1".to_string(),
                timestamp: "2026-03-29T10:00:00Z".to_string(),
                cwd: None,
                git_branch: None,
                parent_uuid: None,
                prompt_id: None,
                permission_mode: None,
                slug: None,
            })
        );
        assert_eq!(
            reader.read().await.unwrap().data,
            AgentStructuredOutput::Claude(ClaudeStructuredOutput::AgentStopped {
                cwd: Some(dir.path().display().to_string()),
                stop_hook_active: Some(false),
            })
        );
    }

    #[tokio::test]
    async fn relink_discards_pending_hook_entries_from_previous_generation() {
        let dir = tempdir().unwrap();
        let transcript_one = dir.path().join("transcript-one.jsonl");
        let transcript_two = dir.path().join("transcript-two.jsonl");
        let log_source = StructuredLogSource::new();

        log_source.link_transcript(transcript_one.clone()).await;
        log_source
            .write(AgentStructuredOutput::Claude(
                ClaudeStructuredOutput::PermissionRequest {
                    tool: crate::claude::types::PreToolUse::Read {
                        tool_input: crate::claude::types::ReadToolInput {
                            file_path: "a.txt".to_string(),
                            offset: None,
                            limit: None,
                            pages: None,
                        },
                    },
                    cwd: Some(dir.path().display().to_string()),
                },
            ))
            .await;

        log_source.link_transcript(transcript_two.clone()).await;

        tokio::fs::write(
            &transcript_two,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        log_source.wait_until_linked().await.unwrap();
        let (mut reader, seq) = log_source.subscribe_with_current_seq().await.unwrap();

        assert_eq!(seq, 1);
        assert_eq!(
            reader.read().await.unwrap().data,
            AgentStructuredOutput::Claude(ClaudeStructuredOutput::UserMessage {
                content: "hello".to_string(),
                uuid: "u1".to_string(),
                timestamp: "2026-03-29T10:00:00Z".to_string(),
                cwd: None,
                git_branch: None,
                parent_uuid: None,
                prompt_id: None,
                permission_mode: None,
                slug: None,
            })
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.read())
                .await
                .is_err(),
            "pending hook entries from the previous generation should be discarded on relink"
        );
    }

    #[tokio::test]
    async fn wait_until_linked_returns_closed_error_when_closed_before_link() {
        let log_source = StructuredLogSource::new();
        let wait_task = tokio::spawn({
            let log_source = log_source.clone();
            async move { log_source.wait_until_linked().await }
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !wait_task.is_finished(),
            "link readiness should still be pending before the source is closed"
        );

        log_source.close().await;

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), wait_task)
            .await
            .unwrap()
            .unwrap();
        let error = result.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("structured transcript source closed"),
            "unexpected error: {error}"
        );
    }
}
