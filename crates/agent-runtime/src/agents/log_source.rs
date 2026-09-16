//! Agent-agnostic sequenced sink for structured log entries.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use model::{ReplayFacts, ReplayOutcome};
use serde_json::Value;
use tokio::sync::Notify;

use crate::agents::{
    MultiplexStructuredBuffer, MultiplexStructuredReader, OutputDebug, SequencedReplayQuery,
    SubscriptionRecord,
};

const RECENT_SUBSCRIPTION_LIMIT: usize = 8;
const ROW_ADMISSION_BYTES: usize = 4 * 1024 * 1024;

/// Byte and row ceilings for one provider's in-memory structured log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RingPolicy {
    pub(crate) max_bytes: usize,
    pub(crate) max_rows: usize,
    pub(crate) idle_trim_bytes: usize,
    pub(crate) idle_after: Duration,
}

impl RingPolicy {
    const fn provider(max_bytes: usize, max_rows: usize) -> Self {
        Self {
            max_bytes,
            max_rows,
            idle_trim_bytes: 1024 * 1024,
            idle_after: Duration::from_secs(30 * 60),
        }
    }

    pub(crate) const fn claude_pty() -> Self {
        Self::provider(4 * 1024 * 1024, 2_000)
    }

    pub(crate) const fn claude_sdk() -> Self {
        Self::provider(16 * 1024 * 1024, 8_192)
    }

    pub(crate) const fn codex() -> Self {
        Self::provider(16 * 1024 * 1024, 8_192)
    }

    pub(crate) fn test(max_rows: usize) -> Self {
        Self::provider(usize::MAX, max_rows)
    }
}

/// A retained structured log that supports replay and live subscriptions.
#[derive(Clone)]
pub(crate) struct StructuredLogSource {
    buffer: MultiplexStructuredBuffer,
    recording: Option<Arc<Mutex<File>>>,
    recent_subscriptions: Arc<Mutex<VecDeque<SubscriptionRecord>>>,
    idle_monitor_started: Arc<AtomicBool>,
    idle_monitor_wake: Arc<Notify>,
}

impl StructuredLogSource {
    /// Create an empty source retaining at most `retention` entries.
    pub(crate) fn new(retention: usize) -> Self {
        Self::with_policy(RingPolicy::test(retention))
    }

