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
    let mut check_hung = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // The deadline binds the check future itself, not just the gaps
        // between polls: an observable that blocks forever must surface as
        // a timed-out assertion with a dump, never as a hung test.
        match tokio::time::timeout(remaining, check()).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(_) => {
                check_hung = true;
                break;
            }
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let hung_note = if check_hung {
        " (the final condition poll itself did not resolve)"
    } else {
        ""
    };
    // The dump itself queries live daemons and must not be able to hang the
    // failing test either.
    let dump = tokio::time::timeout(DEFAULT_TIMEOUT, dump)
        .await
        .unwrap_or_else(|_| "<state dump timed out>".to_string());
    panic!("spec assertion timed out after {DEFAULT_TIMEOUT:?}{hung_note}: {assertion}\n{dump}");
}

/// Polls `check` for `duration` and fails as soon as it returns `false`.
///
/// This is for absence/stability assertions where a one-shot "eventually
/// true" check would race a short-lived bad transition.
pub(crate) async fn consistently_for<C, D>(
    assertion: &str,
    duration: Duration,
    mut check: C,
    dump: D,
) where
    C: AsyncFnMut() -> bool,
    D: Future<Output = String>,
{
    let deadline = Instant::now() + duration;
    loop {
        if Instant::now() >= deadline {
            return;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, check()).await {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                let dump = tokio::time::timeout(DEFAULT_TIMEOUT, dump)
                    .await
                    .unwrap_or_else(|_| "<state dump timed out>".to_string());
                panic!("spec assertion failed during {duration:?}: {assertion}\n{dump}");
            }
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn consistency_predicate_timeout_is_a_failure() {
        let duration = Duration::from_millis(100);
        let assertion = tokio::spawn(consistently_for(
            "the state stays valid",
            duration,
            async || std::future::pending::<bool>().await,
            async { "current state".to_string() },
        ));

        let failure = assertion.await.expect_err("hung predicate must fail");
        let panic = failure.into_panic();
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .expect("panic message");

        assert!(message.contains("spec assertion failed during 100ms"));
        assert!(message.contains("the state stays valid"));
        assert!(message.contains("current state"));
    }
}
