//! Agent-agnostic sequenced sink for structured log entries.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use model::{ReplayFacts, ReplayOutcome};
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::agents::{
    MultiplexStructuredBuffer, MultiplexStructuredReader, OutputDebug, SequencedReplayQuery,
    StructuredPublication, SubscriptionRecord,
};

const RECENT_SUBSCRIPTION_LIMIT: usize = 8;
const ROW_ADMISSION_BYTES: usize = 4 * 1024 * 1024;

/// The durable authority to continue one agent's sequence after suspension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SealedAt {
    pub(crate) id: Uuid,
    pub(crate) through: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LogClosed {
    #[cfg(test)]
    #[error("structured log is sealed")]
    Sealed,
    #[error("structured log is closed")]
    Closed,
    #[error("structured log sequence is exhausted")]
    Exhausted,
}

#[derive(Default)]
struct PublicationState {
    parked: bool,
    active: usize,
    seal: Option<SealedAt>,
    closed: bool,
}

#[derive(Default)]
struct PublicationGate {
    state: Mutex<PublicationState>,
    changed: Notify,
}

struct PublicationPermit<'a>(&'a PublicationGate);

impl Drop for PublicationPermit<'_> {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.active = state
            .active
            .checked_sub(1)
            .expect("publication permit count is balanced");
        drop(state);
        self.0.changed.notify_waiters();
    }
}

impl PublicationGate {
    #[cfg(test)]
    async fn enter(&self) -> Result<PublicationPermit<'_>, LogClosed> {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if state.closed {
                    return Err(LogClosed::Closed);
                }
                if state.seal.is_some() {
                    return Err(LogClosed::Sealed);
                }
                if !state.parked {
                    state.active = state
                        .active
                        .checked_add(1)
                        .expect("active publisher count exhausted");
                    return Ok(PublicationPermit(self));
                }
            }
            changed.await;
        }
    }

    async fn enter_waiting(&self) -> Result<PublicationPermit<'_>, LogClosed> {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if state.closed {
                    return Err(LogClosed::Closed);
                }
                if !state.parked && state.seal.is_none() {
                    state.active = state
                        .active
                        .checked_add(1)
                        .expect("active publisher count exhausted");
                    return Ok(PublicationPermit(self));
                }
            }
            changed.await;
        }
    }

    async fn park(&self) -> Result<Option<SealedAt>, LogClosed> {
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if state.closed {
                    return Err(LogClosed::Closed);
                }
                if let Some(seal) = state.seal {
                    return Ok(Some(seal));
                }
                state.parked = true;
                if state.active == 0 {
                    return Ok(None);
                }
            }
            changed.await;
        }
    }

    fn seal(&self, through: u64) -> Result<SealedAt, LogClosed> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.closed {
            return Err(LogClosed::Closed);
        }
        if let Some(seal) = state.seal {
            return Ok(seal);
        }
        assert!(state.parked, "a structured log is parked before sealing");
        assert_eq!(state.active, 0, "all publishers acknowledge before sealing");
        let seal = SealedAt {
            id: Uuid::new_v4(),
            through,
        };
        state.seal = Some(seal);
        Ok(seal)
    }

    fn abort(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.closed {
            return;
        }
        state.seal = None;
        state.parked = false;
        drop(state);
        self.changed.notify_waiters();
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.closed = true;
        drop(state);
        self.changed.notify_waiters();
    }
}

/// Byte ceiling for one provider's in-memory structured log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RingPolicy {
    pub(crate) max_bytes: usize,
}

impl RingPolicy {
    const fn provider() -> Self {
        Self {
            max_bytes: 1024 * 1024,
        }
    }

    pub(crate) const fn claude_pty() -> Self {
        Self::provider()
    }

    pub(crate) const fn claude_sdk() -> Self {
        Self::provider()
    }

    pub(crate) const fn codex() -> Self {
        Self::provider()
    }

