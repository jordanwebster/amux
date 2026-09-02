//! Folding a recorded report back into the frame it was captured from.
//!
//! A replay is the same chrome walking the same events: it starts from the
//! trace's snapshot, folds each Msg into a Model of its own, and steps
//! every event through [`Chrome::step`] exactly as the live loop did —
//! including the draws, which fill the paint caches the next keypress
//! reads. Effects are dropped: there is no runtime to dispatch to, no
//! terminal to write to and no agent to attach to, and the events that
//! record what those effects produced (an op id, a chat opening) are
//! already in the trace.
//!
//! [`verify`] is what makes a report trustworthy. Replaying to the end and
//! comparing against the captured frame turns "here is a recording" into
//! "this recording still produces this screen in this build" — and when it
//! does not, the diff names the cells, which is usually the fix's first
//! clue rather than a failure.

use std::path::Path;

use amux_ui::report::{FrameCapture, Mark, ReplayVerdict, ReportError, ReportHeader, read_frame};
use amux_ui::{Model, update};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::widgets::Paragraph;
use thiserror::Error;

use crate::chrome::{Chrome, ChromeConfig, TraceEvent};
use crate::render::Theme;
use crate::trace::{TraceError, TraceWindow};

/// The frame in the form a report stores it: the text rows, and one class
/// letter per cell naming the theme token the cell was painted from. The
/// class map is what catches a cell painted from a colour literal — it
/// classifies as `?` rather than passing for a token.
pub fn capture_frame(buffer: &Buffer, theme: Theme) -> FrameCapture {
    let mut text = String::new();
    let mut styles = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.cell((x, y)).expect("cell in area");
            text.push_str(cell.symbol());
            styles.push(theme.classify(cell.style()));
        }
        text.push('\n');
        styles.push('\n');
    }
    FrameCapture { text, styles }
}

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("report has no trace to replay")]
    NoTrace,
    #[error("nothing has been drawn yet at this position")]
    NoFrame,
    #[error("report has no captured frame to verify against")]
    NoCapturedFrame,
    #[error(transparent)]
    Trace(#[from] TraceError),
    #[error(transparent)]
    Report(#[from] ReportError),
    #[error("failed to read report: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to render replayed frame: {0}")]
    Render(String),
}

/// A report, loaded and ready to fold.
pub struct Replay {
    header: ReportHeader,
    window: TraceWindow,
    chrome: Chrome,
    model: Model,
    theme: Theme,
    last_frame: Option<Buffer>,
    position: usize,
}

impl Replay {
    pub fn load(report: &Path) -> Result<Self, ReplayError> {
        let header = amux_ui::report::read_header(report)?;
        let path = report.join("trace.jsonl");
        if !path.is_file() {
            return Err(ReplayError::NoTrace);
        }
        let window = TraceWindow::read_jsonl(&std::fs::read(path)?)?;
        Ok(Self::from_window(header, window))
    }

    fn from_window(header: ReportHeader, window: TraceWindow) -> Self {
        let theme = window.snapshot.theme;
        let chrome = Chrome::new(window.snapshot.view.clone(), ChromeConfig { theme });
        let model = window.snapshot.model.clone();
        Self {
            header,
            window,
            chrome,
            model,
            theme,
            last_frame: None,
            position: 0,
        }
    }

    pub fn header(&self) -> &ReportHeader {
        &self.header
    }

    pub fn len(&self) -> usize {
        self.window.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Where the fold currently sits: the number of events applied.
    pub fn position(&self) -> usize {
        self.position
    }

    /// The indices that draw. `--at` takes one of these; an index between
    /// two draws shows the same frame as the draw before it, which is a
    /// confusing thing to offer.
    pub fn draw_indices(&self) -> Vec<usize> {
        (0..self.len())
            .filter(|index| matches!(self.window.event(*index), Ok(TraceEvent::Draw { .. })))
            .collect()
    }

    /// Fold events up to and including `index`. Stepping backwards starts
    /// over from the snapshot rather than trying to undo: nothing here is
    /// invertible, and a replay of a bounded window is cheap.
    pub fn step_to(&mut self, index: usize) -> Result<(), ReplayError> {
        if self.is_empty() {
            return Ok(());
        }
        let target = index.min(self.len() - 1);
        if target + 1 < self.position {
            self.reset();
        }
        while self.position <= target {
            let event = self.window.event(self.position)?;
            self.apply(&event)?;
            self.position += 1;
        }
        Ok(())
    }

    /// Fold every event in the window.
    pub fn step_to_end(&mut self) -> Result<(), ReplayError> {
        if self.is_empty() {
            return Ok(());
        }
        self.step_to(self.len() - 1)
    }

    fn reset(&mut self) {
        let restored = Self::from_window(self.header.clone(), self.window.clone());
        self.chrome = restored.chrome;
        self.model = restored.model;
        self.last_frame = None;
        self.position = 0;
    }

    fn apply(&mut self, event: &TraceEvent) -> Result<(), ReplayError> {
        // A Msg is folded before the chrome sees it, exactly as the live
        // loop folds it before stepping. Effects are the shell's; a replay
        // has no shell.
        if let TraceEvent::Msg(msg) = event {
            let _ = update(&mut self.model, msg.clone());
        }
        // Effects are dropped: what they produced is already recorded as
        // its own event.
        let _ = self.chrome.step(&self.model, event);
        if let TraceEvent::Draw { viewport, .. } = event {
            let lines = self.chrome.take_frame().unwrap_or_default();
            let mut terminal = Terminal::new(TestBackend::new(viewport.0, viewport.1))
                .map_err(|error| ReplayError::Render(error.to_string()))?;
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
                .map_err(|error| ReplayError::Render(error.to_string()))?;
            self.last_frame = Some(terminal.backend().buffer().clone());
        }
        Ok(())
    }

    /// The frame as last drawn at the current position.
    pub fn frame(&self) -> Result<FrameCapture, ReplayError> {
        let buffer = self.last_frame.as_ref().ok_or(ReplayError::NoFrame)?;
        Ok(capture_frame(buffer, self.theme))
    }
}

/// Replay a report to its end and compare the frame it produces with the
/// one it captured.
pub fn verify(report: &Path) -> Result<ReplayVerdict, ReplayError> {
    let expected = read_frame(report)?.ok_or(ReplayError::NoCapturedFrame)?;
    let mut replay = Replay::load(report)?;
    replay.step_to_end()?;
    let actual = replay.frame()?;
    Ok(verdict(&expected, &actual))
}

/// The verdict for one pair of frames, with the first differing cell named
/// so a divergence points somewhere before anyone opens the files.
pub fn verdict(expected: &FrameCapture, actual: &FrameCapture) -> ReplayVerdict {
    let diff = frame_diff(expected, actual);
    match diff.cells.first() {
        None => ReplayVerdict::Reproduces,
        Some((x, y)) => ReplayVerdict::Diverges {
            first_diff: format!(
                "{} cell(s) differ, first at column {x} row {y}",
                diff.cells.len()
            ),
        },
    }
}

/// Which cells differ, and the rectangle that covers them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameDiff {
    pub cells: Vec<(u16, u16)>,
    pub bounding: Option<Mark>,
}

/// Compare two captures cell by cell. A cell counts as differing when its
/// symbol or its style class differs — the class map is half the capture,
/// so a frame that reads the same but is painted from different tokens is
/// a divergence, not a match.
pub fn frame_diff(expected: &FrameCapture, actual: &FrameCapture) -> FrameDiff {
    let mut cells = Vec::new();
    let expected_rows: Vec<&str> = expected.text.lines().collect();
    let actual_rows: Vec<&str> = actual.text.lines().collect();
    let expected_styles: Vec<&str> = expected.styles.lines().collect();
    let actual_styles: Vec<&str> = actual.styles.lines().collect();
    let rows = expected_rows.len().max(actual_rows.len());
    for y in 0..rows {
        let row = |rows: &[&str], y: usize| rows.get(y).copied().unwrap_or("").chars().count();
        let width = row(&expected_rows, y).max(row(&actual_rows, y));
        for x in 0..width {
            let cell =
                |rows: &[&str], y: usize, x: usize| rows.get(y).and_then(|row| row.chars().nth(x));
            let text_differs = cell(&expected_rows, y, x) != cell(&actual_rows, y, x);
            let style_differs = cell(&expected_styles, y, x) != cell(&actual_styles, y, x);
            if text_differs || style_differs {
                cells.push((x as u16, y as u16));
            }
        }
    }
    let bounding = bounding_mark(&cells);
    FrameDiff { cells, bounding }
}

fn bounding_mark(cells: &[(u16, u16)]) -> Option<Mark> {
    let (first_x, first_y) = *cells.first()?;
    let mut min = (first_x, first_y);
    let mut max = (first_x, first_y);
    for (x, y) in cells {
        min = (min.0.min(*x), min.1.min(*y));
        max = (max.0.max(*x), max.1.max(*y));
    }
    Some(Mark {
        x: min.0,
        y: min.1,
        width: max.0 - min.0 + 1,
        height: max.1 - min.1 + 1,
        note: "replay diverges here".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use amux_ui::BUILD;
    use amux_ui::report::{ReportDraft, ReportKind, ReportParts, ReportWriter};
    use chrono::{DateTime, TimeDelta, Utc};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::chrome::{InputEvent, KeyRecord};
    use crate::fixtures::{NamedState, fixture};
    use crate::view::QuitGuard;
    use crate::trace::{SEGMENT_LEN, TraceRing};

    const VIEWPORT: (u16, u16) = (120, 40);

    /// A live session recorded exactly the way `run.rs` records one: roll,
    /// record the draw, step it, paint what the step built.
    pub(super) struct Session {
        chrome: Chrome,
        model: Model,
        ring: TraceRing,
        frame: Option<Buffer>,
        now: DateTime<Utc>,
    }

    impl Session {
        fn open(state: NamedState) -> Self {
            let built = fixture(state);
            Self {
                chrome: Chrome::new(
                    built.view,
                    ChromeConfig {
                        theme: Theme::default(),
                    },
                ),
                model: built.model,
                ring: TraceRing::new(SEGMENT_LEN),
                frame: None,
                now: built.now,
            }
        }

        fn draw(&mut self) {
            self.ring.roll_if_due(
                &self.model,
                &self.chrome.view,
                self.chrome.theme(),
                self.now,
            );
            let event = TraceEvent::Draw {
                viewport: VIEWPORT,
                now: self.now,
            };
            self.ring.record(&event);
            self.chrome.step(&self.model, &event);
            let lines = self.chrome.take_frame().expect("the draw built its lines");
            let mut terminal =
                Terminal::new(TestBackend::new(VIEWPORT.0, VIEWPORT.1)).expect("terminal");
            terminal
                .draw(|frame| frame.render_widget(Paragraph::new(lines), frame.area()))
                .expect("paint");
            self.frame = Some(terminal.backend().buffer().clone());
        }

        fn press(&mut self, code: KeyCode) {
            self.press_with(code, KeyModifiers::NONE);
        }

        fn press_with(&mut self, code: KeyCode, modifiers: KeyModifiers) {
            let event = TraceEvent::Input {
                event: InputEvent::Key(KeyRecord::from_event(KeyEvent::new(code, modifiers))),
                viewport: VIEWPORT,
                now: self.now,
            };
            self.ring.record(&event);
            self.chrome.step(&self.model, &event);
        }

        /// Move the session's clock, as the wall clock moves between one
        /// terminal event and the next.
        fn advance(&mut self, seconds: i64) {
            self.now += TimeDelta::seconds(seconds);
        }

        /// One turn of the shell's 1 Hz tick, recorded the way `run.rs`
        /// records it: only a tick that actually disarmed a guard.
        fn tick(&mut self) {
            if !self.chrome.quit_guard_armed() {
                return;
            }
            let event = TraceEvent::Tick { now: self.now };
            self.chrome.step(&self.model, &event);
            if !self.chrome.quit_guard_armed() {
                self.ring.record(&event);
            }
        }

        pub(super) fn capture(&self) -> FrameCapture {
            capture_frame(
                self.frame.as_ref().expect("a frame has been drawn"),
                Theme::default(),
            )
        }

        /// Write the session's window and last frame as a report bundle,
        /// the same shape the capture key writes.
        pub(super) fn write_report(&self, dir: &Path) -> std::path::PathBuf {
            let window = self.ring.window().expect("a drawn session has a window");
            ReportWriter::new(dir.to_path_buf(), BUILD, "test-sha")
                .write(
                    ReportDraft {
                        kind: ReportKind::Bug,
                        detail: None,
                        note: "scrolled back through a long feed".to_string(),
                        marks: Vec::new(),
                        viewport: Some(VIEWPORT),
                        replay: ReplayVerdict::Unchecked,
                    },
                    ReportParts {
                        frame: Some(self.capture()),
                        trace: Some(window.to_bytes().expect("trace serializes")),
                        msgs: None,
                        daemon: None,
                        log: None,
                        absent_reason: "not captured by this test".to_string(),
                        log_absent_reason: None,
                    },
                )
                .expect("report writes")
        }
    }

    /// A long feed scrolled back: draws, three page-ups each followed by a
    /// draw, and a final draw. The frame at the end is not the frame at the
    /// start, which is what makes the earlier-index assertion meaningful.
    pub(super) fn scrolled_session() -> Session {
        let mut session = Session::open(NamedState::ClaudeLongFeed);
        session.draw();
        for _ in 0..3 {
            session.press(KeyCode::PageUp);
            session.draw();
        }
        session
    }

    /// The armed footer is the one thing a tick alone can change, and a
    /// tick that expires it must be in the trace: replay the same events
    /// and the warning must be gone from the replayed frame too.
    #[test]
    fn a_tick_that_expires_the_quit_guard_replays() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut session = Session::open(NamedState::Fleet);
        session.draw();
        session.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
        session.draw();
        let armed = session.capture();
        assert!(
            armed.text.contains("press ctrl+c again to quit"),
            "the guarded press must arm the warning footer"
        );

        session.advance(QuitGuard::WINDOW_SECS + 1);
        session.tick();
        session.draw();
        let expired = session.capture();
        assert!(
            !expired.text.contains("press ctrl+c again to quit"),
            "the tick past the window must disarm the guard"
        );

        let report = session.write_report(dir.path());
        let mut replay = Replay::load(&report).expect("report loads");
        replay.step_to_end().expect("replays to the end");
        assert_eq!(
            replay.frame().expect("a frame was drawn"),
            expired,
            "a replay that skipped the expiry would still show the warning"
        );
        assert_eq!(
            verify(&report).expect("verify runs"),
            ReplayVerdict::Reproduces
        );
    }

    #[test]
    fn a_recorded_session_replays_to_the_identical_frame() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = scrolled_session();
        let report = session.write_report(dir.path());

        let mut replay = Replay::load(&report).expect("report loads");
        replay.step_to_end().expect("replays to the end");
        assert_eq!(
            replay.frame().expect("a frame was drawn"),
            session.capture(),
            "the same events through the same chrome must produce the same frame"
        );
        assert_eq!(
            verify(&report).expect("verify runs"),
            ReplayVerdict::Reproduces
        );
    }

    #[test]
    fn an_earlier_draw_index_renders_an_earlier_frame() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = scrolled_session();
        let report = session.write_report(dir.path());

        let mut replay = Replay::load(&report).expect("report loads");
        let draws = replay.draw_indices();
        assert_eq!(draws.len(), 4, "one draw per page-up, plus the first");

        replay.step_to(draws[0]).expect("replays to the first draw");
        let first = replay.frame().expect("the first draw drew");
        replay.step_to_end().expect("replays to the end");
        let last = replay.frame().expect("the last draw drew");
        assert_ne!(
            first, last,
            "three page-ups through a thousand-entry feed must move the frame"
        );
        assert_eq!(last, session.capture());

        // Stepping backwards starts over from the snapshot rather than
        // trying to undo, and must land on the same earlier frame.
        replay.step_to(draws[0]).expect("replays backwards");
        assert_eq!(replay.frame().expect("frame"), first);
    }
}

