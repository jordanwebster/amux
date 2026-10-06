//! Runs: tool steps between two pieces of the agent's text, stated on every
//! row of the run, counted by what they did, live while the agent is still
//! at them, keeping in view the failures a finished turn left unresolved,
//! and folded, hidden or shown whole as the client chooses.

use std::collections::HashSet;

use prost::Message;
use ui_state::{Key, Msg, SessionState};
use ui_view::{ChatOptions, Row, Run, RunCounts, ToolRows, chat_rows};
use wire::{Item, Kind, Phase, SessionEvent, ToolClass, ToolState, session_event};

fn event(of: session_event::Of) -> Msg {
    Msg::Event(SessionEvent { of: Some(of) })
}

/// What one authored item is.
enum It {
    Prompt(&'static str),
    Say(&'static str),
    Think,
    Read(&'static str),
    Run(&'static str, i32),
    Running(&'static str),
    Edit(&'static str),
    Turn,
}

fn item(order: u64, it: &It) -> Item {
    use wire::claude_sdk_item::Kind as K;
    let tool = |name: &str, input: String, state: ToolState, reads: bool, exit: Option<i32>| {
        K::Tool(wire::ToolCall {
            name: name.into(),
            input_json: input.into_bytes(),
            state: state as i32,
            class: if reads {
                ToolClass::Read
            } else {
                ToolClass::Consequential
            } as i32,
            exit_code: exit,
            ..Default::default()
        })
    };
    let (kind, text) = match it {
        It::Prompt(words) => (K::Prompt(wire::Prompt::default()), *words),
        It::Say(words) => (K::Message(wire::Text { complete: true }), *words),
        It::Think => (K::Thinking(wire::Thinking { complete: true }), "hm"),
        It::Read(path) => (
            tool(
                "Read",
                format!(r#"{{"file_path":"{path}"}}"#),
                ToolState::Succeeded,
                true,
                None,
            ),
            "",
        ),
        It::Run(command, exit) => (
            tool(
                "Bash",
                format!(r#"{{"command":"{command}"}}"#),
                ToolState::Succeeded,
                false,
                Some(*exit),
            ),
            "",
        ),
        It::Running(command) => (
            tool(
                "Bash",
                format!(r#"{{"command":"{command}"}}"#),
                ToolState::Running,
                false,
                None,
            ),
            "",
        ),
        It::Edit(path) => (
            tool(
                "Edit",
                format!(r#"{{"file_path":"{path}","old_string":"a","new_string":"b"}}"#),
                ToolState::Succeeded,
                false,
                None,
            ),
            "",
        ),
        It::Turn => (K::Turn(wire::Turn::default()), ""),
    };
    Item {
        key: format!("k{order}"),
        order,
        revision: order,
        text: text.into(),
        kind: wire::kind_tag(Kind::ClaudeSdk).into(),
        body: wire::ClaudeSdkItem { kind: Some(kind) }.encode_to_vec(),
        at_ms: order as i64 * 1_000,
        ..Item::default()
    }
}

fn session(items: &[It]) -> SessionState {
    let agent = wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: Kind::ClaudeSdk as i32,
        lifecycle: wire::Lifecycle::Live as i32,
        phase: Phase::Working as i32,
        incarnation: 1,
        ..wire::Agent::default()
    };
    let mut state = SessionState::new(agent, 200);
    state.update(Msg::Connection(ui_state::Connection::Live));
    state.update(event(
        session_event::Of::Snapshot(wire::Snapshot::default()),
    ));
    for (i, it) in items.iter().enumerate() {
        state.update(event(session_event::Of::Item(item(i as u64 + 1, it))));
    }
    state.update(event(session_event::Of::CaughtUp(wire::CaughtUp {
        revision: 0,
    })));
    state
}

fn rows(state: &SessionState, tools: ToolRows) -> Vec<Row> {
    chat_rows(state, 0..=u64::MAX, &ChatOptions { tools })
}

fn run_at(state: &SessionState, order: u64) -> Option<Run> {
    rows(state, ToolRows::ShowAll)
        .into_iter()
        .find(|row| row.order == order)
        .and_then(|row| row.run)
}

fn at(state: &SessionState, order: u64) -> Run {
    run_at(state, order).unwrap_or_else(|| panic!("no run at {order}"))
}

/// The orders that draw under `tools`.
fn shown(state: &SessionState, tools: ToolRows) -> Vec<u64> {
    rows(state, tools)
        .into_iter()
        .filter(|row| !row.collapsed)
        .map(|row| row.order)
        .collect()
}

#[test]
fn text_splits_a_turn_into_runs_stated_on_every_row() {
    let state = session(&[
        It::Prompt("fix it"),
        It::Read("a.rs"),
        It::Think,
        It::Read("b.rs"),
        It::Say("found it"),
        It::Edit("a.rs"),
        It::Run("just test", 0),
        It::Say("done"),
        It::Turn,
    ]);
    // Thinking passes through the first run without ending it.
    for order in 2..=4 {
        let run = at(&state, order);
        assert_eq!(
            (run.id.as_str(), run.last.as_str(), run.steps),
            ("k2", "k4", 2)
        );
        assert!(!run.live);
    }
    assert_eq!(at(&state, 2).counts, None, "counts sit on the newest step");
    assert_eq!(
        at(&state, 4).counts,
        Some(RunCounts {
            reads: 2,
            ..RunCounts::default()
        })
    );
    assert!(at(&state, 4).is_last());
    assert_eq!(at(&state, 2).recent, Some(1));

    let second = at(&state, 7);
    assert_eq!((second.id.as_str(), second.steps), ("k6", 2));
    let counts = second.counts.unwrap();
    assert_eq!((counts.edits, counts.commands), (1, 1));

    // Text, prompts and turn ends are in no run.
    for order in [1, 5, 8, 9] {
        assert_eq!(run_at(&state, order), None, "order {order}");
    }
}

#[test]
fn a_run_at_the_head_is_live_and_shows_its_newest_steps() {
    let state = session(&[
        It::Prompt("go"),
        It::Say("on it"),
        It::Read("a.rs"),
        It::Read("b.rs"),
        It::Think,
        It::Read("c.rs"),
        It::Running("just lint"),
    ]);
    let run = at(&state, 7);
    assert!(run.live);
    assert!(!run.unresolved_failure, "the turn has not ended");
    assert_eq!(
        (
            at(&state, 3).recent,
            at(&state, 4).recent,
            at(&state, 7).recent
        ),
        (None, Some(2), Some(0))
    );
    let none = HashSet::new();
    assert_eq!(
        shown(&state, ToolRows::Collapse { open: &none }),
        vec![1, 2, 4, 6, 7],
        "the newest three steps show while it runs; the thinking between them folds"
    );
}

#[test]
fn a_folded_run_keeps_its_newest_step_and_a_failure_left_unresolved() {
    let state = session(&[
        It::Prompt("check"),
        It::Read("docs/a.md"),
        It::Run("just docs-check", 1),
        It::Read("docs/b.md"),
        It::Say("one broken link"),
        It::Turn,
    ]);
    assert!(at(&state, 3).unresolved_failure);
    assert!(!at(&state, 2).unresolved_failure);
    let none = HashSet::new();
    assert_eq!(
        shown(&state, ToolRows::Collapse { open: &none }),
        vec![1, 3, 4, 5, 6]
    );
    // Opened by its id, every step shows.
    let open: HashSet<Key> = ["k2".to_owned()].into();
    assert_eq!(
        shown(&state, ToolRows::Collapse { open: &open }),
        vec![1, 2, 3, 4, 5, 6]
    );
    // Hidden, only the failure stays.
    assert_eq!(shown(&state, ToolRows::Hide), vec![1, 3, 5, 6]);
    assert_eq!(shown(&state, ToolRows::ShowAll), vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn a_failure_redone_in_the_turn_is_resolved_and_one_left_is_not() {
    // The lint fails, the edit fixes it, the rerun passes.
    let fixed = session(&[
        It::Prompt("tidy"),
        It::Run("just lint", 1),
        It::Edit("a.rs"),
        It::Run("just lint", 0),
        It::Say("clean"),
        It::Turn,
    ]);
    assert!(!at(&fixed, 2).unresolved_failure);

    // The check fails and the agent says so and stops; until the turn
    // ends, the agent may still fix it.
    let left = [
        It::Prompt("check"),
        It::Run("just docs-check", 1),
        It::Say("one broken link"),
        It::Turn,
    ];
    assert!(!at(&session(&left[..3]), 2).unresolved_failure);
    assert!(at(&session(&left), 2).unresolved_failure);

    // A rerun in a later run of the same turn still resolves it.
    let later = session(&[
        It::Prompt("check"),
        It::Run("just docs-check", 1),
        It::Say("fixing the link"),
        It::Edit("docs/a.md"),
        It::Run("just docs-check", 0),
        It::Say("fixed"),
        It::Turn,
    ]);
    assert!(!at(&later, 2).unresolved_failure);
}
