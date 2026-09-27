//! View data goldens for all three kinds, derived from every interpreter
//! fixture: the interpreter's emission is committed the way the daemon
//! commits it (an order once per new key, a revision per record), reduced
//! into a session, and the views are rendered after every frame.
//!
//! Goldens live in `tests/goldens/<kind>/<fixture>.golden` and are only
//! rewritten with `UI_VIEW_UPDATE_GOLDENS=1`; review the diff.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::{Codex, CodexWith, Drained, Parked};
use interpret::{Interpreter, Replayed, replay};
use ui_state::{InputOutcome, Msg, SessionState};
use ui_view::{
    ChatOptions, ToolRows, ask_card, chat_rows, chat_rows_for, composer, outbox_rows, queue_rows,
    session_strip,
};
use wire::{Kind, SessionEvent, session_event};

mod support;

use support::Committer;

const UPDATE: &str = "UI_VIEW_UPDATE_GOLDENS";

fn fixtures(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../interpret/fixtures")
        .join(kind)
}

fn goldens(kind: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens")
        .join(kind)
}

fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: Some("worker".into()),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: wire::Phase::NeedsYou as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

fn clip(line: String) -> String {
    const MAX: usize = 400;
    if line.chars().count() <= MAX {
        return line;
    }
    let mut clipped: String = line.chars().take(MAX).collect();
    clipped.push('…');
    clipped
}

/// A question row's answers, ahead of the clipped row so they stay legible.
fn answered(kind: &ui_view::RowKind) -> String {
    let ui_view::RowKind::Ask(ui_view::AskRow::Question {
        questions,
        answers,
        note,
        resolution,
    }) = kind
    else {
        return String::new();
    };
    let answers: Vec<String> = questions
        .iter()
        .zip(answers)
        .map(|(question, answer)| {
            let mut said = answer.picked.join(", ");
            if let Some(other) = &answer.other {
                if !said.is_empty() {
                    said.push_str(", ");
                }
                let _ = write!(said, "{other:?}");
            }
            format!("{} = {said}", question.header)
        })
        .collect();
    let note = note
        .as_ref()
        .map(|note| format!(" note={note:?}"))
        .unwrap_or_default();
    format!("[{resolution:?}: {}{note}] ", answers.join("; "))
}

fn describe_row(row: &ui_view::Row) -> String {
    let mut line = format!(
        "{:>4} {} {}{:?}",
        row.order,
        row.id,
        answered(&row.kind),
        row.kind
    );
    if let Some(run) = &row.run {
        let _ = write!(
            line,
            " run[{}..{} x{} reads={} searches={}{}{}]",
            run.oldest,
            run.newest,
            run.len,
            run.reads,
            run.searches,
            if run.is_summary { " summary" } else { "" },
            if run.open_below { " open_below" } else { "" }
        );
    }
    if let Some(decision) = &row.decision {
        let _ = write!(line, " decision={decision:?}");
    }
    if row.collapsed {
        line.push_str(" collapsed");
    }
    if row.attention {
        line.push_str(" attention");
    }
    if let Some(parent) = &row.parent {
        let _ = write!(line, " parent={parent}");
    }
    clip(line)
}

fn describe_card(card: Option<&ui_view::AskCard>) -> String {
    let Some(card) = card else {
        return "  ask none".into();
    };
    let choices: Vec<String> = card
        .choices
        .iter()
        .map(|choice| {
            format!(
                "{:?}{}{}",
                choice.outcome,
                if choice.primary { "*" } else { "" },
                if choice.takes_note { "+note" } else { "" }
            )
        })
        .collect();
    format!(
        "  ask {} -> {:?} {}/{} {:?}\n    choices {}\n    body {}",
        card.key,
        card.item_key,
        card.position,
        card.count,
        card.state,
        choices.join(", "),
        clip(format!("{:?}", card.body))
    )
}

