//! Capturing a report: the key, and the freeze it triggers.
//!
//! A report is worth nothing if it describes a moment other than the one
//! that looked wrong. So the capture key does not start gathering — it
//! stops the world. Everything the bundle will contain is read in one
//! synchronous pass while the loop is still holding the keypress: the
//! buffer that is on screen, its style classification, the trace window,
//! the recorder's Msgs and the log tail. Nothing recorded afterwards can
//! enter the capture, which is what makes the frame and the events that
//! produced it describe the same instant.
//!
//! The daemon is the one exception: it lives in another process and
//! answering takes a round trip. Its fetch is started here and awaited
//! later, so the wait costs the person nothing while they write the note.

use chrono::{DateTime, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use tokio::task::JoinHandle;

use amux_ui::report::ReplayVerdict;
use amux_ui::report::{FrameCapture, LOG_TAIL_BYTES, Mark, ReportDraft, ReportKind, log_tail};
use amux_ui::{RecorderSnapshot, Runtime};

use crate::diagnostics::DiagnosticsSource;
use crate::render::Theme;
use crate::replay::capture_frame;
use crate::trace::{SharedTrace, TraceWindow};

/// The one capture key. The loop intercepts it before either key handler,
/// so it means the same thing on the fleet and inside a chat — a person
/// looking at something wrong should not have to remember which screen
/// they are on.
pub const CAPTURE_KEY: KeyEvent = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);

/// Everything the report will contain, read at the keypress.
pub struct Frozen {
    /// The buffer that was on screen, kept whole so the flow can repaint
    /// it under the prompt and the marks.
    pub frame: Buffer,
    pub capture: FrameCapture,
    pub viewport: (u16, u16),
    pub now: DateTime<Utc>,
    /// The events since the oldest retained snapshot. `None` when nothing
    /// has been drawn yet, so there is no state to replay from.
    pub trace: Option<TraceWindow>,
    pub msgs: RecorderSnapshot,
    /// In flight: started at the keypress, awaited when the flow finishes,
    /// so the round trip to the daemon costs the person nothing while they
    /// are writing the note.
    pub daemon: JoinHandle<Result<String, String>>,
    pub log: Option<String>,
    /// Why the log is missing, when it is missing for a reason worth
    /// recording rather than simply not existing.
    pub log_absent_reason: Option<String>,
}

impl Frozen {
    /// Stop the world. Everything but the daemon dump is read before this
    /// returns; an event recorded after it cannot appear in the report.
    pub fn take(
        last_frame: &Buffer,
        theme: Theme,
        trace: &SharedTrace,
        runtime: &Runtime,
        diagnostics: &DiagnosticsSource,
        now: DateTime<Utc>,
    ) -> Self {
        let window = match trace.lock() {
            Ok(ring) => ring.window(),
            Err(_) => {
                tracing::warn!("report captured without a trace: ring lock poisoned");
                None
            }
        };
        let (log, log_absent_reason) = capture_log(diagnostics);
        Self {
            capture: capture_frame(last_frame, theme),
            viewport: (last_frame.area.width, last_frame.area.height),
            frame: last_frame.clone(),
            now,
            trace: window,
            msgs: runtime.recorder_snapshot(),
            daemon: tokio::spawn((diagnostics.daemon_dump)()),
            log,
            log_absent_reason,
        }
    }
}

/// The log tail, or the reason there isn't one. A log this build cannot
/// read is a fact about the report, not a reason to refuse to write it.
fn capture_log(diagnostics: &DiagnosticsSource) -> (Option<String>, Option<String>) {
    let Some(path) = diagnostics.log_path.as_deref() else {
        return (None, None);
    };
    match log_tail(path, LOG_TAIL_BYTES) {
        Ok(log) => (log, None),
        Err(error) => (
            None,
            Some(format!(
                "failed to read log tail from {}: {error}",
                path.display()
            )),
        ),
    }
}

