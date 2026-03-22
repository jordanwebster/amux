//! Single-writer, multi-reader broadcast buffers for PTY output and structured logs.
//!
//! Both [`MultiplexByteBuffer`] (byte stream) and [`MultiplexStructuredBuffer`] (structured
//! entries) share the same generic core ([`BroadcastBuffer`]) parameterized
//! by a [`BufferPolicy`] that controls storage, truncation, and replay semantics.
//!
//! Key invariant: `write()` and `subscribe()` are mutually exclusive via the
//! storage lock, ensuring no data loss or duplication between replay and live data.

use crate::claude::types::AgentStructuredOutput;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{RwLock, mpsc};

/// Sequenced envelope for structured output entries.
///
/// Every structured output entry gets a monotonically increasing sequence
/// number. Clients must include the latest seq when sending input; the
/// server rejects input with a stale seq.
#[derive(Debug, Clone, PartialEq)]
pub struct StructuredOutput {
    pub seq: u64,
    pub data: AgentStructuredOutput,
}

/// Extra channel capacity beyond the replay snapshot size.
///
/// When a new subscriber joins, the channel must hold both the replayed history
/// and any new items written concurrently. For byte buffers the replay is a
/// single chunk regardless of capacity, so headroom alone is sufficient. For
/// structured buffers the channel needs `capacity + CHANNEL_HEADROOM` slots to
/// hold the per-entry replay plus a margin for concurrent writes.
const CHANNEL_HEADROOM: usize = 256;

// ── Policy trait ─────────────────────────────────────────────────────────

/// Defines how a broadcast buffer stores, truncates, and replays items.
///
/// Two implementations are provided: [`BytePolicy`] for contiguous byte
/// streams and [`StructuredPolicy`] for structured entries.
pub trait BufferPolicy: Send + Sync + 'static {
    /// The message type sent through subscriber channels.
    type Item: Clone + Send + 'static;
    /// Internal storage type.
    type Storage: Send + Sync + Default;

    /// Return `true` to skip a no-op write (e.g. empty byte slice).
    fn should_skip(item: &Self::Item) -> bool;
    /// Append an item to storage.
    fn append(storage: &mut Self::Storage, item: &Self::Item);
    /// Truncate storage to fit within `capacity`.
    fn truncate(storage: &mut Self::Storage, capacity: usize);
    /// Replay existing storage contents to a newly registered subscriber.
    fn replay(storage: &Self::Storage, tx: &mpsc::Sender<Self::Item>);
    /// Channel capacity for a buffer with the given max capacity.
    fn channel_capacity(buffer_capacity: usize) -> usize;
}

// ── Byte buffer policy ──────────────────────────────────────────────────

/// Policy for contiguous byte buffers (PTY output).
///
/// Bytes are appended to a single `Vec<u8>` and truncated by total byte
/// count. Replay sends the entire buffer as one chunk.
pub struct BytePolicy;

impl BufferPolicy for BytePolicy {
    type Item = Vec<u8>;
    type Storage = Vec<u8>;

    fn should_skip(item: &Vec<u8>) -> bool {
        item.is_empty()
    }

    fn append(storage: &mut Vec<u8>, item: &Vec<u8>) {
        storage.extend_from_slice(item);
    }

    fn truncate(storage: &mut Vec<u8>, capacity: usize) {
        if storage.len() > capacity {
            let excess = storage.len() - capacity;
            storage.drain(..excess);
        }
    }

    fn replay(storage: &Vec<u8>, tx: &mpsc::Sender<Vec<u8>>) {
        if !storage.is_empty() {
            let _ = tx.try_send(storage.clone());
        }
    }

    fn channel_capacity(_buffer_capacity: usize) -> usize {
        CHANNEL_HEADROOM
    }
}

// ── Entry buffer policy ─────────────────────────────────────────────────

/// Policy for structured entry buffers (Claude structured I/O).
///
/// Entries are stored in a `Vec` and truncated by entry count. Replay
/// sends each entry individually to preserve message boundaries.
pub struct StructuredPolicy;

impl BufferPolicy for StructuredPolicy {
    type Item = StructuredOutput;
    type Storage = Vec<StructuredOutput>;