    pub(crate) fn with_policy(policy: RingPolicy) -> Self {
        Self {
            buffer: MultiplexStructuredBuffer::with_policy(policy),
            recording: None,
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
            idle_monitor_started: Arc::new(AtomicBool::new(false)),
            idle_monitor_wake: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn recording_with_policy(policy: RingPolicy, path: &Path) -> std::io::Result<Self> {
        Self::recording_resuming_with_policy(policy, 0, path)
    }

    fn recording_resuming_with_policy(
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
            recording: Some(Arc::new(Mutex::new(file))),
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
            idle_monitor_started: Arc::new(AtomicBool::new(false)),
            idle_monitor_wake: Arc::new(Notify::new()),
        })
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

    /// Write a structured output entry.
    pub(crate) async fn write(&self, payload: Value) {
        self.ensure_idle_monitor();
        let payload = clip_oversized_row(payload);
        self.buffer.write(payload.clone()).await;
        self.idle_monitor_wake.notify_waiters();
        if let Some(recording) = &self.recording
            && let Ok(mut file) = recording.lock()
        {
            let _ = writeln!(file, "{payload}");
            let _ = file.flush();
        }
    }

    /// Atomically cut the old semantic generation and retain its marker as the
    /// first row of the new one. Existing readers are told to resubscribe.
    pub(crate) async fn semantic_reset(&self, marker: Value) -> u64 {
        self.ensure_idle_monitor();
        let marker = clip_oversized_row(marker);
        let item = self.buffer.semantic_reset(marker.clone()).await;
        self.idle_monitor_wake.notify_waiters();
        if let Some(recording) = &self.recording
            && let Ok(mut file) = recording.lock()
        {
            let _ = writeln!(file, "{marker}");
            let _ = file.flush();
        }
        item.seq
    }

    /// Return the current sequence number.
    pub(crate) async fn current_seq(&self) -> u64 {
        self.buffer.current_seq().await
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
        self.buffer.close().await;
        self.idle_monitor_wake.notify_waiters();
    }

    fn ensure_idle_monitor(&self) {
        if self
            .idle_monitor_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let buffer = self.buffer.clone();
        let wake = self.idle_monitor_wake.clone();
        tokio::spawn(async move {
            loop {
                let wait = buffer.idle_check_after().await;
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = wake.notified() => {
                        if buffer.is_closed().await {
                            break;
                        }
                        continue;
                    }
                }
                if buffer.trim_if_idle().await {
                    // Keep watching: a later publication starts a fresh idle period.
                    continue;
                }
                if buffer.is_closed().await {
                    break;
                }
            }
        });
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
    use tokio::time::{Duration, timeout};

    use super::*;
    use crate::agents::BroadcastRead;

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
        let log = StructuredLogSource::new(16);
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
        let log = StructuredLogSource::new(16);
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
        let log = StructuredLogSource::new(16);
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
        let log = StructuredLogSource::new(2);
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
        let log = StructuredLogSource::new(16);
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
        let log = StructuredLogSource::new(16);
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
        let log = StructuredLogSource::new(16);
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

        let empty = StructuredLogSource::new(16);
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
        let log = StructuredLogSource::new(16);
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
    async fn daemon_protocol_ring_enforces_provider_rows_bytes_and_oversized_admission() {
        assert_eq!(RingPolicy::claude_pty().max_bytes, 4 * 1024 * 1024);
        assert_eq!(RingPolicy::claude_pty().max_rows, 2_000);
        assert_eq!(RingPolicy::claude_sdk().max_bytes, 16 * 1024 * 1024);
        assert_eq!(RingPolicy::claude_sdk().max_rows, 8_192);
        assert_eq!(RingPolicy::codex(), RingPolicy::claude_sdk());

        let rows = StructuredLogSource::with_policy(RingPolicy {
            max_bytes: usize::MAX,
            max_rows: 2,
            idle_trim_bytes: 1024 * 1024,
            idle_after: Duration::from_secs(60),
        });
        write_rows(&rows, 3).await;
        let (_, facts) = rows.subscribe_with_query(None).await.unwrap();
        assert_eq!(facts.retained_from, 2);

        let bytes = StructuredLogSource::with_policy(RingPolicy::claude_pty());
        for id in 0..5 {
            bytes
                .write(json!({"type": "assistant", "uuid": id, "payload": "x".repeat(1024 * 1024)}))
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
    async fn daemon_protocol_idle_ring_trims_to_one_policy_budget() {
        let log = StructuredLogSource::with_policy(RingPolicy {
            max_bytes: 4096,
            max_rows: 100,
            idle_trim_bytes: 180,
            idle_after: Duration::from_millis(20),
        });
        for id in 0..4 {
            log.write(json!({"type": "test", "id": id, "payload": "x".repeat(100)}))
                .await;
        }
        timeout(Duration::from_secs(1), async {
            loop {
                if log.debug_snapshot().await.buffer.bytes <= 180 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn daemon_protocol_semantic_reset_closes_existing_subscribers_with_reset() {
        let log = StructuredLogSource::new(16);
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
    async fn write_is_visible_to_later_subscribers() {
        let log_source = StructuredLogSource::new(1000);
        log_source
            .write(json!({"type": "hook.stop", "cwd": "/tmp"}))
            .await;

        let (mut reader, replay) = log_source.subscribe_with_query(None).await.unwrap();
        assert_eq!(replay.through, 1);
        assert_eq!(reader.read().await.unwrap().payload["type"], "hook.stop");
    }

    #[tokio::test]
    async fn replay_debug_records_only_post_cursor_rows() {
        let log_source = StructuredLogSource::new(5);
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
        let log_source = StructuredLogSource::new(5);
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
}