/// Replays a fixture's emission through the session and renders the views.
fn render(kind: Kind, frames: &[Replayed]) -> String {
    let mut state = SessionState::new(agent(kind));
    let mut committer = Committer::default();
    let mut out = String::new();
    let expanded = HashSet::new();
    let opts = ChatOptions {
        tools: ToolRows::CollapseRuns {
            expanded: &expanded,
        },
    };
    let mut last_card = None;
    let mut last_composer = String::new();
    for (index, frame) in frames.iter().enumerate() {
        let mut lines = Vec::new();
        if let Some(input) = &frame.input {
            state.update(Msg::Send(input.clone()));
        }
        for (id, verdict) in &frame.replies {
            state.update(Msg::Sent(id.clone(), InputOutcome::Reply(verdict.clone())));
        }
        let mut changed = Vec::new();
        for event in committer.commit(&frame.step) {
            let outcome = state.update(Msg::Event(event));
            assert!(
                outcome.need_get.is_none(),
                "a committed append always has its base"
            );
            changed.extend(outcome.changed);
        }
        if index == 0 {
            state.update(Msg::Event(SessionEvent {
                of: Some(session_event::Of::CaughtUp(wire::CaughtUp {
                    revision: committer.revision,
                })),
            }));
        }
        changed.sort();
        changed.dedup();
        for row in chat_rows_for(&state, &changed, &opts) {
            lines.push(format!("  row {}", describe_row(&row)));
        }
        let card = ask_card(&state);
        if card != last_card {
            lines.push(describe_card(card.as_ref()));
            last_card = card;
        }
        let at = frame
            .step
            .items
            .iter()
            .map(|item| item.at_ms)
            .max()
            .unwrap_or(0);
        let composing = format!(
            "  composer {:?} activity={:?} queue={:?} outbox={:?}",
            composer(&state, at).mode,
            state.activity(at).map(|activity| activity.kind),
            queue_rows(&state),
            outbox_rows(&state)
        );
        if composing != last_composer {
            lines.push(clip(composing.clone()));
            last_composer = composing;
        }
        if !lines.is_empty() {
            let _ = writeln!(out, "## {index} {}", clip(frame.label.clone()));
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
        }
    }
    let _ = writeln!(out, "== chat");
    let head = state.transcript().head().unwrap_or(0);
    for row in chat_rows(&state, 0..=head, &opts) {
        let _ = writeln!(out, "  {}", describe_row(&row));
    }
    let _ = writeln!(out, "== strip {:?}", session_strip(&state));
    out
}

fn check(
    kind: Kind,
    dir: &str,
    name: &str,
    frames: Result<Vec<Replayed>, String>,
) -> Option<String> {
    let frames = match frames {
        Ok(frames) => frames,
        Err(error) => return Some(format!("{dir}/{name}: {error}")),
    };
    let rendered = render(kind, &frames);
    let path = goldens(dir).join(format!("{name}.golden"));
    if std::env::var(UPDATE).as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &rendered).unwrap();
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(golden) if golden == rendered => None,
        Ok(golden) => {
            let at = golden
                .lines()
                .zip(rendered.lines())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| golden.lines().count().min(rendered.lines().count()));
            Some(format!(
                "{dir}/{name}: golden differs at line {}\n  golden:   {}\n  rendered: {}",
                at + 1,
                golden.lines().nth(at).unwrap_or("<end>"),
                rendered.lines().nth(at).unwrap_or("<end>")
            ))
        }
        Err(_) => Some(format!(
            "{dir}/{name}: no golden; run with {UPDATE}=1 and review"
        )),
    }
}

fn run_all<I: Interpreter>(kind: Kind, dir: &str, special: &Special) {
    let mut failures = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            (path.extension()? == "json")
                .then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    assert!(!names.is_empty());
    let mut expected = HashSet::new();
    for name in &names {
        let path = fixtures(dir).join(format!("{name}.json"));
        let frames = special(name, &path).unwrap_or_else(|| replay::<I>(&path));
        expected.insert(format!("{name}.golden"));
        failures.extend(check(kind, dir, name, frames));
    }
    // A golden whose fixture is gone is stale.
    if let Ok(entries) = std::fs::read_dir(goldens(dir)) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !expected.contains(&file) {
                failures.push(format!("{dir}/{file}: golden has no fixture"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A fixture that needs a particular interpreter arm replays through it.
type Special = dyn Fn(&str, &Path) -> Option<Result<Vec<Replayed>, String>>;

fn none(_: &str, _: &Path) -> Option<Result<Vec<Replayed>, String>> {
    None
}

#[test]
fn claude_pty_view_goldens() {
    run_all::<ClaudePty>(Kind::ClaudePty, "claude_pty", &none);
}

#[test]
fn claude_sdk_view_goldens() {
    run_all::<ClaudeSdk>(Kind::ClaudeSdk, "claude_sdk", &none);
}

#[test]
fn codex_view_goldens() {
    // Two fixtures pin a message-consumption arm of the Codex interpreter.
    let arms = |name: &str, path: &Path| match name {
        "inject_parked" => Some(replay::<CodexWith<Parked>>(path)),
        "inject_drained" | "inject_drained_turn_end" => Some(replay::<CodexWith<Drained>>(path)),
        _ => None,
    };
    run_all::<Codex>(Kind::Codex, "codex", &arms);
}
