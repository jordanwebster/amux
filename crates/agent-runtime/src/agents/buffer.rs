//! Single-writer, multi-reader broadcast buffers for PTY output and structured logs.
//!
//! Both [`MultiplexByteBuffer`] (byte stream) and [`MultiplexStructuredBuffer`] (structured
//! entries) share the same generic core ([`BroadcastBuffer`]) parameterized
//! by a [`BufferPolicy`] that controls storage, truncation, and replay semantics.
//!
//! Key invariant: `write()` and `subscribe()` are mutually exclusive via the
//! storage lock, ensuring no data loss or duplication between replay and live data.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use model::{ReplayFacts, ReplayOutcome, ReplayQuery};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;

use super::log_source::RingPolicy;
use super::{BufferDebug, OutputDebug};

/// Replay query for sequenced output buffers.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SequencedReplayQuery {
    After { after: u64, tail_bound: Option<u64> },
    TailCount { count: u64, tail_bound: Option<u64> },
}

impl SequencedReplayQuery {
    pub(crate) fn from_replay(query: ReplayQuery) -> Self {
        match query {
            ReplayQuery::After { after, tail_bound } => Self::After { after, tail_bound },
            ReplayQuery::TailCount { count, tail_bound } => Self::TailCount { count, tail_bound },
        }
    }
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
    pub(crate) published_at_unix_ms: i64,
    pub(crate) activity_at_unix_ms: Option<i64>,
    pub(crate) historical: bool,
    pub(crate) payload: Value,
    pub(crate) encoded_len: usize,
}

#[derive(Debug)]
pub(crate) struct StructuredPublication {
    pub(crate) payload: Value,
    pub(crate) activity_at_unix_ms: Option<i64>,
    pub(crate) historical: bool,
}

impl From<Value> for StructuredPublication {
    fn from(payload: Value) -> Self {
        Self {
            payload,
            activity_at_unix_ms: None,
            historical: false,
        }
    }
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
    /// Retained sequence coordinates and encoded size for diagnostics.
    fn debug(storage: &Self::Storage) -> BufferDebug;
}

// ── Byte buffer policy ──────────────────────────────────────────────────

/// Policy for contiguous byte buffers (PTY output).
///
/// Bytes are appended to a single `Vec<u8>` and truncated by total byte
/// count. Replay sends the entire buffer as one chunk.
pub(crate) struct BytePolicy;

#[derive(Default)]
pub(crate) struct ByteStorage {
    bytes: Vec<u8>,
    tail_seq: u64,
}

impl BufferPolicy for BytePolicy {
    type Input = Vec<u8>;
    type Item = Vec<u8>;
    type Storage = ByteStorage;
    type Filter = Option<ByteReplayQuery>;

    fn publish(storage: &mut ByteStorage, input: Vec<u8>, capacity: usize) -> Option<Vec<u8>> {
        if input.is_empty() {
            return None;
        }

        storage.tail_seq = storage
            .tail_seq
            .saturating_add(u64::try_from(input.len()).unwrap_or(u64::MAX));
        storage.bytes.extend_from_slice(&input);
        if storage.bytes.len() > capacity {
            let excess = storage.bytes.len() - capacity;
            storage.bytes.drain(..excess);
        }

        Some(input)
    }

    fn replay(
        storage: &ByteStorage,
        tx: &mpsc::Sender<Vec<u8>>,
        filter: &Option<ByteReplayQuery>,
    ) -> usize {
        let replay = match filter {
            None => storage.bytes.as_slice(),
            Some(ByteReplayQuery::Tail { count }) => {
                let count = usize::try_from(*count).unwrap_or(usize::MAX);
                let start = storage.bytes.len().saturating_sub(count);
                &storage.bytes[start..]
            }
        };

        if !replay.is_empty() {
            usize::from(tx.try_send(replay.to_vec()).is_ok())
        } else {
            0
        }
    }

    fn clear(storage: &mut ByteStorage) {
        storage.bytes.clear();
    }

    fn channel_capacity(_buffer_capacity: usize) -> usize {
        CHANNEL_HEADROOM
    }

    fn debug(storage: &ByteStorage) -> BufferDebug {
        BufferDebug {
            head_seq: storage
                .tail_seq
                .saturating_sub(u64::try_from(storage.bytes.len()).unwrap_or(u64::MAX)),
            tail_seq: storage.tail_seq,
            bytes: storage.bytes.len(),
        }
    }
}

