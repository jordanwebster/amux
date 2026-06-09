//! Eventually-style polling assertions with failure dumps.
//!
//! Every observable assertion in the spec suite goes through [`eventually`]:
//! tests never contain retry loops or sleeps. On timeout the assertion panics
//! with a dump of the declared topology and the network's current state so a
//! black-box failure is still debuggable.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// Default assertion timeout.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Interval between condition polls.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Polls `check` until it returns `true` or [`DEFAULT_TIMEOUT`] elapses.
///
/// On timeout, panics with `assertion` and the dump produced by `dump`. The
/// dump future is only awaited on failure.
pub(crate) async fn eventually<C, D>(assertion: &str, mut check: C, dump: D)
where
    C: AsyncFnMut() -> bool,
    D: Future<Output = String>,
{
    let deadline = Instant::now() + DEFAULT_TIMEOUT;
    loop {
        if check().await {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!(
        "spec assertion timed out after {DEFAULT_TIMEOUT:?}: {assertion}\n{}",
        dump.await
    );
}
