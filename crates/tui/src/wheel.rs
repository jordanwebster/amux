//! How far one mouse-wheel event scrolls, in lines, the same everywhere a
//! list or page scrolls.
//!
//! A trackpad sends many small wheel events for one gesture, so each event
//! moves one line: stepping several per event made a slow two-finger
//! scroll jump. A fast burst still has to cover distance, so events that
//! arrive close together in the same direction step up to three lines.
//! The steps stay whole lines, and everywhere they apply scrolls by lines,
//! never by rows, so the motion is exact inside a tall row.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Events closer together than this belong to one burst.
const BURST: Duration = Duration::from_millis(50);
/// Burst lengths at which a step grows to two lines, then three.
const TWO_AFTER: u32 = 3;
const THREE_AFTER: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// The last event's time and direction, and how long its burst has run.
#[derive(Clone, Copy, Debug)]
struct Burst {
    at: Instant,
    direction: Direction,
    length: u32,
}

static LAST: Mutex<Option<Burst>> = Mutex::new(None);

/// The lines one wheel event in `direction` scrolls, now.
pub fn lines(direction: Direction) -> usize {
    let mut last = LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (lines, burst) = step(*last, direction, Instant::now());
    *last = Some(burst);
    lines
}

/// The lines for an event at `now`, after `last`: one, rising to three
/// through a burst in one direction. A pause or a turn starts over.
fn step(last: Option<Burst>, direction: Direction, now: Instant) -> (usize, Burst) {
    let length = match last {
        Some(last)
            if last.direction == direction && now.saturating_duration_since(last.at) <= BURST =>
        {
            last.length + 1
        }
        _ => 0,
    };
    let lines = if length >= THREE_AFTER {
        3
    } else if length >= TWO_AFTER {
        2
    } else {
        1
    };
    (
        lines,
        Burst {
            at: now,
            direction,
            length,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_event_moves_one_line_and_a_burst_rises_to_three() {
        let start = Instant::now();
        let mut last = None;
        let mut steps = Vec::new();
        for i in 0..8 {
            let (lines, burst) = step(last, Direction::Down, start + BURST / 2 * i);
            steps.push(lines);
            last = Some(burst);
        }
        assert_eq!(steps, [1, 1, 1, 2, 2, 2, 3, 3]);
        // A pause starts over.
        let (lines, burst) = step(last, Direction::Down, start + Duration::from_secs(5));
        assert_eq!(lines, 1);
        // So does a turn, however fast.
        let (lines, _) = step(Some(burst), Direction::Up, burst.at);
        assert_eq!(lines, 1);
    }
}
