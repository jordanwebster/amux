//! Agent-agnostic sequenced sink for structured log entries.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::agents::{
    MultiplexStructuredBuffer, MultiplexStructuredReader, OutputDebug, SequencedReplayQuery,
};

/// No activity has been written yet.
const NO_ACTIVITY: i64 = i64::MIN;

/// A retained structured log that supports replay and live subscriptions.
///
/// It also dates the agent's activity. Not every entry is activity: a
/// session re-reading its transcript after a restart, or amux writing its own
/// bookkeeping rows, is not the agent doing anything, and counting those
/// would make every agent look active the moment its daemon started. So a
/// writer says which entries are activity, and when they happened.
#[derive(Clone)]
pub(crate) struct StructuredLogSource {
    buffer: MultiplexStructuredBuffer,
    recording: Option<Arc<Mutex<File>>>,
    /// Unix milliseconds of the latest activity written, or `NO_ACTIVITY`.
    activity: Arc<AtomicI64>,
}

impl StructuredLogSource {
    /// Create an empty source retaining at most `retention` entries.
    pub(crate) fn new(retention: usize) -> Self {
        Self {
            buffer: MultiplexStructuredBuffer::new(retention),
            recording: None,
            activity: Arc::new(AtomicI64::new(NO_ACTIVITY)),
        }
    }

    /// Create a source that also writes every structured row as JSONL.
    pub(crate) fn recording(retention: usize, path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            buffer: MultiplexStructuredBuffer::new(retention),
            recording: Some(Arc::new(Mutex::new(file))),
            activity: Arc::new(AtomicI64::new(NO_ACTIVITY)),
        })
    }

    /// Subscribe to the structured log buffer immediately.
    #[allow(dead_code)]
    pub(crate) async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.buffer.subscribe().await
    }

    /// Subscribe with an optional replay filter and return the matching seq.
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<SequencedReplayQuery>,
    ) -> Option<(MultiplexStructuredReader, u64)> {
        self.buffer.subscribe_with_query(query).await
    }

    /// Write an entry that records something the agent did at `at`.
    ///
    /// Activity only moves forward: an entry dated earlier than the latest
    /// one already seen, such as a transcript row re-read on a relink, leaves
    /// it where it is.
    pub(crate) async fn write_activity(&self, payload: Value, at: DateTime<Utc>) {
        self.activity
            .fetch_max(at.timestamp_millis(), Ordering::Relaxed);
        self.write(payload).await;
    }

    /// When the latest activity written to this log happened, if any has been.
    pub(crate) fn last_activity(&self) -> Option<DateTime<Utc>> {
        match self.activity.load(Ordering::Relaxed) {
            NO_ACTIVITY => None,
            millis => Utc.timestamp_millis_opt(millis).single(),
        }
    }

    /// Write a structured output entry that is not itself activity: amux's
    /// own bookkeeping, or history being re-read.
    pub(crate) async fn write(&self, payload: Value) {
        self.buffer.write(payload.clone()).await;
        if let Some(recording) = &self.recording
            && let Ok(mut file) = recording.lock()
        {
            let _ = writeln!(file, "{payload}");
            let _ = file.flush();
        }
    }

    /// Return the current sequence number.
    pub(crate) async fn current_seq(&self) -> u64 {
        self.buffer.current_seq().await
    }

    /// Snapshot retained coordinates and active readers for diagnostics.
    pub(crate) async fn debug_snapshot(&self) -> OutputDebug {
        self.buffer.debug_snapshot().await
    }

    /// Clear retained entries without resetting the sequence number.
    pub(crate) async fn clear(&self) {
        self.buffer.clear().await;
    }

    /// Close the source and all current subscriptions.
    pub(crate) async fn close(&self) {
        self.buffer.close().await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn write_is_visible_to_later_subscribers() {
        let log_source = StructuredLogSource::new(1000);
        log_source
            .write(json!({"type": "hook.stop", "cwd": "/tmp"}))
            .await;

        let (mut reader, seq) = log_source.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 1);
        assert_eq!(reader.read().await.unwrap().payload["type"], "hook.stop");
    }

    #[tokio::test]
    async fn only_activity_writes_date_the_log_and_never_backwards() {
        let log_source = StructuredLogSource::new(1000);
        log_source.write(json!({"type": "amux.ready"})).await;
        assert_eq!(log_source.last_activity(), None);

        let later = Utc.timestamp_millis_opt(1_700_000_060_000).unwrap();
        let earlier = Utc.timestamp_millis_opt(1_700_000_000_000).unwrap();
        log_source
            .write_activity(json!({"type": "user"}), later)
            .await;
        log_source
            .write_activity(json!({"type": "assistant"}), earlier)
            .await;
        log_source.write(json!({"type": "amux.ready"})).await;
        assert_eq!(log_source.last_activity(), Some(later));
        assert_eq!(log_source.current_seq().await, 4);
    }
}
