//! Typed SDK writes, synchronous refusals and authoritative reconciliation.

use amux_ui::claude_sdk::{
    self, AskState, ClaudeSdkCommand as Cmd, ClaudeSdkInput as Input, DialogAnswer,
    ElicitationAnswer, PermissionAnswer, PlanAnswer, QuestionAnswer, SdkAnswer, SendGate,
};
use amux_ui::{
    Attention, Command, Effect, InputPayload, Model, Msg, OpOutcome, StreamCloseReason, StreamMsg,
    update,
};
use serde_json::{Value, json};

use crate::harness::*;

const AGENT: &str = "sdk-write";
fn base() -> Vec<Msg> {
    seq([
        claude_sdk_base(AGENT),
        vec![batch(
            AGENT,
            1,
            vec![
                json!({"type":"amux.claude_sdk.ready", "session_id":"s", "resumed":false}),
                json!({"type":"amux.claude_sdk.session_facts", "permission_mode":"default"}),
            ],
        )],
    ])
}
fn prompt() -> Cmd {
    Cmd::SendPrompt {
        agent: agent_id(AGENT),
        text: "hello\nworld".into(),
    }
}
fn send(n: u8, cmd: Cmd) -> Msg {
    command(op(n), Command::ClaudeSdk(cmd))
}
fn answer(value: SdkAnswer) -> Cmd {
    Cmd::AnswerAsk {
        agent: agent_id(AGENT),
        ask: 0,
        answer: value,
    }
}
fn permission(tool: &str, input: Value) -> Value {
    json!({"type":"amux.claude_sdk.permission_required", "request_id":"p", "tool_name":tool, "input":input, "suggestions":[]})
}
fn result(n: u8, outcome: &str) -> Value {
    json!({"type":"amux.claude_sdk.input_result", "input_id":op(n).0.as_bytes(), "outcome":outcome})
}
fn resolve(channel: &str, id: &str) -> Value {
    json!({"type":format!("amux.claude_sdk.{channel}_resolved"), "request_id":id, "decision":"allow"})
}
fn accepted(n: u8) -> Value {
    json!({"type":"user", "uuid":op(n).0.to_string(), "message":{"content":"hello\nworld"}})
}
fn asked(row: Value) -> Vec<Msg> {
    seq([base(), vec![batch(AGENT, 10, vec![row])]])
}
fn dispatched(effects: &[Effect]) -> &Input {
    match effects {
        [
            Effect::SendInput {
                op: id,
                input_id,
                payload: InputPayload::ClaudeSdk { payload },
                ..
            },
        ] => {
            assert_eq!(input_id, id.0.as_bytes());
            payload
        }
        _ => panic!("expected one SDK send: {effects:?}"),
    }
}
fn refusal(model: &Model, n: u8) -> String {
    match &model.finished_op(op(n)).expect("finished refusal").outcome {
        OpOutcome::Error { error } => error.message(),
        other => panic!("expected refusal: {other:?}"),
    }
}
fn issue(model: &mut Model, n: u8, cmd: Cmd) -> Vec<Effect> {
    let msg = send(n, cmd);
    let mut checkpoint: Model =
        serde_json::from_value(serde_json::to_value(&*model).unwrap()).unwrap();
    let replay_effects = update(
        &mut checkpoint,
        serde_json::from_value(serde_json::to_value(&msg).unwrap()).unwrap(),
    );
    let effects = update(model, msg);
    assert_eq!(*model, checkpoint);
    assert_eq!(effects, replay_effects);
    assert!(
        model.check_invariants().is_empty(),
        "{:?}",
        model.check_invariants()
    );
    effects
}
fn ask_cases() -> Vec<(&'static str, Value, SdkAnswer, Input)> {
    let permission_row = permission("Bash", json!({"command":"pwd", "opaque":{"x":1}}));
    let plan = json!({"plan":"Write a greeting", "planFilePath":"plan.md", "extra":true});
    let question = json!({"questions":[{"question":"Which?", "multiSelect":true, "options":[{"label":"Blue"},{"label":"Red"}]}], "opaque":42});
    let form = json!({"type":"amux.claude_sdk.elicitation_required", "request_id":"e", "schema":{"type":"object", "properties":{"yes":{"type":"boolean"}}, "required":["yes"]}});
    let dialog = json!({"type":"amux.claude_sdk.dialog_required", "request_id":"d", "dialog_kind":"choice", "payload":{"message":"Choose", "options":[{"label":"First", "value":{"opaque":[1,2]}}]}});
    let allow = |input: Value, updates: Value| Input::PermissionDecision {
        request_id: "p".into(),
        decision: json!({"behavior":"allow", "updatedInput":input, "updatedPermissions":updates}),
    };
    let mut scoped = permission_row.clone();
    let suggestion = json!({"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"pwd"}], "behavior":"allow","destination":"session"});
    scoped["suggestions"] = json!([suggestion]);
    vec![
        (
            "allow-once",
            permission_row.clone(),
            SdkAnswer::Permission(PermissionAnswer::AllowOnce),
            allow(permission_row["input"].clone(), json!([])),
        ),
        (
            "allow-scoped",
            scoped,
            SdkAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
            allow(permission_row["input"].clone(), json!([suggestion])),
        ),
        (
            "deny",
            permission_row,
            SdkAnswer::Permission(PermissionAnswer::Deny {
                feedback: Some("Use another tool\nplease".into()),
            }),
            Input::PermissionDecision {
                request_id: "p".into(),
                decision: json!({"behavior":"deny", "message":"Use another tool\nplease"}),
            },
        ),
        (
            "plan-auto",
            permission("ExitPlanMode", plan.clone()),
            SdkAnswer::Plan(PlanAnswer::ApproveAuto),
            allow(
                plan.clone(),
                json!([{"type":"setMode", "mode":"acceptEdits", "destination":"session"}]),
            ),
        ),
        (
            "plan-manual",
            permission("ExitPlanMode", plan.clone()),
            SdkAnswer::Plan(PlanAnswer::ApproveManual),
            allow(
                plan.clone(),
                json!([{"type":"setMode", "mode":"default", "destination":"session"}]),
            ),
        ),
        (
            "plan-changes",
            permission("ExitPlanMode", plan),
            SdkAnswer::Plan(PlanAnswer::RequestChanges {
                feedback: "smaller\nplease".into(),
            }),
            Input::PermissionDecision {
                request_id: "p".into(),
                decision: json!({"behavior":"deny", "message":"smaller\nplease"}),
            },
        ),
        (
            "question",
            permission("AskUserQuestion", question.clone()),
            SdkAnswer::Question(vec![QuestionAnswer {
                selected: vec![1, 0],
                other: Some("Ochre".into()),
            }]),
            {
                let mut input = question;
                input["answers"] = json!({"Which?":"Red, Blue, Ochre"});
                allow(input, json!([]))
            },
        ),
        (
            "form-accept",
            form.clone(),
            SdkAnswer::Elicitation(ElicitationAnswer::Accept {
                content: json!({"yes":true}),
            }),
            Input::ElicitationDecision {
                request_id: "e".into(),
                result: json!({"action":"accept", "content":{"yes":true}}),
            },
        ),
        (
            "form-decline",
            form.clone(),
            SdkAnswer::Elicitation(ElicitationAnswer::Decline),
            Input::ElicitationDecision {
                request_id: "e".into(),
                result: json!({"action":"decline"}),
            },
        ),
        (
            "form-cancel",
            form,
            SdkAnswer::Elicitation(ElicitationAnswer::Cancel),
            Input::ElicitationDecision {
                request_id: "e".into(),
                result: json!({"action":"cancel"}),
            },
        ),
        (
            "dialog-choose",
            dialog.clone(),
            SdkAnswer::Dialog(DialogAnswer::Choose { option: 0 }),
            Input::DialogDecision {
                request_id: "d".into(),
                result: json!({"behavior":"completed", "result":{"label":"First", "value":{"opaque":[1,2]}}}),
            },
        ),
        (
            "dialog-cancel",
            dialog,
            SdkAnswer::Dialog(DialogAnswer::Cancel),
            Input::DialogDecision {
                request_id: "d".into(),
                result: json!({"behavior":"cancelled"}),
            },
        ),
    ]
}

pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    let mut sequences: Vec<_> = ask_cases()
        .into_iter()
        .map(|(name, row, value, _)| {
            let channel = if name.starts_with("form") {
                "elicitation"
            } else if name.starts_with("dialog") {
                "dialog"
            } else {
                "permission"
            };
            let id = row["request_id"].as_str().unwrap().to_owned();
            (
                name,
                seq([
                    asked(row),
                    vec![
                        send(1, answer(value)),
                        op_result(op(1), OpOutcome::InputSent),
                        batch(AGENT, 20, vec![result(1, "ok"), resolve(channel, &id)]),
                    ],
                ]),
            )
        })
        .collect();
    sequences.push((
        "SDK prompt accepted",
        seq([
            base(),
            vec![
                send(1, prompt()),
                op_result(op(1), OpOutcome::InputSent),
                batch(AGENT, 10, vec![accepted(1), result(1, "ok")]),
            ],
        ]),
    ));
    sequences.push((
        "SDK prompt refused remotely",
        seq([
            base(),
            vec![
                send(1, prompt()),
                op_result(op(1), OpOutcome::InputSent),
                batch(AGENT, 10, vec![result(1, "provider closed")]),
            ],
        ]),
    ));
    sequences
}

#[test]
fn every_ask_answer_encodes_its_provider_decision_and_keeps_the_obligation_until_resolution() {
    for (name, row, value, expected) in ask_cases() {
        let channel = if name.starts_with("form") {
            "elicitation"
        } else if name.starts_with("dialog") {
            "dialog"
        } else {
            "permission"
        };
        let id = row["request_id"].as_str().unwrap().to_owned();
        let mut model = fold(asked(row));
        let effects = issue(&mut model, 1, answer(value));
        assert_eq!(dispatched(&effects), &expected, "{name}");
        update(&mut model, op_result(op(1), OpOutcome::InputSent));
        assert!(matches!(
            claude_sdk_layer(&model, AGENT).ask_head().unwrap().state,
            AskState::AnsweredOptimistic { .. }
        ));
        update(&mut model, batch(AGENT, 20, vec![result(1, "ok")]));
        assert!(claude_sdk_layer(&model, AGENT).in_flight_input().is_none());
        assert!(matches!(
            claude_sdk_layer(&model, AGENT).ask_head().unwrap().state,
            AskState::AnsweredOptimistic { .. }
        ));
        let retry = answer(SdkAnswer::Permission(PermissionAnswer::AllowOnce));
        assert!(issue(&mut model, 2, retry).is_empty());
        assert!(refusal(&model, 2).contains("awaiting confirmation"));
        capture(&model, &effects, name);
        update(&mut model, batch(AGENT, 21, vec![resolve("wrong", &id)]));
        assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 1);
        update(&mut model, batch(AGENT, 22, vec![resolve(channel, &id)]));
        assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 0);
        assert!(model.check_invariants().is_empty());
    }
}