/// Divergence: what a report says about itself, checked against what this
/// build actually draws. A report that cannot be checked at all — no trace
/// to fold — is its own answer, and must not be mistaken for either verdict.
#[cfg(test)]
mod divergence {
    use amux_ui::BUILD;
    use amux_ui::report::{ReportDraft, ReportKind, ReportParts, ReportWriter};

    use super::tests::scrolled_session;
    use super::*;

    const VIEWPORT: (u16, u16) = (120, 40);

    #[test]
    fn a_tampered_frame_diverges_and_names_the_cell() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = scrolled_session();
        let report = session.write_report(dir.path());

        // Change one cell of the captured frame, as a stale or hand-edited
        // report would have.
        let captured = std::fs::read_to_string(report.join("frame.txt")).expect("read frame");
        let mut rows: Vec<String> = captured.lines().map(str::to_string).collect();
        let row: Vec<char> = rows[2].chars().collect();
        rows[2] = row
            .iter()
            .enumerate()
            .map(|(x, c)| if x == 5 { '\u{2588}' } else { *c })
            .collect();
        std::fs::write(report.join("frame.txt"), format!("{}\n", rows.join("\n")))
            .expect("write tampered frame");

        match verify(&report).expect("verify runs") {
            ReplayVerdict::Diverges { first_diff } => {
                assert!(
                    first_diff.contains("column 5 row 2"),
                    "the verdict names the first differing cell: {first_diff}"
                );
            }
            other => panic!("a tampered frame must diverge, not {other:?}"),
        }

