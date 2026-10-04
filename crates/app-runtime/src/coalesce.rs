//! Gathers changes between the host's turns: however many updates land
//! before the main thread runs next, the host is woken once and takes them
//! all together.

use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

/// A wake the host schedules onto its main thread. Called from a worker
/// thread under the coalescer's lock, so it must only schedule, never do
/// the work, and never take the batch itself.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// Changed keys and flags, each key once, until the host takes them.
pub struct Coalescer<K> {
    pending: Mutex<Pending<K>>,
    wake: WakeFn,
}

struct Pending<K> {
    keys: Vec<K>,
    seen: HashSet<K>,
    reloaded: bool,
    other: bool,
    /// The host has been woken and has not taken the batch yet.
    woken: bool,
}

/// What the host takes on its turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Batch<K> {
    pub keys: Vec<K>,
    pub reloaded: bool,
    pub other: bool,
}

impl<K> Default for Batch<K> {
    fn default() -> Self {
        Batch {
            keys: Vec::new(),
            reloaded: false,
            other: false,
        }
    }
}

impl<K: Clone + Eq + Hash> Coalescer<K> {
    pub fn new(wake: WakeFn) -> Coalescer<K> {
        Coalescer {
            pending: Mutex::new(Pending {
                keys: Vec::new(),
                seen: HashSet::new(),
                reloaded: false,
                other: false,
                woken: false,
            }),
            wake,
        }
    }

    /// Adds changes; wakes the host unless it already has a wake coming.
    /// The wake is delivered under the lock: a take on another thread
    /// cannot slip between the batch being marked woken and the host
    /// hearing of it, so a wake the host receives always has its batch
    /// still pending, and no second wake arrives before that take.
    pub fn push(&self, keys: impl IntoIterator<Item = K>, reloaded: bool, other: bool) {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        let Pending {
            keys: held, seen, ..
        } = &mut *pending;
        for key in keys {
            if seen.insert(key.clone()) {
                held.push(key);
            }
        }
        pending.reloaded |= reloaded;
        pending.other |= other;
        let moved = !pending.keys.is_empty() || pending.reloaded || pending.other;
        if moved && !pending.woken {
            pending.woken = true;
            (self.wake)();
        }
    }

    /// Everything since the last take; the next change wakes the host again.
    pub fn take(&self) -> Batch<K> {
        let mut pending = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        pending.woken = false;
        pending.seen.clear();
        Batch {
            keys: std::mem::take(&mut pending.keys),
            reloaded: std::mem::take(&mut pending.reloaded),
            other: std::mem::take(&mut pending.other),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn counted() -> (Arc<AtomicUsize>, Coalescer<u32>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        let coalescer = Coalescer::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        (wakes, coalescer)
    }

    #[test]
    fn many_updates_before_a_turn_wake_the_host_once_with_each_key_once() {
        let (wakes, coalescer) = counted();
        coalescer.push([1, 2], false, false);
        coalescer.push([2, 3], false, true);
        coalescer.push([1], true, false);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert_eq!(
            coalescer.take(),
            Batch {
                keys: vec![1, 2, 3],
                reloaded: true,
                other: true,
            }
        );
        assert_eq!(coalescer.take(), Batch::default());
    }

    #[test]
    fn a_change_after_the_take_wakes_the_host_again() {
        let (wakes, coalescer) = counted();
        coalescer.push([1], false, false);
        coalescer.take();
        coalescer.push([1], false, false);
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert_eq!(coalescer.take().keys, vec![1]);
    }

    #[test]
    fn nothing_changed_wakes_nobody() {
        let (wakes, coalescer) = counted();
        coalescer.push([], false, false);
        assert_eq!(wakes.load(Ordering::SeqCst), 0);
    }
}