#[test]
fn prompt_echo_reconciles_only_with_its_accepted_uuid_and_transport_success_does_not_remove_it() {
    let mut model = fold(base());
    let effects = issue(&mut model, 1, prompt());
    assert_eq!(
        dispatched(&effects),
        &Input::Prompt {
            text: "hello\nworld".into()
        }
    );
    update(&mut model, op_result(op(1), OpOutcome::InputSent));
    update(
        &mut model,
        batch(AGENT, 10, vec![result(9, "ok"), accepted(9)]),
    );
    assert!(claude_sdk_layer(&model, AGENT).in_flight_input().is_some());
    update(&mut model, batch(AGENT, 11, vec![result(1, "ok")]));
    assert!(claude_sdk_layer(&model, AGENT).pending_echo().is_some());
    assert_eq!(
        claude_sdk::send_gate(&model, agent_id(AGENT)),
        SendGate::InputInFlight
    );
    capture(&model, &effects, "prompt-awaiting-row");
    update(&mut model, batch(AGENT, 12, vec![accepted(1)]));
    assert!(claude_sdk_layer(&model, AGENT).pending_echo().is_none());
    assert_eq!(
        claude_sdk::send_gate(&model, agent_id(AGENT)),
        SendGate::Working
    );
}

#[test]
fn failed_prompt_resurfaces_the_text_and_failed_answers_can_retry_without_late_resurrection() {
    for rpc in [false, true] {
        let mut model = fold(base());
        issue(&mut model, 1, prompt());
        if rpc {
            update(&mut model, op_failed(op(1), "lost"));
        } else {
            update(&mut model, op_result(op(1), OpOutcome::InputSent));
            update(&mut model, batch(AGENT, 10, vec![result(1, "lost")]));
        }
        let layer = claude_sdk_layer(&model, AGENT);
        assert!(layer.pending_echo().is_none());
        assert!(layer.in_flight_input().is_none());
        assert_eq!(layer.last_input_failure().unwrap().command, prompt());
        assert_eq!(layer.last_input_failure().unwrap().message, "lost");
        capture(&model, &[], "prompt-send-failed");
        assert_eq!(
            claude_sdk::send_gate(&model, agent_id(AGENT)),
            SendGate::Ready
        );
        assert!(matches!(
            dispatched(&issue(&mut model, 2, prompt())),
            Input::Prompt { .. }
        ));
        let mut model = fold(asked(permission("Bash", json!({"command":"pwd"}))));
        let cmd = answer(SdkAnswer::Permission(PermissionAnswer::AllowOnce));
        issue(&mut model, 1, cmd.clone());
        update(&mut model, batch(AGENT, 20, vec![result(1, "lost")]));
        assert!(matches!(
            claude_sdk_layer(&model, AGENT).ask_head().unwrap().state,
            AskState::SendFailed { .. }
        ));
        capture(&model, &[], "answer-send-failed");
        issue(&mut model, 2, cmd);
        update(
            &mut model,
            batch(AGENT, 21, vec![resolve("permission", "p")]),
        );
        update(&mut model, op_failed(op(2), "late failure"));
        assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 0);
    }
}

