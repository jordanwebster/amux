//! MultiplexLogBuffer - A buffer that supports multiple concurrent readers for structured logs.
//!
//! This module provides a buffer abstraction where:
//! - A single writer appends log entries to the buffer
//! - Multiple readers can subscribe and receive all entries (past and future)
//! - New subscribers receive all existing entries, then live updates
//! - No race conditions between subscribe and write operations

use crate::message::StructuredOutput;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

/// A buffer that supports multiple concurrent readers for structured log entries.
///
/// Writers append log entries to the buffer. When a reader subscribes, they receive
/// all existing entries followed by any new entries written after subscription.
///
/// The key invariant is that `write()` and `subscribe()` are mutually exclusive
/// via the buffer lock, ensuring no data loss or duplication.
pub struct MultiplexLogBuffer {
    inner: Arc<MultiplexLogBufferInner>,
}

struct MultiplexLogBufferInner {
    /// Main buffer storing all log entries (up to max_entries)
    entries: RwLock<Vec<StructuredOutput>>,
    /// Per-subscriber channels for sending new entries
    subscribers: RwLock<Vec<mpsc::UnboundedSender<StructuredOutput>>>,
    /// Maximum number of entries before truncation
    max_entries: usize,
    /// Whether the buffer has been closed
    closed: RwLock<bool>,
}

impl MultiplexLogBuffer {
    /// Create a new MultiplexLogBuffer with the given maximum entry count.
    ///
    /// When the buffer exceeds `max_entries`, the oldest entries are dropped.
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Arc::new(MultiplexLogBufferInner {
                entries: RwLock::new(Vec::new()),
                subscribers: RwLock::new(Vec::new()),
                max_entries,
                closed: RwLock::new(false),
            }),
        }
    }

    /// Write a log entry to the buffer and broadcast to all subscribers.
    ///
    /// This method holds the buffer write lock during both the append and
    /// broadcast operations, ensuring atomicity with `subscribe()`.
    /// Dead subscribers (where the receiver has been dropped) are automatically
    /// cleaned up during each write.
    pub async fn write(&self, entry: StructuredOutput) {
        // Hold buffer write lock during append AND broadcast
        let mut entries = self.inner.entries.write().await;
        entries.push(entry.clone());

        // Truncate if over max entries
        if entries.len() > self.inner.max_entries {
            let excess = entries.len() - self.inner.max_entries;
            entries.drain(..excess);
        }

        // Broadcast to subscribers, removing any whose receivers have dropped
        let mut subs = self.inner.subscribers.write().await;
        subs.retain(|tx| tx.send(entry.clone()).is_ok());
    }

    /// Subscribe to the buffer, receiving all existing entries and future writes.
    ///
    /// This method holds the buffer read lock during both the snapshot and
    /// subscription operations, ensuring atomicity with `write()`.
    ///
    /// Returns `None` if the buffer has been closed.
    pub async fn subscribe(&self) -> Option<MultiplexLogReader> {
        // Check if closed first
        if *self.inner.closed.read().await {
            return None;
        }

        let (tx, rx) = mpsc::unbounded_channel();

        // Hold buffer read lock while subscribing (mutual exclusion with write)
        let entries = self.inner.entries.read().await;
        self.inner.subscribers.write().await.push(tx.clone());

        // Send existing entries to new subscriber
        for entry in entries.iter() {
            let _ = tx.send(entry.clone());
        }

        Some(MultiplexLogReader { rx })
    }

    /// Close the buffer, causing all readers to receive None on their next read.
    pub async fn close(&self) {
        *self.inner.closed.write().await = true;
        // Clear subscribers to drop all senders, which closes the channels
        self.inner.subscribers.write().await.clear();
    }
}

impl Clone for MultiplexLogBuffer {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// A reader handle for a MultiplexLogBuffer.
///
/// Each reader receives entries independently. When created via `subscribe()`,
/// the reader first receives all existing buffer contents, then any new entries
/// written after subscription.
pub struct MultiplexLogReader {
    rx: mpsc::UnboundedReceiver<StructuredOutput>,
}

impl MultiplexLogReader {
    /// Read the next log entry.
    ///
    /// Returns `Some(entry)` when data is available, or `None` when the
    /// buffer has been closed and no more data will arrive.
    pub async fn read(&mut self) -> Option<StructuredOutput> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    fn user_msg(content: &str, uuid: &str) -> StructuredOutput {
        StructuredOutput::Claude(crate::message::ClaudeStructuredOutput::UserMessage {
            content: content.to_string(),
            timestamp: "2025-01-15T12:00:00Z".to_string(),
            uuid: uuid.to_string(),
        })
    }

