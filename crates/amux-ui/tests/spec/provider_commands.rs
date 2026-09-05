//! Provider-reported commands remain typed from the draft through delivery.
use amux_ui::codex::CodexInput;
use amux_ui::provider::facts;
use amux_ui::{
    Command, Draft, DraftSegment, Effect, InputPayload, Model, Msg, OpOutcome, QueueCommand,
    StreamMsg, update,
};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "provider-commands";
fn rows() -> Vec<Value> {
    include_str!("../../../amux/tests/fixtures/provider-commands/rows.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn ready() -> Vec<Msg> {
    seq([codex_base(AGENT), vec![batch(AGENT, 10, rows())]])
}
fn draft(name: &str) -> Draft {
    Draft {
        segments: vec![
            DraftSegment::CommandToken { name: name.into() },
            DraftSegment::Text {
                text: " check the changes\nwith tests".into(),
            },
        ],
        attachments: vec![],
    }
}
fn send(name: &str) -> Command {
    Command::Send {
        agent: agent_id(AGENT),
        draft: draft(name),
    }
}
pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        (
            "reported Codex command token",
            seq([ready(), vec![command(op(1), send("review"))]]),
        ),
        (
            "PTY commands unavailable",
            seq([chat_base(AGENT), vec![command(op(1), send("review"))]]),
        ),
    ]
}
fn assert_command(effects: &[Effect]) {
    let [
        Effect::SendInput {
            payload:
                InputPayload::Codex {
                    payload: CodexInput::Command { name, args },
                },
            ..
        },
    ] = effects
    else {
        panic!("typed command delivery: {effects:?}");
    };
    assert_eq!(name, "review");
    assert_eq!(args, " check the changes\nwith tests");
}
#[test]
fn provider_commands_recorded_facts_and_typed_input_match_live() {
    let mut live = Model::default();
    let mut replay = Model::default();
    for msg in seq([ready(), vec![command(op(1), send("review"))]]) {
        let recorded = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            update(&mut live, msg),
            update(&mut replay, serde_json::from_str(&recorded).unwrap())
        );
        assert_eq!(live, replay);
        assert!(live.check_invariants().is_empty());
    }
    let result = facts(&live, agent_id(AGENT));
    assert_eq!(result.commands.len(), 1);
    assert_eq!(result.commands[0].name, "review");
    assert!(!result.commands[0].terminal_only);
    let mut model = fold(ready());
    assert_command(&update(&mut model, command(op(1), send("review"))));
    assert!(
        update(&mut model, command(op(2), send("review"))).is_empty(),
        "in-flight command refuses a second send"
    );
    update(
        &mut model,
        op_result(
            op(1),
            OpOutcome::Error {
                error: amux_ui::OpError::general("transport failed"),
            },
        ),
    );
    assert_command(&update(&mut model, command(op(3), send("review"))));
    println!(
        "Shared provider facts: {}\nDraft: {}",
        serde_json::to_string_pretty(&result).unwrap(),
        serde_json::to_string_pretty(&draft("review")).unwrap()
    );
}
#[test]
fn provider_commands_pty_list_is_empty_and_tokens_refuse() {
    let mut model = fold(chat_base(AGENT));
    assert!(facts(&model, agent_id(AGENT)).commands.is_empty());
    assert!(update(&mut model, command(op(1), send("review"))).is_empty());
    assert!(matches!(
        model.finished_op(op(1)).unwrap().outcome,
        OpOutcome::Error { .. }
    ));
    println!("Claude PTY commands: []; command token refused without input.");
}
#[test]
fn provider_commands_terminal_only_unknown_malformed_and_stale_tokens_never_send() {
    for name in ["terminal-only", "unreported"] {
        let mut row = rows()[0].clone();
        row["session"]["commands"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"terminal-only","source":"codex","terminal_only":true}));
        let mut model = fold(seq([codex_base(AGENT), vec![batch(AGENT, 10, vec![row])]]));
        assert!(
            facts(&model, agent_id(AGENT))
                .commands
                .iter()
                .any(|c| c.terminal_only)
        );
        assert!(update(&mut model, command(op(1), send(name))).is_empty());
    }
    for segments in [
        vec![
            DraftSegment::Text {
                text: "before".into(),
            },
            DraftSegment::CommandToken {
                name: "review".into(),
            },
        ],
        vec![
            DraftSegment::CommandToken {
                name: "review".into(),
            },
            DraftSegment::CommandToken {
                name: "review".into(),
            },
        ],
        vec![DraftSegment::CommandToken {
            name: String::new(),
        }],
    ] {
        let mut model = fold(ready());
        assert!(
            update(
                &mut model,
                command(
                    op(1),
                    Command::Send {
                        agent: agent_id(AGENT),
                        draft: Draft {
                            segments,
                            attachments: vec![]
                        }
                    }
                )
            )
            .is_empty()
        );
    }
    for stale in [
        stream(AGENT, StreamMsg::Opened { truncated: false }),
        batch(
            AGENT,
            20,
            vec![json!({"type":"amux.codex_gap","reason":"connection_lost"})],
        ),
    ] {
        let mut model = fold(ready());
        update(&mut model, stale);
        assert!(update(&mut model, command(op(1), send("review"))).is_empty());
    }
}
#[test]
fn provider_commands_queue_preserves_token_on_cancel_and_delivers_once_at_turn_end() {
    let mut model = fold(seq([
        ready(),
        vec![batch(
            AGENT,
            20,
            vec![json!({"type":"turn/started","turn":{"id":"work"}})],
        )],
    ]));
    for op_id in [1, 3] {
        assert!(
            update(
                &mut model,
                command(
                    op(op_id),
                    Command::Queue(QueueCommand::Hold {
                        agent: agent_id(AGENT),
                        draft: draft("review")
                    })
                )
            )
            .is_empty()
        );
        if op_id == 1 {
            update(
                &mut model,
                command(
                    op(2),
                    Command::Queue(QueueCommand::Cancel {
                        agent: agent_id(AGENT),
                    }),
                ),
            );
            assert_eq!(
                model.finished_op(op(2)).unwrap().outcome,
                OpOutcome::QueueCancelled {
                    draft: draft("review")
                }
            );
        }
    }
    let end = batch(
        AGENT,
        30,
        vec![json!({"type":"turn/completed","turn":{"id":"work","status":"completed"}})],
    );
    assert_command(&update(&mut model, end.clone()));
    assert!(update(&mut model, end).is_empty());
    update(
        &mut model,
        batch(
            AGENT,
            40,
            vec![json!({"type":"amux.input_result","input_id":op(3).0.as_bytes(),"ok":{}})],
        ),
    );
    // The RPC acknowledgement resolves the queued operation; the provider row
    // separately releases the native input-in-flight gate.
    update(&mut model, op_result(op(3), OpOutcome::InputSent));
    assert!(model.queued(agent_id(AGENT)).is_none());
}
