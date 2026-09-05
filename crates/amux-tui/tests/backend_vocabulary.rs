//! Nothing amux draws may tell a person which machinery an agent runs on.
//!
//! A Claude session reached over a terminal and one reached over
//! stream-JSON are the same agent to whoever is talking to it. The moment
//! a screen says which of the two it is, the person has to care — and
//! every later screen has to keep the story straight. So the words that
//! name the machinery are banned from rendered text outright, and this
//! scan reads every frame amux has a committed record of.

use std::path::{Path, PathBuf};

use amux_tui::fixtures::{NamedState, all_states, fixture};
use amux_tui::{FrameContext, Theme, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Words that name how an agent is driven rather than what it is doing.
const MACHINERY_WORDS: &[&str] = &["sdk", "pty", "driver", "backend", "unsupported"];

/// The daemon's wire name for the stream-JSON plane. Its pieces are
/// already banned one by one; naming the whole thing keeps a frame that
/// prints a raw row type from sliding through on a technicality.
const WIRE_PLANE: &str = "claude_sdk_v1";

/// Every word of a stretch of rendered text, lowercased. Splitting on
/// everything that is not a letter or a digit is what keeps `empty` from
/// reading as `pty` and lets `claude-sdk` read as `sdk`.
fn words(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The machinery words this text says out loud, if any.
fn machinery_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = words(text)
        .into_iter()
        .filter(|word| MACHINERY_WORDS.contains(&word.as_str()))
        .collect();
    if text.to_ascii_lowercase().contains(WIRE_PLANE) {
        found.push(WIRE_PLANE.to_string());
    }
    found.sort();
    found.dedup();
    found
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// The rendered text a golden file holds, or `None` when the file records
/// style classifications rather than words. A classification map is a
/// grid of one-letter codes; run together they spell things nobody drew.
fn golden_text(name: &str, body: &str) -> Option<String> {
    if let Some(rest) = body.split_once("--- text ---") {
        let text = rest.1;
        return Some(match text.split_once("--- styles ---") {
            Some((before, _)) => before.to_string(),
            None => text.to_string(),
        });
    }
    if name.contains("styles") {
        return None;
    }
    Some(body.to_string())
}

/// The review page paints a repository's own diff — words the person
/// chose, in files amux never wrote. A machinery word landing there says
/// nothing about how amux describes an agent, so the page's own captures
/// and named states sit outside this scan.
fn is_review_capture(name: &str) -> bool {
    name.contains("review")
}

fn is_review_state(state: NamedState) -> bool {
    is_review_capture(state.name())
}

#[test]
fn backend_vocabulary_stays_out_of_committed_goldens() {
    let mut scanned = 0;
    for entry in std::fs::read_dir(golden_dir()).expect("golden directory") {
        let path = entry.expect("golden entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("golden name")
            .to_string();
        if is_review_capture(&name) {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read golden");
        let Some(text) = golden_text(&name, &body) else {
            continue;
        };
        scanned += 1;
        let found = machinery_in(&text);
        assert!(
            found.is_empty(),
            "the golden frame {name} says {found:?}, which names how the agent is driven:\n{text}"
        );
    }
    assert!(scanned > 100, "the scan found the goldens: {scanned}");
}

#[test]
fn backend_vocabulary_stays_out_of_named_state_frames() {
    for state in all_states() {
        if is_review_state(*state) {
            continue;
        }
        for (label, theme) in [
            ("dark", Theme::default()),
            ("light", Theme::light(amux_tui::ColorMode::TrueColor)),
        ] {
            let text = frame_text(*state, theme);
            let found = machinery_in(&text);
            assert!(
                found.is_empty(),
                "{state} ({label}) says {found:?}, which names how the agent is driven:\n{text}"
            );
        }
    }
}

/// The screen one named state draws at the size every capture uses.
fn frame_text(state: NamedState, theme: Theme) -> String {
    let fixture = fixture(state);
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("terminal");
    let context = FrameContext {
        viewport: (120, 40),
        theme,
        now: fixture.now,
    };
    terminal
        .draw(|frame| render(&fixture.model, &fixture.view, &context, frame))
        .unwrap_or_else(|error| panic!("{state} failed to render: {error}"));
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).expect("cell in area").symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The scan is only worth running if it can actually catch something, and
/// the trap it must not fall into is spelling: `empty` ends in the three
/// letters of a machinery word, and `claude-sdk-idle` hides one behind a
/// hyphen.
#[test]
fn backend_vocabulary_reads_words_rather_than_letters() {
    assert_eq!(
        machinery_in("  fix-auth · claude @ mbp"),
        Vec::<String>::new()
    );
    assert_eq!(
        machinery_in("ctrl+c clear field; empty: press twice"),
        Vec::<String>::new()
    );
    assert_eq!(
        machinery_in("supported by the reader"),
        Vec::<String>::new()
    );
    assert_eq!(
        machinery_in("  sdk-writer · claude @ mbp"),
        vec!["sdk".to_string()]
    );
    assert_eq!(machinery_in("claude-sdk-idle"), vec!["sdk".to_string()]);
    assert_eq!(
        machinery_in("driver: PTY"),
        vec!["driver".to_string(), "pty".to_string()]
    );
    assert_eq!(
        machinery_in("this chat is unsupported in this build"),
        vec!["unsupported".to_string()]
    );
    assert_eq!(
        machinery_in("row claude_sdk_v1.ready"),
        vec!["claude_sdk_v1".to_string(), "sdk".to_string()]
    );
}
