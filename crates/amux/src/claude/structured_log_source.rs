//! Composite type owning a structured log buffer and its transcript tailer.
//!
//! `StructuredLogSource` manages the lifecycle of transcript tailing and
//! provides clean reset semantics when Claude Code's session changes
//! (e.g. via `/clear`, `/compact`, `/fork`). On re-link, the old tailer
//! is stopped, the buffer is cleared, and a new tailer begins writing
//! to the same buffer — keeping existing subscribers connected.

use super::transcript::TranscriptTailer;
use super::types::AgentStructuredOutput;
use crate::buffer::{MultiplexStructuredReader, SequencedStructuredBuffer};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Maximum number of structured log entries to keep
const MAX_LOG_ENTRIES: usize = 1000;

/// Owns a structured log buffer and an optional transcript tailer.
pub struct StructuredLogSource {
    buffer: Arc<SequencedStructuredBuffer>,
    tailer: Mutex<Option<(TranscriptTailer, JoinHandle<()>)>>,
}

impl StructuredLogSource {
    /// Create a new source with an empty buffer.
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(SequencedStructuredBuffer::new(MAX_LOG_ENTRIES)),
            tailer: Mutex::new(None),
        }
    }

    /// Link a transcript file to this source.
    ///
    /// On first call, starts tailing the file. On subsequent calls (session
    /// change), stops the old tailer, clears the buffer, and starts tailing
    /// the new path. Existing subscribers remain connected and receive entries
    /// from the new transcript.
    pub async fn link_transcript(&self, path: PathBuf) {
        let mut guard = self.tailer.lock().await;
        if let Some((old_tailer, _handle)) = guard.take() {
            old_tailer.stop();
            self.buffer.clear().await;
        }
        let tailer = TranscriptTailer::new(path, self.buffer.clone());
        let handle = tailer.start();
        *guard = Some((tailer, handle));
    }

    /// Subscribe to the structured log buffer.
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.buffer.subscribe().await
    }

    /// Write a structured output entry directly (e.g. permission requests).
    pub async fn write(&self, entry: AgentStructuredOutput) {
        self.buffer.write(entry).await;
    }

    /// Return the current sequence number.
    pub fn current_seq(&self) -> u64 {
        self.buffer.current_seq()
    }

    /// Access the underlying buffer (needed by child waiter task).
    pub fn buffer(&self) -> &Arc<SequencedStructuredBuffer> {
        &self.buffer
    }

    /// Stop the tailer and close the buffer.
    pub async fn close(&self) {
        if let Some((tailer, _handle)) = self.tailer.lock().await.take() {
            tailer.stop();
        }
        self.buffer.close().await;
    }
}
