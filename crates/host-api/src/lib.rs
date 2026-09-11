use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

/// An admitted operation. Holding this opaque value keeps lifecycle teardown
/// from removing the profile's storage until the operation has finished.
pub struct OperationLease {
    _guard: OwnedRwLockReadGuard<()>,
}

/// Exclusive lifecycle access after all admitted operations have drained.
pub struct OperationBarrier {
    _guard: OwnedRwLockWriteGuard<()>,
}

/// Service work shares access to profile storage; lifecycle and trust commits
/// take exclusive access. Closing under the write lock drains accepted storage
/// work and prevents queued work from recreating a deleted device's state.
#[derive(Default)]
pub struct OperationGate {
    lock: Arc<RwLock<()>>,
    closed: AtomicBool,
    frozen: AtomicBool,
}

impl OperationGate {
    pub async fn admit(&self) -> Result<OperationLease, model::ProtocolError> {
        let guard = self.lock.clone().read_owned().await;
        self.check()?;
        Ok(OperationLease { _guard: guard })
    }

    pub async fn admit_mutation(&self) -> Result<OperationLease, model::ProtocolError> {
        let guard = self.lock.clone().read_owned().await;
        self.check_mutation()?;
        Ok(OperationLease { _guard: guard })
    }

    pub async fn barrier(&self) -> OperationBarrier {
        OperationBarrier {
            _guard: self.lock.clone().write_owned().await,
        }
    }

    /// Refuse new work, then wait until every accepted operation releases its
    /// owned lease. The returned barrier keeps storage teardown exclusive.
    pub async fn close_and_drain(&self) -> OperationBarrier {
        self.close();
        self.barrier().await
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Call under the exclusive gate to drain admitted lifecycle work first.
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    pub fn thaw(&self) {
        self.frozen.store(false, Ordering::Release);
    }

    pub fn check_mutation(&self) -> Result<(), model::ProtocolError> {
        self.check()?;
        if self.frozen.load(Ordering::Acquire) {
            return Err(model::ProtocolError::FailedPrecondition {
                message: "installation update is in progress".into(),
            });
        }
        Ok(())
    }

    pub fn check(&self) -> Result<(), model::ProtocolError> {
        if self.closed.load(Ordering::Acquire) {
            Err(model::ProtocolError::FailedPrecondition {
                message: "profile is unavailable".into(),
            })
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn close_drains_accepted_work_and_rejects_queued_work() {
        let gate = Arc::new(OperationGate::default());
        let accepted = gate.admit_mutation().await.unwrap();

        let closing = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.close_and_drain().await })
        };
        tokio::task::yield_now().await;

        let queued = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.admit_mutation().await })
        };
        assert!(
            !closing.is_finished(),
            "accepted work must drain before teardown"
        );
        drop(accepted);
        let barrier = closing.await.unwrap();
        drop(barrier);
        assert!(matches!(
            queued.await.unwrap(),
            Err(model::ProtocolError::FailedPrecondition { .. })
        ));
    }
}
