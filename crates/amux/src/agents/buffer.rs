//! Single-writer, multi-reader broadcast buffers for PTY output and structured logs.
//!
//! Both [`MultiplexByteBuffer`] (byte stream) and [`MultiplexStructuredBuffer`] (structured
//! entries) share the same generic core ([`BroadcastBuffer`]) parameterized
//! by a [`BufferPolicy`] that controls storage, truncation, and replay semantics.
//!
//! Key invariant: `write()` and `subscribe()` are mutually exclusive via the
//! storage lock, ensuring no data loss or duplication between replay and live data.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{RwLock, mpsc};

/// Replay query for sequenced output buffers.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SequencedReplayQuery {
    /// Replay entries with `seq >= seq`.
    Since { seq: u64 },
    /// Replay only the last `count` entries.
    Tail { count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ByteReplayQuery {
    Tail { count: u64 },
}

/// Sequenced envelope for structured output entries.
///
/// Every structured output entry gets a monotonically increasing sequence
/// number. Clients must include the latest seq when sending input; the
/// server rejects input with a stale seq. The payload is opaque JSON —
/// amux transports it without interpreting the schema.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StructuredOutput {
    pub(crate) seq: u64,
    pub(crate) payload: Value,
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
pub(crate) trait BufferPolicy: Send + Sync + 'static {
    /// The caller-provided value published into the buffer.
    type Input: Send + 'static;
    /// The message type sent through subscriber channels.
    type Item: Clone + Send + 'static;
    /// Internal storage type.
    type Storage: Send + Sync + Default;
    /// Filter applied during replay to control which entries are sent.
    type Filter: Send + Sync + Default;

    /// Publish one logical input into storage, returning the broadcast item.
    ///
    /// Return `None` to skip a no-op publish (e.g. empty byte slice).
    fn publish(
        storage: &mut Self::Storage,
        input: Self::Input,
        capacity: usize,
    ) -> Option<Self::Item>;
    /// Replay existing storage contents to a newly registered subscriber,
    /// filtered by the given filter. The default filter replays everything.
    ///
    /// Returns the number of replay items actually queued.
    fn replay(
        storage: &Self::Storage,
        tx: &mpsc::Sender<Self::Item>,
        filter: &Self::Filter,
    ) -> usize;
    /// Clear stored data while preserving any policy-owned metadata.
    fn clear(storage: &mut Self::Storage);
    /// Channel capacity for a buffer with the given max capacity.
    fn channel_capacity(buffer_capacity: usize) -> usize;
}

// ── Byte buffer policy ──────────────────────────────────────────────────

/// Policy for contiguous byte buffers (PTY output).
///
/// Bytes are appended to a single `Vec<u8>` and truncated by total byte
/// count. Replay sends the entire buffer as one chunk.
pub(crate) struct BytePolicy;

impl BufferPolicy for BytePolicy {
    type Input = Vec<u8>;
    type Item = Vec<u8>;
    type Storage = Vec<u8>;
    type Filter = Option<ByteReplayQuery>;

    fn publish(storage: &mut Vec<u8>, input: Vec<u8>, capacity: usize) -> Option<Vec<u8>> {
        if input.is_empty() {
            return None;
        }

        storage.extend_from_slice(&input);
        if storage.len() > capacity {
            let excess = storage.len() - capacity;
            storage.drain(..excess);
        }

        Some(input)
    }

    fn replay(
        storage: &Vec<u8>,
        tx: &mpsc::Sender<Vec<u8>>,
        filter: &Option<ByteReplayQuery>,
    ) -> usize {
        let replay = match filter {
            None => storage.as_slice(),
            Some(ByteReplayQuery::Tail { count }) => {
                let count = usize::try_from(*count).unwrap_or(usize::MAX);
                let start = storage.len().saturating_sub(count);
                &storage[start..]
            }
        };

        if !replay.is_empty() {
            usize::from(tx.try_send(replay.to_vec()).is_ok())
        } else {
            0
        }
    }

    fn clear(storage: &mut Vec<u8>) {
        storage.clear();
    }

    fn channel_capacity(_buffer_capacity: usize) -> usize {
        CHANNEL_HEADROOM
    }
}

// ── Entry buffer policy ─────────────────────────────────────────────────

/// Policy for structured entry buffers (Claude structured I/O).
///
/// Entries are stored in a `Vec` and truncated by entry count. The storage also
/// tracks the most recently published sequence number. Replay sends each entry
/// individually to preserve message boundaries.
pub(crate) struct StructuredPolicy;

