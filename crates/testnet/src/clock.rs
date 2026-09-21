use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use node::Clock;
use tokio::sync::watch;
use tokio::time::Instant;

/// Policy time advanced explicitly by a specification.
///
/// Sleepers observe every advance through a watch channel. A sleeper that is
/// first polled after its deadline was crossed still completes immediately by
/// reading the shared current instant before it waits.
#[derive(Clone)]
pub struct DrivenClock {
    state: Arc<Mutex<State>>,
    changed: watch::Sender<u64>,
}

struct State {
    now: Instant,
    system_now: SystemTime,
}

impl DrivenClock {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            state: Arc::new(Mutex::new(State {
                now: Instant::now(),
                system_now: SystemTime::now(),
            })),
            changed,
        }
    }

    /// Moves policy time forward and wakes every sleeper whose deadline may
    /// now have passed.
    pub fn advance(&self, duration: Duration) {
        {
            let mut state = self.state.lock().expect("driven clock poisoned");
            state.now += duration;
            state.system_now = state
                .system_now
                .checked_add(duration)
                .expect("driven system time overflow");
        }
        self.changed.send_modify(|generation| *generation += 1);
    }
}

impl Default for DrivenClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for DrivenClock {
    fn now(&self) -> Instant {
        self.state.lock().expect("driven clock poisoned").now
    }

    fn system_now(&self) -> SystemTime {
        self.state.lock().expect("driven clock poisoned").system_now
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        let state = self.state.clone();
        let mut changed = self.changed.subscribe();
        Box::pin(async move {
            loop {
                if state.lock().expect("driven clock poisoned").now >= deadline {
                    return;
                }
                if changed.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        })
    }
}

impl node_test_support::IdentityClock for DrivenClock {
    fn system_now(&self) -> SystemTime {
        <Self as Clock>::system_now(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advance_wakes_every_deadline_crossed_and_moves_wall_time_together() {
        let clock = DrivenClock::new();
        let start = clock.now();
        let wall = clock.system_now();
        let first = tokio::spawn(clock.sleep_until(start + Duration::from_secs(2)));
        let second = tokio::spawn(clock.sleep_until(start + Duration::from_secs(3)));

        clock.advance(Duration::from_secs(2));
        first.await.expect("first sleeper");
        assert!(!second.is_finished());
        assert_eq!(clock.system_now(), wall + Duration::from_secs(2));

        clock.advance(Duration::from_secs(1));
        second.await.expect("second sleeper");
    }
}
