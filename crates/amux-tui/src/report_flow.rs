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
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use tokio::task::JoinHandle;

use amux_ui::report::{FrameCapture, LOG_TAIL_BYTES, log_tail};
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
    fn runtime() -> Runtime {
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

    fn diagnostics(log_path: Option<PathBuf>, dump: &'static str) -> DiagnosticsSource {
        DiagnosticsSource {
            daemon_dump: Arc::new(move || Box::pin(async move { Ok(dump.to_string()) })),
            log_path,
            reports_dir: PathBuf::from("/unused"),
            git_sha: "abc1234",
        }
    }

    fn drawn_frame() -> Buffer {
        let mut frame = Buffer::empty(Rect::new(0, 0, 8, 2));
        frame.set_string(0, 0, "fleet", ratatui::style::Style::default());
        frame
    }

    /// Prime a ring the way the live loop does: roll at the frame
    /// boundary, then record the draw.
    fn primed_trace(runtime: &Runtime) -> SharedTrace {
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