    fn assistant_msg(content: &str, uuid: &str) -> StructuredOutput {
        StructuredOutput::Claude(crate::message::ClaudeStructuredOutput::AssistantMessage {
            content: content.to_string(),
            timestamp: "2025-01-15T12:00:01Z".to_string(),
            uuid: uuid.to_string(),
        })
    }

    #[tokio::test]
    async fn test_single_subscriber_receives_entries() {
        let buffer = MultiplexLogBuffer::new(100);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("hello", "1")).await;
        buffer.write(assistant_msg("hi there", "2")).await;

        let entry1 = reader.read().await.unwrap();
        let entry2 = reader.read().await.unwrap();

        assert_eq!(entry1, user_msg("hello", "1"));
        assert_eq!(entry2, assistant_msg("hi there", "2"));
    }

    #[tokio::test]
    async fn test_multiple_subscribers_receive_same_entries() {
        let buffer = MultiplexLogBuffer::new(100);
        let mut reader1 = buffer.subscribe().await.unwrap();
        let mut reader2 = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("broadcast", "1")).await;

        let entry1 = reader1.read().await.unwrap();
        let entry2 = reader2.read().await.unwrap();

        assert_eq!(entry1, user_msg("broadcast", "1"));
        assert_eq!(entry2, user_msg("broadcast", "1"));
    }

    #[tokio::test]
    async fn test_late_subscriber_gets_replay() {
        let buffer = MultiplexLogBuffer::new(100);

        // Write some data before anyone subscribes
        buffer.write(user_msg("existing", "1")).await;

        // Now subscribe - should get existing data
        let mut reader = buffer.subscribe().await.unwrap();
        let replay = reader.read().await.unwrap();
        assert_eq!(replay, user_msg("existing", "1"));

        // Write more data - should get it too
        buffer.write(assistant_msg("new", "2")).await;
        let live = reader.read().await.unwrap();
        assert_eq!(live, assistant_msg("new", "2"));
    }

    #[tokio::test]
    async fn test_buffer_truncation() {
        let buffer = MultiplexLogBuffer::new(3); // Only 3 entries max

        // Write 5 entries
        buffer.write(user_msg("msg1", "1")).await;
        buffer.write(user_msg("msg2", "2")).await;
        buffer.write(user_msg("msg3", "3")).await;
        buffer.write(user_msg("msg4", "4")).await;
        buffer.write(user_msg("msg5", "5")).await;

        // New subscriber should only see last 3 entries
        let mut reader = buffer.subscribe().await.unwrap();
        let entry1 = reader.read().await.unwrap();
        let entry2 = reader.read().await.unwrap();
        let entry3 = reader.read().await.unwrap();

        assert_eq!(entry1, user_msg("msg3", "3"));
        assert_eq!(entry2, user_msg("msg4", "4"));
        assert_eq!(entry3, user_msg("msg5", "5"));
    }

    #[tokio::test]
    async fn test_close_returns_none() {
        let buffer = MultiplexLogBuffer::new(100);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("data", "1")).await;
        assert_eq!(reader.read().await.unwrap(), user_msg("data", "1"));

        buffer.close().await;

        // After close, read returns None
        assert!(reader.read().await.is_none());
    }

    #[tokio::test]
    async fn test_subscribe_after_close_returns_none() {
        let buffer = MultiplexLogBuffer::new(100);
        buffer.write(user_msg("data", "1")).await;
        buffer.close().await;

        // Subscribe after close should return None
        assert!(buffer.subscribe().await.is_none());
    }

    #[tokio::test]
    async fn test_dropped_reader_doesnt_block_writes() {
        let buffer = MultiplexLogBuffer::new(100);
        let reader = buffer.subscribe().await.unwrap();

        // Drop the reader
        drop(reader);

        // Writes should still work (not block)
        let result = timeout(
            Duration::from_millis(100),
            buffer.write(user_msg("data", "1")),
        )
        .await;
        assert!(
            result.is_ok(),
            "Write should not block after reader dropped"
        );
    }
}