    fn should_skip(_item: &StructuredOutput) -> bool {
        false
    }

    fn append(storage: &mut Vec<StructuredOutput>, item: &StructuredOutput) {
        storage.push(item.clone());
    }

    fn truncate(storage: &mut Vec<StructuredOutput>, capacity: usize) {
        if storage.len() > capacity {
            let excess = storage.len() - capacity;
            storage.drain(..excess);
        }
    }

    fn replay(storage: &Vec<StructuredOutput>, tx: &mpsc::Sender<StructuredOutput>) {
        for entry in storage {
            let _ = tx.try_send(entry.clone());
        }
    }

    fn channel_capacity(buffer_capacity: usize) -> usize {
        buffer_capacity + CHANNEL_HEADROOM
    }
}

// ── Generic broadcast buffer ────────────────────────────────────────────

/// A single-writer, multi-reader broadcast buffer.
///
/// Writers append items via [`write()`](Self::write). When a reader
/// [`subscribe()`](Self::subscribe)s, they receive all existing items
/// followed by any new items written after subscription.
///
/// Use via the type aliases [`MultiplexByteBuffer`] (bytes) or
/// [`MultiplexStructuredBuffer`] (structured entries).
pub struct BroadcastBuffer<P: BufferPolicy> {
    inner: Arc<BroadcastInner<P>>,
}

struct BroadcastInner<P: BufferPolicy> {
    storage: RwLock<P::Storage>,
    /// Per-subscriber channels (bounded to prevent memory exhaustion)
    subscribers: RwLock<Vec<mpsc::Sender<P::Item>>>,
    capacity: usize,
    closed: RwLock<bool>,
}

impl<P: BufferPolicy> BroadcastBuffer<P> {
    /// Create a new buffer with the given capacity.
    ///
    /// For byte buffers, capacity is the maximum byte count.
    /// For entry buffers, capacity is the maximum entry count.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(BroadcastInner {
                storage: RwLock::new(P::Storage::default()),
                subscribers: RwLock::new(Vec::new()),
                capacity,
                closed: RwLock::new(false),
            }),
        }
    }

    /// Write an item to the buffer and broadcast to all subscribers.
    ///
    /// Holds the storage write lock during both append and broadcast,
    /// ensuring atomicity with `subscribe()`. Dead or full subscribers
    /// are automatically cleaned up.
    pub async fn write(&self, item: P::Item) {
        if P::should_skip(&item) {
            return;
        }

        let mut storage = self.inner.storage.write().await;
        P::append(&mut storage, &item);
        P::truncate(&mut storage, self.inner.capacity);

        // Broadcast to subscribers, removing any that are disconnected or full.
        // Dropping full subscribers provides backpressure: a consumer that can't
        // keep up gets disconnected rather than causing unbounded memory growth.
        let mut subs = self.inner.subscribers.write().await;
        subs.retain(|tx| tx.try_send(item.clone()).is_ok());
    }

    /// Subscribe to the buffer, receiving all existing items and future writes.
    ///
    /// Holds the storage read lock during both the snapshot and subscription
    /// registration, ensuring atomicity with `write()` and `close()`.
    ///
    /// Returns `None` if the buffer has been closed.
    pub async fn subscribe(&self) -> Option<BroadcastReader<P>> {
        let capacity = P::channel_capacity(self.inner.capacity);
        let (tx, rx) = mpsc::channel(capacity);

        // Acquire storage read lock FIRST to synchronize with both write() (which
        // holds storage write lock) and close() (which also holds storage write lock).
        // The closed check must be inside this lock to prevent a TOCTOU race where
        // close() clears subscribers between our closed check and registration.
        let storage = self.inner.storage.read().await;

        if *self.inner.closed.read().await {
            return None;
        }

        self.inner.subscribers.write().await.push(tx.clone());
        P::replay(&storage, &tx);

        Some(BroadcastReader { rx })
    }

    /// Clear all stored data but keep the buffer open.
    ///
    /// Existing subscribers remain connected and will receive future writes.
    /// Late subscribers will only see data written after the clear.
    pub async fn clear(&self) {
        let mut storage = self.inner.storage.write().await;
        *storage = P::Storage::default();
    }

    /// Close the buffer, causing all readers to receive `None` on their next read.
    ///
    /// Acquires the storage write lock first to synchronize with subscribe(),
    /// which holds the storage read lock. This prevents a race where subscribe()
    /// registers a new subscriber after close() has already cleared the list.
    pub async fn close(&self) {
        let _storage = self.inner.storage.write().await;
        *self.inner.closed.write().await = true;
        self.inner.subscribers.write().await.clear();
    }
}

