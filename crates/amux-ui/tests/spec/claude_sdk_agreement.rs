//! SDK phase, fleet attention and send gate agree after every reducer message.
//! Recorded requests exercise the daemon boundary; structural examples cover
//! concurrent channels, opaque dialogs and unsupported form schemas.

use amux_ui::claude_sdk::{
    self, AskKind, AskWhy, ElicitationFieldKind, ElicitationForm, FEED_RETAINED, SdkPhase, SendGate,
};
use amux_ui::{Attention, Model, Msg, StreamCloseReason, StreamMsg, Why, update};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "sdk-agreement";

fn ready() -> Value {
    json!({"type":"amux.claude_sdk.ready","session_id":"s","resumed":false})
}
fn prompt() -> Value {
    json!({"type":"user","uuid":"prompt","message":{"content":"hello"}})
}
fn result() -> Value {
    json!({"type":"result","subtype":"success","is_error":false})
}
fn permission(id: &str, tool: &str, input: Value) -> Value {
    json!({"type":"amux.claude_sdk.permission_required","request_id":id,"tool_name":tool,"input":input,"suggestions":[]})
}
fn elicitation(schema: Value) -> Value {
    json!({"type":"amux.claude_sdk.elicitation_required","request_id":"e","server":"external","message":"Confirm","schema":schema})
}
fn dialog() -> Value {
    json!({"type":"amux.claude_sdk.dialog_required","request_id":"d","dialog_kind":"future","payload":{"message":"Choose","nested":{"opaque":[1,2]}}})
}
fn asks() -> Vec<Value> {
    vec![
        permission(
            "p",
            "Write",
            json!({"file_path":"hello.txt","content":"hello"}),
        ),
        permission(
            "plan",
            "ExitPlanMode",
            json!({"plan":"Write a greeting","planFilePath":"plan.md"}),
        ),
        permission(
            "q",
            "AskUserQuestion",
            json!({"questions":[{"header":"Color","question":"Which color?","options":[{"label":"Blue","description":"cool"}],"multiSelect":true}]}),
        ),
        elicitation(
            json!({"type":"object","properties":{"confirmed":{"type":"string"}},"required":["confirmed"]}),
        ),
        dialog(),
    ]
}
fn sequence(rows: Vec<Value>) -> Vec<Msg> {
    seq([
        claude_sdk_base(AGENT),
        rows.into_iter()
            .enumerate()
            .map(|(i, r)| batch(AGENT, i as i64, vec![r]))
            .collect(),
    ])
}
fn recorded(name: &str) -> Vec<Value> {
    let raw = match name {
        "permission" => include_str!(
            "../../../amux/tests/fixtures/rows/claude-sdk/permission_callback.rows.jsonl"
        ),
        "elicitation" => include_str!(
            "../../../amux/tests/fixtures/rows/claude-sdk/elicitation_accepted.rows.jsonl"
        ),
        "streamed" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl")
        }
        "interrupted" => {
            include_str!("../../../amux/tests/fixtures/rows/claude-sdk/interrupted.rows.jsonl")
        }
        _ => unreachable!(),
    };
    raw.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn exited_family(newer: &str) -> Vec<Msg> {
    let mut msgs = sequence(vec![ready(), asks()[0].clone()]);
    msgs.push(agent_up(&an_agent("lead", "nova")));
    for name in [AGENT, "pty-agreement"] {
        let mut child = an_agent(name, "nova");
        if name == AGENT {
            child.kind = amux::AgentKind::Claude {
                driver: amux::ClaudeDriver::Sdk,
            };
        }
        child.parent = Some(amux_ui::AgentParent {
            agent_id: agent_id("lead"),
            host_id: host_id("nova"),
        });
        msgs.push(agent_up(&child));
    }
    msgs.extend([
        stream("pty-agreement", StreamMsg::Opened { truncated: false }),
        stream("pty-agreement", StreamMsg::ReplayComplete),
        batch(
            "pty-agreement",
            10,
            chat_rows_through("question_single", ChatAnchor::PermissionRequest(0)),
        ),
        batch(newer, 100, vec![json!({"type":"unrecognized"})]),
    ]);
    for name in [AGENT, "pty-agreement"] {
        msgs.push(stream(
            name,
            StreamMsg::Closed {
                reason: StreamCloseReason::AgentExited { exit_code: Some(7) },
            },
        ));
    }
    msgs
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    let mut values: Vec<_> = ["permission", "elicitation", "streamed", "interrupted"]
        .into_iter()
        .map(|n| (n, sequence(recorded(n))))
        .collect();
    let mut pending = sequence(vec![ready(), prompt()]);
    pending.push(batch(AGENT, 100, asks()));
    values.push(("all ask channels", pending.clone()));
    let mut resolved = pending.clone();
    for (i, (channel, id)) in [
        ("permission", "p"),
        ("permission", "plan"),
        ("permission", "q"),
        ("elicitation", "e"),
        ("dialog", "d"),
    ]
    .into_iter()
    .enumerate()
    {
        resolved.push(batch(AGENT,101+i as i64,vec![json!({"type":format!("amux.claude_sdk.{channel}_resolved"),"request_id":id,"decision":"allow"})]));
    }
    resolved.push(batch(AGENT, 110, vec![result()]));
    values.push(("all asks resolved", resolved));
    let mut replay = pending.clone();
    replay.push(stream(
        AGENT,
        StreamMsg::Closed {
            reason: StreamCloseReason::HostUnreachable,
        },
    ));
    replay.push(stream(AGENT, StreamMsg::Opened { truncated: true }));
    replay.push(batch(AGENT, 101, vec![asks()[0].clone()]));
    replay.push(stream(AGENT, StreamMsg::ReplayComplete));
    values.push(("reconnect with ask in replay tail", replay));
    for (name, reason) in [
        (
            "exited",
            StreamCloseReason::AgentExited { exit_code: Some(7) },
        ),
        ("deleted", StreamCloseReason::AgentDeleted),
        (
            "transport lost",
            StreamCloseReason::TransportError {
                message: "lost".into(),
            },
        ),
    ] {
        let mut lifecycle = pending.clone();
        lifecycle.push(stream(AGENT, StreamMsg::Closed { reason }));
        values.push((name, lifecycle));
    }
    let mut offline = sequence(vec![ready(), prompt()]);
    let mut host = a_host("nova");
    host.online = false;
    offline.push(host_up(&host));
    values.push(("offline host", offline));
    values.push((
        "no timer inference",
        seq([sequence(vec![ready(), prompt()]), vec![tick(100_000)]]),
    ));
    values.push((
        "unknown tail",
        seq([
            claude_sdk_base(AGENT),
            vec![
                stream(AGENT, StreamMsg::Opened { truncated: true }),
                stream(AGENT, StreamMsg::ReplayComplete),
            ],
        ]),
    ));
    values.push((
        "resume gap",
        sequence(vec![
            ready(),
            prompt(),
            json!({"type":"amux.claude_sdk.gap","resumed_session_id":"s"}),
            ready(),
        ]),
    ));
    let mut readonly = pending;
    for msg in &mut readonly {
        if let Msg::Server(amux_ui::ServerMsg::AgentUpserted { agent }) = msg {
            agent.readonly = true;
        }
    }
    values.push(("readonly asks", readonly));
    let mut reopened = sequence(vec![ready(), result()]);
    reopened.push(stream(
        AGENT,
        StreamMsg::Closed {
            reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
        },
    ));
    let mut agent = an_agent(AGENT, "nova");
    agent.kind = amux::AgentKind::Claude {
        driver: amux::ClaudeDriver::Sdk,
    };
    reopened.push(agent_up(&agent));
    reopened.push(stream(AGENT, StreamMsg::Opened { truncated: false }));
    reopened.push(batch(AGENT, 100, vec![ready(), asks()[0].clone()]));
    reopened.push(stream(AGENT, StreamMsg::ReplayComplete));
    values.push(("exited session history replay", reopened));
    values.push(("exited family newer sdk", exited_family(AGENT)));
    values.push(("exited family newer pty", exited_family("pty-agreement")));
    values
}