#[test]
fn controls_encode_one_input_each_and_the_mode_changes_only_from_observed_facts() {
    let agent = agent_id(AGENT);
    for (cmd, expected) in [
        (
            Cmd::SetModel {
                agent,
                model: Some("sonnet".into()),
            },
            Input::SetModel {
                model: Some("sonnet".into()),
            },
        ),
        (
            Cmd::SetModel { agent, model: None },
            Input::SetModel { model: None },
        ),
        (
            Cmd::RequestContextBreakdown { agent },
            Input::RequestContextBreakdown,
        ),
        (
            Cmd::CyclePermissionMode { agent },
            Input::SetPermissionMode {
                mode: "acceptEdits".into(),
            },
        ),
    ] {
        let mut model = fold(base());
        let effects = issue(&mut model, 1, cmd.clone());
        assert_eq!(dispatched(&effects), &expected);
        let name = match &expected {
            Input::SetModel { model: Some(_) } => "select-model",
            Input::SetModel { model: None } => "restore-model",
            Input::SetPermissionMode { .. } => "cycle-mode",
            Input::RequestContextBreakdown => "request-context",
            _ => unreachable!(),
        };
        capture(&model, &effects, name);
        assert!(issue(&mut model, 2, Cmd::RequestContextBreakdown { agent }).is_empty());
        assert!(refusal(&model, 2).contains("in flight"));
        capture(&model, &[], "refused-context-while-input-pending");
        update(&mut model, batch(AGENT, 10, vec![result(1, "ok")]));
        assert_eq!(dispatched(&issue(&mut model, 3, cmd)), &expected);
    }
    for (mode, next) in [
        ("acceptEdits", "plan"),
        ("plan", "default"),
        ("bypassPermissions", "default"),
        ("dontAsk", "default"),
        ("auto", "default"),
    ] {
        let mut model = fold(base());
        update(
            &mut model,
            batch(
                AGENT,
                10,
                vec![json!({"type":"amux.claude_sdk.session_facts","permission_mode":mode})],
            ),
        );
        assert_eq!(
            dispatched(&issue(&mut model, 1, Cmd::CyclePermissionMode { agent })),
            &Input::SetPermissionMode { mode: next.into() }
        );
    }
    let mut model = fold(base());
    update(&mut model, batch(AGENT, 10, vec![accepted(9)]));
    let effects = issue(&mut model, 1, Cmd::Interrupt { agent });
    assert_eq!(dispatched(&effects), &Input::Interrupt);
    capture(&model, &effects, "interrupt");
}

