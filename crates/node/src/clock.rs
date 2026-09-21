//! Product-policy time, kept separate from transport and harness deadlines.

use std::future::Future;
use std::pin::Pin;
use std::time::SystemTime;

use tokio::time::Instant;

/// Supplies monotonic and wall time to product policy.
///
/// Transport timers and test failure deadlines deliberately do not use this
/// clock: a harness may stop or advance policy time while real sockets and
/// bounded observations continue to make progress.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
    fn system_now(&self) -> SystemTime;
    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
}

/// Production policy time backed by the wall and Tokio clocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn system_now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        Box::pin(tokio::time::sleep_until(deadline))
    }
}