fn projections(model: &Model) -> (SdkPhase, Attention, SendGate) {
    let id = agent_id(AGENT);
    (
        claude_sdk::phase(model, id),
        model
            .agent(id)
            .map(|c| model.effective_attention(c))
            .unwrap_or(Attention::Unknown),
        claude_sdk::send_gate(model, id),
    )
}

#[test]
fn all_public_projections_and_cached_attention_agree_after_every_msg() {
    for (name, msgs) in sequences() {
        let mut model = Model::default();
        for (i, msg) in msgs.into_iter().enumerate() {
            update(&mut model, msg);
            assert!(
                model.check_invariants().is_empty(),
                "{name} step {i}: {:?}",
                model.check_invariants()
            );
            let (phase, attention, gate) = projections(&model);
            match phase {
                SdkPhase::Unavailable | SdkPhase::Unknown | SdkPhase::Replaying => {
                    assert_eq!(attention, Attention::Unknown)
                }
                SdkPhase::Idle | SdkPhase::Interrupted | SdkPhase::Exited => {
                    assert_eq!(attention, Attention::Idle)
                }
                SdkPhase::Working => assert_eq!(attention, Attention::Working),
                SdkPhase::Finished | SdkPhase::Errored => {
                    assert_eq!(attention, Attention::NeedsYou { why: Why::Finished })
                }
                SdkPhase::NeedsYou { why, .. } => assert_eq!(
                    attention,
                    Attention::NeedsYou {
                        why: match why {
                            AskWhy::Permission | AskWhy::Plan => Why::Permission,
                            _ => Why::Question,
                        }
                    }
                ),
            }
            if gate == SendGate::Ready {
                assert!(matches!(
                    phase,
                    SdkPhase::Idle | SdkPhase::Finished | SdkPhase::Errored | SdkPhase::Interrupted
                ));
            }
            if let Some(card) = model.agent(agent_id(AGENT))
                && let Some(layer) = card.claude_sdk()
            {
                if phase == SdkPhase::Exited {
                    assert_eq!(
                        layer.ask_count(),
                        0,
                        "exited processes cannot reacquire historical obligations"
                    );
                }
                assert_eq!(
                    card.attention,
                    layer.attention(model.stream(agent_id(AGENT)).map(|s| &s.phase))
                );
            }
        }
        capture(&model, name);
    }
}

