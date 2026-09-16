use chrono::{DateTime, Utc};
use serde::Serialize;

/// Retained output coordinates for one session buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct BufferDebug {
    pub(crate) head_seq: u64,
    pub(crate) tail_seq: u64,
    pub(crate) bytes: usize,
}

/// Atomic diagnostic snapshot of a replay buffer and its subscribers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputDebug {
    pub(crate) epoch: u64,
    pub(crate) subscriber_count: usize,
    pub(crate) buffer: BufferDebug,
    pub(crate) closed: bool,
}

/// Provider process lifecycle as observed by the owning backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum BackendState {
    Starting,
    Running { pid: Option<u32> },
    Exited { code: Option<i32> },
}

/// One provider ask that still requires a client response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ObligationDebug {
    pub(crate) kind: String,
    pub(crate) id: Option<String>,
}

/// One recently opened structured subscription and the replay it selected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SubscriptionRecord {
    pub(crate) opened_at: DateTime<Utc>,
    pub(crate) query: String,
    pub(crate) replayed_first: Option<u64>,
    pub(crate) replayed_last: Option<u64>,
    pub(crate) replayed_count: usize,
    pub(crate) gap: bool,
}

/// Live per-session state embedded in each backend's debug view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionDebug {
    pub(crate) epoch: Option<u64>,
    pub(crate) subscriber_count: usize,
    pub(crate) buffer: Option<BufferDebug>,
    pub(crate) backend: BackendState,
    pub(crate) obligations: Vec<ObligationDebug>,
    pub(crate) recent_subscriptions: Vec<SubscriptionRecord>,
}

impl SessionDebug {
    pub(crate) fn new(
        primary: Option<&OutputDebug>,
        subscriber_count: usize,
        backend: BackendState,
        mut obligations: Vec<ObligationDebug>,
        recent_subscriptions: Vec<SubscriptionRecord>,
    ) -> Self {
        obligations.sort_unstable_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.id.cmp(&right.id))
        });
        Self {
            epoch: primary.map(|output| output.epoch),
            subscriber_count,
            buffer: primary.map(|output| output.buffer.clone()),
            backend,
            obligations,
            recent_subscriptions,
        }
    }
}
