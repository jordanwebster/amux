//! MultiplexBuffer - A buffer that supports multiple concurrent readers.
//!
//! This module provides a buffer abstraction where:
//! - A single writer appends bytes to the buffer
//! - Multiple readers can subscribe and receive all bytes (past and future)
//! - New subscribers receive all existing bytes, then live updates
//! - No race conditions between subscribe and write operations

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// A buffer that supports multiple concurrent readers.
///
/// Writers append bytes to the buffer. When a reader subscribes, they receive
/// all existing bytes followed by any new bytes written after subscription.
///
/// The key invariant is that `write()` and `subscribe()` are mutually exclusive
/// via the buffer lock, ensuring no data loss or duplication.
pub struct MultiplexBuffer {
    inner: Arc<MultiplexBufferInner>,
}

struct MultiplexBufferInner {
    /// Main buffer storing all bytes (up to max_size)
    buffer: RwLock<Vec<u8>>,
    /// Per-subscriber channels for sending new bytes
    subscribers: RwLock<Vec<mpsc::UnboundedSender<Vec<u8>>>>,
    /// Maximum buffer size before truncation
    max_size: usize,
    /// Whether the buffer has been closed
    closed: RwLock<bool>,
}

impl MultiplexBuffer {
    /// Create a new MultiplexBuffer with the given maximum size.
    ///
    /// When the buffer exceeds `max_size`, the oldest bytes are dropped.
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: Arc::new(MultiplexBufferInner {
                buffer: RwLock::new(Vec::new()),
                subscribers: RwLock::new(Vec::new()),
                max_size,
                closed: RwLock::new(false),
            }),
        }
    }

    /// Write bytes to the buffer and broadcast to all subscribers.
    ///
    /// This method holds the buffer write lock during both the append and
    /// broadcast operations, ensuring atomicity with `subscribe()`.
    pub async fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        // Hold buffer write lock during append AND broadcast
        let mut buf = self.inner.buffer.write().await;
        buf.extend_from_slice(bytes);

        // Truncate if over max size
        if buf.len() > self.inner.max_size {
            let excess = buf.len() - self.inner.max_size;
            buf.drain(..excess);
        }

        // Broadcast to all subscribers (while still holding buffer lock)
        let subs = self.inner.subscribers.read().await;
        for tx in subs.iter() {
            // Ignore send errors - subscriber may have dropped
            let _ = tx.send(bytes.to_vec());
        }
    }

    /// Subscribe to the buffer, receiving all existing bytes and future writes.
    ///
    /// This method holds the buffer read lock during both the snapshot and
    /// subscription operations, ensuring atomicity with `write()`.
    ///
    /// Returns `None` if the buffer has been closed.
    pub async fn subscribe(&self) -> Option<MultiplexReader> {
        // Check if closed first
        if *self.inner.closed.read().await {
            return None;
        }

        let (tx, rx) = mpsc::unbounded_channel();

        // Hold buffer read lock while subscribing (mutual exclusion with write)
        let buf = self.inner.buffer.read().await;
        self.inner.subscribers.write().await.push(tx.clone());

        // Send existing bytes to new subscriber
        if !buf.is_empty() {
            let _ = tx.send(buf.clone());
        }

        Some(MultiplexReader { rx })
    }

    /// Close the buffer, causing all readers to receive None on their next read.
    pub async fn close(&self) {
        *self.inner.closed.write().await = true;
        // Clear subscribers to drop all senders, which closes the channels
        self.inner.subscribers.write().await.clear();
    }

    /// Check if the buffer has been closed.
    pub async fn is_closed(&self) -> bool {
        *self.inner.closed.read().await
    }
}

impl Clone for MultiplexBuffer {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// A reader handle for a MultiplexBuffer.
///
/// Each reader receives bytes independently. When created via `subscribe()`,
/// the reader first receives all existing buffer contents, then any new bytes
/// written after subscription.
pub struct MultiplexReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl MultiplexReader {
    /// Read the next chunk of bytes.
    ///
    /// Returns `Some(bytes)` when data is available, or `None` when the
    /// buffer has been closed and no more data will arrive.
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn test_single_subscriber_receives_data() {
        let buffer = MultiplexBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(b"hello").await;
        buffer.write(b" world").await;

        let chunk1 = reader.read().await.unwrap();
        let chunk2 = reader.read().await.unwrap();

        assert_eq!(chunk1, b"hello");
        assert_eq!(chunk2, b" world");
    }

    #[tokio::test]
    async fn test_multiple_subscribers_receive_same_data() {
        let buffer = MultiplexBuffer::new(1024);
        let mut reader1 = buffer.subscribe().await.unwrap();
        let mut reader2 = buffer.subscribe().await.unwrap();

        buffer.write(b"broadcast").await;

        let data1 = reader1.read().await.unwrap();
        let data2 = reader2.read().await.unwrap();

        assert_eq!(data1, b"broadcast");
        assert_eq!(data2, b"broadcast");
    }