#[test]
fn exited_claude_backends_share_badge_rank_and_clear_family_needs() {
    for newer in [AGENT, "pty-agreement"] {
        let mut msgs = exited_family(newer);
        let exits = msgs.split_off(msgs.len() - 2);
        let mut model = fold(msgs);
        assert_eq!(model.family_needs(agent_id("lead")).len(), 2);
        for exit in exits {
            update(&mut model, exit);
            assert!(model.check_invariants().is_empty());
        }
        let sdk = model.agent(agent_id(AGENT)).unwrap();
        let pty = model.agent(agent_id("pty-agreement")).unwrap();
        assert_eq!(model.effective_attention(sdk), Attention::Idle);
        assert_eq!(model.effective_attention(pty), Attention::Idle);
        assert_eq!(model.status_label_for(sdk), "exited(7)");
        assert_eq!(model.status_label_for(sdk), model.status_label_for(pty));
        assert_eq!(
            claude_sdk::send_gate(&model, sdk.agent.id),
            SendGate::Exited
        );
        assert_eq!(
            amux_ui::claude::send_gate(&model, pty.agent.id),
            amux_ui::claude::SendGate::Exited
        );
        let family = model.family_of(agent_id("lead"));
        assert_eq!(family.len(), 2);
        assert!(family[0].card.last_activity > family[1].card.last_activity);
        assert_eq!(
            family[0].card.agent.id,
            agent_id(newer),
            "equal attention_rank lets recency decide for either backend"
        );
        assert!(model.family_needs(agent_id("lead")).is_empty());
        capture(&model, &format!("exited-family-newer-{newer}"));
    }
}

#[test]
fn five_asks_keep_typed_payloads_and_fifo_order_without_feed_entries() {
    let model = fold(sequence(asks()));
    let layer = claude_sdk_layer(&model, AGENT);
    assert_eq!(layer.entry_count(), 0);
    assert_eq!(
        layer.asks().map(|a| a.why()).collect::<Vec<_>>(),
        [
            AskWhy::Permission,
            AskWhy::Plan,
            AskWhy::Question,
            AskWhy::Elicitation,
            AskWhy::Dialog
        ]
    );
    let kinds: Vec<_> = layer.asks().map(|a| &a.kind).collect();
    assert!(matches!(kinds[1],AskKind::Plan { plan:Some(p), .. } if p=="Write a greeting"));
    assert!(
        matches!(kinds[2],AskKind::Question { questions } if questions[0].multi_select && questions[0].question.as_deref()==Some("Which color?"))
    );
    assert!(
        matches!(kinds[3],AskKind::Elicitation { form:ElicitationForm::Fields(fields), .. } if fields[0].required && fields[0].kind==ElicitationFieldKind::String)
    );
    assert!(
        matches!(kinds[4],AskKind::Dialog { payload, .. } if payload["nested"]["opaque"]==json!([1,2]))
    );
    capture(&model, "five-pending-asks");
}

