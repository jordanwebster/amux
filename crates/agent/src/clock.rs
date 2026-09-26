//! Time for the agent process: the interpreter's ticks and the lifecycle's
//! deadlines (the grace after the daemon leaves, the drain deadline for an
//! orphaned ask, the wait for a stopping provider) all read one clock, so a
//! test can drive every deadline by hand.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{Notify, watch};

pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Milliseconds since the Unix epoch, and a way to wait for one.
pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> i64;
    /// Resolves once `now_ms() >= at_ms`.
    fn sleep_until(&self, at_ms: i64) -> Sleep;
}

/// The wall clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as i64)
    }

    fn sleep_until(&self, at_ms: i64) -> Sleep {
        let wait = at_ms.saturating_sub(self.now_ms()).max(0) as u64;
        Box::pin(tokio::time::sleep(Duration::from_millis(wait)))
    }
}

/// A clock that moves only when told to. It also reports who is sleeping,
/// so a test can wait until the agent has armed a deadline before moving
/// past it.
#[derive(Clone)]
pub struct ManualClock {
    now: Arc<watch::Sender<i64>>,
    sleepers: Arc<Mutex<Sleepers>>,
    armed: Arc<Notify>,
}

#[derive(Default)]
struct Sleepers {
    next: u64,
    until: BTreeMap<u64, i64>,
}

impl ManualClock {
    pub fn new(now_ms: i64) -> Self {
        Self {
            now: Arc::new(watch::Sender::new(now_ms)),
            sleepers: Arc::default(),
            armed: Arc::default(),
        }
    }

    pub fn set(&self, now_ms: i64) {
        self.now.send_modify(|now| *now = (*now).max(now_ms));
    }

    pub fn advance(&self, ms: i64) {
        self.now.send_modify(|now| *now += ms);
    }

    /// The deadlines someone is waiting for, soonest first.
    pub fn sleeping(&self) -> Vec<i64> {
        let sleepers = self
            .sleepers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut until: Vec<i64> = sleepers.until.values().copied().collect();
        until.sort_unstable();
        until
    }

    /// Waits until someone sleeps until exactly `at_ms`.
    pub async fn armed(&self, at_ms: i64) {
        loop {
            let notified = self.armed.notified();
            if self.sleeping().contains(&at_ms) {
                return;
            }
            notified.await;
        }
    }

    fn register(&self, at_ms: i64) -> u64 {
        let mut sleepers = self
            .sleepers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        sleepers.next += 1;
        let id = sleepers.next;
        sleepers.until.insert(id, at_ms);
        drop(sleepers);
        self.armed.notify_waiters();
        id
    }

    fn release(&self, id: u64) {
        self.sleepers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .until
            .remove(&id);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        *self.now.borrow()
    }

    fn sleep_until(&self, at_ms: i64) -> Sleep {
        let clock = self.clone();
        Box::pin(async move {
            let _registered = Registered {
                id: clock.register(at_ms),
                clock: clock.clone(),
            };
            let mut now = clock.now.subscribe();
            while *now.borrow_and_update() < at_ms {
                if now.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        })
    }
}

struct Registered {
    id: u64,
    clock: ManualClock,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.clock.release(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_sleeper_wakes_only_when_the_clock_reaches_its_deadline() {
        let clock = ManualClock::new(1_000);
        let sleep = tokio::spawn(clock.sleep_until(1_500));
        clock.armed(1_500).await;
        clock.advance(499);
        tokio::task::yield_now().await;
        assert!(!sleep.is_finished());
        clock.advance(1);
        sleep.await.expect("the sleeper finishes");
        assert!(clock.sleeping().is_empty(), "a woken sleeper is released");
    }
}
