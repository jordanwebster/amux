//! Composite type owning a structured log buffer and its transcript tailer.
//!
//! `StructuredLogSource` manages the lifecycle of transcript tailing and
//! provides clean reset semantics when Claude Code's session changes
//! (e.g. via `/clear`, `/compact`, `/fork`). On re-link, the old tailer
//! is stopped, the buffer is cleared, and a new tailer begins writing
//! to the same buffer — keeping existing subscribers connected.

use super::transcript::TranscriptTailer;
use crate::buffer::{MultiplexStructuredBuffer, MultiplexStructuredReader};
use crate::debug::{DebugView, LossyPath};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
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
    /// Held only briefly for read/replace — never across an `await`,
    /// so a `std::sync::Mutex` is appropriate (and lets the debug
    /// `Serialize` impl read it from sync context).
    current_path: StdMutex<Option<PathBuf>>,
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
                current_path: StdMutex::new(None),
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
            let current_path = self.inner.current_path.lock().expect("mutex poisoned");
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
            let mut current_path = self.inner.current_path.lock().expect("mutex poisoned");
            *current_path = Some(path.clone());
        }
        self.inner.link_state_tx.send_replace(LinkState::Linking);

        let tailer = TranscriptTailer::new(path, self.inner.buffer.clone(), self.clone());
        let handle = tailer.start();
        let mut guard = self.inner.tailer.lock().await;
        *guard = Some((tailer, handle));
    }

    /// Subscribe to the structured log buffer immediately.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.inner.buffer.subscribe().await
    }

    /// Subscribe to the structured log buffer immediately and return the matching seq.
    pub async fn subscribe_with_current_seq(&self) -> Option<(MultiplexStructuredReader, u64)> {
        self.inner.buffer.subscribe_with_current_seq().await
    }

    /// Write a structured output entry directly (e.g. hook events).
    pub async fn write(&self, payload: Value) {
        self.inner.buffer.write(payload).await;
    }

    /// Return the current sequence number.
    pub async fn current_seq(&self) -> u64 {
        self.inner.buffer.current_seq().await
    }

    /// Access the underlying buffer (needed by child waiter task).
    pub fn buffer(&self) -> &Arc<MultiplexStructuredBuffer> {
        &self.inner.buffer
    }

    pub async fn mark_linked(&self) {
        self.inner.link_state_tx.send_replace(LinkState::Linked);
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

impl LinkState {
    fn as_str(&self) -> &'static str {
        match self {
            LinkState::Unlinked => "unlinked",
            LinkState::Linking => "linking",
            LinkState::Linked => "linked",
            LinkState::Failed(_) => "failed",
            LinkState::Closed => "closed",
        }
    }
}

impl Serialize for DebugView<'_, StructuredLogSource> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let state = self.inner.inner.link_state_tx.borrow().clone();
        let path = self
            .inner
            .inner
            .current_path
            .lock()
            .expect("mutex poisoned")
            .clone();

        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("link_state", state.as_str())?;
        if let LinkState::Failed(err) = &state {
            map.serialize_entry("link_error", err)?;
        }
        if let Some(path) = &path {
            map.serialize_entry("current_path", &LossyPath(path))?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn subscribe_immediately_returns_empty_snapshot_before_transcript_exists() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let log_source = StructuredLogSource::new();

        log_source.link_transcript(transcript_path.clone()).await;
        let (_reader, seq) = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            log_source.subscribe_with_current_seq(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn subscriber_receives_replay_after_immediate_subscribe() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let log_source = StructuredLogSource::new();

        log_source.link_transcript(transcript_path.clone()).await;
        let (mut reader, seq) = log_source.subscribe_with_current_seq().await.unwrap();
        assert_eq!(seq, 0);

        tokio::fs::write(
            &transcript_path,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        let entry = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(entry.payload["type"], "user");
        assert_eq!(entry.payload["uuid"], "u1");
    }

    #[tokio::test]
    async fn write_is_visible_before_transcript_linking() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let log_source = StructuredLogSource::new();

        log_source.link_transcript(transcript_path).await;
        log_source
            .write(json!({"type": "hook.stop", "cwd": "/tmp"}))
            .await;

        let (mut reader, seq) = log_source.subscribe_with_current_seq().await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(reader.read().await.unwrap().payload["type"], "hook.stop");
    }

    #[tokio::test]
    async fn relink_discards_entries_from_previous_generation() {
        let dir = tempdir().unwrap();
        let transcript_one = dir.path().join("transcript-one.jsonl");
        let transcript_two = dir.path().join("transcript-two.jsonl");
        let log_source = StructuredLogSource::new();

        log_source.link_transcript(transcript_one.clone()).await;
        log_source
            .write(json!({"type": "hook.permission_request"}))
            .await;

        log_source.link_transcript(transcript_two.clone()).await;

        tokio::fs::write(
            &transcript_two,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if log_source.current_seq().await == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (mut reader, seq) = log_source.subscribe_with_current_seq().await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(reader.read().await.unwrap().payload["type"], "user");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), reader.read())
                .await
                .is_err(),
            "entries from the previous generation should be discarded on relink"
        );
    }

    #[tokio::test]
    async fn same_path_relink_is_ignored() {
        let dir = tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let log_source = StructuredLogSource::new();

        tokio::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        log_source.link_transcript(transcript.clone()).await;
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if log_source.current_seq().await == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        log_source.link_transcript(transcript).await;
        let (_reader, seq) = log_source.subscribe_with_current_seq().await.unwrap();
        assert_eq!(seq, 1);
    }
}