impl<P: BufferPolicy> Clone for BroadcastBuffer<P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

// ── Generic reader ──────────────────────────────────────────────────────

/// A reader handle for a [`BroadcastBuffer`].
///
/// Each reader receives items independently. When created via `subscribe()`,
/// the reader first receives all existing buffer contents, then any new items
/// written after subscription.
pub struct BroadcastReader<P: BufferPolicy> {
    rx: mpsc::Receiver<P::Item>,
}

impl<P: BufferPolicy> BroadcastReader<P> {
    /// Read the next item.
    ///
    /// Returns `Some(item)` when data is available, or `None` when the
    /// buffer has been closed and no more data will arrive.
    pub async fn read(&mut self) -> Option<P::Item> {
        self.rx.recv().await
    }
}

// ── Type aliases ────────────────────────────────────────────────────────

/// Byte-stream broadcast buffer for PTY output (replay + broadcast).
pub type MultiplexByteBuffer = BroadcastBuffer<BytePolicy>;
/// Reader for [`MultiplexByteBuffer`].
pub type MultiplexByteReader = BroadcastReader<BytePolicy>;

/// Structured broadcast buffer for Claude structured I/O (replay + broadcast).
type MultiplexStructuredBuffer = BroadcastBuffer<StructuredPolicy>;
/// Reader for structured output (yields sequenced envelopes).
pub type MultiplexStructuredReader = BroadcastReader<StructuredPolicy>;

/// Sequenced wrapper around a structured broadcast buffer.
///
/// Assigns monotonically increasing sequence numbers to each entry.
/// Clients include the latest seq when sending input; the server
/// can reject stale input by comparing sequence numbers.
pub struct SequencedStructuredBuffer {
    inner: MultiplexStructuredBuffer,
    seq: AtomicU64,
}