#[derive(Default)]
#[doc(hidden)]
pub(crate) struct StructuredStorage {
    entries: Vec<StructuredOutput>,
    last_seq: u64,
}

impl BufferPolicy for StructuredPolicy {
    type Input = Value;
    type Item = StructuredOutput;
    type Storage = StructuredStorage;
    type Filter = Option<SequencedReplayQuery>;

    fn publish(
        storage: &mut StructuredStorage,
        input: Value,
        capacity: usize,
    ) -> Option<StructuredOutput> {
        storage.last_seq += 1;
        let item = StructuredOutput {
            seq: storage.last_seq,
            payload: input,
        };

        storage.entries.push(item.clone());
        if storage.entries.len() > capacity {
            let excess = storage.entries.len() - capacity;
            storage.entries.drain(..excess);
        }

        Some(item)
    }

    fn replay(
        storage: &StructuredStorage,
        tx: &mpsc::Sender<StructuredOutput>,
        filter: &Option<SequencedReplayQuery>,
    ) -> usize {
        structured_replay_entries(storage, filter)
            .iter()
            .filter(|entry| tx.try_send((*entry).clone()).is_ok())
            .count()
    }

    fn clear(storage: &mut StructuredStorage) {
        storage.entries.clear();
    }

    fn channel_capacity(buffer_capacity: usize) -> usize {
        buffer_capacity + CHANNEL_HEADROOM
    }
}

