use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

/// Serializes profile lifecycle with agent mutations and trust commits. Closing
/// under the lock prevents queued work from recreating a deleted device's state.
#[derive(Default)]
pub(crate) struct OperationGate {
    mutex: Arc<Mutex<()>>,
    closed: AtomicBool,
}

impl OperationGate {
    pub(crate) async fn lock(&self) -> MutexGuard<'_, ()> {
        self.mutex.lock().await
    }

    pub(crate) async fn lock_owned(self: Arc<Self>) -> OwnedMutexGuard<()> {
        self.mutex.clone().lock_owned().await
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(crate) fn check(&self) -> Result<(), crate::protocol::ProtocolError> {
        if self.closed.load(Ordering::Acquire) {
            Err(crate::protocol::ProtocolError::FailedPrecondition {
                message: "profile is unavailable".into(),
            })
        } else {
            Ok(())
        }
    }
}
