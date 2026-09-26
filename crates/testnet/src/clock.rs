//! Policy time that moves only when a specification says so.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agent_dir::{Clock, ManualClock, Sleep};

/// The clock every daemon in a driven net runs its policy timers on:
/// retention, the outboxes' retries and notification delays, credential
/// refresh on the edges, reply and start deadlines. It starts at the wall
/// time the net was built, so credentials and certificates minted against
/// it look current, and then moves only on [`DrivenClock::advance`].
///
/// Transports stay on real time: a QUIC idle timer or a socket read is not
/// policy, and the harness's own waits bound real work.
#[derive(Clone)]
pub struct DrivenClock {
    inner: ManualClock,
}

impl DrivenClock {
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64);
        Self {
            inner: ManualClock::new(now),
        }
    }

    /// Moves policy time forward; every sleeper whose deadline is crossed
    /// wakes.
    pub fn advance(&self, by: Duration) {
        self.inner.advance(by.as_millis() as i64);
    }

    /// The deadlines someone is waiting for, soonest first.
    pub fn sleeping(&self) -> Vec<i64> {
        self.inner.sleeping()
    }

    /// Resolves once something sleeps until exactly `at_ms`, so a test can
    /// move past a deadline knowing it was armed.
    pub async fn armed(&self, at_ms: i64) {
        self.inner.armed(at_ms).await;
    }
}

impl Default for DrivenClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for DrivenClock {
    fn now_ms(&self) -> i64 {
        self.inner.now_ms()
    }

    fn sleep_until(&self, at_ms: i64) -> Sleep {
        self.inner.sleep_until(at_ms)
    }
}