fn structured_replay_entries<'a>(
    storage: &'a StructuredStorage,
    filter: &Option<SequencedReplayQuery>,
) -> &'a [StructuredOutput] {
    match filter {
        None => &storage.entries[..],
        Some(SequencedReplayQuery::Since { seq }) => {
            let start = storage.entries.partition_point(|e| e.seq < *seq);
            &storage.entries[start..]
        }
        Some(SequencedReplayQuery::Tail { count }) => {
            let start = storage.entries.len().saturating_sub(*count as usize);
            &storage.entries[start..]
        }
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
pub(crate) struct BroadcastBuffer<P: BufferPolicy> {
    inner: Arc<BroadcastInner<P>>,
}

struct BroadcastInner<P: BufferPolicy> {
    storage: RwLock<P::Storage>,
    /// Per-subscriber channels (bounded to prevent memory exhaustion)
    subscribers: RwLock<Vec<BroadcastSubscriber<P::Item>>>,
    capacity: usize,
    closed: RwLock<bool>,
}

struct BroadcastSubscriber<T> {
    tx: mpsc::Sender<T>,
    overflowed: Arc<AtomicBool>,
}

impl<P: BufferPolicy> BroadcastBuffer<P> {
    /// Create a new buffer with the given capacity.
    ///
    /// For byte buffers, capacity is the maximum byte count.
    /// For entry buffers, capacity is the maximum entry count.
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(BroadcastInner {
                storage: RwLock::new(P::Storage::default()),
                subscribers: RwLock::new(Vec::new()),
                capacity,
                closed: RwLock::new(false),
            }),
        }
    }

    /// Publish an input into the buffer and broadcast the resulting item.
    ///
    /// Holds the storage write lock during both publication and broadcast,
    /// ensuring atomicity with `subscribe()`. Dead or full subscribers are
    /// automatically cleaned up.
    pub(crate) async fn write(&self, input: P::Input) -> Option<P::Item> {
        let mut storage = self.inner.storage.write().await;
        let item = P::publish(&mut storage, input, self.inner.capacity)?;

        // Broadcast to subscribers, removing any that are disconnected or full.
        // Dropping full subscribers provides backpressure: a consumer that can't
        // keep up gets disconnected rather than causing unbounded memory growth.
        let mut subs = self.inner.subscribers.write().await;
        subs.retain(|subscriber| match subscriber.tx.try_send(item.clone()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                subscriber.overflowed.store(true, Ordering::Release);
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        });

        Some(item)
    }

    /// Subscribe to the buffer, receiving all existing items and future writes.
    ///
    /// Holds the storage read lock during both the snapshot and subscription
    /// registration, ensuring atomicity with `write()` and `close()`.
    ///
    /// Returns `None` if the buffer has been closed.
    pub(crate) async fn subscribe(&self) -> Option<BroadcastReader<P>> {
        self.subscribe_filtered(P::Filter::default(), |_| ())
            .await
            .map(|(reader, ())| reader)
    }

    /// Subscribe with a replay filter and a snapshot function.
    ///
    /// The filter controls which stored entries are replayed. The default
    /// filter replays everything. The snapshot is read under the same storage
    /// lock to ensure consistency.
    pub(crate) async fn subscribe_filtered<R>(
        &self,
        filter: P::Filter,
        snapshot: impl FnOnce(&P::Storage) -> R,
    ) -> Option<(BroadcastReader<P>, R)> {
        let capacity = P::channel_capacity(self.inner.capacity);
        let (tx, rx) = mpsc::channel(capacity);
        let overflowed = Arc::new(AtomicBool::new(false));

        // Acquire storage read lock FIRST to synchronize with both write() (which
        // holds storage write lock) and close() (which also holds storage write lock).
        // The closed check must be inside this lock to prevent a TOCTOU race where
        // close() clears subscribers between our closed check and registration.
        let storage = self.inner.storage.read().await;

        if *self.inner.closed.read().await {
            return None;
        }

        let snapshot = snapshot(&storage);
        self.inner
            .subscribers
            .write()
            .await
            .push(BroadcastSubscriber {
                tx: tx.clone(),
                overflowed: overflowed.clone(),
            });
        let replay_remaining = P::replay(&storage, &tx, &filter);

        Some((
            BroadcastReader {
                rx,
                overflowed,
                replay_remaining,
                replay_complete_emitted: false,
            },
            snapshot,
        ))
    }

    /// Inspect storage under the buffer's read lock.
    pub(crate) async fn inspect<R>(&self, inspect: impl FnOnce(&P::Storage) -> R) -> R {
        let storage = self.inner.storage.read().await;
        inspect(&storage)
    }

    /// Clear all stored data but keep the buffer open.
    ///
    /// Existing subscribers remain connected and will receive future writes.
    /// Late subscribers will only see data written after the clear.
    pub(crate) async fn clear(&self) {
        let mut storage = self.inner.storage.write().await;
        P::clear(&mut storage);
    }

    /// Close the buffer, causing all readers to receive `None` on their next read.
    ///
    /// Acquires the storage write lock first to synchronize with subscribe(),
    /// which holds the storage read lock. This prevents a race where subscribe()
    /// registers a new subscriber after close() has already cleared the list.
    pub(crate) async fn close(&self) {
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
pub(crate) struct BroadcastReader<P: BufferPolicy> {
    rx: mpsc::Receiver<P::Item>,
    overflowed: Arc<AtomicBool>,
    replay_remaining: usize,
    replay_complete_emitted: bool,
}

/// Event read from a broadcast buffer.
pub(crate) enum BroadcastRead<P: BufferPolicy> {
    ReplayItem(P::Item),
    ReplayComplete,
    LiveItem(P::Item),
    Lagged,
}

impl<P: BufferPolicy> BroadcastReader<P> {
    /// Read the next replay/live event.
    ///
    /// `ReplayComplete` is emitted exactly once: immediately for an empty
    /// replay, or immediately after the final replay item has been read.
    pub(crate) async fn read_event(&mut self) -> Option<BroadcastRead<P>> {
        if self.replay_remaining == 0 && !self.replay_complete_emitted {
            self.replay_complete_emitted = true;
            return Some(BroadcastRead::ReplayComplete);
        }

        let Some(item) = self.rx.recv().await else {
            return self
                .overflowed
                .swap(false, Ordering::AcqRel)
                .then_some(BroadcastRead::Lagged);
        };
        if self.replay_remaining > 0 {
            self.replay_remaining -= 1;
            Some(BroadcastRead::ReplayItem(item))
        } else {
            Some(BroadcastRead::LiveItem(item))
        }
    }

    /// Read the next item.
    ///
    /// Returns `Some(item)` when data is available, or `None` when the
    /// buffer has been closed and no more data will arrive.
    pub(crate) async fn read(&mut self) -> Option<P::Item> {
        loop {
            match self.read_event().await? {
                BroadcastRead::ReplayItem(item) | BroadcastRead::LiveItem(item) => {
                    return Some(item);
                }
                BroadcastRead::ReplayComplete => {}
                BroadcastRead::Lagged => return None,
            }
        }
    }
}

// ── Type aliases ────────────────────────────────────────────────────────

/// Byte-stream broadcast buffer for PTY output (replay + broadcast).
pub(crate) type MultiplexByteBuffer = BroadcastBuffer<BytePolicy>;
/// Reader for [`MultiplexByteBuffer`].
pub(crate) type MultiplexByteReader = BroadcastReader<BytePolicy>;

/// Structured broadcast buffer for Claude structured I/O (replay + broadcast).
pub(crate) type MultiplexStructuredBuffer = BroadcastBuffer<StructuredPolicy>;
/// Reader for structured output (yields sequenced envelopes).
pub(crate) type MultiplexStructuredReader = BroadcastReader<StructuredPolicy>;

impl BroadcastBuffer<BytePolicy> {
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<ByteReplayQuery>,
    ) -> Option<MultiplexByteReader> {
        self.subscribe_filtered(query, |_| ())
            .await
            .map(|(reader, ())| reader)
    }
}

