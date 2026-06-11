use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::time::Duration;

use tokio::time::Instant;

pub(crate) const ROUTING_HOST_CAP: usize = 1000;
pub(crate) const CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW: Duration = Duration::from_secs(5 * 60);
pub(crate) const EXTERNAL_TCP_TLS_HANDSHAKE_RATE_LIMIT: usize = 10;
pub(crate) const EXTERNAL_TCP_TLS_HANDSHAKE_RATE_WINDOW: Duration = Duration::from_secs(60);
pub(crate) const EXTERNAL_TCP_TLS_HANDSHAKE_CONCURRENCY: usize = 128;
pub(crate) const CLOUD_INBOUND_TUNNEL_RATE_LIMIT: usize = 30;
pub(crate) const CLOUD_INBOUND_TUNNEL_RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) struct SlidingWindowRateLimiter<K> {
    limit: usize,
    window: Duration,
    entries: HashMap<K, VecDeque<Instant>>,
}

impl<K> SlidingWindowRateLimiter<K>
where
    K: Eq + Hash,
{
    pub(crate) fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            entries: HashMap::new(),
        }
    }

    pub(crate) fn allow(&mut self, key: K) -> bool {
        self.allow_at(key, Instant::now())
    }

    fn allow_at(&mut self, key: K, now: Instant) -> bool {
        self.prune(now);
        let timestamps = self.entries.entry(key).or_default();
        if timestamps.len() >= self.limit {
            return false;
        }
        timestamps.push_back(now);
        true
    }

    fn prune(&mut self, now: Instant) {
        let window = self.window;
        self.entries.retain(|_, timestamps| {
            while timestamps
                .front()
                .is_some_and(|timestamp| *timestamp + window <= now)
            {
                timestamps.pop_front();
            }
            !timestamps.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_window_limiter_caps_then_recovers_after_window() {
        let start = Instant::now();
        let mut limiter = SlidingWindowRateLimiter::new(2, Duration::from_secs(60));

        assert!(limiter.allow_at("peer", start));
        assert!(limiter.allow_at("peer", start + Duration::from_secs(1)));
        assert!(!limiter.allow_at("peer", start + Duration::from_secs(2)));
        assert!(limiter.allow_at("peer", start + Duration::from_secs(60)));
    }

    #[test]
    fn sliding_window_limiter_is_keyed_independently() {
        let start = Instant::now();
        let mut limiter = SlidingWindowRateLimiter::new(1, Duration::from_secs(60));

        assert!(limiter.allow_at("one", start));
        assert!(!limiter.allow_at("one", start));
        assert!(limiter.allow_at("two", start));
    }
}