/// Where the flow is. Each stage owns only what is being edited right
/// now; what has been decided already lives on the [`ReportFlow`], so a
/// stage can be left and re-entered without carrying the answers along.
pub enum Stage {
    /// What kind of report this is. The only stage `Esc` cancels from —
    /// once something has been typed, `Esc` steps back rather than
    /// throwing it away.
    Kind,
    Note {
        text: String,
    },
    /// Marking rectangles on the frozen frame, by mouse drag or by
    /// keyboard. `anchor` is a keyboard rectangle waiting for its second
    /// corner; `drag` is the mouse equivalent.
    Marks {
        cursor: (u16, u16),
        anchor: Option<(u16, u16)>,
        drag: Option<(u16, u16)>,
    },
    /// What is wrong with the rectangle just drawn. Every mark gets asked:
    /// a rectangle with no words is a place, not a report.
    MarkNote {
        rect: Mark,
        text: String,
    },
    Finish,
}

/// What one event did to the flow.
pub enum FlowStep {
    Continue,
    Cancel,
    Finish(ReportDraft),
}

/// The capture, and the answers gathered over it.
pub struct ReportFlow {
    pub frozen: Frozen,
    /// The palette the frozen frame was painted with, so the prompt and
    /// the marks belong to the same screen they sit on.
    theme: Theme,
    stage: Stage,
    kind: Option<ReportKind>,
    note: String,
    marks: Vec<Mark>,
}

impl ReportFlow {
    pub fn begin(frozen: Frozen, theme: Theme) -> Self {
        Self {
            frozen,
            theme,
            stage: Stage::Kind,
            kind: None,
            note: String::new(),
            marks: Vec::new(),
        }
    }

    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// Consume one terminal event. The flow's own events never reach the
    /// chrome and are never recorded: a trace that contained the act of
    /// reporting would replay into the report prompt instead of the
    /// screen the report is about.
    pub fn handle(&mut self, event: Event) -> FlowStep {
        match event {
            Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Release => {
                FlowStep::Continue
            }
            Event::Key(key) => self.key(key),
            Event::Mouse(mouse) => self.mouse(mouse),
            _ => FlowStep::Continue,
        }
    }

    fn key(&mut self, key: KeyEvent) -> FlowStep {
        match &mut self.stage {
            Stage::Kind => match key.code {
                KeyCode::Esc => FlowStep::Cancel,
                KeyCode::Char('b') => self.choose(ReportKind::Bug),
                KeyCode::Char('t') => self.choose(ReportKind::Tweak),
                _ => FlowStep::Continue,
            },
            Stage::Note { text } => match key.code {
                KeyCode::Esc => {
                    self.stage = Stage::Kind;
                    FlowStep::Continue
                }
                KeyCode::Enter => {
                    self.note = std::mem::take(text);
                    self.stage = Stage::Marks {
                        cursor: (0, 0),
                        anchor: None,
                        drag: None,
                    };
                    FlowStep::Continue
                }
                KeyCode::Backspace => {
                    text.pop();
                    FlowStep::Continue
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text.push(c);
                    FlowStep::Continue
                }
                _ => FlowStep::Continue,
            },
            Stage::Marks {
                cursor,
                anchor,
                drag,
            } => {
                let viewport = self.frozen.viewport;
                match key.code {
                    KeyCode::Esc => {
                        // Abandon the rectangle in progress, not the report.
                        *anchor = None;
                        *drag = None;
                        FlowStep::Continue
                    }
                    KeyCode::Char(' ') => {
                        *anchor = Some(*cursor);
                        FlowStep::Continue
                    }
                    KeyCode::Enter => match anchor.take() {
                        Some(corner) => {
                            self.stage = Stage::MarkNote {
                                rect: rectangle(corner, *cursor),
                                text: String::new(),
                            };
                            FlowStep::Continue
                        }
                        // Enter with no rectangle open means "that is all
                        // of it" — including the common case of a report
                        // that marks nothing.
                        None => self.finish(),
                    },
                    code => {
                        if let Some(step) = step_for(code) {
                            *cursor = moved(*cursor, step, viewport);
                        }
                        FlowStep::Continue
                    }
                }
            }
            Stage::MarkNote { rect, text } => match key.code {
                KeyCode::Esc => {
                    self.stage = marks_stage(rect);
                    FlowStep::Continue
                }
                KeyCode::Enter => {
                    let mut mark = rect.clone();
                    mark.note = std::mem::take(text);
                    let resume = marks_stage(rect);
                    self.marks.push(mark);
                    self.stage = resume;
                    FlowStep::Continue
                }
                KeyCode::Backspace => {
                    text.pop();
                    FlowStep::Continue
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    text.push(c);
                    FlowStep::Continue
                }
                _ => FlowStep::Continue,
            },
            Stage::Finish => FlowStep::Continue,
        }
    }