// ── Entry buffer policy ─────────────────────────────────────────────────

/// Policy for structured entry buffers (Claude structured I/O).
///
/// Entries are stored in a `Vec` and truncated by entry count. The storage also
/// tracks the most recently published sequence number. Replay sends each entry
/// individually to preserve message boundaries.
pub(crate) struct StructuredPolicy;

#[doc(hidden)]
pub(crate) struct StructuredStorage {
    entries: Vec<StructuredOutput>,
    last_seq: u64,
    reset_at: u64,
    retained_bytes: usize,
    policy: RingPolicy,
    last_published_at: Option<Instant>,
}

impl Default for StructuredStorage {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            last_seq: 0,
            reset_at: 0,
            retained_bytes: 0,
            policy: RingPolicy::test(usize::MAX),
            last_published_at: None,
        }
    }
}

impl BufferPolicy for StructuredPolicy {
    type Input = StructuredPublication;
    type Item = StructuredOutput;
    type Storage = StructuredStorage;
    type Filter = Option<SequencedReplayQuery>;

    fn publish(
        storage: &mut StructuredStorage,
        input: StructuredPublication,
        capacity: usize,
    ) -> Option<StructuredOutput> {
        storage.last_seq = storage.last_seq.checked_add(1)?;
        let encoded_len = serde_json::to_vec(&input.payload).map_or(0, |encoded| encoded.len());
        let item = StructuredOutput {
            seq: storage.last_seq,
            published_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            activity_at_unix_ms: input.activity_at_unix_ms,
            historical: input.historical,
            payload: input.payload,
            encoded_len,
        };

        storage.entries.push(item.clone());
        storage.retained_bytes = storage.retained_bytes.saturating_add(encoded_len);
        storage.last_published_at = Some(Instant::now());
        trim_structured(
            storage,
            storage.policy.max_bytes,
            storage.policy.max_rows.min(capacity),
        );

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
        storage.retained_bytes = 0;
    }

    fn channel_capacity(buffer_capacity: usize) -> usize {
        buffer_capacity + CHANNEL_HEADROOM
    }

    fn debug(storage: &StructuredStorage) -> BufferDebug {
        let head_seq = storage
            .entries
            .first()
            .map(|entry| entry.seq)
            .unwrap_or_else(|| {
                storage
                    .last_seq
                    .saturating_add(u64::from(storage.last_seq > 0))
            });
        let tail_seq = storage
            .entries
            .last()
            .map(|entry| entry.seq.saturating_add(1))
            .unwrap_or(head_seq);
        BufferDebug {
            head_seq,
            tail_seq,
            bytes: storage.retained_bytes,
        }
    }
}

fn trim_structured(storage: &mut StructuredStorage, max_bytes: usize, max_rows: usize) {
    let mut remove = 0;
    let mut retained_bytes = storage.retained_bytes;
    while storage.entries.len().saturating_sub(remove) > max_rows || retained_bytes > max_bytes {
        let Some(entry) = storage.entries.get(remove) else {
            break;
        };
        retained_bytes = retained_bytes.saturating_sub(entry.encoded_len);
        remove += 1;
    }
    if remove > 0 {
        storage.entries.drain(..remove);
        storage.retained_bytes = retained_bytes;
    }
}

fn structured_replay_entries<'a>(
    storage: &'a StructuredStorage,
    filter: &Option<SequencedReplayQuery>,
) -> &'a [StructuredOutput] {
    match filter {
        None => &storage.entries[..],
        Some(SequencedReplayQuery::After { after, tail_bound }) => {
            if structured_replay_has_gap(storage, *after) {
                match tail_bound {
                    Some(count) => structured_tail(&storage.entries, *count),
                    None => &storage.entries,
                }
            } else {
                let start = storage.entries.partition_point(|entry| entry.seq <= *after);
                let rows = &storage.entries[start..];
                match tail_bound {
                    Some(count) => structured_tail(rows, *count),
                    None => rows,
                }
            }
        }
        Some(SequencedReplayQuery::TailCount { count, tail_bound }) => structured_tail(
            &storage.entries,
            (*count).min(tail_bound.unwrap_or(u64::MAX)),
        ),
    }
}

fn structured_replay_has_gap(storage: &StructuredStorage, after: u64) -> bool {
    let cursor_is_ahead = after > storage.last_seq;
    cursor_is_ahead
        || storage
            .entries
            .first()
            .map_or(storage.last_seq > after, |entry| {
                after < entry.seq.saturating_sub(1)
            })
}

