//! The trace ring: what the chrome saw, bounded.
//!
//! A diagnostic capture has to answer "how did the screen get like this?",
//! and the honest answer is the events that produced it — every Msg,
//! input and draw since a known-good starting state. Keeping all of them
//! would mean keeping a whole chat session in memory, so the ring keeps
//! two segments: the events since the older of two snapshots, and nothing
//! before it. Whatever the ring holds is replayable in full; the moment a
//! third segment would start, the oldest one is dropped along with the
//! snapshot it began from.
//!
//! Snapshots are taken at segment boundaries, just before a draw, by
//! cloning the live Model, view and theme. Cloning at a boundary costs
//! one clone per five thousand events; the alternative — folding evicted
//! events into a shadow view as they leave — costs a render per evicted
//! input on the live loop, which is the one place that cannot afford it.
//!
//! Events are serialized on the way in, not on the way out. A ring that
//! held live `TraceEvent`s could hold a `Msg` whose serialization fails
//! only at capture time, exactly when the report is being written; doing
//! it now means an event that cannot be recorded is dropped while the
//! screen is still there to be looked at.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use amux_ui::Model;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::chrome::TraceEvent;
use crate::render::Theme;
use crate::view::ViewState;

/// Events per segment. Two segments are retained, so a capture replays
/// from between one and two of these back — long enough to cover the
/// exchange a person is looking at, short enough that the snapshot pair
/// stays a rounding error next to the Model itself.
pub const SEGMENT_LEN: usize = 5_000;

/// Everything a replay starts from: a live clone taken at a segment
/// boundary, right before a draw.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub model: Model,
    pub view: ViewState,
    pub theme: Theme,
    pub at: DateTime<Utc>,
}

/// The events recorded since one snapshot, already serialized.
struct Segment {
    snapshot: Snapshot,
    events: Vec<String>,
}

/// The replayable window, bounded to two segments.
pub struct TraceRing {
    segment_len: usize,
    previous: Option<Segment>,
    /// `None` until the first draw takes the first snapshot. Events
    /// recorded before that are dropped: with nothing to replay them
    /// from, keeping them would only make a window that cannot be folded.
    current: Option<Segment>,
}

impl TraceRing {
    pub fn new(segment_len: usize) -> Self {
        Self {
            segment_len: segment_len.max(1),
            previous: None,
            current: None,
        }
    }