#[test]
fn content_eviction_and_large_requests_never_drop_or_clip_pending_obligations() {
    let mut model = fold(sequence(asks()));
    let long_id = "x".repeat(600);
    let long_plan = "plan ".repeat(20_000);
    let mut rows = vec![permission(
        &long_id,
        "ExitPlanMode",
        json!({"plan":long_plan}),
    )];
    rows.extend(
        (0..FEED_RETAINED + 5).map(
            |i| json!({"type":"user","uuid":i.to_string(),"message":{"content":"more content"}}),
        ),
    );
    update(&mut model, batch(AGENT, 100, rows));
    let layer = claude_sdk_layer(&model, AGENT);
    assert_eq!(layer.entry_count(), FEED_RETAINED);
    assert_eq!(layer.ask_count(), 6);
    let ask = layer.asks().last().unwrap();
    assert_eq!(ask.request_id, long_id);
    assert!(matches!(&ask.kind,AskKind::Plan { plan:Some(p), .. } if *p==long_plan));
    assert!(model.check_invariants().is_empty());
}

#[test]
fn duplicate_requests_are_idempotent_and_resolution_matches_both_id_and_channel() {
    let mut rows = asks();
    rows.extend(asks());
    rows.push(
        json!({"type":"amux.claude_sdk.dialog_resolved","request_id":"p","decision":"cancelled"}),
    );
    rows.push(result());
    let mut model = fold(sequence(rows));
    assert_eq!(
        claude_sdk_layer(&model, AGENT).ask_count(),
        5,
        "a turn result cannot erase obligations"
    );
    update(
        &mut model,
        batch(
            AGENT,
            100,
            vec![
                json!({"type":"amux.claude_sdk.permission_resolved","request_id":"p","decision":"deny"}),
            ],
        ),
    );
    assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 4);
    assert_eq!(
        claude_sdk_layer(&model, AGENT)
            .ask_head()
            .unwrap()
            .request_id,
        "plan"
    );
}

#[test]
fn lifecycle_authority_survives_ticks_and_child_results_but_not_disconnect() {
    let mut model = fold(sequence(vec![
        ready(),
        prompt(),
        json!({"type":"result","parent_tool_use_id":"child","is_error":false}),
    ]));
    update(&mut model, tick(1_000_000));
    assert_eq!(
        projections(&model),
        (SdkPhase::Working, Attention::Working, SendGate::Working)
    );
    update(&mut model, batch(AGENT, 100, vec![result()]));
    assert_eq!(
        projections(&model),
        (
            SdkPhase::Finished,
            Attention::NeedsYou { why: Why::Finished },
            SendGate::Ready
        )
    );
    update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::HostUnreachable,
            },
        ),
    );
    assert_eq!(
        projections(&model),
        (SdkPhase::Unknown, Attention::Unknown, SendGate::Unknown)
    );
}

#[test]
fn recorded_request_fields_and_resolution_survive_checkpoint_replay() {
    for name in ["permission", "elicitation"] {
        let mut model = fold(claude_sdk_base(AGENT));
        let mut seen = false;
        for (i, row) in recorded(name).into_iter().enumerate() {
            let mut checkpoint: Model =
                serde_json::from_value(serde_json::to_value(&model).unwrap()).unwrap();
            let msg = batch(AGENT, i as i64, vec![row]);
            update(&mut checkpoint, msg.clone());
            update(&mut model, msg);
            assert_eq!(checkpoint, model);
            if claude_sdk_layer(&model, AGENT).ask_count() > 0 {
                seen = true;
                capture(&model, &format!("recorded-{name}-pending"));
            }
        }
        assert!(seen);
        assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 0);
    }
}