    pub(crate) const fn test(max_bytes: usize) -> Self {
        Self { max_bytes }
    }
}
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
    publication: Arc<PublicationGate>,
    recording: Option<Arc<Mutex<File>>>,
    /// Unix milliseconds of the latest activity written, or `NO_ACTIVITY`.
    activity: Arc<AtomicI64>,
    recent_subscriptions: Arc<Mutex<VecDeque<SubscriptionRecord>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PendingStructuredInput {
    pub(crate) rows: u64,
    pub(crate) oldest_published_at_unix_ms: Option<i64>,
}

impl StructuredLogSource {
    /// Create an empty source retaining at most `max_bytes` encoded bytes.
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self::with_policy(RingPolicy::test(max_bytes))
    }

    pub(crate) fn with_policy(policy: RingPolicy) -> Self {
        Self {
            buffer: MultiplexStructuredBuffer::with_policy(policy),
            publication: Arc::new(PublicationGate::default()),
            recording: None,
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
            activity: Arc::new(AtomicI64::new(NO_ACTIVITY)),
        }
    }

    pub(crate) fn recording_resuming_with_policy(
        policy: RingPolicy,
        last_seq: u64,
        path: &Path,
    ) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            buffer: MultiplexStructuredBuffer::with_policy_and_last_seq(policy, last_seq),
            publication: Arc::new(PublicationGate::default()),
            recording: Some(Arc::new(Mutex::new(file))),
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
            activity: Arc::new(AtomicI64::new(NO_ACTIVITY)),
        })
    }

    /// Create an empty resumed ring whose next publication follows `last_seq`.
    pub(crate) fn resuming_with_policy(policy: RingPolicy, last_seq: u64) -> Self {
        Self {
            buffer: MultiplexStructuredBuffer::with_policy_and_last_seq(policy, last_seq),
            publication: Arc::new(PublicationGate::default()),
            recording: None,
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
            activity: Arc::new(AtomicI64::new(NO_ACTIVITY)),
        }
    }

    /// Subscribe to the structured log buffer immediately.
    #[allow(dead_code)]
    pub(crate) async fn subscribe(&self) -> Option<MultiplexStructuredReader> {
        self.buffer.subscribe().await
    }

    /// Subscribe with an optional replay filter and return its snapshot facts.
    pub(crate) async fn subscribe_with_query(
        &self,
        query: Option<SequencedReplayQuery>,
    ) -> Option<(MultiplexStructuredReader, ReplayFacts)> {
        let query_label = replay_query_label(query.as_ref());
        let (reader, replay) = self.buffer.subscribe_with_query(query).await?;
        if matches!(replay.outcome, ReplayOutcome::Reset { ref reason } if reason == "ahead") {
            tracing::error!(
                through = replay.through,
                selected_from = replay.selected_from,
                "structured replay cursor is ahead of the publication watermark"
            );
        }
        let replayed_first = (replay.selected_from != 0).then_some(replay.selected_from);
        let replayed_last = replayed_first.map(|_| replay.through);
        let replayed_count = replayed_first
            .map(|first| replay.through.saturating_sub(first).saturating_add(1))
            .map(|count| usize::try_from(count).unwrap_or(usize::MAX))
            .unwrap_or(0);
        let record = SubscriptionRecord {
            opened_at: chrono::Utc::now(),
            query: query_label,
            replayed_first,
            replayed_last,
            replayed_count,
            gap: !matches!(replay.outcome, ReplayOutcome::Continuous),
        };
        let mut recent = self
            .recent_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        recent.push_front(record);
        recent.truncate(RECENT_SUBSCRIPTION_LIMIT);
        Some((reader, replay))
    }

    /// Write an entry that records something the agent did at `at`.
    ///
    /// Activity only moves forward: an entry dated earlier than the latest
    /// one already seen, such as a transcript row re-read on a relink, leaves
    /// it where it is.
    pub(crate) async fn write_activity(&self, payload: Value, at: DateTime<Utc>) {
        self.write_row(payload, Some(at.timestamp_millis()), false)
            .await;
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
        self.write_row(payload, None, false).await;
    }

    /// Write a structured row with facts fixed at publication time.
    pub(crate) async fn write_row(
        &self,
        payload: Value,
        activity_at_unix_ms: Option<i64>,
        historical: bool,
    ) {
        let Ok(permit) = self.publication.enter_waiting().await else {
            return;
        };
        let _ = self
            .publish(permit, payload, activity_at_unix_ms, historical)
            .await;
    }

    /// Write a row, refusing publication while the log is sealed or closed.
    #[cfg(test)]
    pub(crate) async fn try_write(&self, payload: Value) -> Result<u64, LogClosed> {
        let permit = self.publication.enter().await?;
        self.publish(permit, payload, None, false).await
    }

    /// Publish like a provider ingestion task, waiting behind a suspend seal.
    #[cfg(test)]
    pub(crate) async fn write_waiting_for_test(&self, payload: Value) -> Result<u64, LogClosed> {
        let permit = self.publication.enter_waiting().await?;
        self.publish(permit, payload, None, false).await
    }

    async fn publish(
        &self,
        _permit: PublicationPermit<'_>,
        payload: Value,
        activity_at_unix_ms: Option<i64>,
        historical: bool,
    ) -> Result<u64, LogClosed> {
        let payload = clip_oversized_row(payload);
        let item = self
            .buffer
            .write(StructuredPublication {
                payload: payload.clone(),
                activity_at_unix_ms,
                historical,
            })
            .await
            .ok_or(LogClosed::Exhausted)?;
        if let Some(at) = activity_at_unix_ms
            && !historical
        {
            self.activity.fetch_max(at, Ordering::Relaxed);
        }
        if let Some(recording) = &self.recording
            && let Ok(mut file) = recording.lock()
        {
            let _ = writeln!(file, "{payload}");
            let _ = file.flush();
        }
        Ok(item.seq)
    }

    /// Atomically cut the old semantic generation and retain its marker as the
    /// first row of the new one. Existing readers are told to resubscribe.
    pub(crate) async fn semantic_reset(&self, marker: Value) -> u64 {
        self.semantic_reset_row(marker, None, false).await
    }

    pub(crate) async fn semantic_reset_row(
        &self,
        marker: Value,
        activity_at_unix_ms: Option<i64>,
        historical: bool,
    ) -> u64 {
        let Ok(_permit) = self.publication.enter_waiting().await else {
            return self.current_seq().await;
        };
        let marker = clip_oversized_row(marker);
        let Some(item) = self
            .buffer
            .semantic_reset(StructuredPublication {
                payload: marker.clone(),
                activity_at_unix_ms,
                historical,
            })
            .await
        else {
            return u64::MAX;
        };
        if let Some(at) = activity_at_unix_ms
            && !historical
        {
            self.activity.fetch_max(at, Ordering::Relaxed);
        }
        if let Some(recording) = &self.recording
            && let Ok(mut file) = recording.lock()
        {
            let _ = writeln!(file, "{marker}");
            let _ = file.flush();
        }
        item.seq
    }

    /// Wait for every in-flight publisher, block new ones, and seal the next
    /// sequence behind a single-use token.
    pub(crate) async fn prepare_suspend(&self) -> Result<SealedAt, LogClosed> {
        if let Some(seal) = self.publication.park().await? {
            return Ok(seal);
        }
        let through = self.buffer.current_seq().await;
        if through == u64::MAX {
            self.publication.abort();
            return Err(LogClosed::Exhausted);
        }
        self.publication.seal(through)
    }

    /// Re-open a prepared source after its durable seal has been invalidated.
    pub(crate) fn abort_suspend(&self) {
        self.publication.abort();
    }

    /// Return the current sequence number.
    pub(crate) async fn current_seq(&self) -> u64 {
        self.buffer.current_seq().await
    }

    pub(crate) async fn pending_after(&self, through: u64) -> PendingStructuredInput {
        let (rows, oldest_published_at_unix_ms) = self.buffer.pending_after(through).await;
        PendingStructuredInput {
            rows,
            oldest_published_at_unix_ms,
        }
    }

    /// Snapshot retained coordinates and active readers for diagnostics.
    pub(crate) async fn debug_snapshot(&self) -> OutputDebug {
        self.buffer.debug_snapshot().await
    }

    pub(crate) fn recent_subscriptions(&self) -> Vec<SubscriptionRecord> {
        self.recent_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// Close the source and all current subscriptions.
    pub(crate) async fn close(&self) {
        self.publication.close();
        self.buffer.close().await;
    }
}