impl SequencedStructuredBuffer {
    /// Create a new sequenced buffer with the given entry capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: MultiplexStructuredBuffer::new(capacity),
            seq: AtomicU64::new(0),
        }
    }

    /// Write an entry, assigning the next sequence number.
    pub async fn write(&self, entry: AgentStructuredOutput) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner
            .write(StructuredOutput { seq, data: entry })
            .await;
    }

    /// Return the current (most recently assigned) sequence number.
    /// Returns 0 if no entries have been written.
    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }

    /// Subscribe to the buffer (replay + live).
    pub async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.inner.subscribe().await
    }

    /// Clear stored data but keep the buffer open.
    /// Does NOT reset the sequence counter.
    pub async fn clear(&self) {
        self.inner.clear().await;
    }

    /// Close the buffer.
    pub async fn close(&self) {
        self.inner.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::ClaudeStructuredOutput;
    use std::time::Duration;
    use tokio::time::timeout;

    // ── Byte buffer tests ───────────────────────────────────────────

    #[tokio::test]
    async fn test_single_subscriber_receives_data() {
        let buffer = MultiplexByteBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(b"hello".to_vec()).await;
        buffer.write(b" world".to_vec()).await;

        let chunk1 = reader.read().await.unwrap();
        let chunk2 = reader.read().await.unwrap();

        assert_eq!(chunk1, b"hello");
        assert_eq!(chunk2, b" world");
    }

    #[tokio::test]
    async fn test_multiple_subscribers_receive_same_data() {
        let buffer = MultiplexByteBuffer::new(1024);
        let mut reader1 = buffer.subscribe().await.unwrap();
        let mut reader2 = buffer.subscribe().await.unwrap();

        buffer.write(b"broadcast".to_vec()).await;

        let data1 = reader1.read().await.unwrap();
        let data2 = reader2.read().await.unwrap();

        assert_eq!(data1, b"broadcast");
        assert_eq!(data2, b"broadcast");
    }

    #[tokio::test]
    async fn test_late_subscriber_gets_replay_plus_live() {
        let buffer = MultiplexByteBuffer::new(1024);

        let mut early = buffer.subscribe().await.unwrap();

        buffer.write(b"first".to_vec()).await;
        assert_eq!(early.read().await.unwrap(), b"first");

        let mut late = buffer.subscribe().await.unwrap();
        let replay = late.read().await.unwrap();
        assert_eq!(replay, b"first");

        buffer.write(b"second".to_vec()).await;

        assert_eq!(early.read().await.unwrap(), b"second");
        assert_eq!(late.read().await.unwrap(), b"second");
    }

    #[tokio::test]
    async fn test_byte_truncation() {
        let buffer = MultiplexByteBuffer::new(10); // Only 10 bytes max

        buffer.write(b"12345".to_vec()).await;
        buffer.write(b"67890".to_vec()).await;
        buffer.write(b"ABCDE".to_vec()).await; // truncated to "67890ABCDE"

        let mut reader = buffer.subscribe().await.unwrap();
        let data = reader.read().await.unwrap();
        assert_eq!(data.len(), 10);
        assert_eq!(data, b"67890ABCDE");
    }

    #[tokio::test]
    async fn test_close_returns_none() {
        let buffer = MultiplexByteBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(b"data".to_vec()).await;
        assert_eq!(reader.read().await.unwrap(), b"data");

        buffer.close().await;
        assert!(reader.read().await.is_none());
    }

    #[tokio::test]
    async fn test_subscribe_after_close_returns_none() {
        let buffer = MultiplexByteBuffer::new(1024);
        buffer.write(b"data".to_vec()).await;
        buffer.close().await;

        assert!(buffer.subscribe().await.is_none());
    }

    #[tokio::test]
    async fn test_empty_write_is_noop() {
        let buffer = MultiplexByteBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(b"".to_vec()).await; // Empty write
        buffer.write(b"data".to_vec()).await;

        let data = reader.read().await.unwrap();
        assert_eq!(data, b"data");
    }

    #[tokio::test]
    async fn test_concurrent_subscribe_no_gaps() {
        let buffer = MultiplexByteBuffer::new(10 * 1024 * 1024);

        let buffer_clone = buffer.clone();
        let writer = tokio::spawn(async move {
            for i in 0..100 {
                buffer_clone
                    .write(format!("msg{:03}", i).into_bytes())
                    .await;
                tokio::task::yield_now().await;
            }
        });

        tokio::time::sleep(Duration::from_micros(100)).await;
        let mut reader = buffer.subscribe().await.unwrap();

        writer.await.unwrap();
        buffer.close().await;

        let mut all_data = Vec::new();
        while let Some(chunk) = reader.read().await {
            all_data.extend(chunk);
        }

        let data_str = String::from_utf8(all_data).unwrap();

        for i in 0..100 {
            let expected = format!("msg{:03}", i);
            assert!(
                data_str.contains(&expected),
                "Missing message: {}",
                expected
            );
        }

        for i in 0..100 {
            let expected = format!("msg{:03}", i);
            let count = data_str.matches(&expected).count();
            assert_eq!(count, 1, "Message {} appears {} times", expected, count);
        }
    }

    #[tokio::test]
    async fn test_dropped_reader_doesnt_block_writes() {
        let buffer = MultiplexByteBuffer::new(1024);
        let reader = buffer.subscribe().await.unwrap();

        drop(reader);

        let result = timeout(Duration::from_millis(100), buffer.write(b"data".to_vec())).await;
        assert!(
            result.is_ok(),
            "Write should not block after reader dropped"
        );
    }

    // ── Sequenced structured buffer tests ─────────────────────────

    fn user_msg(content: &str, uuid: &str) -> AgentStructuredOutput {
        AgentStructuredOutput::Claude(ClaudeStructuredOutput::UserMessage {
            content: content.to_string(),
            timestamp: "2025-01-15T12:00:00Z".to_string(),
            uuid: uuid.to_string(),
        })
    }

    fn envelope(seq: u64, content: &str, uuid: &str) -> StructuredOutput {
        StructuredOutput {
            seq,
            data: user_msg(content, uuid),
        }
    }

    #[tokio::test]
    async fn test_entry_truncation_by_count() {
        let buffer = SequencedStructuredBuffer::new(3); // Only 3 entries max

        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()))
                .await;
        }

        // Late subscriber should see only the last 3 entries (with original seqs)
        let mut reader = buffer.subscribe().await.unwrap();
        assert_eq!(reader.read().await.unwrap(), envelope(3, "msg3", "3"));
        assert_eq!(reader.read().await.unwrap(), envelope(4, "msg4", "4"));
        assert_eq!(reader.read().await.unwrap(), envelope(5, "msg5", "5"));
    }

    #[tokio::test]
    async fn test_entry_per_entry_replay() {
        let buffer = SequencedStructuredBuffer::new(100);

        buffer.write(user_msg("first", "1")).await;
        buffer.write(user_msg("second", "2")).await;
        buffer.write(user_msg("third", "3")).await;

        // Each entry should arrive as a separate read() call
        let mut reader = buffer.subscribe().await.unwrap();
        assert_eq!(reader.read().await.unwrap(), envelope(1, "first", "1"));
        assert_eq!(reader.read().await.unwrap(), envelope(2, "second", "2"));
        assert_eq!(reader.read().await.unwrap(), envelope(3, "third", "3"));

        // Live writes still work
        buffer.write(user_msg("fourth", "4")).await;
        assert_eq!(reader.read().await.unwrap(), envelope(4, "fourth", "4"));
    }

    #[tokio::test]
    async fn test_clear_resets_storage_keeps_subscribers() {
        let buffer = SequencedStructuredBuffer::new(100);

        buffer.write(user_msg("before", "1")).await;
        buffer.write(user_msg("also-before", "2")).await;

        let mut early = buffer.subscribe().await.unwrap();
        assert_eq!(early.read().await.unwrap(), envelope(1, "before", "1"));
        assert_eq!(early.read().await.unwrap(), envelope(2, "also-before", "2"));

        buffer.clear().await;

        let mut late = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("after", "3")).await;

        assert_eq!(early.read().await.unwrap(), envelope(3, "after", "3"));
        assert_eq!(late.read().await.unwrap(), envelope(3, "after", "3"));
    }

    #[tokio::test]
    async fn test_entry_close_returns_none() {
        let buffer = SequencedStructuredBuffer::new(100);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("data", "1")).await;
        assert_eq!(reader.read().await.unwrap(), envelope(1, "data", "1"));

        buffer.close().await;
        assert!(reader.read().await.is_none());
    }

    #[tokio::test]
    async fn test_seq_increments_on_each_write() {
        let buffer = SequencedStructuredBuffer::new(100);
        assert_eq!(buffer.current_seq(), 0);

        buffer.write(user_msg("a", "1")).await;
        assert_eq!(buffer.current_seq(), 1);

        buffer.write(user_msg("b", "2")).await;
        assert_eq!(buffer.current_seq(), 2);

        buffer.write(user_msg("c", "3")).await;
        assert_eq!(buffer.current_seq(), 3);
    }

    #[tokio::test]
    async fn test_seq_survives_clear() {
        let buffer = SequencedStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;
        assert_eq!(buffer.current_seq(), 2);

        buffer.clear().await;
        assert_eq!(buffer.current_seq(), 2, "clear must not reset seq");

        buffer.write(user_msg("c", "3")).await;
        assert_eq!(buffer.current_seq(), 3);

        let mut reader = buffer.subscribe().await.unwrap();
        assert_eq!(reader.read().await.unwrap(), envelope(3, "c", "3"));
    }

    #[tokio::test]
    async fn test_subscribers_receive_correct_seq_in_replay_and_live() {
        let buffer = SequencedStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;

        // Late subscriber gets replay with correct seq values
        let mut reader = buffer.subscribe().await.unwrap();
        let item1 = reader.read().await.unwrap();
        assert_eq!(item1.seq, 1);
        let item2 = reader.read().await.unwrap();
        assert_eq!(item2.seq, 2);

        // Live write has next seq
        buffer.write(user_msg("c", "3")).await;
        let item3 = reader.read().await.unwrap();
        assert_eq!(item3.seq, 3);
    }
}
