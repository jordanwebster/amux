//! Claude transcript-tailing lifecycle around a structured log sink.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::transcript::TranscriptTailer;
use crate::agents::StructuredLogSource;
use crate::debug::{DebugView, LossyPath};

struct TranscriptIngestInner {
    source: StructuredLogSource,
    tailer: Mutex<Option<(TranscriptTailer, JoinHandle<()>)>>,
    /// Held only briefly for read/replace — never held across an `await`,
    /// so a `std::sync::Mutex` is appropriate (and lets the debug
    /// `Serialize` impl read it from sync context).
    current_path: StdMutex<Option<PathBuf>>,
}

/// Owns Claude transcript tailing around a structured log sink.
#[derive(Clone)]
pub(super) struct TranscriptIngest {
    inner: Arc<TranscriptIngestInner>,
}

impl TranscriptIngest {
    pub(super) fn new(source: StructuredLogSource) -> Self {
        Self {
            inner: Arc::new(TranscriptIngestInner {
                source,
                tailer: Mutex::new(None),
                current_path: StdMutex::new(None),
            }),
        }
    }

    pub(super) fn log_source(&self) -> &StructuredLogSource {
        &self.inner.source
    }

    /// Link a transcript file to this ingest.
    ///
    /// On first call, starts tailing the file. On subsequent calls with a
    /// different path, stops the old tailer, clears retained entries, and
    /// starts tailing the new path. Existing subscribers remain connected.
    /// Calls with the same path as the current link are ignored.
    pub(super) async fn link_transcript(&self, path: PathBuf) {
        {
            let current_path = self.inner.current_path.lock().expect("mutex poisoned");
            if current_path.as_ref() == Some(&path) {
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
            self.inner.source.clear().await;
        }

        {
            let mut current_path = self.inner.current_path.lock().expect("mutex poisoned");
            *current_path = Some(path.clone());
        }

        let tailer = TranscriptTailer::new(path, self.inner.source.clone());
        let handle = tailer.start();
        let mut guard = self.inner.tailer.lock().await;
        *guard = Some((tailer, handle));
    }

    /// Stop transcript tailing and close the underlying sink.
    pub(super) async fn close(&self) {
        let tailer = {
            let mut guard = self.inner.tailer.lock().await;
            guard.take()
        };
        if let Some((tailer, handle)) = tailer {
            tailer.stop();
            let _ = handle.await;
        }
        self.inner.source.close().await;
    }
}

impl Serialize for DebugView<'_, TranscriptIngest> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let path = self
            .inner
            .inner
            .current_path
            .lock()
            .expect("mutex poisoned")
            .clone();

        let mut map = serializer.serialize_map(None)?;
        if let Some(path) = &path {
            map.serialize_entry("current_path", &LossyPath(path))?;
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    fn new_ingest() -> TranscriptIngest {
        TranscriptIngest::new(StructuredLogSource::new(1000))
    }

    #[tokio::test]
    async fn subscribe_immediately_returns_empty_snapshot_before_transcript_exists() {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let ingest = new_ingest();

        ingest.link_transcript(transcript_path).await;
        let (_reader, seq) = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            ingest.log_source().subscribe_with_query(None),
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
        let ingest = new_ingest();

        ingest.link_transcript(transcript_path.clone()).await;
        let (mut reader, seq) = ingest
            .log_source()
            .subscribe_with_query(None)
            .await
            .unwrap();
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

        let marker = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(marker.payload["type"], "amux.transcript_ready");
    }

    #[tokio::test]
    async fn relink_discards_entries_from_previous_generation() {
        let dir = tempdir().unwrap();
        let transcript_one = dir.path().join("transcript-one.jsonl");
        let transcript_two = dir.path().join("transcript-two.jsonl");
        let ingest = new_ingest();

        ingest.link_transcript(transcript_one).await;
        ingest
            .log_source()
            .write(json!({"type": "hook.permission_request"}))
            .await;

        ingest.link_transcript(transcript_two.clone()).await;

        tokio::fs::write(
            &transcript_two,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        // Wait for the new tailer to drain (one user entry) and emit its
        // transcript_ready marker. seq is preserved across clear(), so the hook
        // counted as 1 → user is 2 → marker is 3.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if ingest.log_source().current_seq().await == 3 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let (mut reader, seq) = ingest
            .log_source()
            .subscribe_with_query(None)
            .await
            .unwrap();
        assert_eq!(seq, 3);
        assert_eq!(reader.read().await.unwrap().payload["type"], "user");
        assert_eq!(
            reader.read().await.unwrap().payload["type"],
            "amux.transcript_ready"
        );
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
        let ingest = new_ingest();

        tokio::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"content\":\"hello\"},\"uuid\":\"u1\",\"timestamp\":\"2026-03-29T10:00:00Z\"}\n",
        )
        .await
        .unwrap();

        ingest.link_transcript(transcript.clone()).await;
        // One user entry + one transcript_ready marker.
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if ingest.log_source().current_seq().await == 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        // Second link to the same path is a no-op — no new tailer, no new marker.
        ingest.link_transcript(transcript).await;
        let (_reader, seq) = ingest
            .log_source()
            .subscribe_with_query(None)
            .await
            .unwrap();
        assert_eq!(seq, 2);
    }
}
