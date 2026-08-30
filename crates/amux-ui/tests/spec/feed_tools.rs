//! Chapter 8 — The chat feed: tool pairing and result facts.
//!
//! `docs/CHAT.md` B4/B5/B6 against the derived provider fixtures. Pairing is FACT:
//! `tool_use.id` ↔ the `tool_result` block's `tool_use_id`. File change
//! magnitude restates the `toolUseResult.structuredPatch` sidecar
//! verbatim; denial is the typed `toolDenialKind` fact, never an
//! error-string sniff; question answers are keyed by the question TEXT
//! (provider-capture correction); an approved plan is retained as session
//! state keyed by tool_use id, outside feed windowing.

use amux_ui::Msg;
use amux_ui::claude::{
    ClaudeLayer, FeedEntryKind, SuccessFacts, ToolEntry, ToolInvocation, ToolOutcome,
};
use serde_json::json;

use crate::harness::*;

fn tools_of(layer: &ClaudeLayer) -> Vec<&ToolEntry> {
    layer
        .entries()
        .filter_map(|entry| match &entry.kind {
            FeedEntryKind::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect()
}

/// The tools fixture: Read, then Edit with its FACT magnitude from
/// `structuredPatch`, then Bash with its bounded output head. Every pair
/// resolves; nothing stays pending.
#[test]
fn read_edit_bash_pair_with_typed_result_facts() {
    let model = fold(chat_feed("fix-auth-bug", "tools"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let tools = tools_of(layer);
    assert_eq!(tools.len(), 3);

    assert!(matches!(
        &tools[0].invocation,
        ToolInvocation::Read { file_path: Some(path) } if path == "<MACHINE_PATH>"
    ));
    assert!(matches!(
        &tools[0].outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::Output { head, .. }
        } if head.contains("VALUE=1")
    ));

    assert!(matches!(
        &tools[1].invocation,
        ToolInvocation::Edit { file_path: Some(path), replace_all: false }
            if path == "<MACHINE_PATH>"
    ));
    assert_eq!(
        tools[1].outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::Edit {
                file_path: "<MACHINE_PATH>".to_string(),
                added: 1,
                removed: 1,
            }
        },
        "the feed line's (+1 -1) is the sidecar's FACT, never recomputed"
    );

    assert!(matches!(
        &tools[2].invocation,
        ToolInvocation::Bash { command: Some(command), .. } if command.contains("config.txt")
    ));
    assert!(matches!(
        &tools[2].outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::Output { head, truncated: false }
        } if head == "VALUE=2"
    ));
    assert_eq!(layer.open_tool_count(), 0);
}

/// Denial is typed (B5): `is_error:true` plus `toolDenialKind:
/// "user-rejected"` — the allow in the same fixture is the non-error
/// result of the tool actually running, preserved in a separate recording.
#[test]
fn a_denied_tool_carries_the_typed_denial_kind() {
    let allowed = fold(chat_feed("fix-auth-bug", "permission"));
    let allowed_tools = tools_of(claude_layer(&allowed, "fix-auth-bug"));
    assert_eq!(allowed_tools.len(), 1);
    assert!(matches!(
        &allowed_tools[0].outcome,
        ToolOutcome::Success { .. }
    ));

    let denied = fold(chat_feed("fix-auth-bug", "permission_deny_feedback"));
    let tools = tools_of(claude_layer(&denied, "fix-auth-bug"));
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].outcome,
        ToolOutcome::Denied {
            kind: Some("user-rejected".to_string())
        }
    );
}

/// AskUserQuestion facts (single-select): options are `{label,
/// description}` objects, and the answer object is keyed by the question
/// TEXT, not the header (provider-capture correction to B5).
#[test]
fn question_answers_are_keyed_by_question_text() {
    let model = fold(chat_feed("fix-auth-bug", "question_single"));
    let tools = tools_of(claude_layer(&model, "fix-auth-bug"));
    assert_eq!(tools.len(), 1);
    let ToolInvocation::Question { questions } = &tools[0].invocation else {
        panic!("a question invocation: {:?}", tools[0].invocation);
    };
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].header.as_deref(), Some("Color"));
    assert_eq!(
        questions[0].question.as_deref(),
        Some("Which color do you prefer?")
    );
    assert!(!questions[0].multi_select);
    assert_eq!(
        questions[0]
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["Red", "Blue"]
    );
    assert!(questions[0].options[0].description.is_some());

    let ToolOutcome::Success {
        facts: SuccessFacts::Answers { answers },
    } = &tools[0].outcome
    else {
        panic!("answered: {:?}", tools[0].outcome);
    };
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].question, "Which color do you prefer?");
    assert_eq!(answers[0].answer, "Red");
}

/// Multi-select joins every selection — including the committed `Other…`
/// free text — into ONE string, trailing space preserved by the recording.
#[test]
fn multi_select_answers_join_into_one_string() {
    let model = fold(chat_feed("fix-auth-bug", "question_multi"));
    let tools = tools_of(claude_layer(&model, "fix-auth-bug"));
    let ToolOutcome::Success {
        facts: SuccessFacts::Answers { answers },
    } = &tools[0].outcome
    else {
        panic!("answered: {:?}", tools[0].outcome);
    };
    assert_eq!(answers[0].question, "Which tools would you like to use?");
    assert_eq!(answers[0].answer, "Hammer, Saw, Torque wrench ");
}

