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

/// Live per-session state embedded in each backend's debug view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SessionDebug {
    pub(crate) epoch: Option<u64>,
    pub(crate) subscriber_count: usize,
    pub(crate) buffer: Option<BufferDebug>,
    pub(crate) backend: BackendState,
    pub(crate) obligations: Vec<ObligationDebug>,
}

impl SessionDebug {
    pub(crate) fn new(
        primary: Option<&OutputDebug>,
        subscriber_count: usize,
        backend: BackendState,
        mut obligations: Vec<ObligationDebug>,
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
        }
    }
}