    /// Append one event. Called from the live loop and from the runtime's
    /// Msg tap, so it must never fail loudly: an event that will not
    /// serialize is dropped with a warning rather than taking the chrome
    /// down over a diagnostic.
    pub fn record(&mut self, event: &TraceEvent) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        match serde_json::to_string(event) {
            Ok(line) => current.events.push(line),
            Err(error) => tracing::warn!(%error, "trace event dropped: not serializable"),
        }
    }

    /// Start a new segment if the current one is full, or take the very
    /// first snapshot. Called just before each draw, so the snapshot is
    /// state as of a frame boundary and the `Draw` event that follows is
    /// the first thing replayed from it.
    pub fn roll_if_due(
        &mut self,
        model: &Model,
        view: &ViewState,
        theme: Theme,
        now: DateTime<Utc>,
    ) {
        let due = match self.current.as_ref() {
            None => true,
            Some(current) => current.events.len() >= self.segment_len,
        };
        if !due {
            return;
        }
        let snapshot = Snapshot {
            model: model.clone(),
            view: view.clone(),
            theme,
            at: now,
        };
        // The oldest segment leaves with its snapshot: everything the ring
        // still holds remains foldable from the snapshot it now starts at.
        self.previous = self.current.take();
        self.current = Some(Segment {
            snapshot,
            events: Vec::new(),
        });
    }

    /// The replayable window as of now: the oldest retained snapshot plus
    /// every event since it. `None` before the first draw, when there is
    /// no state to replay from.
    pub fn window(&self) -> Option<TraceWindow> {
        let current = self.current.as_ref()?;
        let (snapshot, mut events) = match self.previous.as_ref() {
            Some(previous) => (previous.snapshot.clone(), previous.events.clone()),
            None => (current.snapshot.clone(), Vec::new()),
        };
        events.extend(current.events.iter().cloned());
        Some(TraceWindow { snapshot, events })
    }

    /// Events currently retained, across both segments.
    pub fn len(&self) -> usize {
        let count = |segment: &Option<Segment>| {
            segment
                .as_ref()
                .map(|segment| segment.events.len())
                .unwrap_or(0)
        };
        count(&self.previous) + count(&self.current)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One snapshot and the events after it. Nothing recorded after a window
/// is taken can enter it — that is what makes a capture an instant rather
/// than a period.
#[derive(Clone, Debug)]
pub struct TraceWindow {
    pub snapshot: Snapshot,
    pub events: Vec<String>,
}

impl TraceWindow {
    /// `trace.jsonl`: the snapshot on the first line, then one event per
    /// line, in the order they happened.
    pub fn write_jsonl(&self, out: &mut dyn Write) -> io::Result<()> {
        let snapshot = serde_json::to_string(&self.snapshot).map_err(io::Error::other)?;
        writeln!(out, "{snapshot}")?;
        for event in &self.events {
            writeln!(out, "{event}")?;
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write_jsonl(&mut out)?;
        Ok(out)
    }

    /// Read a `trace.jsonl` back. Events stay as text: a reader that only
    /// wants the count, or the event at one index, should not pay to parse
    /// a whole session.
    pub fn read_jsonl(bytes: &[u8]) -> Result<Self, TraceError> {
        let text = std::str::from_utf8(bytes).map_err(|_| TraceError::NotUtf8)?;
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let first = lines.next().ok_or(TraceError::Empty)?;
        let snapshot = serde_json::from_str(first).map_err(|error| TraceError::Line {
            line: 1,
            problem: error.to_string(),
        })?;
        Ok(Self {
            snapshot,
            events: lines.map(str::to_string).collect(),
        })
    }

    /// The event at `index`, parsed. The line number in an error counts
    /// the snapshot line, so it points at the line a person would open.
    pub fn event(&self, index: usize) -> Result<TraceEvent, TraceError> {
        let line = self
            .events
            .get(index)
            .ok_or(TraceError::OutOfRange { index })?;
        serde_json::from_str(line).map_err(|error| TraceError::Line {
            line: index + 2,
            problem: error.to_string(),
        })
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TraceError {
    #[error("trace is empty: no snapshot line")]
    Empty,
    #[error("trace is not UTF-8")]
    NotUtf8,
    #[error("trace line {line}: {problem}")]
    Line { line: usize, problem: String },
    #[error("no trace event at index {index}")]
    OutOfRange { index: usize },
}

/// The ring as the live loop holds it: the loop records inputs, draws and
/// dispatches, and the runtime's Msg tap records folds from wherever it
/// folds them.
pub type SharedTrace = Arc<Mutex<TraceRing>>;

pub fn shared(segment_len: usize) -> SharedTrace {
    Arc::new(Mutex::new(TraceRing::new(segment_len)))
}

/// Record one event into a shared ring, tolerating a poisoned lock: a
/// panic elsewhere must not turn every later keypress into a second
/// panic, and the panic hook writes its own report anyway.
pub fn record_shared(trace: &SharedTrace, event: &TraceEvent) {
    match trace.lock() {
        Ok(mut ring) => ring.record(event),
        Err(_) => tracing::warn!("trace event dropped: ring lock poisoned"),
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::claude::AskDocument;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::chrome::{InputEvent, KeyRecord};
    use crate::fixtures::{NamedState, fixture};

    fn press(label: char) -> TraceEvent {
        TraceEvent::Input {
            event: InputEvent::Key(KeyRecord::from_event(KeyEvent::new(
                KeyCode::Char(label),
                KeyModifiers::NONE,
            ))),
            viewport: (120, 40),
            now: Utc::now(),
        }
    }

    /// Read back which characters a window's input events carry, so a test
    /// can say exactly where the retained window starts.
    fn typed(window: &TraceWindow) -> String {
        (0..window.events.len())
            .filter_map(|index| match window.event(index).expect("event parses") {
                TraceEvent::Input {
                    event: InputEvent::Key(key),
                    ..
                } => match key.to_event().code {
                    KeyCode::Char(c) => Some(c),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn nothing_is_retained_before_the_first_draw() {
        let mut ring = TraceRing::new(4);
        ring.record(&press('a'));
        assert!(ring.is_empty());
        assert!(
            ring.window().is_none(),
            "a window with no state to fold from is no window at all"
        );
    }

    #[test]
    fn a_rolled_ring_starts_at_the_previous_snapshot() {
        let built = fixture(NamedState::Fleet);
        let mut ring = TraceRing::new(3);
        let roll = |ring: &mut TraceRing| {
            ring.roll_if_due(&built.model, &built.view, Theme::default(), Utc::now());
        };

        // First segment: a b c. The roll that opens it is the first draw.
        roll(&mut ring);
        for label in "abc".chars() {
            ring.record(&press(label));
        }
        // Full, so the next draw rolls: a b c become the previous segment.
        roll(&mut ring);
        for label in "de".chars() {
            ring.record(&press(label));
        }
        assert_eq!(typed(&ring.window().expect("window")), "abcde");

        // Filling the second segment and rolling again drops the oldest.
        ring.record(&press('f'));
        roll(&mut ring);
        ring.record(&press('g'));
        assert_eq!(
            typed(&ring.window().expect("window")),
            "defg",
            "the window starts at the older of the two retained snapshots"
        );
        assert_eq!(ring.len(), 4);
    }

    #[test]
    fn a_partly_filled_segment_does_not_roll() {
        let built = fixture(NamedState::Fleet);
        let mut ring = TraceRing::new(10);
        ring.roll_if_due(&built.model, &built.view, Theme::default(), Utc::now());
        ring.record(&press('a'));
        ring.roll_if_due(&built.model, &built.view, Theme::default(), Utc::now());
        ring.record(&press('b'));
        assert_eq!(typed(&ring.window().expect("window")), "ab");
        assert_eq!(ring.len(), 2, "a draw mid-segment records nothing extra");
    }

    #[test]
    fn a_window_round_trips_through_jsonl() {
        let built = fixture(NamedState::ClaudeIdle);
        let mut ring = TraceRing::new(SEGMENT_LEN);
        let at = Utc::now();
        ring.roll_if_due(&built.model, &built.view, Theme::default(), at);
        ring.record(&TraceEvent::Draw {
            viewport: (120, 40),
            now: at,
        });
        ring.record(&press('h'));
        ring.record(&TraceEvent::Drained);

        let window = ring.window().expect("window");
        let bytes = window.to_bytes().expect("window serializes");
        let back = TraceWindow::read_jsonl(&bytes).expect("window parses");

        assert_eq!(back.events.len(), 3);
        assert_eq!(typed(&back), "h");
        assert_eq!(back.snapshot.at, window.snapshot.at);
        assert_eq!(back.snapshot.theme, window.snapshot.theme);
        assert!(back.snapshot.view.chat.is_some(), "the open chat travels");
        assert_eq!(
            back.to_bytes().expect("re-serializes"),
            bytes,
            "reading and writing a trace is lossless"
        );
    }

    #[test]
    fn a_snapshot_round_trip_retains_an_edit_ask_document() {
        let built = fixture(NamedState::ClaudePermissionAsk);
        let agent = built.view.chat.as_ref().expect("Claude chat open").agent;
        let mut ring = TraceRing::new(SEGMENT_LEN);
        ring.roll_if_due(&built.model, &built.view, Theme::default(), built.now);

        let bytes = ring
            .window()
            .expect("snapshot window")
            .to_bytes()
            .expect("snapshot serializes");
        let back = TraceWindow::read_jsonl(&bytes).expect("snapshot parses");
        let ask = back
            .snapshot
            .model
            .claude(agent)
            .and_then(|layer| layer.ask_head())
            .expect("Edit ask retained");
        assert!(
            matches!(ask.document, Some(AskDocument::Diff(_))),
            "Edit ask retains its diff document: {:?}",
            ask.document
        );
    }

    #[test]
    fn a_bad_line_names_the_line() {
        assert!(matches!(
            TraceWindow::read_jsonl(b""),
            Err(TraceError::Empty)
        ));
        assert!(matches!(
            TraceWindow::read_jsonl(b"not a snapshot\n"),
            Err(TraceError::Line { line: 1, .. })
        ));

        let built = fixture(NamedState::Fleet);
        let mut ring = TraceRing::new(SEGMENT_LEN);
        ring.roll_if_due(&built.model, &built.view, Theme::default(), Utc::now());
        ring.record(&press('a'));
        let mut bytes = ring.window().expect("window").to_bytes().expect("bytes");
        bytes.extend_from_slice(b"{\"NotAnEvent\":true}\n");
        let window = TraceWindow::read_jsonl(&bytes).expect("the snapshot still parses");
        assert!(matches!(
            window.event(1),
            Err(TraceError::Line { line: 3, .. })
        ));
        assert!(matches!(
            window.event(9),
            Err(TraceError::OutOfRange { index: 9 })
        ));
    }
}