#[test]
fn every_command_refuses_readonly_offline_replay_exited_unknown_unavailable_and_busy_input() {
    let agent = agent_id(AGENT);
    let commands = vec![
        prompt(),
        answer(SdkAnswer::Permission(PermissionAnswer::AllowOnce)),
        Cmd::Interrupt { agent },
        Cmd::CyclePermissionMode { agent },
        Cmd::SetModel { agent, model: None },
        Cmd::RequestContextBreakdown { agent },
    ];
    let mut scenarios = vec![
        ("not connected", Model::default()),
        (
            "unavailable",
            fold(seq([
                vec![connected("nova"), host_up(&a_host("nova"))],
                synced(),
            ])),
        ),
        ("unknown", fold(claude_sdk_base(AGENT))),
    ];
    let mut readonly = base();
    for msg in &mut readonly {
        if let Msg::Server(amux_ui::ServerMsg::AgentUpserted { agent }) = msg {
            agent.readonly = true;
        }
    }
    scenarios.push(("read-only", fold(readonly)));
    let mut offline = fold(base());
    let mut host = a_host("nova");
    host.online = false;
    update(&mut offline, host_up(&host));
    scenarios.push(("unknown", offline));
    let mut replay = fold(base());
    update(
        &mut replay,
        stream(AGENT, StreamMsg::Opened { truncated: false }),
    );
    scenarios.push(("replaying", replay));
    let mut exited = fold(base());
    update(
        &mut exited,
        stream(
            AGENT,
            StreamMsg::Closed {
                reason: StreamCloseReason::AgentExited { exit_code: Some(0) },
            },
        ),
    );
    scenarios.push(("exited", exited));
    let mut inflight = fold(base());
    issue(&mut inflight, 1, prompt());
    scenarios.push(("in flight", inflight));
    for (reason, original) in scenarios {
        for cmd in &commands {
            let mut model = original.clone();
            assert!(
                issue(&mut model, 9, cmd.clone()).is_empty(),
                "{reason} {cmd:?}"
            );
            assert!(
                refusal(&model, 9).contains(reason),
                "{}",
                refusal(&model, 9)
            );
            assert!(!model.pending_ops().any(|p| p.op == op(9)));
        }
    }
    for (row, reason) in [
        (accepted(8), "working"),
        (permission("Bash", json!({})), "pending ask"),
    ] {
        let mut model = fold(asked(row));
        assert!(issue(&mut model, 1, prompt()).is_empty());
        assert!(refusal(&model, 1).contains(reason));
    }
}

