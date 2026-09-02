//! Chapter 10 — Determinism: the properties everything else stands on.
//!
//! The determinism guarantee is scoped and enforced here: the same reducer
//! build, folding the same checkpoint and ordered Msgs, produces identical
//! Models. Replay folds but never executes Effects.

use amux_ui::claude::answer::{
    AskAnswer, PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse,
};
use amux_ui::report::{ReplayVerdict, ReportDraft, ReportKind, ReportParts, ReportWriter};
use amux_ui::{
    BUILD, DisconnectReason, DumpReason, Effect, Model, Msg, OpOutcome, Recorder,
    StreamCloseReason, StreamEntry, StreamMsg, replay_msgs, update,
};

use crate::harness::*;

/// THE load-bearing test: after every Msg of every registered chapter
/// sequence, folding the serialized recording from scratch equals the live
/// incrementally-updated Model. A property, not a snapshot — it proves
/// purity, serde fidelity, and absence of hidden state in one sweep.
///
/// It is ALSO the suite's invariant sweep: it calls `check_invariants` after
/// every Msg, so every registered sequence — present and future, across both
/// agent layers — is a probe of every structural invariant, including the
/// Codex projection-agreement one. Registering a sequence with the harness is
/// therefore the cheapest way to put a new state under that control; nothing
/// else in the suite needs to opt in.
#[test]
fn differential_fold_matches_live_state_after_every_msg() {
    let sequences = all_sequences();
    assert!(!sequences.is_empty(), "chapters must register sequences");
    for (name, msgs) in sequences {
        let mut live = Model::default();
        let mut recording: Vec<String> = Vec::new();
        for (index, msg) in msgs.into_iter().enumerate() {
            recording
                .push(serde_json::to_string(&msg).unwrap_or_else(|error| {
                    panic!("{name}[{index}] failed to serialize: {error}")
                }));
            update(&mut live, msg);

            let mut folded = Model::default();
            for (line_index, line) in recording.iter().enumerate() {
                let recorded: Msg = serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("{name}[{line_index}] failed to deserialize: {error}")
                });
                update(&mut folded, recorded);
            }
            assert_eq!(folded, live, "{name}: fold != live after Msg {index}");
            assert_eq!(
                serde_json::to_value(&folded).unwrap(),
                serde_json::to_value(&live).unwrap(),
                "{name}: serialized fold != live after Msg {index}"
            );

            // No public fold sequence may ever leave the Model structurally
            // incoherent — the same check the shell enforces at the fold
            // seam (panic in debug, report-once-per-kind in release).
            let violations = live.check_invariants();
            assert!(
                violations.is_empty(),
                "{name}: invariants violated after Msg {index}: {violations:?}"
            );
        }
    }
}

/// A recorder report is a self-contained replay bundle: replaying it twice
/// yields byte-identical Model JSON, and both equal the live Model. The
/// recorder capacity is deliberately tiny so eviction folds Msgs into the
/// checkpoint on the way.
#[test]
fn replaying_a_recorded_log_twice_yields_identical_models() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut live = Model::default();
    let mut recorder = Recorder::new(4, &live);

    for (_, msgs) in all_sequences() {
        for msg in msgs {
            recorder.record(&msg);
            update(&mut live, msg);
        }
    }
    let report = ReportWriter::new(dir.path().to_path_buf(), BUILD, "test")
        .write(
            ReportDraft {
                kind: ReportKind::Bug,
                detail: None,
                note: String::new(),
                marks: Vec::new(),
                viewport: None,
                replay: ReplayVerdict::Unchecked,
            },
            ReportParts {
                frame: None,
                trace: None,
                msgs: Some(recorder.snapshot()),
                daemon: None,
                log: None,
                absent_reason: "test".to_string(),
            },
        )
        .expect("report written");
    let path = report.join("msgs.jsonl");

    let first = replay_msgs(&path).expect("first replay");
    let second = replay_msgs(&path).expect("second replay");
    let first_json = serde_json::to_value(&first).unwrap();
    assert_eq!(first_json, serde_json::to_value(&second).unwrap());
    assert_eq!(first_json, serde_json::to_value(&live).unwrap());
    assert_eq!(first, live);
}

