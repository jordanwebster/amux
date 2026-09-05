//! A held draft is a local obligation. Both providers use the same queue;
//! only the already-tested native write path decides what crosses the wire.

use amux_ui::{
    Command, Draft, Effect, InputPayload, Model, Msg, OpOutcome, QueueCommand, QueueDelivery,
    StreamCloseReason, StreamMsg, update,
};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "queue-agent";

fn draft(text: &str) -> Draft {
    Draft {
        text: text.into(),
        attachments: vec![],
    }
}
fn hold(n: u8, text: &str) -> Msg {
    command(
        op(n),
        Command::Queue(QueueCommand::Hold {
            agent: agent_id(AGENT),
            draft: draft(text),
        }),
    )
}
fn cancel(n: u8) -> Msg {
    command(
        op(n),
        Command::Queue(QueueCommand::Cancel {
            agent: agent_id(AGENT),
        }),
    )
}
fn replace(n: u8, text: &str) -> Msg {
    command(
        op(n),
        Command::Queue(QueueCommand::Replace {
            agent: agent_id(AGENT),
            draft: draft(text),
        }),
    )
}

fn rows(codex: bool) -> Vec<Value> {
    if codex {
        vec![
            json!({"type":"amux.codex_ready"}),
            json!({"type":"turn/started","turn":{"id":"turn-one","status":"inProgress"}}),
        ]
    } else {
        vec![
            json!({"type":"amux.transcript_ready"}),
            json!({"type":"user", "uuid":"00000000-0000-0000-0000-000000000001", "origin":{"kind":"human"}, "timestamp":t0(), "message":{"role":"user","content":"work"}}),
        ]
    }
}
fn end(codex: bool) -> Value {
    if codex {
        json!({"type":"turn/completed", "turn":{"id":"turn-one","status":"completed"}})
    } else {
        json!({"type":"system", "subtype":"turn_duration", "durationMs":1000})
    }
}
fn working(codex: bool) -> Vec<Msg> {
    seq([
        if codex {
            codex_base(AGENT)
        } else {
            chat_base(AGENT)
        },
        vec![tick(10), batch(AGENT, 10, rows(codex))],
    ])
}
fn sends(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|e| matches!(e, Effect::SendInput { .. } | Effect::PutThenSend { .. }))
        .count()
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    [false, true]
        .into_iter()
        .map(|codex| {
            (
                if codex {
                    "queue Codex delivery"
                } else {
                    "queue Claude delivery"
                },
                seq([
                    working(codex),
                    vec![
                        hold(1, "next"),
                        replace(2, "revised"),
                        batch(AGENT, 20, vec![end(codex)]),
                        op_result(op(2), OpOutcome::InputSent),
                    ],
                ]),
            )
        })
        .collect()
}

/// Hold does not inject input. A newer turn end delivers exactly once,
/// even if the same end arrives again, and acknowledgement clears the row.
#[test]
fn queue_hold_delivers_one_native_input_at_turn_end() {
    for codex in [false, true] {
        let mut model = fold(working(codex));
        assert!(
            amux_ui::queue::can_hold(&model, agent_id(AGENT)),
            "{model:?}"
        );
        assert_eq!(sends(&update(&mut model, hold(1, "next"))), 0);
        assert_eq!(model.queued(agent_id(AGENT)).unwrap().held_at, t0_plus(10));
        let effects = update(&mut model, batch(AGENT, 20, vec![end(codex)]));
        assert_eq!(sends(&effects), 1, "{codex}: {model:?}");
        match &effects[0] {
            Effect::SendInput {
                op: sent,
                payload: InputPayload::Claude { intent, .. },
                ..
            } => {
                assert_eq!(*sent, op(1));
                assert_eq!(
                    intent,
                    &amux_ui::claude_io::Intent::Prompt {
                        text: "next".into()
                    }
                );
            }
            Effect::SendInput {
                op: sent,
                payload: InputPayload::Codex { payload },
                ..
            } => {
                assert_eq!(*sent, op(1));
                let amux_ui::CodexInput::UserTurn { input } = payload else {
                    panic!("new user turn");
                };
                assert_eq!(
                    serde_json::from_slice::<Value>(input).unwrap(),
                    json!([{"type":"text", "text":"next"}])
                );
            }
            other => panic!("native input: {other:?}"),
        }
        assert_eq!(
            sends(&update(&mut model, batch(AGENT, 20, vec![end(codex)]))),
            0
        );
        update(&mut model, op_result(op(1), OpOutcome::InputSent));
        assert!(model.queued(agent_id(AGENT)).is_none());
        assert_eq!(
            sends(&update(&mut model, batch(AGENT, 30, vec![end(codex)]))),
            0
        );
    }
}

