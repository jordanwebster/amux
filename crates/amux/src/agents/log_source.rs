//! Agent-agnostic sequenced sink for structured log entries.

use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::agents::{MultiplexStructuredBuffer, MultiplexStructuredReader, SequencedReplayQuery};

/// A retained structured log that supports replay and live subscriptions.
#[derive(Clone)]
pub(crate) struct StructuredLogSource {
    buffer: MultiplexStructuredBuffer,
    recording: Option<Arc<Mutex<File>>>,
}

impl StructuredLogSource {
    /// Create an empty source retaining at most `retention` entries.
    pub(crate) fn new(retention: usize) -> Self {
        Self {
            buffer: MultiplexStructuredBuffer::new(retention),
            recording: None,
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

    /// Write a structured output entry.
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
}
