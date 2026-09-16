//! Agent-agnostic sequenced sink for structured log entries.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use model::{ReplayFacts, ReplayOutcome};
use serde_json::Value;

use crate::agents::{
    MultiplexStructuredBuffer, MultiplexStructuredReader, OutputDebug, SequencedReplayQuery,
    SubscriptionRecord,
};

const RECENT_SUBSCRIPTION_LIMIT: usize = 8;

/// A retained structured log that supports replay and live subscriptions.
#[derive(Clone)]
pub(crate) struct StructuredLogSource {
    buffer: MultiplexStructuredBuffer,
    recording: Option<Arc<Mutex<File>>>,
    recent_subscriptions: Arc<Mutex<VecDeque<SubscriptionRecord>>>,
}

impl StructuredLogSource {
    /// Create an empty source retaining at most `retention` entries.
    pub(crate) fn new(retention: usize) -> Self {
        Self {
            buffer: MultiplexStructuredBuffer::new(retention),
            recording: None,
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// Create a source that also writes every structured row as JSONL.
    pub(crate) fn recording(retention: usize, path: &Path) -> std::io::Result<Self> {
        Self::recording_resuming(retention, 0, path)
    }

    pub(crate) fn recording_resuming(
        retention: usize,
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
            buffer: MultiplexStructuredBuffer::with_last_seq(retention, last_seq),
            recording: Some(Arc::new(Mutex::new(file))),
            recent_subscriptions: Arc::new(Mutex::new(VecDeque::new())),
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

    pub(crate) fn recent_subscriptions(&self) -> Vec<SubscriptionRecord> {
        self.recent_subscriptions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .cloned()
            .collect()
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