#[test]
fn invalid_answers_and_stale_or_queued_asks_finish_without_effects() {
    let mut cases = vec![
        (
            permission("Bash", json!({})),
            SdkAnswer::Plan(PlanAnswer::ApproveAuto),
        ),
        (
            permission("Bash", json!({})),
            SdkAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 0 }),
        ),
        (
            permission("ExitPlanMode", json!({"plan":"x"})),
            SdkAnswer::Plan(PlanAnswer::RequestChanges {
                feedback: " ".into(),
            }),
        ),
    ];
    let question = permission(
        "AskUserQuestion",
        json!({"questions":[{"question":"Which?","options":[{"label":"one"}],"multiSelect":false}]}),
    );
    for selected in [vec![], vec![5], vec![0, 0]] {
        cases.push((
            question.clone(),
            SdkAnswer::Question(vec![QuestionAnswer {
                selected,
                other: None,
            }]),
        ));
    }
    cases.push((question, SdkAnswer::Question(vec![])));
    for content in [
        json!({}),
        json!({"yes":"true"}),
        json!({"yes":true,"extra":1}),
    ] {
        cases.push((
            ask_cases()[7].1.clone(),
            SdkAnswer::Elicitation(ElicitationAnswer::Accept { content }),
        ));
    }
    cases.push((json!({"type":"amux.claude_sdk.elicitation_required","request_id":"e","schema":{"type":"array"}}),SdkAnswer::Elicitation(ElicitationAnswer::Accept { content:json!({}) })));
    cases.push((
        ask_cases()[10].1.clone(),
        SdkAnswer::Dialog(DialogAnswer::Choose { option: 9 }),
    ));
    cases.push((json!({"type":"amux.claude_sdk.dialog_required","request_id":"d","payload":{"opaque":true}}),SdkAnswer::Dialog(DialogAnswer::Choose { option:0 })));
    for (row, value) in cases {
        let mut model = fold(asked(row));
        assert!(issue(&mut model, 1, answer(value)).is_empty());
        assert!(!refusal(&model, 1).is_empty());
        assert!(matches!(
            claude_sdk_layer(&model, AGENT).ask_head().unwrap().state,
            AskState::Pending
        ));
    }
    let mut model = fold(asked(permission("Bash", json!({}))));
    let mut row = permission("Bash", json!({}));
    row["request_id"] = json!("second");
    update(&mut model, batch(AGENT, 20, vec![row]));
    for (id, reason) in [(1, "head"), (9, "resolved")] {
        let mut model = model.clone();
        assert!(
            issue(
                &mut model,
                1,
                Cmd::AnswerAsk {
                    agent: agent_id(AGENT),
                    ask: id,
                    answer: SdkAnswer::Permission(PermissionAnswer::AllowOnce)
                }
            )
            .is_empty()
        );
        assert!(refusal(&model, 1).contains(reason));
    }
}