/// Every Msg variant (and every nested enum arm the kernel emits) survives a
/// serde round trip unchanged — the recording IS the input stream.
#[test]
fn every_msg_variant_round_trips_through_serde() {
    let mut variants: Vec<Msg> = vec![
        command(op(1), create_cmd("claude-4", Some("nova"))),
        command(op(2), rename_cmd("claude-4", "renamed")),
        command(op(3), delete_cmd("claude-4")),
        command(
            op(6),
            amux_ui::Command::Claude(amux_ui::ClaudeCommand::SendPrompt {
                agent: agent_id("fix-auth-bug"),
                text: "fix the sync bug\nthen test".to_string(),
            }),
        ),
        command(
            op(7),
            amux_ui::Command::Claude(amux_ui::ClaudeCommand::AnswerAsk {
                agent: agent_id("fix-auth-bug"),
                ask: 0,
                answer: AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some("not that file".to_string()),
                }),
            }),
        ),
        command(
            op(8),
            amux_ui::Command::Claude(amux_ui::ClaudeCommand::AnswerAsk {
                agent: agent_id("fix-auth-bug"),
                ask: 1,
                answer: AskAnswer::Question(QuestionResponse {
                    answers: vec![QuestionAnswer {
                        selected: vec![0, 2],
                        other: Some("a torque wrench".to_string()),
                    }],
                }),
            }),
        ),
        command(
            op(9),
            amux_ui::Command::Claude(amux_ui::ClaudeCommand::AnswerAsk {
                agent: agent_id("fix-auth-bug"),
                ask: 2,
                answer: AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "document VALUE too".to_string(),
                }),
            }),
        ),
        command(
            op(10),
            amux_ui::Command::Claude(amux_ui::ClaudeCommand::Interrupt {
                agent: agent_id("fix-auth-bug"),
            }),
        ),
        command(
            op(11),
            amux_ui::Command::Claude(amux_ui::ClaudeCommand::CyclePermissionMode {
                agent: agent_id("fix-auth-bug"),
            }),
        ),
        command(
            op(12),
            amux_ui::Command::Codex(amux_ui::CodexCommand::Prompt {
                agent: agent_id("fix-auth-bug"),
                text: "inspect the failure".to_string(),
            }),
        ),
        command(
            op(13),
            amux_ui::Command::Codex(amux_ui::CodexCommand::Steer {
                agent: agent_id("fix-auth-bug"),
                text: "also run tests".to_string(),
            }),
        ),
        command(
            op(14),
            amux_ui::Command::Codex(amux_ui::CodexCommand::Answer {
                agent: agent_id("fix-auth-bug"),
                request_id: serde_json::json!("req-1"),
                decision: amux_ui::CodexDecision::Decline,
            }),
        ),
        command(
            op(15),
            amux_ui::Command::Codex(amux_ui::CodexCommand::Interrupt {
                agent: agent_id("fix-auth-bug"),
            }),
        ),
        op_result(op(6), OpOutcome::InputSent),
        connected("nova"),
        disconnected(DisconnectReason::ServerShutdown {
            detail: "updating".to_string(),
        }),
        disconnected(DisconnectReason::TransportError {
            message: "connection reset".to_string(),
        }),
        disconnected(DisconnectReason::AuthenticationRequired),
        disconnected(DisconnectReason::SubscriptionRequired),
        disconnected(DisconnectReason::ApplicationShutdown),
        host_up(&a_host("nova")),
        host_up(&an_offline_host("hetzner")),
        Msg::Server(amux_ui::ServerMsg::HostRemoved {
            id: host_id("hetzner"),
        }),
        hosts_synced(),
        agent_up(&an_agent("fix-auth-bug", "nova")),
        agent_gone("fix-auth-bug"),
        agents_synced(),
        op_result(
            op(1),
            OpOutcome::AgentCreated {
                agent: an_agent("claude-4", "nova"),
            },
        ),
        op_result(
            op(2),
            OpOutcome::AgentRenamed {
                agent: an_agent("claude-4", "nova"),
            },
        ),
        op_result(op(3), OpOutcome::AgentDeleted),
        op_failed(op(4), "boom"),
        op_failed_auth(op(5)),
        op_failed_subscription(op(16)),
        stream("fix-auth-bug", StreamMsg::Opened { truncated: true }),
        stream(
            "fix-auth-bug",
            StreamMsg::Batch {
                at: t0_plus(1),
                entries: vec![StreamEntry {
                    seq: 1,
                    payload: serde_json::json!({"type": "hook.stop", "session_id": "s"}),
                }],
            },
        ),
        stream("fix-auth-bug", StreamMsg::ReplayComplete),
        Msg::UserAttached {
            agent: agent_id("fix-auth-bug"),
        },
        tick(60),
    ];
    for reason in [
        StreamCloseReason::AgentDeleted,
        StreamCloseReason::AgentExited { exit_code: Some(1) },
        StreamCloseReason::HostUnreachable,
        StreamCloseReason::InternalError {
            detail: "boom".to_string(),
        },
        StreamCloseReason::TransportError {
            message: "reset".to_string(),
        },
        StreamCloseReason::AuthenticationRequired,
        StreamCloseReason::SubscriptionRequired,
        StreamCloseReason::ClientClosed,
    ] {
        variants.push(stream("fix-auth-bug", StreamMsg::Closed { reason }));
    }

    for msg in variants {
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: Msg = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, msg, "round trip changed {json}");
    }
}

/// A reducer tripwire: entity events cannot arrive while disconnected (the
/// subscription that would carry them is down). The reducer requests a report
/// and refuses the write instead of corrupting the Model.
#[test]
fn impossible_input_requests_a_report() {
    let (model, effects) = fold_with_effects(vec![agent_up(&an_agent("ghost", "nova"))]);
    assert_eq!(model.agent_count(), 0, "the impossible write is refused");
    assert!(
        matches!(
            effects.as_slice(),
            [Effect::RequestDump {
                reason: DumpReason::Tripwire { .. }
            }]
        ),
        "expected a tripwire report request: {effects:?}"
    );
}