    #[tokio::test]
    async fn test_late_subscriber_gets_replay() {
        let buffer = MultiplexBuffer::new(1024);

        // Write some data before anyone subscribes
        buffer.write(b"existing").await;

        // Now subscribe - should get existing data
        let mut reader = buffer.subscribe().await.unwrap();
        let replay = reader.read().await.unwrap();
        assert_eq!(replay, b"existing");

        // Write more data - should get it too
        buffer.write(b" new").await;
        let live = reader.read().await.unwrap();
        assert_eq!(live, b" new");
    }

    #[tokio::test]
    async fn test_late_subscriber_gets_replay_plus_live() {
        let buffer = MultiplexBuffer::new(1024);

        // Early subscriber
        let mut early = buffer.subscribe().await.unwrap();

        // Write some data
        buffer.write(b"first").await;
        assert_eq!(early.read().await.unwrap(), b"first");

        // Late subscriber joins
        let mut late = buffer.subscribe().await.unwrap();

        // Late subscriber gets replay
        let replay = late.read().await.unwrap();
        assert_eq!(replay, b"first");

        // Write more data
        buffer.write(b"second").await;

        // Both get the new data
        assert_eq!(early.read().await.unwrap(), b"second");
        assert_eq!(late.read().await.unwrap(), b"second");
    }

    #[tokio::test]
    async fn test_buffer_truncation() {
        let buffer = MultiplexBuffer::new(10); // Only 10 bytes max

        // Write 15 bytes total
        buffer.write(b"12345").await; // buffer: "12345" (5 bytes)
        buffer.write(b"67890").await; // buffer: "1234567890" (10 bytes)
        buffer.write(b"ABCDE").await; // buffer: "67890ABCDE" (truncated to 10)

        // New subscriber should only see last 10 bytes
        let mut reader = buffer.subscribe().await.unwrap();
        let data = reader.read().await.unwrap();
        assert_eq!(data.len(), 10);
        assert_eq!(data, b"67890ABCDE");
    }

    #[tokio::test]
    async fn test_close_returns_none() {
        let buffer = MultiplexBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(b"data").await;
        assert_eq!(reader.read().await.unwrap(), b"data");

        buffer.close().await;

        // After close, read returns None
        assert!(reader.read().await.is_none());
    }

    #[tokio::test]
    async fn test_subscribe_after_close_returns_none() {
        let buffer = MultiplexBuffer::new(1024);
        buffer.write(b"data").await;
        buffer.close().await;

        // Subscribe after close should return None
        assert!(buffer.subscribe().await.is_none());
    }

    #[tokio::test]
    async fn test_is_closed() {
        let buffer = MultiplexBuffer::new(1024);
        assert!(!buffer.is_closed().await);

        buffer.close().await;
        assert!(buffer.is_closed().await);
    }

    #[tokio::test]
    async fn test_empty_write_is_noop() {
        let buffer = MultiplexBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(b"").await; // Empty write
        buffer.write(b"data").await;

        // Should only receive "data", not an empty chunk
        let data = reader.read().await.unwrap();
        assert_eq!(data, b"data");
    }

    #[tokio::test]
    async fn test_concurrent_subscribe_no_gaps() {
        // This test verifies that subscribing while writes are happening
        // doesn't cause data loss or duplication
        let buffer = MultiplexBuffer::new(10 * 1024 * 1024);

        // Start writing in background
        let buffer_clone = buffer.clone();
        let writer = tokio::spawn(async move {
            for i in 0..100 {
                buffer_clone.write(format!("msg{:03}", i).as_bytes()).await;
                tokio::task::yield_now().await;
            }
        });

        // Subscribe partway through
        tokio::time::sleep(Duration::from_micros(100)).await;
        let mut reader = buffer.subscribe().await.unwrap();

        // Collect all data
        writer.await.unwrap();
        buffer.close().await;

        let mut all_data = Vec::new();
        while let Some(chunk) = reader.read().await {
            all_data.extend(chunk);
        }

        // Verify we got all messages with no gaps
        let data_str = String::from_utf8(all_data).unwrap();

        // Check that messages are in order and no gaps
        // First chunk might be replay (multiple messages), rest are live
        for i in 0..100 {
            let expected = format!("msg{:03}", i);
            assert!(
                data_str.contains(&expected),
                "Missing message: {}",
                expected
            );
        }

        // Check no duplicates by counting occurrences
        for i in 0..100 {
            let expected = format!("msg{:03}", i);
            let count = data_str.matches(&expected).count();
            assert_eq!(count, 1, "Message {} appears {} times", expected, count);
        }
    }

    #[tokio::test]
    async fn test_dropped_reader_doesnt_block_writes() {
        let buffer = MultiplexBuffer::new(1024);
        let reader = buffer.subscribe().await.unwrap();

        // Drop the reader
        drop(reader);

        // Writes should still work (not block)
        let result = timeout(Duration::from_millis(100), buffer.write(b"data")).await;
        assert!(
            result.is_ok(),
            "Write should not block after reader dropped"
        );
    }
}