fn structured_tail(entries: &[StructuredOutput], count: u64) -> &[StructuredOutput] {
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    let start = entries.len().saturating_sub(count);
    &entries[start..]
}

fn structured_replay_facts(
    storage: &StructuredStorage,
    filter: &Option<SequencedReplayQuery>,
) -> ReplayFacts {
    let replay = structured_replay_entries(storage, filter);
    let retained_from = storage.entries.first().map_or(0, |entry| entry.seq);
    let selected_from = replay.first().map_or(0, |entry| entry.seq);
    let outcome = match filter {
        Some(SequencedReplayQuery::After { after, .. }) if *after > storage.last_seq => {
            ReplayOutcome::Reset {
                reason: "ahead".to_string(),
            }
        }
        Some(SequencedReplayQuery::After { after, .. }) if *after < storage.reset_at => {
            ReplayOutcome::Reset {
                reason: "reset".to_string(),
            }
        }
        Some(SequencedReplayQuery::After { after, .. })
            if structured_replay_has_gap(storage, *after) =>
        {
            ReplayOutcome::Truncated {
                missing_after: *after,
            }
        }
        Some(SequencedReplayQuery::After { after, .. })
            if selected_from > after.saturating_add(1) =>
        {
            ReplayOutcome::Truncated {
                missing_after: *after,
            }
        }
        Some(SequencedReplayQuery::TailCount { .. }) if selected_from > 1 => {
            ReplayOutcome::Truncated {
                missing_after: selected_from - 1,
            }
        }
        Some(SequencedReplayQuery::TailCount { .. })
            if selected_from == 0 && storage.last_seq > 0 =>
        {
            ReplayOutcome::Truncated {
                missing_after: storage.last_seq,
            }
        }
        _ => ReplayOutcome::Continuous,
    };
    ReplayFacts {
        retained_from,
        through: storage.last_seq,
        selected_from,
        reset_at: storage.reset_at,
        outcome,
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
    epoch: AtomicU64,
}

struct BroadcastSubscriber<T> {
    tx: mpsc::Sender<T>,
    overflowed: Arc<AtomicBool>,
    reset: Arc<AtomicBool>,
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
                epoch: AtomicU64::new(0),
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
    #[allow(dead_code)]
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
        let reset = Arc::new(AtomicBool::new(false));

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
                reset: reset.clone(),
            });
        let replay_remaining = P::replay(&storage, &tx, &filter);

        Some((
            BroadcastReader {
                rx,
                overflowed,
                reset,
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

    /// Snapshot retained coordinates and live subscribers under the same lock
    /// ordering used by write and subscribe.
    pub(crate) async fn debug_snapshot(&self) -> OutputDebug {
        let storage = self.inner.storage.read().await;
        let subscribers = self.inner.subscribers.read().await;
        OutputDebug {
            epoch: self.inner.epoch.load(Ordering::Relaxed),
            subscriber_count: subscribers
                .iter()
                .filter(|subscriber| !subscriber.tx.is_closed())
                .count(),
            buffer: P::debug(&storage),
            closed: *self.inner.closed.read().await,
        }
    }

    /// Clear all stored data but keep the buffer open.
    ///
    /// Existing subscribers remain connected and will receive future writes.
    /// Late subscribers will only see data written after the clear.
    #[allow(dead_code)]
    pub(crate) async fn clear(&self) {
        let mut storage = self.inner.storage.write().await;
        P::clear(&mut storage);
        self.inner.epoch.fetch_add(1, Ordering::Relaxed);
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
    reset: Arc<AtomicBool>,
    replay_remaining: usize,
    replay_complete_emitted: bool,
}

/// Event read from a broadcast buffer.
pub(crate) enum BroadcastRead<P: BufferPolicy> {
    ReplayItem(P::Item),
    ReplayComplete,
    LiveItem(P::Item),
    Lagged,
    Reset,
}

impl<P: BufferPolicy> BroadcastReader<P> {
    /// Read the next replay/live event.
    ///
    /// `ReplayComplete` is emitted exactly once: immediately for an empty
    /// replay, or immediately after the final replay item has been read.
    pub(crate) async fn read_event(&mut self) -> Option<BroadcastRead<P>> {
        if self.reset.swap(false, Ordering::AcqRel) {
            self.rx.close();
            return Some(BroadcastRead::Reset);
        }
        if self.replay_remaining == 0 && !self.replay_complete_emitted {
            self.replay_complete_emitted = true;
            return Some(BroadcastRead::ReplayComplete);
        }

        let Some(item) = self.rx.recv().await else {
            if self.reset.swap(false, Ordering::AcqRel) {
                return Some(BroadcastRead::Reset);
            }
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
    ///
    /// Production readers consume [`Self::read_event`] because they act on
    /// lag and epoch changes; only tests and the derived-rows fixture
    /// harness want items alone, so this exists under the harness's gate.
    pub(crate) async fn read(&mut self) -> Option<P::Item> {
        loop {
            match self.read_event().await? {
                BroadcastRead::ReplayItem(item) | BroadcastRead::LiveItem(item) => {
                    return Some(item);
                }
                BroadcastRead::ReplayComplete => {}
                BroadcastRead::Lagged | BroadcastRead::Reset => return None,
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
    pub(crate) fn with_policy(policy: RingPolicy) -> Self {
        Self::with_policy_and_last_seq(policy, 0)
    }

    pub(crate) fn with_policy_and_last_seq(policy: RingPolicy, last_seq: u64) -> Self {
        Self {
            inner: Arc::new(BroadcastInner {
                storage: RwLock::new(StructuredStorage {
                    entries: Vec::new(),
                    last_seq,
                    reset_at: 0,
                    retained_bytes: 0,
                    policy,
                    last_published_at: None,
                }),
                subscribers: RwLock::new(Vec::new()),
                capacity: policy.max_rows,
                closed: RwLock::new(false),
                epoch: AtomicU64::new(0),
            }),
        }
    }

    /// Cut the semantic generation, publish its marker and disconnect every
    /// subscriber under the same publication lock used by writes and opens.
    pub(crate) async fn semantic_reset(
        &self,
        marker: StructuredPublication,
    ) -> Option<StructuredOutput> {
        let mut storage = self.inner.storage.write().await;
        if storage.last_seq == u64::MAX {
            return None;
        }
        StructuredPolicy::clear(&mut storage);
        let item = StructuredPolicy::publish(&mut storage, marker, self.inner.capacity)?;
        storage.reset_at = item.seq;
        let mut subscribers = self.inner.subscribers.write().await;
        for subscriber in subscribers.iter() {
            subscriber.reset.store(true, Ordering::Release);
        }
        subscribers.clear();
        self.inner.epoch.fetch_add(1, Ordering::Relaxed);
        Some(item)
    }

    pub(crate) async fn idle_check_after(&self) -> std::time::Duration {
        self.inspect(|storage| {
            let Some(last) = storage.last_published_at else {
                return storage.policy.idle_after;
            };
            storage
                .policy
                .idle_after
                .saturating_sub(Instant::now().saturating_duration_since(last))
        })
        .await
    }

    pub(crate) async fn trim_if_idle(&self) -> bool {
        let mut storage = self.inner.storage.write().await;
        let idle = storage.last_published_at.is_some_and(|last| {
            Instant::now().saturating_duration_since(last) >= storage.policy.idle_after
        });
        if idle {
            let max_rows = storage.policy.max_rows;
            let idle_trim_bytes = storage.policy.idle_trim_bytes;
            trim_structured(&mut storage, idle_trim_bytes, max_rows);
            storage.last_published_at = None;
        }
        idle
    }

    pub(crate) async fn is_closed(&self) -> bool {
        *self.inner.closed.read().await
    }

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
    ) -> Option<(MultiplexStructuredReader, ReplayFacts)> {
        let snapshot_query = query.clone();
        self.subscribe_filtered(query, move |storage| {
            structured_replay_facts(storage, &snapshot_query)
        })
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
    async fn debug_snapshot_tracks_epoch_subscribers_and_retained_byte_range() {
        let buffer = MultiplexByteBuffer::new(4);
        let reader = buffer.subscribe().await.unwrap();

        let initial = buffer.debug_snapshot().await;
        assert_eq!(initial.epoch, 0);
        assert_eq!(initial.subscriber_count, 1);
        assert_eq!(initial.buffer.head_seq, 0);
        assert_eq!(initial.buffer.tail_seq, 0);

        buffer.write(b"abcdef".to_vec()).await;
        let written = buffer.debug_snapshot().await;
        assert_eq!(written.buffer.head_seq, 2);
        assert_eq!(written.buffer.tail_seq, 6);
        assert_eq!(written.buffer.bytes, 4);

        drop(reader);
        buffer.clear().await;
        let cleared = buffer.debug_snapshot().await;
        assert_eq!(cleared.epoch, 1);
        assert_eq!(cleared.subscriber_count, 0);
        assert_eq!(cleared.buffer.head_seq, 6);
        assert_eq!(cleared.buffer.tail_seq, 6);
        assert_eq!(cleared.buffer.bytes, 0);
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

    fn assert_envelope(actual: StructuredOutput, seq: u64, content: &str, uuid: &str) {
        assert_eq!(actual.seq, seq);
        assert_eq!(actual.payload, user_msg(content, uuid));
        assert!(actual.published_at_unix_ms > 0);
        assert_eq!(actual.activity_at_unix_ms, None);
        assert!(!actual.historical);
    }

    #[tokio::test]
    async fn test_entry_truncation_by_count() {
        let buffer = MultiplexStructuredBuffer::new(3); // Only 3 entries max

        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()).into())
                .await;
        }

        // Late subscriber should see only the last 3 entries (with original seqs)
        let mut reader = buffer.subscribe().await.unwrap();
        assert_envelope(reader.read().await.unwrap(), 3, "msg3", "3");
        assert_envelope(reader.read().await.unwrap(), 4, "msg4", "4");
        assert_envelope(reader.read().await.unwrap(), 5, "msg5", "5");
    }

    #[tokio::test]
    async fn test_entry_per_entry_replay() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("first", "1").into()).await;
        buffer.write(user_msg("second", "2").into()).await;
        buffer.write(user_msg("third", "3").into()).await;

        // Each entry should arrive as a separate read() call
        let mut reader = buffer.subscribe().await.unwrap();
        assert_envelope(reader.read().await.unwrap(), 1, "first", "1");
        assert_envelope(reader.read().await.unwrap(), 2, "second", "2");
        assert_envelope(reader.read().await.unwrap(), 3, "third", "3");

        // Live writes still work
        buffer.write(user_msg("fourth", "4").into()).await;
        assert_envelope(reader.read().await.unwrap(), 4, "fourth", "4");
    }

    #[tokio::test]
    async fn test_clear_resets_storage_keeps_subscribers() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("before", "1").into()).await;
        buffer.write(user_msg("also-before", "2").into()).await;

        let mut early = buffer.subscribe().await.unwrap();
        assert_envelope(early.read().await.unwrap(), 1, "before", "1");
        assert_envelope(early.read().await.unwrap(), 2, "also-before", "2");

        buffer.clear().await;

        let mut late = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("after", "3").into()).await;

        assert_envelope(early.read().await.unwrap(), 3, "after", "3");
        assert_envelope(late.read().await.unwrap(), 3, "after", "3");
    }

    #[tokio::test]
    async fn test_entry_close_returns_none() {
        let buffer = MultiplexStructuredBuffer::new(100);
        let mut reader = buffer.subscribe().await.unwrap();

        buffer.write(user_msg("data", "1").into()).await;
        assert_envelope(reader.read().await.unwrap(), 1, "data", "1");

        buffer.close().await;
        assert!(reader.read().await.is_none());
    }

    #[tokio::test]
    async fn test_seq_increments_on_each_write() {
        let buffer = MultiplexStructuredBuffer::new(100);
        assert_eq!(buffer.current_seq().await, 0);

        buffer.write(user_msg("a", "1").into()).await;
        assert_eq!(buffer.current_seq().await, 1);

        buffer.write(user_msg("b", "2").into()).await;
        assert_eq!(buffer.current_seq().await, 2);

        buffer.write(user_msg("c", "3").into()).await;
        assert_eq!(buffer.current_seq().await, 3);
    }

    #[tokio::test]
    async fn test_seq_survives_clear() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1").into()).await;
        buffer.write(user_msg("b", "2").into()).await;
        assert_eq!(buffer.current_seq().await, 2);

        buffer.clear().await;
        assert_eq!(buffer.current_seq().await, 2, "clear must not reset seq");

        buffer.write(user_msg("c", "3").into()).await;
        assert_eq!(buffer.current_seq().await, 3);

        let mut reader = buffer.subscribe().await.unwrap();
        assert_envelope(reader.read().await.unwrap(), 3, "c", "3");
    }

    #[tokio::test]
    async fn test_subscribers_receive_correct_seq_in_replay_and_live() {
        let buffer = MultiplexStructuredBuffer::new(100);

        buffer.write(user_msg("a", "1").into()).await;
        buffer.write(user_msg("b", "2").into()).await;

        // Late subscriber gets replay with correct seq values
        let mut reader = buffer.subscribe().await.unwrap();
        let item1 = reader.read().await.unwrap();
        assert_eq!(item1.seq, 1);
        let item2 = reader.read().await.unwrap();
        assert_eq!(item2.seq, 2);

        // Live write has next seq
        buffer.write(user_msg("c", "3").into()).await;
        let item3 = reader.read().await.unwrap();
        assert_eq!(item3.seq, 3);
    }

    #[tokio::test]
    async fn structured_replay_marks_boundary_and_then_delivers_live_rows() {
        let buffer = MultiplexStructuredBuffer::new(100);
        for i in 1..=3 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()).into())
                .await;
        }

        let (mut reader, facts) = buffer
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 2,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(
            facts,
            ReplayFacts {
                retained_from: 1,
                through: 3,
                selected_from: 2,
                reset_at: 0,
                outcome: ReplayOutcome::Truncated { missing_after: 1 },
            }
        );
        assert!(
            matches!(reader.read_event().await.unwrap(), BroadcastRead::ReplayItem(item) if item.seq == 2)
        );
        assert!(
            matches!(reader.read_event().await.unwrap(), BroadcastRead::ReplayItem(item) if item.seq == 3)
        );
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));
        buffer.write(user_msg("msg4", "4").into()).await;
        assert!(
            matches!(reader.read_event().await.unwrap(), BroadcastRead::LiveItem(item) if item.seq == 4)
        );
    }

    #[test]
    fn replay_query_conversion_preserves_exclusive_cursor_and_bounds() {
        assert_eq!(
            SequencedReplayQuery::from_replay(ReplayQuery::After {
                after: 40,
                tail_bound: Some(8),
            }),
            SequencedReplayQuery::After {
                after: 40,
                tail_bound: Some(8),
            }
        );
        assert_eq!(
            SequencedReplayQuery::from_replay(ReplayQuery::TailCount {
                count: 9,
                tail_bound: Some(5),
            }),
            SequencedReplayQuery::TailCount {
                count: 9,
                tail_bound: Some(5),
            }
        );
    }

    #[tokio::test]
    async fn subscribing_after_exactly_the_stored_through_replays_no_rows_and_is_continuous() {
        let buffer = MultiplexStructuredBuffer::new(100);
        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()).into())
                .await;
        }

        let (mut reader, facts) = buffer
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 5,
                tail_bound: Some(2),
            }))
            .await
            .unwrap();
        assert_eq!(
            facts,
            ReplayFacts {
                retained_from: 1,
                through: 5,
                selected_from: 0,
                reset_at: 0,
                outcome: ReplayOutcome::Continuous,
            }
        );
        assert!(matches!(
            reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));
    }

    #[tokio::test]
    async fn after_cursor_selects_only_later_rows() {
        let buffer = MultiplexStructuredBuffer::new(100);
        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()).into())
                .await;
        }
        let (mut reader, facts) = buffer
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 2,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 3);
        assert_eq!(facts.outcome, ReplayOutcome::Continuous);
        for seq in 3..=5 {
            assert_eq!(reader.read().await.unwrap().seq, seq);
        }
    }

    #[tokio::test]
    async fn cursor_behind_retention_gets_bounded_tail_and_truncated_facts() {
        let buffer = MultiplexStructuredBuffer::new(2);
        for i in 1..=5 {
            buffer
                .write(user_msg(&format!("msg{i}"), &i.to_string()).into())
                .await;
        }
        let (mut reader, facts) = buffer
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 1,
                tail_bound: Some(1),
            }))
            .await
            .unwrap();
        assert_eq!(facts.retained_from, 4);
        assert_eq!(facts.selected_from, 5);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 1 });
        assert_eq!(reader.read().await.unwrap().seq, 5);
    }

    #[tokio::test]
    async fn tail_zero_replays_nothing_but_keeps_live_delivery() {
        let buffer = MultiplexStructuredBuffer::new(100);
        buffer.write(user_msg("a", "1").into()).await;
        buffer.write(user_msg("b", "2").into()).await;
        let (mut reader, facts) = buffer
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 0,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 0);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 2 });
        buffer.write(user_msg("c", "3").into()).await;
        assert_eq!(reader.read().await.unwrap().seq, 3);
    }
}