    fn mouse(&mut self, mouse: crossterm::event::MouseEvent) -> FlowStep {
        let Stage::Marks {
            cursor,
            anchor,
            drag,
        } = &mut self.stage
        else {
            return FlowStep::Continue;
        };
        let at = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                *drag = Some(at);
                *anchor = None;
                *cursor = at;
            }
            MouseEventKind::Drag(MouseButton::Left) => *cursor = at,
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(corner) = drag.take() {
                    *cursor = at;
                    self.stage = Stage::MarkNote {
                        rect: rectangle(corner, at),
                        text: String::new(),
                    };
                }
            }
            _ => {}
        }
        FlowStep::Continue
    }

    fn choose(&mut self, kind: ReportKind) -> FlowStep {
        self.kind = Some(kind);
        self.stage = Stage::Note {
            text: String::new(),
        };
        FlowStep::Continue
    }

    fn finish(&mut self) -> FlowStep {
        self.stage = Stage::Finish;
        FlowStep::Finish(ReportDraft {
            // Only a chosen kind reaches this stage; Bug is the answer a
            // flow that somehow skipped the question would have given.
            kind: self.kind.unwrap_or(ReportKind::Bug),
            detail: None,
            note: self.note.clone(),
            marks: std::mem::take(&mut self.marks),
            viewport: Some(self.frozen.viewport),
            replay: ReplayVerdict::Unchecked,
        })
    }

    /// Paint the frozen frame, the marks over it, and the stage's prompt.
    pub fn render(&self, frame: &mut Frame) {
        let pending = match &self.stage {
            Stage::Marks {
                cursor,
                anchor,
                drag,
            } => anchor.or(*drag).map(|corner| rectangle(corner, *cursor)),
            Stage::MarkNote { rect, .. } => Some(rect.clone()),
            _ => None,
        };
        paint(
            frame,
            &self.frozen.frame,
            self.theme,
            self.marks.iter().chain(pending.iter()),
            &prompt(&self.stage, self.marks.len()),
        );
    }
}

/// The Marks stage a MarkNote returns to: the cursor sits at the corner
/// the rectangle ended on, so a second mark starts where the eye already is.
fn marks_stage(rect: &Mark) -> Stage {
    Stage::Marks {
        cursor: (
            rect.x.saturating_add(rect.width.saturating_sub(1)),
            rect.y.saturating_add(rect.height.saturating_sub(1)),
        ),
        anchor: None,
        drag: None,
    }
}

/// A rectangle from two corners, in either order, inclusive of both.
fn rectangle(a: (u16, u16), b: (u16, u16)) -> Mark {
    Mark {
        x: a.0.min(b.0),
        y: a.1.min(b.1),
        width: a.0.abs_diff(b.0) + 1,
        height: a.1.abs_diff(b.1) + 1,
        note: String::new(),
    }
}

