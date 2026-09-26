//! The Claude PTY interpreter against the recorded terminal sessions in
//! claude-specs and authored facts for what no recording shows.

use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::{Event, Interpreter, claude_pty_input, run_golden};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude_pty")
}

fn golden(name: &str) {
    run_golden::<ClaudePty>(&fixtures().join(format!("{name}.json"))).assert_ok();
}

macro_rules! goldens {
    ($($test:ident => $fixture:literal,)*) => {
        $(
            #[test]
            fn $test() {
                golden($fixture);
            }
        )*

        const FIXTURES: &[&str] = &[$($fixture),*];
    };
}

goldens! {
    recorded_prompt => "recorded_prompt",
    recorded_prompt_multiline => "recorded_prompt_multiline",
    recorded_tools => "recorded_tools",
    recorded_interrupt => "recorded_interrupt",
    recorded_mode_cycle => "recorded_mode_cycle",
    recorded_permission_allow_once => "recorded_permission_allow_once",
    recorded_permission_allow_scoped => "recorded_permission_allow_scoped",
    recorded_permission_deny_feedback => "recorded_permission_deny_feedback",
    recorded_plan_approve => "recorded_plan_approve",
    recorded_plan_auto => "recorded_plan_auto",
    recorded_plan_request_changes => "recorded_plan_request_changes",
    recorded_question_single => "recorded_question_single",
    recorded_question_multi_other => "recorded_question_multi_other",
    recorded_question_mixed => "recorded_question_mixed",
    recorded_question_tabs => "recorded_question_tabs",
    recorded_question_other_single => "recorded_question_other_single",
    recorded_compact_relink => "recorded_compact_relink",
    recorded_clear_relink => "recorded_clear_relink",
    recorded_task_tools => "recorded_task_tools",
    recorded_socket_delivery => "recorded_socket_delivery",
    recorded_background_shell => "recorded_background_shell",
    recorded_file_changes_and_failure => "recorded_file_changes_and_failure",
    recorded_image => "recorded_image",
    recorded_question_cancelled => "recorded_question_cancelled",
    recorded_question_every_shape => "recorded_question_every_shape",
    recorded_subagent => "recorded_subagent",
    recorded_task_list => "recorded_task_list",
    recorded_thinking => "recorded_thinking",
    recorded_steer_queued => "recorded_steer_queued",
    recorded_api_error => "recorded_api_error",
    recorded_sign_in_problem => "recorded_sign_in_problem",
    hook_before_row_and_row_before_hook_yield_one_tool_call => "hook_row_order",
    asks_open_on_the_hook_and_close_only_on_facts => "ask_rules",
    answers_through_amux_become_terminal_inputs => "answers",
    semantic_inputs_become_effects => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    background_subagent_answers_on_its_agent_row => "subagent_notification",
    boundaries_carry_the_provider_session_and_version => "boundaries",
    rows_no_recording_shows => "rows",
    recorded_steer_send_now => "recorded_steer_send_now",
}

#[test]
fn every_fixture_has_a_test() {
    let mut on_disk = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect::<Vec<_>>();
    on_disk.sort();
    let mut named = FIXTURES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    named.sort();
    assert_eq!(on_disk, named);
}

/// The consumption point for agent messages on a terminal Claude: an
/// accepted message stays pending, gating a one-shot exit, until Claude
/// shows it to the model; then the pending set is empty.
#[test]
fn agent_messages_leave_the_pending_set_when_claude_shows_them_to_the_model() {
    let spec = wire::AgentSpec {
        agent_id: b"recipient".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    for (id, text) in [
        ("env-idle", "A2A_SOCKET_IDLE_21240"),
        ("env-busy", "A2A_SOCKET_BUSY_21240"),
    ] {
        let input = wire::Input {
            input_id: id.as_bytes().to_vec(),
            of: Some(wire::input::Of::AgentMessage(wire::Envelope {
                id: id.as_bytes().to_vec(),
                text: text.to_owned(),
                ..Default::default()
            })),
        };
        ClaudePty::step(&mut state, Event::Input(input));
    }
    assert_eq!(state.shared().pending_messages().len(), 2);
    assert!(!state.shared().quiescent());
    let recording = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../claude-specs/fixtures/claude-pty/socket_delivery.rows.jsonl"),
    )
    .unwrap();
    let mut consumed_after = Vec::new();
    for (index, event) in ClaudePty::recording("transcript_rows", &recording)
        .unwrap()
        .into_iter()
        .enumerate()
    {
        let before = state.shared().pending_messages().len();
        ClaudePty::step(&mut state, event);
        if state.shared().pending_messages().len() < before {
            consumed_after.push(index);
        }
    }
    assert_eq!(consumed_after.len(), 2, "each message consumed once");
    // The recording ends as Claude starts answering the second message, so
    // the agent is busy, but nothing it accepted is still waiting.
    assert!(state.shared().pending_messages().is_empty());
}

/// Every fixture input the shared vocabulary names has a terminal arm.
#[test]
fn the_fixture_vocabulary_maps_to_terminal_inputs() {
    let key = interpret::FixtureInput::Key {
        name: "toggle_thinking".into(),
    };
    assert!(claude_pty_input(b"k".to_vec(), &key).is_some());
}