fn clip_oversized_row(payload: Value) -> Value {
    let original_bytes = serde_json::to_vec(&payload).map_or(0, |encoded| encoded.len());
    if original_bytes <= ROW_ADMISSION_BYTES {
        return payload;
    }

    fn identity(value: &Value) -> Option<Value> {
        match value {
            Value::Object(fields) => {
                let kept = fields
                    .iter()
                    .filter_map(|(key, value)| {
                        let identity_key = matches!(
                            key.as_str(),
                            "type"
                                | "subtype"
                                | "method"
                                | "uuid"
                                | "id"
                                | "index"
                                | "session_id"
                                | "sessionId"
                                | "parent_uuid"
                                | "parentUuid"
                                | "message_id"
                                | "messageId"
                                | "item_id"
                                | "itemId"
                                | "turn_id"
                                | "turnId"
                                | "thread_id"
                                | "threadId"
                                | "request_id"
                                | "requestId"
                                | "input_id"
                                | "inputId"
                                | "tool_use_id"
                                | "toolUseId"
                                | "parent_tool_use_id"
                                | "parentToolUseId"
                                | "task_id"
                                | "taskId"
                                | "new_conversation_id"
                        );
                        if identity_key {
                            Some((key.clone(), value.clone()))
                        } else {
                            identity(value).map(|nested| (key.clone(), nested))
                        }
                    })
                    .collect::<serde_json::Map<_, _>>();
                (!kept.is_empty()).then_some(Value::Object(kept))
            }
            Value::Array(values) => {
                let kept = values.iter().filter_map(identity).collect::<Vec<_>>();
                (!kept.is_empty()).then_some(Value::Array(kept))
            }
            _ => None,
        }
    }

    let mut clipped = identity(&payload)
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    clipped.insert(
        "amux_clipped".to_string(),
        serde_json::json!({"original_bytes": original_bytes}),
    );
    Value::Object(clipped)
}