impl BroadcastBuffer<StructuredPolicy> {
    /// Return the current structured output sequence number.
    ///
    /// Returns 0 if no entries have been published.
    pub(crate) async fn current_seq(&self) -> u64 {
        self.inspect(|storage| storage.last_seq).await
    }

    /// Subscribe to structured output with an optional query filter and return
    /// the sequence number that matches the replayed snapshot.
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<SequencedReplayQuery>,
    ) -> Option<(MultiplexStructuredReader, u64)> {
        self.subscribe_filtered(query, |storage| storage.last_seq)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::Value;
    use tokio::time::timeout;

    use super::*;

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
    async fn test_byte_reader_tags_replay_boundary() {
        let buffer = MultiplexByteBuffer::new(1024);
        buffer.write(b"replay".to_vec()).await;

        let mut reader = buffer.subscribe().await.unwrap();
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayItem(item) if item == b"replay"
        ));
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));

        buffer.write(b"live".to_vec()).await;
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::LiveItem(item) if item == b"live"
        ));
    }

    #[tokio::test]
    async fn test_byte_reader_reports_empty_replay_complete_immediately() {
        let buffer = MultiplexByteBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));

        buffer.write(b"live".to_vec()).await;
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::LiveItem(item) if item == b"live"
        ));
    }

    #[tokio::test]
    async fn test_byte_reader_replay_tail_counts_bytes() {
        let buffer = MultiplexByteBuffer::new(1024);
        buffer.write(b"abcdef".to_vec()).await;

        let mut reader = buffer
            .subscribe_with_query(Some(ByteReplayQuery::Tail { count: 3 }))
            .await
            .unwrap();
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayItem(item) if item == b"def"
        ));
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));
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

    #[tokio::test]
    async fn test_full_subscriber_reports_lagged() {
        let buffer = MultiplexByteBuffer::new(1024);
        let mut reader = buffer.subscribe().await.unwrap();

        for _ in 0..(CHANNEL_HEADROOM + 1) {
            buffer.write(b"x".to_vec()).await;
        }

        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));
        for _ in 0..CHANNEL_HEADROOM {
            assert!(matches!(
                reader.read_event().await.unwrap(),
                BroadcastRead::LiveItem(item) if item == b"x"
            ));
        }
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::Lagged
        ));
        assert!(reader.read_event().await.is_none());
    }

    // ── Sequenced structured buffer tests ─────────────────────────

    fn user_msg(content: &str, uuid: &str) -> Value {
        serde_json::json!({
            "type": "user",
            "message": {"content": content},
            "uuid": uuid,
            "timestamp": "2025-01-15T12:00:00Z"
        })
    }

    fn envelope(seq: u64, content: &str, uuid: &str) -> StructuredOutput {
        StructuredOutput {
            seq,
            payload: user_msg(content, uuid),
        }
    }

    #[tokio::test]
    async fn test_entry_truncation_by_count() {
        let buffer = MultiplexStructuredBuffer::new(3); // Only 3 entries max

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
        let buffer = MultiplexStructuredBuffer::new(100);

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
        let buffer = MultiplexStructuredBuffer::new(100);

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
        let buffer = MultiplexStructuredBuffer::new(100);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("data", "1")).await;
        assert_eq!(reader.read().await.unwrap(), envelope(1, "data", "1"));

        buffer.close().await;
        assert!(reader.read().await.is_none());
    }

    #[tokio::test]
    async fn test_seq_increments_on_each_write() {
        let buffer = MultiplexStructuredBuffer::new(100);
        assert_eq!(buffer.current_seq().await, 0);

        buffer.write(user_msg("a", "1")).await;
        assert_eq!(buffer.current_seq().await, 1);

        buffer.write(user_msg("b", "2")).await;
        assert_eq!(buffer.current_seq().await, 2);

        buffer.write(user_msg("c", "3")).await;
        assert_eq!(buffer.current_seq().await, 3);
    }

    #[tokio::test]
    async fn test_seq_survives_clear() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;
        assert_eq!(buffer.current_seq().await, 2);

        buffer.clear().await;
        assert_eq!(buffer.current_seq().await, 2, "clear must not reset seq");

        buffer.write(user_msg("c", "3")).await;
        assert_eq!(buffer.current_seq().await, 3);

        let mut reader = buffer.subscribe().await.unwrap();
        assert_eq!(reader.read().await.unwrap(), envelope(3, "c", "3"));
    }

    #[tokio::test]
    async fn test_subscribers_receive_correct_seq_in_replay_and_live() {
        let buffer = MultiplexStructuredBuffer::new(100);

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

    #[tokio::test]
    async fn test_structured_reader_tags_filtered_replay_boundary() {
        let buffer = MultiplexStructuredBuffer::new(100);

        for i in 1..=3 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()))
                .await;
        }

        let query = Some(SequencedReplayQuery::Tail { count: 2 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 3);

        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayItem(item) if item == envelope(2, "msg2", "2")
        ));

        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayItem(item) if item == envelope(3, "msg3", "3")
        ));

        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));

        buffer.write(user_msg("msg4", "4")).await;
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::LiveItem(item) if item == envelope(4, "msg4", "4")
        ));
    }

    #[tokio::test]
    async fn test_subscribe_with_query_none_replays_all() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;

        let (mut reader, seq) = buffer.subscribe_with_query(None).await.unwrap();
        assert_eq!(seq, 2);
        assert_eq!(reader.read().await.unwrap(), envelope(1, "a", "1"));
        assert_eq!(reader.read().await.unwrap(), envelope(2, "b", "2"));
    }

    // ── SequencedReplayQuery tests ─────────────────────────────────────

    #[tokio::test]
    async fn test_query_since_mid_buffer() {
        let buffer = MultiplexStructuredBuffer::new(100);

        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()))
                .await;
        }

        let query = Some(SequencedReplayQuery::Since { seq: 3 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 5);
        assert_eq!(reader.read().await.unwrap(), envelope(3, "msg3", "3"));
        assert_eq!(reader.read().await.unwrap(), envelope(4, "msg4", "4"));
        assert_eq!(reader.read().await.unwrap(), envelope(5, "msg5", "5"));
    }

    #[tokio::test]
    async fn test_query_since_older_than_oldest() {
        let buffer = MultiplexStructuredBuffer::new(3);

        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()))
                .await;
        }

        // seq 1 has been evicted; Since { seq: 1 } replays everything available
        let query = Some(SequencedReplayQuery::Since { seq: 1 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 5);
        assert_eq!(reader.read().await.unwrap(), envelope(3, "msg3", "3"));
        assert_eq!(reader.read().await.unwrap(), envelope(4, "msg4", "4"));
        assert_eq!(reader.read().await.unwrap(), envelope(5, "msg5", "5"));
    }

    #[tokio::test]
    async fn test_query_since_beyond_current() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;

        // seq 10 is beyond current (2); no replay, but live writes still arrive
        let query = Some(SequencedReplayQuery::Since { seq: 10 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 2);

        buffer.write(user_msg("c", "3")).await;
        assert_eq!(reader.read().await.unwrap(), envelope(3, "c", "3"));
    }

    #[tokio::test]
    async fn test_query_tail_less_than_buffer() {
        let buffer = MultiplexStructuredBuffer::new(100);

        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()))
                .await;
        }

        let query = Some(SequencedReplayQuery::Tail { count: 2 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 5);
        assert_eq!(reader.read().await.unwrap(), envelope(4, "msg4", "4"));
        assert_eq!(reader.read().await.unwrap(), envelope(5, "msg5", "5"));
    }

    #[tokio::test]
    async fn test_query_tail_greater_than_buffer() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;

        // Tail { count: 100 } with only 2 entries replays everything
        let query = Some(SequencedReplayQuery::Tail { count: 100 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 2);
        assert_eq!(reader.read().await.unwrap(), envelope(1, "a", "1"));
        assert_eq!(reader.read().await.unwrap(), envelope(2, "b", "2"));
    }

    #[tokio::test]
    async fn test_query_tail_zero() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1")).await;
        buffer.write(user_msg("b", "2")).await;

        // Tail { count: 0 } replays nothing, but live writes still arrive
        let query = Some(SequencedReplayQuery::Tail { count: 0 });
        let (mut reader, seq) = buffer.subscribe_with_query(query).await.unwrap();
        assert_eq!(seq, 2);

        buffer.write(user_msg("c", "3")).await;
        assert_eq!(reader.read().await.unwrap(), envelope(3, "c", "3"));
    }
}