/// ExitPlanMode approval (B6): the plan payload rides `tool_use.input`,
/// the resolution FACT is the non-error result with the plan sidecar, and
/// the accepted plan is retained as session state keyed by tool_use id —
/// outside feed windowing.
#[test]
fn an_approved_plan_is_retained_as_session_state() {
    let model = fold(chat_feed("fix-auth-bug", "plan_approve"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let plan_tool = tools_of(layer)
        .into_iter()
        .find(|tool| matches!(tool.invocation, ToolInvocation::Plan { .. }))
        .expect("the ExitPlanMode line");
    let ToolInvocation::Plan {
        plan: Some(plan),
        plan_file_path: Some(path),
    } = &plan_tool.invocation
    else {
        panic!("plan payload on the invocation: {:?}", plan_tool.invocation);
    };
    assert_eq!(plan.chars().count(), 267);
    assert_eq!(path, "<MACHINE_PATH>");
    assert!(matches!(
        &plan_tool.outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::PlanApproved {
                plan_file_path: Some(_)
            }
        }
    ));

    let plans = layer.accepted_plans();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].tool_use_id, plan_tool.tool_use_id);
    assert_eq!(plans[0].plan, *plan);
}

/// A rejected plan is a denial like any other (B5): `is_error:true` with
/// the typed kind; nothing is retained.
#[test]
fn a_rejected_plan_retains_nothing() {
    let model = fold(chat_feed("fix-auth-bug", "plan_reject"));
    let layer = claude_layer(&model, "fix-auth-bug");
    let plan_tool = tools_of(layer)
        .into_iter()
        .find(|tool| matches!(tool.invocation, ToolInvocation::Plan { .. }))
        .expect("the ExitPlanMode line");
    assert_eq!(
        plan_tool.outcome,
        ToolOutcome::Denied {
            kind: Some("user-rejected".to_string())
        }
    );
    assert!(layer.accepted_plans().is_empty());
}

/// A Write that CREATES a file carries an empty `structuredPatch` (Phase 1
/// fixture observation — drift from the survey, which listed the patch as
/// the magnitude source for Write too): the honest magnitude is the
/// created content's line count, zero removals.
#[test]
fn a_created_file_states_its_line_count_as_the_magnitude() {
    let model = fold(chat_feed("fix-auth-bug", "plan_approve"));
    let write = tools_of(claude_layer(&model, "fix-auth-bug"))
        .into_iter()
        .find(|tool| matches!(tool.invocation, ToolInvocation::Write { .. }))
        .expect("the Write line");
    let ToolOutcome::Success {
        facts: SuccessFacts::Edit { added, removed, .. },
    } = &write.outcome
    else {
        panic!("patch facts: {:?}", write.outcome);
    };
    assert_eq!(*added, 11, "the created plan file's line count");
    assert_eq!(*removed, 0);
}

/// Grouping is a fold-computed fact (B4): strictly consecutive read/search
/// one-liners group; a file-modifying tool breaks the run.
#[test]
fn consecutive_read_search_lines_carry_the_grouping_fact() {
    let session = "22222222-2222-4222-8222-222222222222";
    let tool_row = |uuid: u128, name: &str, input: serde_json::Value| {
        json!({
            "type": "assistant",
            "uuid": uuid::Uuid::from_u128(uuid).to_string(),
            "sessionId": session,
            "timestamp": "2026-08-11T22:00:01.000Z",
            "message": {
                "id": "msg_explore", "role": "assistant", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": format!("toolu_{uuid}"), "name": name, "input": input}]
            }
        })
    };
    let model = fold(seq([
        chat_base("fix-auth-bug"),
        vec![batch(
            "fix-auth-bug",
            10,
            vec![
                tool_row(0x6000, "Read", json!({"file_path": "a.rs"})),
                tool_row(0x6001, "Grep", json!({"pattern": "reconnect"})),
                tool_row(0x6002, "Edit", json!({"file_path": "a.rs"})),
                tool_row(0x6003, "Read", json!({"file_path": "b.rs"})),
            ],
        )],
    ]));
    let grouping: Vec<bool> = tools_of(claude_layer(&model, "fix-auth-bug"))
        .iter()
        .map(|tool| tool.group_with_previous)
        .collect();
    assert_eq!(
        grouping,
        vec![false, true, false, false],
        "Read←Grep groups; Edit breaks the run and never groups"
    );
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("feed_tools::tools", chat_feed("fix-auth-bug", "tools")),
        (
            "feed_tools::question_single",
            chat_feed("fix-auth-bug", "question_single"),
        ),
        (
            "feed_tools::question_multi",
            chat_feed("fix-auth-bug", "question_multi"),
        ),
        (
            "feed_tools::plan_approve",
            chat_feed("fix-auth-bug", "plan_approve"),
        ),
        (
            "feed_tools::plan_reject",
            chat_feed("fix-auth-bug", "plan_reject"),
        ),
    ]
}