#[test]
fn flat_form_types_defaults_and_rejections_are_explicit() {
    let supported = json!({"type":"object","properties":{
        "text":{"type":"string","default":"hello"},"number":{"type":"number","default":1.5},
        "integer":{"type":"integer"},"yes":{"type":"boolean","default":true},
        "choice":{"type":"string","enum":["a","b"],"default":"a"}
    },"required":["text"]});
    assert!(
        matches!(ElicitationForm::from_schema(&supported),ElicitationForm::Fields(f) if f.len()==5)
    );
    for schema in [
        Value::Null,
        json!({"type":"array"}),
        json!({"type":"object","oneOf":[]}),
        json!({"type":"object","properties":{"x":{"type":"object"}}}),
        json!({"type":"object","properties":{"x":{"type":"string","pattern":".*"}}}),
        json!({"type":"object","properties":{"x":{"type":"boolean","default":"yes"}}}),
        json!({"type":"object","properties":{"x":{"type":"number","enum":["bad"]}}}),
    ] {
        let model = fold(sequence(vec![elicitation(schema)]));
        assert!(
            matches!(&claude_sdk_layer(&model,AGENT).ask_head().unwrap().kind,AskKind::Elicitation { form:ElicitationForm::Unsupported { reason }, .. } if !reason.is_empty())
        );
        assert_eq!(projections(&model).2, SendGate::NeedsYou);
        capture(&model, "unsupported-elicitation");
    }
}

fn capture(model: &Model, name: &str) {
    if let Some(directory) = std::env::var_os("CLAUDE_SDK_AGREEMENT_EVIDENCE") {
        let path = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&path).unwrap();
        let (phase, attention, gate) = projections(model);
        let family: Vec<_> = model.family_of(agent_id("lead")).into_iter().map(|member| {
            json!({"name":member.card.display_name(),"attention":model.effective_attention(member.card),"status":model.status_label_for(member.card),"last_activity":member.card.last_activity})
        }).collect();
        let needs: Vec<_> = model
            .family_needs(agent_id("lead"))
            .into_iter()
            .map(|need| need.card.display_name())
            .collect();
        let value = json!({"phase":phase,"attention":attention,"send_gate":gate,"cached_attention":model.agent(agent_id(AGENT)).map(|c|c.attention),"asks":claude_sdk_layer(model,AGENT).asks().collect::<Vec<_>>(),"family_in_rank_order":family,"family_needs":needs});
        std::fs::write(
            path.join(format!("{}.json", name.replace(' ', "-"))),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn replay_gates_retained_asks_and_exit_resolves_them() {
    let mut model = fold(sequence(asks()));
    update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::HostUnreachable,
            },
        ),
    );
    assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 5);
    assert_eq!(projections(&model).2, SendGate::Unknown);
    update(
        &mut model,
        stream(AGENT, StreamMsg::Opened { truncated: true }),
    );
    update(&mut model, batch(AGENT, 100, asks()));
    assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 5);
    assert_eq!(
        projections(&model),
        (SdkPhase::Replaying, Attention::Unknown, SendGate::Replaying)
    );
    update(&mut model, stream(AGENT, StreamMsg::ReplayComplete));
    assert_eq!(projections(&model).2, SendGate::NeedsYou);
    update(
        &mut model,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::AgentExited { exit_code: Some(7) },
            },
        ),
    );
    assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 0);
    assert_eq!(
        projections(&model),
        (SdkPhase::Exited, Attention::Idle, SendGate::Exited)
    );
}

#[test]
fn invariant_detects_kernel_exit_disagreeing_with_a_working_cached_card() {
    let model = fold(sequence(vec![ready(), prompt()]));
    let mut checkpoint = serde_json::to_value(model).unwrap();
    let card = checkpoint["agents"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .next()
        .unwrap();
    card["phase"] = json!({"phase":"exited", "exit_code":0});
    let corrupted: Model = serde_json::from_value(checkpoint).unwrap();
    let violations = corrupted.check_invariants();
    assert!(
        violations
            .iter()
            .any(|v| matches!(v, amux_ui::Violation::ClaudeSdkProjection { .. }))
    );
    assert!(
        !violations
            .iter()
            .any(|v| matches!(v, amux_ui::Violation::AttentionMismatch { .. })),
        "the public projection relation detects drift independently of the layer cache check"
    );
}