/// Arrows and `hjkl` both move: whichever the person's hands already know.
fn step_for(code: KeyCode) -> Option<(i32, i32)> {
    match code {
        KeyCode::Left | KeyCode::Char('h') => Some((-1, 0)),
        KeyCode::Down | KeyCode::Char('j') => Some((0, 1)),
        KeyCode::Up | KeyCode::Char('k') => Some((0, -1)),
        KeyCode::Right | KeyCode::Char('l') => Some((1, 0)),
        _ => None,
    }
}

fn moved(cursor: (u16, u16), step: (i32, i32), viewport: (u16, u16)) -> (u16, u16) {
    let clamp = |value: u16, delta: i32, limit: u16| -> u16 {
        let moved = i32::from(value) + delta;
        moved.clamp(0, i32::from(limit.saturating_sub(1))) as u16
    };
    (
        clamp(cursor.0, step.0, viewport.0),
        clamp(cursor.1, step.1, viewport.1),
    )
}

/// What the bottom row asks for. Written as one line per stage so the
/// prompt never advertises a key the stage does not take.
fn prompt(stage: &Stage, marked: usize) -> String {
    match stage {
        Stage::Kind => "report this screen:  b bug · t tweak · esc cancel".to_string(),
        Stage::Note { text } => format!("what happened? {text}▏  enter continue · esc back"),
        Stage::Marks { anchor, drag, .. } => {
            let open = anchor.is_some() || drag.is_some();
            let act = if open {
                "enter close the box · esc drop it"
            } else {
                "drag or space to start a box · enter finish"
            };
            format!("marked {marked}:  move hjkl/arrows · {act}")
        }
        Stage::MarkNote { text, .. } => {
            format!("what is wrong here? {text}▏  enter keep · esc discard")
        }
        Stage::Finish => "writing the report…".to_string(),
    }
}

/// Paint one frame of the flow. Pure over its inputs so the overlay can be
/// held to a golden without a live capture behind it.
pub fn paint<'a>(
    frame: &mut Frame,
    frozen: &Buffer,
    theme: Theme,
    marks: impl Iterator<Item = &'a Mark>,
    prompt: &str,
) {
    let area = frame.area();
    let buffer = frame.buffer_mut();
    for y in 0..area.height.min(frozen.area.height) {
        for x in 0..area.width.min(frozen.area.width) {
            let Some(source) = frozen.cell((x, y)) else {
                continue;
            };
            if let Some(cell) = buffer.cell_mut((x, y)) {
                *cell = source.clone();
            }
        }
    }
    // The marks are the point of the screen, so they are painted over
    // whatever the frame said, not blended with it.
    let highlight = theme.mark();
    for mark in marks {
        for y in mark.y..mark.y.saturating_add(mark.height).min(area.height) {
            for x in mark.x..mark.x.saturating_add(mark.width).min(area.width) {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_style(highlight);
                }
            }
        }
    }
    if area.height == 0 {
        return;
    }
    let row = area.height - 1;
    let prompt_style = theme.report_prompt();
    for x in 0..area.width {
        if let Some(cell) = buffer.cell_mut((x, row)) {
            cell.set_symbol(" ");
            cell.set_style(prompt_style);
        }
    }
    for (x, grapheme) in prompt.chars().take(usize::from(area.width)).enumerate() {
        if let Some(cell) = buffer.cell_mut((x as u16, row)) {
            cell.set_char(grapheme);
        }
    }
}

#[cfg(test)]
mod frozen {
    use std::path::PathBuf;
    use std::sync::Arc;

    use amux_ui::{ConnectFailure, Connector, RuntimeOptions};
    use ratatui::layout::Rect;

    use super::*;
    use crate::bindings::{Tier, report_key_row, report_key_row_for};
    use crate::chrome::TraceEvent;
    use crate::trace::{SEGMENT_LEN, record_shared, shared};
    use crate::view::ViewState;

