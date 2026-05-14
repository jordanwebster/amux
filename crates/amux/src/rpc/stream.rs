use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct RpcCallCancellation {
    inner: Arc<RpcCallCancellationInner>,
}

struct RpcCallCancellationInner {
    cancelled: AtomicBool,
    tx: watch::Sender<bool>,
}

impl RpcCallCancellation {
    pub(in crate::rpc) fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            inner: Arc::new(RpcCallCancellationInner {
                cancelled: AtomicBool::new(false),
                tx,
            }),
        }
    }

    pub(in crate::rpc) fn cancel(&self) {
        if !self.inner.cancelled.swap(true, Ordering::SeqCst) {
            let _ = self.inner.tx.send(true);
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        let mut rx = self.inner.tx.subscribe();
        if self.is_cancelled() {
            return;
        }
        while rx.changed().await.is_ok() {
            if self.is_cancelled() {
                return;
            }
        }
    }
}

impl fmt::Debug for RpcCallCancellation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcCallCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}
