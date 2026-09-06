//! Shared writing actions consume the SDK reducer and unchanged recorded daemon rows.
use amux_ui::claude_sdk::{self, FeedEntryKind, SendGate};
use amux_ui::provider::{self, PermissionFacts, SettingsGate};
use amux_ui::{
    ClaudeSdkCommand as SdkCommand, ClaudeSdkInput as SdkInput, Command, Draft, DraftSegment,
    Effect, InputPayload, Model, Msg, OpOutcome, QueueCommand, QueueDelivery, StreamCloseReason,
    StreamMsg, update,
};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "sdk-integration";
const STREAM: &str =
    include_str!("../../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl");
fn rows(raw: &str) -> Vec<Value> {
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn ready() -> Vec<Msg> {
    seq([
        claude_sdk_base(AGENT),
        vec![batch(AGENT, 1, rows(STREAM)[..2].to_vec())],
    ])
}
fn working() -> Vec<Msg> {
    seq([ready(), vec![batch(AGENT, 10, rows(STREAM)[2..4].to_vec())]])
}
fn hold(n: u8, text: &str) -> Msg {
    command(
        op(n),
        Command::Queue(QueueCommand::Hold {
            agent: agent_id(AGENT),
            draft: Draft::plain(text, vec![]),
        }),
    )
}
fn end() -> Value {
    rows(STREAM)
        .into_iter()
        .find(|row| row["type"] == "result")
        .unwrap()
}
fn input(effects: &[Effect]) -> &SdkInput {
    match effects {
        [
            Effect::SendInput {
                payload: InputPayload::ClaudeSdk { payload },
                ..
            },
        ] => payload,
        other => panic!("one SDK input expected: {other:?}"),
    }
}
fn capture(name: &str, model: &Model, effects: &[Effect]) {
    if let Some(path) = std::env::var_os("SDK_INTEGRATION_EVIDENCE") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        let layer = model.claude_sdk(agent_id(AGENT)).unwrap();
        let value = json!({"provider": provider::facts(model, agent_id(AGENT)),
            "gate": claude_sdk::send_gate(model, agent_id(AGENT)),
            "queue": model.queued(agent_id(AGENT)), "effects": effects,
            "entries": layer.entries().collect::<Vec<_>>()});
        std::fs::write(
            path.join(format!("{name}.json")),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}
fn prompt_sequence() -> Vec<Msg> {
    let mut msgs = claude_sdk_base(AGENT);
    for (i, row) in rows(STREAM).into_iter().enumerate() {
        if row["type"] == "user" && row["input_id"].is_string() {
            msgs.push(command(
                amux_ui::OpId(uuid::Uuid::parse_str(row["uuid"].as_str().unwrap()).unwrap()),
                Command::Send {
                    agent: agent_id(AGENT),
                    draft: Draft::plain(row["message"]["content"].as_str().unwrap(), vec![]),
                },
            ));
        }
        msgs.push(batch(AGENT, i as i64 + 1, vec![row]));
    }
    msgs
}
pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        ("SDK shared prompt recording", prompt_sequence()),
        (
            "SDK shared task-list fixtures",
            seq([
                ready(),
                vec![batch(
                    AGENT,
                    100,
                    rows(include_str!("../fixtures/todos/sdk-rows.jsonl")),
                )],
            ]),
        ),
        (
            "SDK held prompt delivery",
            seq([
                working(),
                vec![
                    hold(2, "next"),
                    batch(AGENT, 100, vec![end()]),
                    op_result(op(2), OpOutcome::InputSent),
                ],
            ]),
        ),
    ]
}
#[test]
fn sdk_integration_shared_prompt_round_trips_recorded_sdk_session() {
    let (model, effects) = fold_with_effects(prompt_sequence());
    let sends: Vec<_> = effects
        .into_iter()
        .filter(|e| matches!(e, Effect::SendInput { .. }))
        .collect();
    let expected = rows(STREAM)[2]["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(input(&sends), &SdkInput::Prompt { text: expected });
    let layer = model.claude_sdk(agent_id(AGENT)).unwrap();
    assert!(layer.pending_echo().is_none());
    assert!(layer.entries().any(
        |entry| matches!(&entry.kind, FeedEntryKind::Message(message) if message.text == "SEVEN")
    ));
    assert_eq!(
        claude_sdk::send_gate(&model, agent_id(AGENT)),
        SendGate::Ready
    );
    capture("recorded-prompt-and-reply", &model, &sends);
}
#[test]
fn sdk_integration_queue_replaces_cancels_and_delivers_once_at_sdk_turn_end() {
    let mut model = fold(working());
    assert!(update(&mut model, hold(2, "old")).is_empty());
    update(
        &mut model,
        command(
            op(3),
            Command::Queue(QueueCommand::Replace {
                agent: agent_id(AGENT),
                draft: Draft::plain("replacement", vec![]),
            }),
        ),
    );
    update(
        &mut model,
        command(
            op(4),
            Command::Queue(QueueCommand::Cancel {
                agent: agent_id(AGENT),
            }),
        ),
    );
    assert_eq!(
        model.finished_op(op(4)).unwrap().outcome,
        OpOutcome::QueueCancelled {
            draft: Draft::plain("replacement", vec![])
        }
    );
    update(&mut model, hold(5, "next"));
    let effects = update(&mut model, batch(AGENT, 100, vec![end()]));
    assert_eq!(
        input(&effects),
        &SdkInput::Prompt {
            text: "next".into()
        }
    );
    capture("queue-delivered-at-turn-end", &model, &effects);
    assert!(update(&mut model, batch(AGENT, 100, vec![end()])).is_empty());
    update(&mut model, op_result(op(5), OpOutcome::InputSent));
    assert!(model.queued(agent_id(AGENT)).is_none());
}
fn closed() -> Msg {
    stream(
        AGENT,
        StreamMsg::Closed {
            reason: StreamCloseReason::HostUnreachable,
        },
    )
}
fn reopen(ended: bool) -> Vec<Msg> {
    let mut msgs = vec![
        stream(AGENT, StreamMsg::Opened { truncated: false }),
        batch(AGENT, 1, rows(STREAM)[..4].to_vec()),
    ];
    if ended {
        msgs.push(batch(AGENT, 100, vec![end()]));
    }
    msgs.push(stream(AGENT, StreamMsg::ReplayComplete));
    msgs
}
#[test]
fn sdk_integration_queue_keeps_draft_on_disconnect_and_retries_failed_delivery() {
    for failed in [false, true] {
        let mut model = fold(working());
        update(&mut model, hold(2, "retry"));
        if failed {
            let effects = update(&mut model, batch(AGENT, 100, vec![end()]));
            assert_eq!(
                input(&effects),
                &SdkInput::Prompt {
                    text: "retry".into()
                }
            );
            update(&mut model, op_failed(op(2), "host unreachable"));
            assert!(
                model
                    .claude_sdk(agent_id(AGENT))
                    .unwrap()
                    .pending_echo()
                    .is_none()
            );
            assert!(matches!(
                model.queued(agent_id(AGENT)).unwrap().delivery,
                QueueDelivery::Failed { .. }
            ));
        }
        update(&mut model, closed());
        if !failed {
            for msg in reopen(false) {
                assert!(update(&mut model, msg).is_empty());
            }
            update(&mut model, closed());
        }
        let effects: Vec<_> = reopen(true)
            .into_iter()
            .flat_map(|msg| update(&mut model, msg))
            .collect();
        assert_eq!(
            input(&effects),
            &SdkInput::Prompt {
                text: "retry".into()
            }
        );
        assert!(update(&mut model, tick(100)).is_empty());
        update(&mut model, op_result(op(2), OpOutcome::InputSent));
        assert!(model.queued(agent_id(AGENT)).is_none());
    }
}
#[test]
fn sdk_integration_settings_use_sdk_inputs_and_authoritative_facts() {
    let agent = agent_id(AGENT);
    for (cmd, expected) in [
        (
            Command::SetModel {
                agent,
                model: "sonnet".into(),
            },
            SdkInput::SetModel {
                model: Some("sonnet".into()),
            },
        ),
        (
            Command::SetEffort {
                agent,
                effort: "high".into(),
            },
            SdkInput::SetEffort {
                effort: Some("high".into()),
            },
        ),
        (
            Command::ClaudeSdk(SdkCommand::SetEffort {
                agent,
                effort: None,
            }),
            SdkInput::SetEffort { effort: None },
        ),
        (
            Command::ClaudeSdk(SdkCommand::SetPermissionMode {
                agent,
                mode: "plan".into(),
            }),
            SdkInput::SetPermissionMode {
                mode: "plan".into(),
            },
        ),
    ] {
        for base in [ready(), working()] {
            let mut model = fold(base);
            let before = provider::facts(&model, agent);
            assert_eq!(provider::settings_gate(&model, agent), SettingsGate::Ready);
            let effects = update(&mut model, command(op(2), cmd.clone()));
            assert_eq!(input(&effects), &expected);
            assert_eq!(
                provider::facts(&model, agent),
                before,
                "an input is not a provider fact"
            );
            update(&mut model, op_failed(op(2), "refused"));
            assert_eq!(provider::facts(&model, agent), before);
            assert!(model.claude_sdk(agent).unwrap().in_flight_input().is_none());
        }
    }
    let mut model = fold(ready());
    let mut snapshot = rows(STREAM)[1].clone();
    snapshot["model"] = json!("sonnet");
    snapshot["effort"] = json!("high");
    snapshot["permission_mode"] = json!("plan");
    update(&mut model, batch(AGENT, 100, vec![snapshot]));
    let facts = provider::facts(&model, agent);
    assert_eq!(facts.model.as_deref(), Some("sonnet"));
    assert_eq!(facts.effort.as_deref(), Some("high"));
    assert_eq!(
        facts.permission,
        PermissionFacts::Claude {
            mode: Some("plan".into())
        }
    );
    assert!(!facts.commands.is_empty());
    capture("shared-session-settings", &model, &[]);
    assert!(
        update(
            &mut model,
            command(
                op(3),
                Command::SetEffort {
                    agent,
                    effort: "invented".into()
                }
            )
        )
        .is_empty()
    );
    assert!(model.finished_op(op(3)).unwrap().outcome.is_error());
    assert!(model.claude_sdk(agent).unwrap().in_flight_input().is_none());

    // The SDK's open permission enum preserves future provider modes. The
    // provider decides whether such a mode is available; effort is closed.
    let before = provider::facts(&model, agent);
    let effects = update(
        &mut model,
        command(
            op(4),
            Command::ClaudeSdk(SdkCommand::SetPermissionMode {
                agent,
                mode: "future-mode".into(),
            }),
        ),
    );
    assert_eq!(
        input(&effects),
        &SdkInput::SetPermissionMode {
            mode: "future-mode".into()
        }
    );
    update(&mut model, op_failed(op(4), "provider refused this mode"));
    assert_eq!(provider::facts(&model, agent), before);
    assert!(model.claude_sdk(agent).unwrap().in_flight_input().is_none());
}
#[test]
fn sdk_integration_command_tokens_use_the_published_sdk_prompt_route() {
    let raw = include_str!("../../../amux/tests/fixtures/rows/claude-sdk/introspection.rows.jsonl");
    let base = seq([claude_sdk_base(AGENT), vec![batch(AGENT, 1, rows(raw))]]);
    for (name, allowed) in [("compact", true), ("doctor", false), ("invented", false)] {
        let mut model = fold(base.clone());
        let effects = update(
            &mut model,
            command(
                op(2),
                Command::Send {
                    agent: agent_id(AGENT),
                    draft: Draft {
                        segments: vec![
                            DraftSegment::CommandToken { name: name.into() },
                            DraftSegment::Text {
                                text: " keep decisions".into(),
                            },
                        ],
                        attachments: vec![],
                    },
                },
            ),
        );
        if allowed {
            assert_eq!(
                input(&effects),
                &SdkInput::Prompt {
                    text: "/compact keep decisions".into()
                }
            );
            capture("selected-command-input", &model, &effects);
        } else {
            assert!(effects.is_empty());
            assert!(model.finished_op(op(2)).unwrap().outcome.is_error());
        }
    }
}
#[test]
fn sdk_integration_todos_share_native_blocks_and_replay_without_feed_rows() {
    let fixtures = rows(include_str!("../fixtures/todos/sdk-rows.jsonl"));
    let mut sdk = fold(ready());
    let mut pty = fold(chat_base(AGENT));
    let count = sdk.claude_sdk(agent_id(AGENT)).unwrap().entry_count();
    for (i, row) in fixtures.iter().enumerate() {
        let msg = batch(AGENT, 100 + i as i64, vec![row.clone()]);
        update(&mut sdk, msg.clone());
        update(&mut pty, msg);
        assert_eq!(
            provider::facts(&sdk, agent_id(AGENT)).todos,
            provider::facts(&pty, agent_id(AGENT)).todos
        );
        assert_eq!(
            sdk.claude_sdk(agent_id(AGENT)).unwrap().entry_count(),
            count
        );
        let checkpoint: Model =
            serde_json::from_value(serde_json::to_value(&sdk).unwrap()).unwrap();
        assert_eq!(checkpoint, sdk);
    }
    capture("confirmed-task-list", &sdk, &[]);
    update(&mut sdk, batch(AGENT, 200, fixtures.clone()));
    assert_eq!(
        provider::facts(&sdk, agent_id(AGENT)).todos.unwrap().total,
        0,
        "old blocks cannot restore an older list"
    );
    for reset in [
        json!({"type":"conversation_reset"}),
        rows(STREAM)[0].clone(),
    ] {
        let mut model = fold(ready());
        update(&mut model, batch(AGENT, 100, fixtures[..2].to_vec()));
        update(&mut model, batch(AGENT, 200, vec![reset]));
        assert!(provider::facts(&model, agent_id(AGENT)).todos.is_none());
    }
}
#[test]
fn sdk_integration_todo_failure_is_named_and_child_lists_do_not_replace_parent() {
    let fixtures = rows(include_str!("../fixtures/todos/sdk-rows.jsonl"));
    let mut model = fold(ready());
    update(&mut model, batch(AGENT, 100, fixtures[..2].to_vec()));
    let before = provider::facts(&model, agent_id(AGENT)).todos;
    let mut child = fixtures[2..4].to_vec();
    for row in &mut child {
        row["parent_tool_use_id"] = json!("child");
    }
    update(&mut model, batch(AGENT, 110, child));
    assert_eq!(provider::facts(&model, agent_id(AGENT)).todos, before);
    let mut failed = fixtures[4..6].to_vec();
    failed[1]["message"]["content"][0]["is_error"] = json!(true);
    update(&mut model, batch(AGENT, 120, failed));
    assert_eq!(provider::facts(&model, agent_id(AGENT)).todos, before);
    assert!(model.claude_sdk(agent_id(AGENT)).unwrap().entries().any(|entry| matches!(&entry.kind,
        FeedEntryKind::Tool(tool) if tool.name == "TodoWrite" && tool.result.as_ref().is_some_and(|result| result.is_error))));
    capture("failed-task-list-write", &model, &[]);
}

#[test]
fn sdk_integration_queue_interrupt_and_replayed_old_end_cannot_deliver_early() {
    let mut model = fold(working());
    update(&mut model, hold(2, "after interrupt"));
    let effects = update(
        &mut model,
        command(
            op(3),
            Command::ClaudeSdk(SdkCommand::Interrupt {
                agent: agent_id(AGENT),
            }),
        ),
    );
    assert_eq!(input(&effects), &SdkInput::Interrupt);
    assert!(matches!(
        model.queued(agent_id(AGENT)).unwrap().delivery,
        QueueDelivery::Held
    ));
    update(&mut model, op_result(op(3), OpOutcome::InputSent));
    update(
        &mut model,
        batch(
            AGENT,
            50,
            vec![
                json!({"type":"amux.claude_sdk.input_result", "input_id":op(3).0.as_bytes(), "outcome":"ok"}),
            ],
        ),
    );
    let effects = update(&mut model, batch(AGENT, 100, vec![end()]));
    assert_eq!(
        input(&effects),
        &SdkInput::Prompt {
            text: "after interrupt".into()
        }
    );

    let mut model = fold(working());
    update(&mut model, batch(AGENT, 100, vec![end()]));
    let mut next = rows(STREAM)[2].clone();
    next["uuid"] = json!(uuid::Uuid::from_u128(123));
    update(&mut model, batch(AGENT, 200, vec![next.clone()]));
    update(&mut model, hold(2, "after second turn"));
    update(&mut model, closed());
    for msg in [
        stream(AGENT, StreamMsg::Opened { truncated: false }),
        batch(AGENT, 1, rows(STREAM)[..4].to_vec()),
        batch(AGENT, 100, vec![end()]),
        batch(AGENT, 200, vec![next]),
        stream(AGENT, StreamMsg::ReplayComplete),
    ] {
        assert!(update(&mut model, msg).is_empty());
    }
    let effects = update(&mut model, batch(AGENT, 300, vec![end()]));
    assert_eq!(
        input(&effects),
        &SdkInput::Prompt {
            text: "after second turn".into()
        }
    );
}

#[test]
fn sdk_integration_replay_and_input_in_flight_refuse_settings_and_drafts() {
    for replay in [false, true] {
        let mut model = fold(ready());
        if replay {
            update(
                &mut model,
                stream(AGENT, StreamMsg::Opened { truncated: false }),
            );
        } else {
            update(
                &mut model,
                command(
                    op(1),
                    Command::Send {
                        agent: agent_id(AGENT),
                        draft: Draft::plain("pending", vec![]),
                    },
                ),
            );
        }
        for (n, cmd) in [
            (
                2,
                Command::SetModel {
                    agent: agent_id(AGENT),
                    model: "sonnet".into(),
                },
            ),
            (
                3,
                Command::SetEffort {
                    agent: agent_id(AGENT),
                    effort: "high".into(),
                },
            ),
            (
                4,
                Command::ClaudeSdk(SdkCommand::SetPermissionMode {
                    agent: agent_id(AGENT),
                    mode: "plan".into(),
                }),
            ),
            (
                5,
                Command::Send {
                    agent: agent_id(AGENT),
                    draft: Draft::plain("refused", vec![]),
                },
            ),
        ] {
            assert!(update(&mut model, command(op(n), cmd)).is_empty());
            assert!(model.finished_op(op(n)).unwrap().outcome.is_error());
        }
    }
}

#[test]
fn sdk_integration_queued_command_preserves_multiline_arguments() {
    let mut model = fold(working());
    let draft = Draft {
        segments: vec![
            DraftSegment::CommandToken {
                name: "compact".into(),
            },
            DraftSegment::Text {
                text: "\nkeep decisions\nand pending work".into(),
            },
        ],
        attachments: vec![],
    };
    assert!(
        update(
            &mut model,
            command(
                op(2),
                Command::Queue(QueueCommand::Hold {
                    agent: agent_id(AGENT),
                    draft: draft.clone()
                })
            )
        )
        .is_empty()
    );
    assert_eq!(model.queued(agent_id(AGENT)).unwrap().draft, draft);
    let effects = update(&mut model, batch(AGENT, 100, vec![end()]));
    assert_eq!(
        input(&effects),
        &SdkInput::Prompt {
            text: "/compact\nkeep decisions\nand pending work".into()
        }
    );
}

#[test]
fn sdk_integration_streamed_todo_final_removes_provisional_tool_without_losing_sibling() {
    let mut model = fold(ready());
    let pair = rows(include_str!("../fixtures/todos/sdk-rows.jsonl"));
    let block = pair[0]["message"]["content"][0].clone();
    let id = pair[0]["message"]["id"].clone();
    for (i, event) in [
        json!({"type":"message_start", "message":{"id":id}}),
        json!({"type":"content_block_start", "index":0, "content_block":{"type":"tool_use","id":block["id"],"name":"TodoWrite","input":{}}}),
        json!({"type":"content_block_delta", "index":0, "delta":{"type":"input_json_delta","partial_json":block["input"].to_string()}}),
        json!({"type":"content_block_stop", "index":0}),
    ].into_iter().enumerate() {
        update(&mut model, batch(AGENT, 10 + i as i64, vec![json!({"type":"stream_event","parent_tool_use_id":null,"event":event})]));
    }
    update(&mut model, batch(AGENT, 20, pair[..2].to_vec()));
    let mut sibling = pair[0].clone();
    sibling["uuid"] = json!(uuid::Uuid::from_u128(111));
    sibling["message"]["content"] = json!([{"type":"text","text":"The task list is current."}]);
    update(&mut model, batch(AGENT, 21, vec![sibling]));
    let layer = model.claude_sdk(agent_id(AGENT)).unwrap();
    assert!(layer.entries().all(
        |entry| !matches!(&entry.kind, FeedEntryKind::Tool(tool) if tool.name == "TodoWrite")
    ));
    assert!(layer.entries().any(|entry| matches!(&entry.kind, FeedEntryKind::Message(message) if message.text == "The task list is current.")));
    assert_eq!(
        provider::facts(&model, agent_id(AGENT))
            .todos
            .unwrap()
            .total,
        3
    );
}