    /// A runtime that never connects: `Frozen::take` reads its recorder,
    /// which exists from the first moment, not its connection.
    pub(super) fn runtime() -> Runtime {
        let connector: Connector = Box::new(|| {
            Box::pin(async {
                Err(ConnectFailure {
                    message: "no daemon in this test".into(),
                    auth_required: false,
                    subscription_required: false,
                })
            })
        });
        Runtime::start(connector, RuntimeOptions::default())
    }

    pub(super) fn diagnostics(log_path: Option<PathBuf>, dump: &'static str) -> DiagnosticsSource {
        DiagnosticsSource {
            daemon_dump: Arc::new(move || Box::pin(async move { Ok(dump.to_string()) })),
            log_path,
            reports_dir: PathBuf::from("/unused"),
            git_sha: "abc1234",
        }
    }

    pub(super) fn drawn_frame() -> Buffer {
        let mut frame = Buffer::empty(Rect::new(0, 0, 8, 2));
        frame.set_string(0, 0, "fleet", ratatui::style::Style::default());
        frame
    }

    /// Prime a ring the way the live loop does: roll at the frame
    /// boundary, then record the draw.
    pub(super) fn primed_trace(runtime: &Runtime) -> SharedTrace {
        let trace = shared(SEGMENT_LEN);
        let now = Utc::now();
        {
            let mut ring = trace.lock().expect("fresh ring");
            ring.roll_if_due(
                runtime.model(),
                &ViewState::default(),
                Theme::dark(crate::theme::ColorMode::TrueColor),
                now,
            );
        }
        record_shared(
            &trace,
            &TraceEvent::Draw {
                viewport: (8, 2),
                now,
            },
        );
        trace
    }

