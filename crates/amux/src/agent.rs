//! Agent runtime: session lifecycle, PTY management, and hook dispatch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

pub mod claude;
mod hook;
mod log_source;
mod naming;
mod pty;
mod session;
#[cfg(any(debug_assertions, test))]
mod test_agent;

pub(crate) use hook::{ExternalHookBootstrap, HookError, HookOutcome};
pub(crate) use log_source::StructuredLogSource;
pub(crate) use naming::LocalAgentNameSource;
pub(crate) use pty::{PtyHandle, spawn_pty_agent};
pub(crate) use session::{Agent, AgentSession, SessionEvent, StopPolicy, StructuredInputTarget};
#[cfg(any(debug_assertions, test))]
pub(crate) use test_agent::TestAgentSession;
#[cfg(test)]
pub(crate) use test_agent::io::{TEST_ECHO_COMMAND, TEST_ECHO_V1};

#[derive(Clone)]
pub(crate) struct StructuredInputCancel {
    inner: Arc<StructuredInputCancelInner>,
}

struct StructuredInputCancelInner {
    cancelled: AtomicBool,
    tx: watch::Sender<bool>,
}

impl StructuredInputCancel {
    pub(crate) fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        Self {
            inner: Arc::new(StructuredInputCancelInner {
                cancelled: AtomicBool::new(false),
                tx,
            }),
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

impl Default for StructuredInputCancel {
    fn default() -> Self {
        Self::new()
    }
}
