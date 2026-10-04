//! Stretches: tool steps between two pieces of the agent's text, found from
//! any member, counted by what they did, open while the agent is still at
//! them, and naming the failures a finished turn left unresolved.

use prost::Message;
use ui_state::{Msg, SessionState};
use ui_view::{Stretch, stretch_at, stretch_steps};
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

fn at(state: &SessionState, order: u64) -> Stretch {
    stretch_at(state, order).unwrap_or_else(|| panic!("no stretch at {order}"))
}

#[test]
fn text_splits_a_turn_into_stretches_found_from_any_member() {
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
    // Thinking passes through the first stretch without ending it.
    let first = at(&state, 4);
    assert_eq!(first, at(&state, 2));
    assert_eq!((first.oldest_order, first.newest_order), (2, 4));
    assert_eq!((first.counts.steps, first.counts.reads), (2, 2));
    assert_eq!(stretch_steps(&state, &first), vec![2, 4]);
    assert!(first.closed);

    let second = at(&state, 6);
    assert_eq!((second.oldest_order, second.newest_order), (6, 7));
    assert_eq!((second.counts.edits, second.counts.commands), (1, 1));

    // Text, prompts and turn ends are in no stretch.
    for order in [1, 5, 8, 9] {
        assert_eq!(stretch_at(&state, order), None, "order {order}");
    }
}

#[test]
fn a_stretch_at_the_head_is_open_and_running_while_its_step_runs() {
    let state = session(&[
        It::Prompt("go"),
        It::Say("on it"),
        It::Read("a.rs"),
        It::Running("just lint"),
    ]);
    let stretch = at(&state, 4);
    assert!(!stretch.closed);
    assert!(stretch.running);
    assert!(stretch.unresolved.is_empty(), "the turn has not ended");
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
    assert!(at(&fixed, 2).unresolved.is_empty());

    // The check fails and the agent says so and stops.
    let left = session(&[
        It::Prompt("check"),
        It::Run("just docs-check", 1),
        It::Say("one broken link"),
        It::Turn,
    ]);
    assert_eq!(at(&left, 2).unresolved, vec!["k2".to_owned()]);

    // A rerun in a later stretch of the same turn still resolves it.
    let later = session(&[
        It::Prompt("check"),
        It::Run("just docs-check", 1),
        It::Say("fixing the link"),
        It::Edit("docs/a.md"),
        It::Run("just docs-check", 0),
        It::Say("fixed"),
        It::Turn,
    ]);
    assert!(at(&later, 2).unresolved.is_empty());
}
