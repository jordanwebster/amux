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
    eventually_with_timeout(
        assertion,
        DEFAULT_TIMEOUT,
        &mut check,
        async || std::future::pending::<bool>().await,
        dump,
    )
    .await;
}

/// Waits for a causal state-change notification, retaining a short poll as a
/// fallback for closed or replaced event sources.
pub(crate) async fn eventually_on<C, N, D>(assertion: &str, mut check: C, notified: N, dump: D)
where
    C: AsyncFnMut() -> bool,
    N: AsyncFnMut() -> bool,
    D: Future<Output = String>,
{
    eventually_with_timeout(assertion, DEFAULT_TIMEOUT, &mut check, notified, dump).await;
}

async fn eventually_with_timeout<C, N, D>(
    assertion: &str,
    timeout: Duration,
    mut check: C,
    mut notified: N,
    dump: D,
) where
    C: AsyncFnMut() -> bool,
    N: AsyncFnMut() -> bool,
    D: Future<Output = String>,
{
    let deadline = Instant::now() + timeout;
    let mut check_hung = false;
    let mut notifications_open = true;
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
        let remaining = deadline.saturating_duration_since(Instant::now());
        if notifications_open {
            tokio::select! {
                open = notified() => notifications_open = open,
                _ = tokio::time::sleep(POLL_INTERVAL.min(remaining)) => {}
            }
        } else {
            tokio::time::sleep(POLL_INTERVAL.min(remaining)).await;
        }
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
    panic!("spec assertion timed out after {timeout:?}{hung_note}: {assertion}\n{dump}");
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
    use futures_util::FutureExt;
    use node::Clock;

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

    #[tokio::test]
    async fn real_assertion_deadline_fires_while_policy_clock_is_stopped() {
        let clock = crate::DrivenClock::new();
        let stopped_at = clock.now();
        let waiting_clock = clock.clone();
        let assertion = std::panic::AssertUnwindSafe(async move {
            eventually_with_timeout(
                "a stopped policy timer does not stop the harness",
                Duration::from_millis(25),
                async || {
                    waiting_clock
                        .sleep_until(stopped_at + Duration::from_secs(1))
                        .await;
                    true
                },
                async || std::future::pending::<bool>().await,
                async { "policy clock remained stopped".to_string() },
            )
            .await;
        })
        .catch_unwind();

        let failure = tokio::time::timeout(Duration::from_millis(500), assertion)
            .await
            .expect("real harness deadline")
            .expect_err("stopped policy clock must time out");
        let message = failure
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| failure.downcast_ref::<&str>().copied())
            .expect("panic message");
        assert!(message.contains("spec assertion timed out after 25ms"));
        assert_eq!(clock.now(), stopped_at);
    }
}