#[test]
fn empty_prompts_models_unknown_modes_and_idle_interrupts_refuse() {
    let agent = agent_id(AGENT);
    for cmd in [
        Cmd::SendPrompt {
            agent,
            text: " \n".into(),
        },
        Cmd::SetModel {
            agent,
            model: Some(" ".into()),
        },
        Cmd::Interrupt { agent },
        answer(SdkAnswer::Permission(PermissionAnswer::AllowOnce)),
    ] {
        let mut model = fold(base());
        assert!(issue(&mut model, 1, cmd).is_empty());
        assert!(!refusal(&model, 1).is_empty());
    }
    for mode in [Value::Null, json!("future")] {
        let mut model = fold(base());
        update(
            &mut model,
            batch(
                AGENT,
                10,
                vec![json!({"type":"amux.claude_sdk.session_facts","permission_mode":mode})],
            ),
        );
        assert!(issue(&mut model, 1, Cmd::CyclePermissionMode { agent }).is_empty());
        assert!(refusal(&model, 1).contains("unknown"));
    }
}

#[test]
fn attachments_share_the_sdk_gate_and_failure_recovery() {
    let mut model = fold(base());
    let effects = update(
        &mut model,
        command(
            op(1),
            Command::SendPromptWithAttachments {
                agent: agent_id(AGENT),
                text: "hello".into(),
                attachments: vec![],
            },
        ),
    );
    assert!(
        matches!(&effects[..],[Effect::PutThenSend { input:InputPayload::ClaudeSdk { payload:Input::Prompt { text } },.. }] if text=="hello")
    );
    update(&mut model, op_failed(op(1), "put failed"));
    assert!(claude_sdk_layer(&model, AGENT).pending_echo().is_none());
    assert_eq!(
        claude_sdk_layer(&model, AGENT)
            .last_input_failure()
            .unwrap()
            .message,
        "put failed"
    );
}

