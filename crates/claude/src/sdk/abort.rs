use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum ShutdownReason {
    Running = 0,
    Closed = 1,
    Aborted = 2,
    Dropped = 3,
    TransportFailed = 4,
}

pub(crate) struct Shutdown {
    token: CancellationToken,
    reason: AtomicU8,
}

impl Shutdown {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            token: CancellationToken::new(),
            reason: AtomicU8::new(ShutdownReason::Running as u8),
        })
    }

    pub(crate) fn request(&self, reason: ShutdownReason) {
        let _ = self.reason.compare_exchange(
            ShutdownReason::Running as u8,
            reason as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.token.cancel();
    }

    pub(crate) fn reason(&self) -> ShutdownReason {
        match self.reason.load(Ordering::SeqCst) {
            1 => ShutdownReason::Closed,
            2 => ShutdownReason::Aborted,
            3 => ShutdownReason::Dropped,
            4 => ShutdownReason::TransportFailed,
            _ => ShutdownReason::Running,
        }
    }

    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

/// A cloneable handle that forcefully terminates an owned query.
///
/// This is distinct from [`Control::interrupt`](crate::sdk::Control::interrupt), which
/// asks Claude to stop only the active turn while keeping the query alive.
#[derive(Clone)]
pub struct AbortHandle {
    shutdown: Arc<Shutdown>,
}

impl AbortHandle {
    pub(crate) fn new(shutdown: Arc<Shutdown>) -> Self {
        Self { shutdown }
    }

    pub fn abort(&self) {
        self.shutdown.request(ShutdownReason::Aborted);
    }

    pub fn is_aborted(&self) -> bool {
        self.shutdown.reason() == ShutdownReason::Aborted
    }
}