fn replay_query_label(query: Option<&SequencedReplayQuery>) -> String {
    match query {
        None => "all".to_string(),
        Some(SequencedReplayQuery::After {
            after,
            tail_bound: Some(bound),
        }) => format!("after {after}, bound {bound}"),
        Some(SequencedReplayQuery::After {
            after,
            tail_bound: None,
        }) => format!("after {after}"),
        Some(SequencedReplayQuery::TailCount { count, tail_bound }) => match tail_bound {
            Some(bound) => format!("tail {count}, bound {bound}"),
            None => format!("tail {count}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agents::BroadcastRead;

    const TEST_RING_BYTES: usize = 1024 * 1024;

    fn test_log() -> StructuredLogSource {
        StructuredLogSource::new(TEST_RING_BYTES)
    }

    fn test_log_retaining_rows(count: usize) -> StructuredLogSource {
        let row_bytes = serde_json::to_vec(&json!({"type": "test", "id": 1}))
            .unwrap()
            .len();
        StructuredLogSource::new(row_bytes * count)
    }

    async fn write_rows(log: &StructuredLogSource, count: u64) {
        for seq in 1..=count {
            log.write(json!({"type": "test", "id": seq})).await;
        }
    }

    async fn replay_seqs(mut reader: MultiplexStructuredReader) -> Vec<u64> {
        let mut seqs = Vec::new();
        loop {
            match reader.read_event().await.unwrap() {
                BroadcastRead::ReplayItem(row) => seqs.push(row.seq),
                BroadcastRead::ReplayComplete => return seqs,
                BroadcastRead::LiveItem(_) => panic!("live row arrived during replay"),
                BroadcastRead::Lagged => panic!("replay subscriber lagged"),
                BroadcastRead::Reset => panic!("replay subscriber was reset"),
            }
        }
    }

    #[tokio::test]
    async fn daemon_protocol_truth_after_before_reset_uses_the_exact_cursor_cut() {
        let log = test_log();
        write_rows(&log, 3).await;
        let exact_cursor = log.current_seq().await;
        let reset_at = log
            .semantic_reset(json!({"type": "conversation_reset", "uuid": "reset-1"}))
            .await;
        assert_eq!(reset_at, exact_cursor + 1);

        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: exact_cursor,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.retained_from, reset_at);
        assert_eq!(facts.through, reset_at);
        assert_eq!(facts.selected_from, reset_at);
        assert_eq!(facts.reset_at, reset_at);
        assert_eq!(
            facts.outcome,
            ReplayOutcome::Reset {
                reason: "reset".into()
            }
        );
        assert_eq!(replay_seqs(reader).await, vec![reset_at]);
    }

    #[tokio::test]
    async fn daemon_protocol_truth_after_at_through_is_continuous_and_empty() {
        let log = test_log();
        write_rows(&log, 3).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 3,
                tail_bound: Some(1),
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 0);
        assert_eq!(facts.outcome, ReplayOutcome::Continuous);
        assert!(replay_seqs(reader).await.is_empty());
    }

    #[tokio::test]
    async fn daemon_protocol_truth_after_retained_cursor_is_continuous_unless_bound_drops_rows() {
        let log = test_log();
        write_rows(&log, 5).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 2,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.outcome, ReplayOutcome::Continuous);
        assert_eq!(replay_seqs(reader).await, vec![3, 4, 5]);

        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 2,
                tail_bound: Some(2),
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 4);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 2 });
        assert_eq!(replay_seqs(reader).await, vec![4, 5]);
    }

    #[tokio::test]
    async fn daemon_protocol_truth_after_missing_cursor_is_truncated_including_empty_retention() {
        let log = test_log_retaining_rows(2);
        write_rows(&log, 5).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 2,
                tail_bound: Some(1),
            }))
            .await
            .unwrap();
        assert_eq!(facts.retained_from, 4);
        assert_eq!(facts.selected_from, 5);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 2 });
        assert_eq!(replay_seqs(reader).await, vec![5]);

        let empty = StructuredLogSource::new(0);
        write_rows(&empty, 3).await;
        let (reader, facts) = empty
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 0,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.retained_from, 0);
        assert_eq!(facts.selected_from, 0);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 0 });
        assert!(replay_seqs(reader).await.is_empty());
    }

    #[tokio::test]
    async fn daemon_protocol_truth_after_cursor_ahead_is_a_reset_discipline_violation() {
        let log = test_log();
        write_rows(&log, 3).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 8,
                tail_bound: Some(2),
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 2);
        assert_eq!(
            facts.outcome,
            ReplayOutcome::Reset {
                reason: "ahead".into()
            }
        );
        assert_eq!(replay_seqs(reader).await, vec![2, 3]);
        assert!(log.recent_subscriptions()[0].gap);
    }

    #[tokio::test]
    async fn daemon_protocol_truth_tail_positive_with_retention_reports_selected_prefix() {
        let log = test_log();
        write_rows(&log, 5).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 2,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 4);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 3 });
        assert_eq!(replay_seqs(reader).await, vec![4, 5]);

        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 10,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 1);
        assert_eq!(facts.outcome, ReplayOutcome::Continuous);
        assert_eq!(replay_seqs(reader).await, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn daemon_protocol_truth_tail_positive_empty_retention_with_watermark_is_truncated() {
        let log = StructuredLogSource::new(0);
        write_rows(&log, 3).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 2,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.retained_from, 0);
        assert_eq!(facts.selected_from, 0);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 3 });
        assert!(replay_seqs(reader).await.is_empty());
    }

    #[tokio::test]
    async fn daemon_protocol_truth_tail_zero_is_empty_and_reflects_the_watermark() {
        let log = test_log();
        write_rows(&log, 3).await;
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 0,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.selected_from, 0);
        assert_eq!(facts.outcome, ReplayOutcome::Truncated { missing_after: 3 });
        assert!(replay_seqs(reader).await.is_empty());

        let empty = test_log();
        let (reader, facts) = empty
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 0,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert_eq!(facts.outcome, ReplayOutcome::Continuous);
        assert!(replay_seqs(reader).await.is_empty());
    }

    #[tokio::test]
    async fn daemon_protocol_truth_tail_on_never_written_log_is_continuous_and_empty() {
        let log = test_log();
        let (reader, facts) = log
            .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                count: 10,
                tail_bound: Some(3),
            }))
            .await
            .unwrap();
        assert_eq!(facts.retained_from, 0);
        assert_eq!(facts.through, 0);
        assert_eq!(facts.selected_from, 0);
        assert_eq!(facts.outcome, ReplayOutcome::Continuous);
        assert!(replay_seqs(reader).await.is_empty());
    }

    #[tokio::test]
    async fn daemon_protocol_ring_enforces_one_provider_byte_budget_and_oversized_admission() {
        assert_eq!(RingPolicy::claude_pty().max_bytes, 1024 * 1024);
        assert_eq!(RingPolicy::claude_sdk().max_bytes, 1024 * 1024);
        assert_eq!(RingPolicy::codex(), RingPolicy::claude_sdk());

        let byte_only = StructuredLogSource::with_policy(RingPolicy::codex());
        for _ in 0..9_000 {
            byte_only.write(Value::Null).await;
        }
        let (_, facts) = byte_only.subscribe_with_query(None).await.unwrap();
        assert_eq!(facts.retained_from, 1, "there is no hidden row ceiling");

        let bytes = StructuredLogSource::with_policy(RingPolicy::claude_pty());
        for id in 0..4 {
            bytes
                .write(json!({"type": "assistant", "uuid": id, "payload": "x".repeat(400 * 1024)}))
                .await;
        }
        let debug = bytes.debug_snapshot().await;
        assert!(debug.buffer.bytes <= RingPolicy::claude_pty().max_bytes);
        assert!(debug.buffer.head_seq > 1);

        let oversized = StructuredLogSource::with_policy(RingPolicy::codex());
        oversized
            .write(json!({
                "method": "item/completed",
                "type": "event",
                "item": {"id": "item-7", "type": "agentMessage", "text": "x".repeat(ROW_ADMISSION_BYTES + 1)}
            }))
            .await;
        let (mut reader, _) = oversized.subscribe_with_query(None).await.unwrap();
        let row = reader.read().await.unwrap().payload;
        assert_eq!(row["type"], "event");
        assert_eq!(row["item"]["id"], "item-7");
        assert!(row["item"].get("text").is_none());
        assert!(row["amux_clipped"]["original_bytes"].as_u64().unwrap() > 4 * 1024 * 1024);
    }

    #[tokio::test]
    async fn daemon_protocol_semantic_reset_closes_existing_subscribers_with_reset() {
        let log = test_log();
        write_rows(&log, 2).await;
        let (mut old_reader, _) = log
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 2,
                tail_bound: None,
            }))
            .await
            .unwrap();
        assert!(matches!(
            old_reader.read_event().await.unwrap(),
            BroadcastRead::ReplayComplete
        ));
        log.semantic_reset(json!({"type": "amux.codex_ready"}))
            .await;
        assert!(matches!(
            old_reader.read_event().await.unwrap(),
            BroadcastRead::Reset
        ));
    }

    #[tokio::test]
    async fn daemon_protocol_seal_parks_publishers_and_reuses_one_token() {
        let log = test_log();
        log.write(json!({"type": "before"})).await;

        let seal = log.prepare_suspend().await.unwrap();
        assert_eq!(seal.through, 1);
        assert_eq!(log.prepare_suspend().await.unwrap(), seal);
        assert_eq!(
            log.try_write(json!({"type": "sealed"})).await,
            Err(LogClosed::Sealed)
        );
        assert_eq!(log.current_seq().await, 1);

        log.abort_suspend();
        assert_eq!(log.try_write(json!({"type": "after"})).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn daemon_protocol_seal_close_releases_a_parked_publisher_without_numbering_it() {
        let log = test_log();
        let publication = log.publication.clone();
        {
            let mut state = publication
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.parked = true;
        }
        let writer_log = log.clone();
        let writer =
            tokio::spawn(async move { writer_log.try_write(json!({"type": "waiting"})).await });
        tokio::task::yield_now().await;
        log.close().await;

        assert_eq!(writer.await.unwrap(), Err(LogClosed::Closed));
        assert_eq!(log.current_seq().await, 0);
    }

    #[tokio::test]
    async fn daemon_protocol_seal_abort_releases_an_emitting_publisher_after_the_barrier() {
        let log = test_log();
        log.write(json!({"type": "before-seal"})).await;
        let seal = log.prepare_suspend().await.unwrap();

        let emitting_log = log.clone();
        let emitting = tokio::spawn(async move {
            emitting_log
                .write(json!({"type": "held-during-seal"}))
                .await;
        });
        tokio::task::yield_now().await;
        assert_eq!(log.current_seq().await, seal.through);

        log.abort_suspend();
        emitting.await.unwrap();
        assert_eq!(log.current_seq().await, seal.through + 1);
    }

    #[tokio::test]
    async fn write_is_visible_to_later_subscribers() {
        let log_source = test_log();
        log_source
            .write(json!({"type": "hook.stop", "cwd": "/tmp"}))
            .await;

        let (mut reader, replay) = log_source.subscribe_with_query(None).await.unwrap();
        assert_eq!(replay.through, 1);
        assert_eq!(reader.read().await.unwrap().payload["type"], "hook.stop");
    }

    #[tokio::test]
    async fn replay_debug_records_only_post_cursor_rows() {
        let log_source = StructuredLogSource::new(64);
        for seq in 1..=10 {
            log_source.write(json!({"seq": seq})).await;
        }

        let (_reader, replay) = log_source
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 8,
                tail_bound: Some(2),
            }))
            .await
            .unwrap();
        assert_eq!(replay.selected_from, 9);
        assert_eq!(replay.outcome, ReplayOutcome::Continuous);

        let records = log_source.recent_subscriptions();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].query, "after 8, bound 2");
        assert_eq!(records[0].replayed_first, Some(9));
        assert_eq!(records[0].replayed_last, Some(10));
        assert_eq!(records[0].replayed_count, 2);
        assert!(!records[0].gap);

        let output = log_source.debug_snapshot().await;
        let session = crate::agents::SessionDebug::new(
            Some(&output),
            output.subscriber_count,
            crate::agents::BackendState::Running { pid: None },
            Vec::new(),
            records,
        );
        let json = serde_json::to_value(session).unwrap();
        assert_eq!(json["recent_subscriptions"][0]["query"], "after 8, bound 2");
        assert_eq!(json["recent_subscriptions"][0]["replayed_first"], 9);
        assert_eq!(json["recent_subscriptions"][0]["replayed_last"], 10);
        assert_eq!(json["recent_subscriptions"][0]["replayed_count"], 2);
        assert_eq!(json["recent_subscriptions"][0]["gap"], false);

        let _ = log_source
            .subscribe_with_query(Some(SequencedReplayQuery::After {
                after: 2,
                tail_bound: Some(2),
            }))
            .await
            .unwrap();
        let records = log_source.recent_subscriptions();
        assert_eq!(records[0].replayed_first, Some(9));
        assert_eq!(records[0].replayed_last, Some(10));
        assert_eq!(records[0].replayed_count, 2);
        assert!(records[0].gap);
    }

    #[tokio::test]
    async fn replay_debug_records_keep_only_eight_newest() {
        let log_source = test_log();
        log_source.write(json!({"seq": 1})).await;

        for count in 0..10 {
            let _ = log_source
                .subscribe_with_query(Some(SequencedReplayQuery::TailCount {
                    count,
                    tail_bound: None,
                }))
                .await
                .unwrap();
        }

        let records = log_source.recent_subscriptions();
        assert_eq!(records.len(), RECENT_SUBSCRIPTION_LIMIT);
        assert_eq!(records[0].query, "tail 9");
        assert_eq!(records[7].query, "tail 2");
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