fn capture(model: &Model, effects: &[Effect], name: &str) {
    if let Some(path) = std::env::var_os("CLAUDE_SDK_WRITE_EVIDENCE") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        let layer = claude_sdk_layer(model, AGENT);
        let value = json!({"effects":effects,"asks":layer.asks().collect::<Vec<_>>(),"echo":layer.pending_echo(),"input_failure":layer.last_input_failure(),"send_gate":claude_sdk::send_gate(model,agent_id(AGENT)), "finished_ops":model.finished_ops()});
        std::fs::write(
            path.join(format!("{name}.json")),
            serde_json::to_string_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn authoritative_rows_win_over_late_transport_failures_and_closure_releases_pending_inputs() {
    let mut model = fold(base());
    issue(&mut model, 1, prompt());
    update(&mut model, batch(AGENT, 10, vec![accepted(1)]));
    update(&mut model, op_failed(op(1), "late failure"));
    assert!(
        claude_sdk_layer(&model, AGENT)
            .last_input_failure()
            .is_none()
    );
    assert!(claude_sdk_layer(&model, AGENT).pending_echo().is_none());
    for close in [
        StreamCloseReason::HostUnreachable,
        StreamCloseReason::AgentExited { exit_code: Some(0) },
    ] {
        let mut model = fold(asked(permission("Bash", json!({}))));
        issue(
            &mut model,
            1,
            answer(SdkAnswer::Permission(PermissionAnswer::AllowOnce)),
        );
        update(
            &mut model,
            stream(
                AGENT,
                StreamMsg::Closed {
                    reason: close.clone(),
                },
            ),
        );
        assert!(claude_sdk_layer(&model, AGENT).in_flight_input().is_none());
        if matches!(close, StreamCloseReason::HostUnreachable) {
            assert!(matches!(
                claude_sdk_layer(&model, AGENT).ask_head().unwrap().state,
                AskState::SendFailed { .. }
            ));
        } else {
            assert_eq!(claude_sdk_layer(&model, AGENT).ask_count(), 0);
        }
        assert!(model.check_invariants().is_empty());
    }
}

/// The moment a prompt is dispatched the fleet badge says Working, as it
/// does for the terminal and Codex chats: the person asked for work and
/// the badge should not wait one round trip for the session to agree.
/// The phase stays what the session last stated; only attention and the
/// send gate reflect the input on its way.
#[test]
fn a_dispatched_prompt_projects_working_before_the_session_reports_its_turn() {
    let mut model = fold(base());
    let agent = agent_id(AGENT);
    let card = model.agent(agent).expect("the agent");
    assert_eq!(model.effective_attention(card), Attention::Idle);

    update(&mut model, send(1, prompt()));
    assert_eq!(claude_sdk::phase(&model, agent), claude_sdk::SdkPhase::Idle);
    assert_eq!(
        claude_sdk::send_gate(&model, agent),
        SendGate::InputInFlight
    );
    let card = model.agent(agent).expect("the agent");
    assert_eq!(model.effective_attention(card), Attention::Working);
    assert_eq!(
        card.attention,
        Attention::Working,
        "the fleet's cached badge agrees"
    );
    assert!(model.check_invariants().is_empty());
}