    #[tokio::test]
    async fn take_gathers_every_part_of_the_report() {
        let directory = tempfile::tempdir().expect("tempdir");
        let log = directory.path().join("amux.log");
        std::fs::write(&log, "opened the fleet\n").expect("write log");
        let runtime = runtime();
        let trace = primed_trace(&runtime);

        let frozen = Frozen::take(
            &drawn_frame(),
            Theme::dark(crate::theme::ColorMode::TrueColor),
            &trace,
            &runtime,
            &diagnostics(Some(log), r#"{"agents":[]}"#),
            Utc::now(),
        );

        assert!(frozen.capture.text.starts_with("fleet"));
        assert_eq!(frozen.capture.styles.lines().count(), 2);
        assert_eq!(frozen.viewport, (8, 2));
        assert_eq!(frozen.trace.as_ref().expect("primed ring").events.len(), 1);
        assert_eq!(frozen.log.as_deref(), Some("opened the fleet\n"));
        assert_eq!(frozen.log_absent_reason, None);
        assert_eq!(
            frozen.daemon.await.expect("dump task"),
            Ok(r#"{"agents":[]}"#.to_string())
        );
    }

    #[tokio::test]
    async fn take_records_a_missing_log_as_a_reason_not_a_failure() {
        let directory = tempfile::tempdir().expect("tempdir");
        let log = directory.path().join("amux.log");
        std::fs::write(&log, b"\xff not utf-8\n").expect("write unreadable log");
        let runtime = runtime();
        let trace = primed_trace(&runtime);

        let frozen = Frozen::take(
            &drawn_frame(),
            Theme::dark(crate::theme::ColorMode::TrueColor),
            &trace,
            &runtime,
            &diagnostics(Some(log), "{}"),
            Utc::now(),
        );

        assert_eq!(frozen.log, None);
        assert!(
            frozen
                .log_absent_reason
                .as_deref()
                .expect("a log that will not read explains itself")
                .starts_with("failed to read log tail from ")
        );
    }

    #[tokio::test]
    async fn nothing_recorded_after_the_freeze_enters_it() {
        let runtime = runtime();
        let trace = primed_trace(&runtime);

        let frozen = Frozen::take(
            &drawn_frame(),
            Theme::dark(crate::theme::ColorMode::TrueColor),
            &trace,
            &runtime,
            &diagnostics(None, "{}"),
            Utc::now(),
        );
        record_shared(&trace, &TraceEvent::Notice(Some("after the freeze".into())));

        let window = frozen.trace.expect("primed ring");
        assert_eq!(window.events.len(), 1);
        assert!(
            !window
                .events
                .iter()
                .any(|event| event.contains("after the freeze")),
            "the frozen window must not grow after it was taken"
        );
        assert_eq!(
            trace.lock().expect("ring").len(),
            2,
            "the live ring keeps recording; only the window is frozen"
        );
    }

    #[test]
    fn the_capture_key_row_exists_only_where_the_key_does() {
        assert_eq!(report_key_row().is_some(), cfg!(debug_assertions));

        let row = report_key_row_for(true).expect("a debug build binds the capture key");
        assert_eq!(row.keys, "C-g");
        assert_eq!(row.tier, Tier::Plain);
        assert_eq!(report_key_row_for(false), None);
    }
}

#[cfg(test)]
mod flow {
    use crossterm::event::{KeyEventKind, KeyEventState, MouseEvent};
    use ratatui::layout::Rect;

    use super::frozen::{diagnostics, primed_trace, runtime};
    use super::*;

    const VIEWPORT: (u16, u16) = (20, 6);

    fn begin() -> ReportFlow {
        let runtime = runtime();
        let trace = primed_trace(&runtime);
        let theme = Theme::dark(crate::theme::ColorMode::TrueColor);
        let frame = Buffer::empty(Rect::new(0, 0, VIEWPORT.0, VIEWPORT.1));
        let frozen = Frozen::take(
            &frame,
            theme,
            &trace,
            &runtime,
            &diagnostics(None, "{}"),
            Utc::now(),
        );
        ReportFlow::begin(frozen, theme)
    }

    fn press(flow: &mut ReportFlow, code: KeyCode) -> FlowStep {
        flow.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn type_text(flow: &mut ReportFlow, text: &str) {
        for c in text.chars() {
            press(flow, KeyCode::Char(c));
        }
    }

    fn mouse(flow: &mut ReportFlow, kind: MouseEventKind, at: (u16, u16)) {
        flow.handle(Event::Mouse(MouseEvent {
            kind,
            column: at.0,
            row: at.1,
            modifiers: KeyModifiers::NONE,
        }));
    }

    fn drag(flow: &mut ReportFlow, from: (u16, u16), to: (u16, u16)) {
        mouse(flow, MouseEventKind::Down(MouseButton::Left), from);
        mouse(flow, MouseEventKind::Drag(MouseButton::Left), to);
        mouse(flow, MouseEventKind::Up(MouseButton::Left), to);
    }

    fn finish(step: FlowStep) -> ReportDraft {
        match step {
            FlowStep::Finish(draft) => draft,
            FlowStep::Continue => panic!("the flow kept going instead of finishing"),
            FlowStep::Cancel => panic!("the flow cancelled instead of finishing"),
        }
    }

    #[tokio::test]
    async fn two_dragged_marks_and_one_typed_mark_reach_the_draft() {
        let mut flow = begin();
        press(&mut flow, KeyCode::Char('b'));
        type_text(&mut flow, "the footer overlaps the composer");
        press(&mut flow, KeyCode::Enter);

        drag(&mut flow, (2, 1), (5, 3));
        type_text(&mut flow, "footer");
        press(&mut flow, KeyCode::Enter);

        drag(&mut flow, (10, 0), (11, 1));
        type_text(&mut flow, "clipped title");
        press(&mut flow, KeyCode::Enter);

        // Keyboard rectangle: anchor where the last one ended, then move.
        press(&mut flow, KeyCode::Char(' '));
        press(&mut flow, KeyCode::Char('j'));
        press(&mut flow, KeyCode::Char('l'));
        press(&mut flow, KeyCode::Enter);
        type_text(&mut flow, "stray cell");
        press(&mut flow, KeyCode::Enter);

        let draft = finish(press(&mut flow, KeyCode::Enter));
        assert_eq!(draft.kind, ReportKind::Bug);
        assert_eq!(draft.note, "the footer overlaps the composer");
        assert_eq!(draft.viewport, Some(VIEWPORT));
        assert_eq!(
            draft.marks,
            vec![
                Mark {
                    x: 2,
                    y: 1,
                    width: 4,
                    height: 3,
                    note: "footer".into()
                },
                Mark {
                    x: 10,
                    y: 0,
                    width: 2,
                    height: 2,
                    note: "clipped title".into()
                },
                Mark {
                    x: 11,
                    y: 1,
                    width: 2,
                    height: 2,
                    note: "stray cell".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_tweak_with_nothing_marked_still_finishes() {
        let mut flow = begin();
        press(&mut flow, KeyCode::Char('t'));
        type_text(&mut flow, "this row could be quieter");
        press(&mut flow, KeyCode::Enter);

        let draft = finish(press(&mut flow, KeyCode::Enter));
        assert_eq!(draft.kind, ReportKind::Tweak);
        assert_eq!(draft.note, "this row could be quieter");
        assert!(draft.marks.is_empty());
    }

    #[tokio::test]
    async fn esc_cancels_at_the_kind_question_and_steps_back_after_it() {
        let mut flow = begin();
        press(&mut flow, KeyCode::Char('b'));
        type_text(&mut flow, "half a thought");
        // Esc in the note goes back to the question, not out of the flow.
        assert!(matches!(press(&mut flow, KeyCode::Esc), FlowStep::Continue));
        assert!(matches!(flow.stage(), Stage::Kind));
        assert!(matches!(press(&mut flow, KeyCode::Esc), FlowStep::Cancel));
    }

    #[tokio::test]
    async fn esc_over_a_half_drawn_box_drops_the_box_not_the_report() {
        let mut flow = begin();
        press(&mut flow, KeyCode::Char('b'));
        press(&mut flow, KeyCode::Enter);
        press(&mut flow, KeyCode::Char(' '));
        press(&mut flow, KeyCode::Char('l'));
        assert!(matches!(press(&mut flow, KeyCode::Esc), FlowStep::Continue));

        // With the anchor gone, Enter means "that is all of it".
        assert!(finish(press(&mut flow, KeyCode::Enter)).marks.is_empty());
    }

    #[tokio::test]
    async fn a_discarded_mark_note_leaves_no_mark() {
        let mut flow = begin();
        press(&mut flow, KeyCode::Char('b'));
        press(&mut flow, KeyCode::Enter);
        drag(&mut flow, (1, 1), (3, 2));
        type_text(&mut flow, "never mind");
        press(&mut flow, KeyCode::Esc);

        assert!(finish(press(&mut flow, KeyCode::Enter)).marks.is_empty());
    }

    #[tokio::test]
    async fn a_key_release_is_not_a_keypress() {
        let mut flow = begin();
        let release = Event::Key(KeyEvent::new_with_kind_and_state(
            KeyCode::Char('b'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
            KeyEventState::NONE,
        ));
        flow.handle(release);
        assert!(matches!(flow.stage(), Stage::Kind));
    }

    #[test]
    fn the_cursor_stays_inside_the_frozen_viewport() {
        assert_eq!(moved((0, 0), (-1, 0), VIEWPORT), (0, 0));
        assert_eq!(moved((0, 0), (0, -1), VIEWPORT), (0, 0));
        assert_eq!(moved((19, 5), (1, 0), VIEWPORT), (19, 5));
        assert_eq!(moved((19, 5), (0, 1), VIEWPORT), (19, 5));
    }

    #[test]
    fn a_box_reads_the_same_drawn_in_any_direction() {
        assert_eq!(rectangle((5, 3), (2, 1)), rectangle((2, 1), (5, 3)));
    }
}
