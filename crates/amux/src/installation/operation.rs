use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{OwnedRwLockWriteGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Service work shares access to profile storage; lifecycle and trust commits
/// take exclusive access. Closing under the write lock drains accepted storage
/// work and prevents queued work from recreating a deleted device's state.
#[derive(Default)]
pub(crate) struct OperationGate {
    lock: Arc<RwLock<()>>,
    closed: AtomicBool,
    frozen: AtomicBool,
}

impl OperationGate {
    pub(crate) async fn lock(&self) -> RwLockWriteGuard<'_, ()> {
        self.lock.write().await
    }

    pub(crate) async fn lock_owned(self: Arc<Self>) -> OwnedRwLockWriteGuard<()> {
        self.lock.clone().write_owned().await
    }

    pub(crate) async fn read(&self) -> RwLockReadGuard<'_, ()> {
        self.lock.read().await
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Call under the exclusive gate to drain admitted lifecycle work first.
    pub(crate) fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    pub(crate) fn thaw(&self) {
        self.frozen.store(false, Ordering::Release);
    }

    pub(crate) fn check_mutation(&self) -> Result<(), crate::protocol::ProtocolError> {
        self.check()?;
        if self.frozen.load(Ordering::Acquire) {
            return Err(crate::protocol::ProtocolError::FailedPrecondition {
                message: "installation update is in progress".into(),
            });
        }
        Ok(())
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