        let expected = read_frame(&report).expect("read").expect("frame present");
        let diff = frame_diff(&expected, &session.capture());
        assert_eq!(diff.cells, vec![(5, 2)]);
        let bounding = diff.bounding.expect("a diff has a bounding rectangle");
        assert_eq!(
            (bounding.x, bounding.y, bounding.width, bounding.height),
            (5, 2, 1, 1)
        );
    }

    #[test]
    fn a_report_without_a_trace_neither_reproduces_nor_diverges() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session = scrolled_session();
        let report = ReportWriter::new(dir.path().to_path_buf(), BUILD, "test-sha")
            .write(
                ReportDraft {
                    kind: ReportKind::Tripwire,
                    detail: None,
                    note: String::new(),
                    marks: Vec::new(),
                    viewport: Some(VIEWPORT),
                    replay: ReplayVerdict::Unchecked,
                },
                ReportParts {
                    frame: Some(session.capture()),
                    trace: None,
                    msgs: None,
                    daemon: None,
                    log: None,
                    absent_reason: "unavailable in release build".to_string(),
                    log_absent_reason: None,
                },
            )
            .expect("report writes");
        assert!(matches!(Replay::load(&report), Err(ReplayError::NoTrace)));
        assert!(matches!(verify(&report), Err(ReplayError::NoTrace)));
    }
}