/// Replace swaps the one held message. Cancel returns its exact draft,
/// and neither the old nor the cancelled words can leak at turn end.
#[test]
fn queue_replace_and_cancel_return_the_latest_draft() {
    for codex in [false, true] {
        let mut model = fold(seq([
            working(codex),
            vec![hold(1, "old"), hold(2, "must refuse")],
        ]));
        assert!(model.finished_op(op(2)).unwrap().outcome.is_error());
        update(&mut model, replace(3, "new"));
        assert_eq!(model.queued(agent_id(AGENT)).unwrap().draft.text, "new");
        assert_eq!(
            model.finished_op(op(1)).unwrap().outcome,
            OpOutcome::QueueRemoved
        );
        update(&mut model, cancel(4));
        assert_eq!(
            model.finished_op(op(4)).unwrap().outcome,
            OpOutcome::QueueCancelled {
                draft: draft("new")
            }
        );
        assert_eq!(
            sends(&update(&mut model, batch(AGENT, 20, vec![end(codex)]))),
            0
        );
    }
}

/// Interrupt is its own input and does not clear or deliver the queue.
/// Only the subsequent provider turn-end fact releases the held words.
#[test]
fn queue_interrupt_keeps_the_held_message() {
    for codex in [false, true] {
        let mut model = fold(seq([working(codex), vec![hold(1, "after interrupt")]]));
        let input = if codex {
            Command::Codex(amux_ui::CodexCommand::Interrupt {
                agent: agent_id(AGENT),
            })
        } else {
            Command::Claude(amux_ui::ClaudeCommand::Interrupt {
                agent: agent_id(AGENT),
            })
        };
        assert_eq!(sends(&update(&mut model, command(op(2), input))), 1);
        assert!(matches!(
            model.queued(agent_id(AGENT)).unwrap().delivery,
            QueueDelivery::Held
        ));
        update(&mut model, op_result(op(2), OpOutcome::InputSent));
        if codex {
            update(
                &mut model,
                batch(
                    AGENT,
                    15,
                    vec![
                        json!({"type":"amux.input_result", "input_id":op(2).0.as_bytes(), "ok":{}}),
                    ],
                ),
            );
        }
        assert_eq!(
            sends(&update(&mut model, batch(AGENT, 20, vec![end(codex)]))),
            1
        );
    }
}

fn closed() -> Msg {
    stream(
        AGENT,
        StreamMsg::Closed {
            reason: StreamCloseReason::HostUnreachable,
        },
    )
}
fn reopen(codex: bool, ended: bool) -> Vec<Msg> {
    let mut replay = rows(codex);
    if ended {
        replay.push(end(codex));
    }
    vec![
        stream(AGENT, StreamMsg::Opened { truncated: false }),
        batch(AGENT, 10, replay),
        stream(AGENT, StreamMsg::ReplayComplete),
    ]
}

/// Reconnect refolds retained history. Replaying the still-running turn
/// does not deliver; a turn that ended while disconnected delivers once
/// after catch-up. Repeated ticks are never a retry mechanism.
#[test]
fn queue_disconnect_keeps_draft_and_waits_for_live_turn_end() {
    for codex in [false, true] {
        let mut model = fold(seq([
            working(codex),
            vec![hold(1, "after reconnect"), closed()],
        ]));
        assert!(model.queued(agent_id(AGENT)).is_some());
        for msg in reopen(codex, false) {
            assert_eq!(sends(&update(&mut model, msg)), 0);
        }
        update(&mut model, closed());
        let effects: Vec<_> = reopen(codex, true)
            .into_iter()
            .flat_map(|msg| update(&mut model, msg))
            .collect();
        assert_eq!(sends(&effects), 1, "{codex}: {model:?}");
        assert_eq!(sends(&update(&mut model, tick(50))), 0);
    }
}

/// A failed delivery retains the full draft and removes native optimism.
/// Retry waits for a fresh stream, then traverses the same native send gate.
#[test]
fn queue_failed_delivery_retries_once_on_reconnect() {
    for codex in [false, true] {
        let mut model = fold(seq([
            working(codex),
            vec![
                hold(1, "retry me"),
                batch(AGENT, 20, vec![end(codex)]),
                op_failed(op(1), "host unreachable"),
            ],
        ]));
        assert!(matches!(
            model.queued(agent_id(AGENT)).unwrap().delivery,
            QueueDelivery::Failed { .. }
        ));
        assert_eq!(sends(&update(&mut model, tick(30))), 0);
        update(&mut model, closed());
        let effects: Vec<_> = reopen(codex, true)
            .into_iter()
            .flat_map(|msg| update(&mut model, msg))
            .collect();
        assert_eq!(sends(&effects), 1);
        update(&mut model, op_result(op(1), OpOutcome::InputSent));
        assert!(model.queued(agent_id(AGENT)).is_none());
    }
}

/// An in-flight send cannot be replaced or cancelled: accepting either
/// would claim to withdraw input that may already be at the host.
#[test]
fn queue_sending_refuses_replace_and_cancel() {
    for codex in [false, true] {
        let model = fold(seq([
            working(codex),
            vec![
                hold(1, "sending"),
                batch(AGENT, 20, vec![end(codex)]),
                replace(2, "too late"),
                cancel(3),
            ],
        ]));
        assert!(model.finished_op(op(2)).unwrap().outcome.is_error());
        assert!(model.finished_op(op(3)).unwrap().outcome.is_error());
        assert_eq!(model.queued(agent_id(AGENT)).unwrap().draft.text, "sending");
    }
}

