//! Claude transcript-tailing lifecycle around a structured log sink.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::ClaudeVersionCache;
use crate::agents::StructuredLogSource;
use crate::debug::{DebugView, LossyPath};
use claude::transcript::TranscriptTailer;

struct TranscriptIngestInner {
    source: StructuredLogSource,
    claude_version_cache: ClaudeVersionCache,
    delivery_ready: Option<Arc<AtomicBool>>,
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
    pub(super) fn new(
        source: StructuredLogSource,
        claude_version_cache: ClaudeVersionCache,
    ) -> Self {
        Self {
            inner: Arc::new(TranscriptIngestInner {
                source,
                claude_version_cache,
                delivery_ready: None,
                tailer: Mutex::new(None),
                current_path: StdMutex::new(None),
            }),
        }
    }

    pub(super) fn with_delivery_ready(
        source: StructuredLogSource,
        delivery_ready: Arc<AtomicBool>,
        claude_version_cache: ClaudeVersionCache,
    ) -> Self {
        Self {
            inner: Arc::new(TranscriptIngestInner {
                source,
                claude_version_cache,
                delivery_ready: Some(delivery_ready),
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
            drop(old_tailer);
            let _ = handle.await;
            self.inner.source.clear().await;
        }

        {
            let mut current_path = self.inner.current_path.lock().expect("mutex poisoned");
            *current_path = Some(path.clone());
        }

        let tailer = claude::transcript::TranscriptTailer::follow(path);
        let mut rows = tailer.rows();
        let source = self.inner.source.clone();
        let version = self.inner.claude_version_cache.clone();
        let delivery_ready = self.inner.delivery_ready.clone();
        let handle = tokio::spawn(async move {
            while let Some(row) = rows.recv().await {
                let value = row.into_value();
                version.observe_transcript_row(&value);
                if value.get("type").and_then(serde_json::Value::as_str)
                    == Some("amux.transcript_ready")
                    && let Some(delivery_ready) = &delivery_ready
                {
                    delivery_ready.store(true, std::sync::atomic::Ordering::Release);
                }
                source.write(value).await;
            }
        });
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
            drop(tailer);
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
        TranscriptIngest::new(
            StructuredLogSource::new(1000),
            ClaudeVersionCache::default(),
        )
    }

    async fn assert_fixture_passes_through_ingest(name: &str, fixture: &str) {
        let dir = tempdir().unwrap();
        let transcript_path = dir.path().join(format!("{name}.jsonl"));
        tokio::fs::write(&transcript_path, fixture).await.unwrap();
        let expected: Vec<serde_json::Value> = fixture
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("fixture row is JSON"))
            .collect();
        let ingest = new_ingest();

        ingest.link_transcript(transcript_path).await;
        let expected_seq = u64::try_from(expected.len() + 1).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if ingest.log_source().current_seq().await == expected_seq {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{name} did not finish replaying"));

        let (mut reader, seq) = ingest
            .log_source()
            .subscribe_with_query(None)
            .await
            .unwrap();
        assert_eq!(seq, expected_seq);
        for expected_row in expected {
            let actual = reader.read().await.expect("fixture row retained").payload;
            assert_eq!(actual, expected_row, "{name} row was reclassified");
        }
        let marker = reader.read().await.expect("ingest readiness marker");
        assert_eq!(marker.payload, json!({"type": "amux.transcript_ready"}));
        ingest.close().await;
    }

    #[tokio::test]
    async fn a2a_fixture_fold_preserves_socket_and_pty_carrier_rows() {
        assert_fixture_passes_through_ingest(
            "socket-delivery",
            include_str!("../../../tests/fixtures/a2a/socket_delivery.jsonl"),
        )
        .await;
        assert_fixture_passes_through_ingest(
            "pty-delivery",
            include_str!("../../../tests/fixtures/a2a/pty_delivery.jsonl"),
        )
        .await;
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
