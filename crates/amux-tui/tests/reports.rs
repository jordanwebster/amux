use std::fs;
use std::path::Path;

use amux_tui::replay;
use amux_ui::report::ReplayVerdict;

/// Re-record the captured frame of every committed fixture from its own
/// trace, with `UPDATE_GOLDENS=1`.
///
/// A fixture holds the frame a person was looking at when they captured a
/// report, and the trace that reproduces it. A deliberate change to how a
/// frame is drawn makes every committed frame stale at once — the trace
/// still replays, but into a screen that no longer matches the one filed
/// beside it. Re-recording rewrites the captured frame from the replay,
/// which is the same thing the original capture did.
fn rerecord(fixture: &Path) {
    let mut replay = replay::Replay::load(fixture)
        .unwrap_or_else(|error| panic!("failed to load {}: {error}", fixture.display()));
    replay
        .step_to_end()
        .unwrap_or_else(|error| panic!("failed to replay {}: {error}", fixture.display()));
    let frame = replay
        .frame()
        .unwrap_or_else(|error| panic!("failed to draw {}: {error}", fixture.display()));
    fs::write(fixture.join("frame.txt"), frame.text).expect("write frame.txt");
    fs::write(fixture.join("frame.styles"), frame.styles).expect("write frame.styles");
}

#[test]
fn every_committed_report_fixture_reproduces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reports");
    let mut fixtures = fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()))
        .map(|entry| entry.expect("failed to read fixture entry").path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    fixtures.sort();
    assert!(
        !fixtures.is_empty(),
        "no report fixtures in {}",
        root.display()
    );

    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    for fixture in fixtures {
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            rerecord(&fixture);
        }
        let verdict = replay::verify(&fixture)
            .unwrap_or_else(|error| panic!("failed to replay {}: {error}", fixture.display()));
        assert_eq!(
            verdict,
            ReplayVerdict::Reproduces,
            "{} no longer reproduces",
            fixture.display()
        );
        assert_redacted(&fixture, &hostname);
    }
}

fn assert_redacted(path: &Path, hostname: &str) {
    for entry in fs::read_dir(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
    {
        let entry = entry.expect("failed to read fixture entry");
        let entry_path = entry.path();
        if entry_path.is_dir() {
            assert_redacted(&entry_path, hostname);
            continue;
        }
        assert_private_text_absent(&entry_path, hostname);
    }
}

fn assert_private_text_absent(path: &Path, hostname: &str) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("fixture file {} is not UTF-8: {error}", path.display()));
    for private in ["/Users/", "/home/", hostname] {
        if !private.is_empty() {
            assert!(
                !text.contains(private),
                "fixture file {} contains private text {private:?}",
                path.display()
            );
        }
    }
}

/// A bundle from the phone: a picture of a composited view and the trace
/// the view recorded. Nothing in this crate draws that screen, so the
/// terminal replay says so rather than comparing a picture with cells.
#[test]
fn report_image_frame_bundle_is_not_replayed_by_the_terminal_chrome() {
    use amux_ui::report::{
        FrameCapture, ImageFrame, Mark, ReportDraft, ReportKind, ReportParts, ReportWriter,
        TraceKind, read_frame, read_header,
    };

    let root = tempfile::tempdir().expect("temp dir");
    let report = ReportWriter::new(root.path().to_path_buf(), "phone", "sha1234")
        .write(
            ReportDraft {
                kind: ReportKind::Bug,
                detail: None,
                note: "the send button sits under the keyboard".to_string(),
                marks: vec![Mark {
                    x: 8.5,
                    y: 712.0,
                    width: 120.0,
                    height: 48.0,
                    note: "hidden".to_string(),
                }],
                viewport: None,
                replay: ReplayVerdict::Unchecked,
            },
            ReportParts {
                frame: Some(FrameCapture::Image {
                    png: b"\x89PNG\r\n\x1a\nphone screen".to_vec(),
                    frame: ImageFrame {
                        width_pt: 393.0,
                        height_pt: 852.0,
                        scale: 3,
                    },
                }),
                trace: Some(b"{\"screen\":\"conversation\"}\n".to_vec()),
                trace_kind: TraceKind::NativeView,
                msgs: None,
                daemon: None,
                log: None,
                absent_reason: "not captured on the phone".to_string(),
                log_absent_reason: None,
                daemon_absent_reason: None,
            },
        )
        .expect("report writes");

    let header = read_header(&report).expect("header reads");
    assert_eq!(header.parts.trace_kind, Some(TraceKind::NativeView));
    assert!(header.image_frame.is_some());
    assert!(
        read_frame(&report)
            .expect("frame reads")
            .expect("a frame was captured")
            .as_terminal()
            .is_none()
    );

    let error = replay::verify(&report).expect_err("the chrome cannot redraw a picture");
    assert!(
        matches!(error, replay::ReplayError::NotATerminalFrame),
        "unexpected error: {error}"
    );
}