/// Held payloads are part of the recorded model, with attachment metadata kept separate from live binary payloads.
/// A wire-free replay reproduces their lifecycle and effects exactly.
#[test]
fn queue_recording_matches_live_at_every_message() {
    for (_, msgs) in sequences() {
        let mut live = Model::default();
        let mut replay = Model::default();
        for msg in msgs {
            let recorded = serde_json::to_string(&msg).unwrap();
            assert_eq!(
                update(&mut live, msg),
                update(&mut replay, serde_json::from_str(&recorded).unwrap())
            );
            assert_eq!(live, replay);
            assert!(live.check_invariants().is_empty());
        }
    }
}

/// Local queue edits remain possible with the entire daemon disconnected.
/// An exited or removed agent cannot receive held input.
#[test]
fn queue_offline_cancel_and_agent_removal_never_send() {
    for codex in [false, true] {
        let mut model = fold(seq([
            working(codex),
            vec![
                hold(1, "keep me"),
                disconnected(amux_ui::DisconnectReason::ApplicationShutdown),
            ],
        ]));
        assert_eq!(sends(&update(&mut model, replace(2, "offline edit"))), 0);
        update(&mut model, cancel(3));
        assert_eq!(
            model.finished_op(op(3)).unwrap().outcome,
            OpOutcome::QueueCancelled {
                draft: draft("offline edit")
            }
        );

        let mut model = fold(seq([
            working(codex),
            vec![hold(1, "never deliver"), agent_gone(AGENT)],
        ]));
        assert!(model.queued(agent_id(AGENT)).is_none());
        assert_eq!(
            sends(&update(&mut model, batch(AGENT, 20, vec![end(codex)]))),
            0
        );
    }
}

/// A previous turn's completion is older than a new hold even when it
/// arrives again during reconnect. The current turn still has to end.
#[test]
fn queue_replayed_old_end_does_not_release_a_new_hold() {
    for codex in [false, true] {
        let mut msgs = working(codex);
        msgs.push(batch(AGENT, 20, vec![end(codex)]));
        let mut next = rows(codex);
        if codex {
            next[1]["turn"]["id"] = json!("turn-two");
        } else {
            next[1]["uuid"] = json!("00000000-0000-0000-0000-000000000002");
        }
        msgs.extend([
            batch(AGENT, 30, next.clone()),
            hold(1, "after second turn"),
            closed(),
        ]);
        let mut model = fold(msgs);
        for msg in [
            stream(AGENT, StreamMsg::Opened { truncated: false }),
            batch(AGENT, 10, rows(codex)),
            batch(AGENT, 20, vec![end(codex)]),
            batch(AGENT, 30, next),
            stream(AGENT, StreamMsg::ReplayComplete),
        ] {
            assert_eq!(sends(&update(&mut model, msg)), 0);
        }
        let mut ending = end(codex);
        if codex {
            ending["turn"]["id"] = json!("turn-two");
        }
        assert_eq!(
            sends(&update(&mut model, batch(AGENT, 40, vec![ending]))),
            1
        );
    }
}

/// Attachments survive queue edits as canonical elements and metadata;
/// live-only bytes are retained by the runtime, never by the recorded model.
#[test]
fn queue_attachments_replay_and_cancel_losslessly() {
    let attachment = amux_ui::DraftAttachment::from_bytes(
        amux_ui::ArtifactKind::File,
        "notes.txt",
        "text/plain",
        b"private bytes".to_vec(),
    );
    let text = amux_ui::format_mention(&amux_ui::Mention {
        kind: amux_ui::MentionKind::File {
            id: attachment.id.clone(),
        },
        name: attachment.name.clone(),
        size: Some(attachment.size),
        path: None,
    });
    let original = Draft {
        text,
        attachments: vec![attachment],
    };
    let msg = command(
        op(1),
        Command::Queue(QueueCommand::Hold {
            agent: agent_id(AGENT),
            draft: original.clone(),
        }),
    );
    let mut live = fold(working(false));
    let mut replay = live.clone();
    let recorded = serde_json::to_string(&msg).unwrap();
    update(&mut live, msg);
    update(&mut replay, serde_json::from_str(&recorded).unwrap());
    assert_eq!(live, replay);
    assert!(
        live.queued(agent_id(AGENT)).unwrap().draft.attachments[0]
            .bytes
            .is_none()
    );
    update(&mut live, cancel(2));
    let mut metadata = original;
    metadata.attachments[0].bytes = None;
    assert_eq!(
        live.finished_op(op(2)).unwrap().outcome,
        OpOutcome::QueueCancelled { draft: metadata }
    );
}
