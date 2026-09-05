//! Model choices and effort levels are session facts; replay cannot invent them.
use amux_ui::codex::{CodexInput, SendGate};
use amux_ui::provider::{ApprovalPolicy, SandboxPolicy, SettingsGate, facts, settings_gate};
use amux_ui::{Command, Effect, InputPayload, Model, Msg, OpOutcome, StreamMsg, update};
use serde_json::{Value, json};

use crate::harness::*;
const AGENT: &str = "model-effort";
fn rows() -> Vec<Value> {
    include_str!("../../../amux/tests/fixtures/model-effort/rows.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn ready() -> Vec<Msg> {
    seq([
        codex_base(AGENT),
        vec![batch(AGENT, 10, vec![rows()[0].clone()])],
    ])
}
fn change() -> Command {
    Command::SetModel {
        agent: agent_id(AGENT),
        model: "model-b".into(),
    }
}
pub fn sequences() -> Vec<(&'static str, Vec<Msg>)> {
    vec![
        (
            "model and effort session recording",
            seq([codex_base(AGENT), vec![batch(AGENT, 10, rows())]]),
        ),
        (
            "PTY refuses model change",
            seq([chat_base(AGENT), vec![command(op(1), change())]]),
        ),
    ]
}
#[test]
fn model_effort_recording_matches_live_and_changes_facts_in_place() {
    let mut live = Model::default();
    let mut replay = Model::default();
    for msg in seq([
        codex_base(AGENT),
        rows()
            .into_iter()
            .enumerate()
            .map(|(i, row)| batch(AGENT, 10 + i as i64, vec![row]))
            .collect(),
    ]) {
        let encoded = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            update(&mut live, msg),
            update(&mut replay, serde_json::from_str(&encoded).unwrap())
        );
        assert_eq!(live, replay);
        assert!(live.check_invariants().is_empty());
    }
    let result = facts(&live, agent_id(AGENT));
    assert_eq!(result.model.as_deref(), Some("model-b"));
    assert_eq!(result.effort.as_deref(), Some("high"));
    assert_eq!(result.models.len(), 2);
    assert_eq!(result.efforts, ["medium", "high"]);
    assert_eq!(
        codex_layer(&live, AGENT).entry_count(),
        0,
        "settings replace facts without adding feed rows"
    );
    println!(
        "Codex shared session facts: {}",
        serde_json::to_string_pretty(&result).unwrap()
    );
}
#[test]
fn model_effort_typed_changes_wait_for_host_facts_and_correlated_result() {
    for cmd in [
        change(),
        Command::SetEffort {
            agent: agent_id(AGENT),
            effort: "medium".into(),
        },
        Command::SetPreset {
            agent: agent_id(AGENT),
            approval: ApprovalPolicy::OnRequest,
            sandbox: SandboxPolicy::ReadOnly,
        },
    ] {
        let mut model = fold(ready());
        let before = facts(&model, agent_id(AGENT));
        let effects = update(&mut model, command(op(1), cmd.clone()));
        let payload = effects
            .iter()
            .find_map(|e| match e {
                Effect::SendInput {
                    payload: InputPayload::Codex { payload },
                    input_id,
                    ..
                } => {
                    assert_eq!(input_id, op(1).0.as_bytes());
                    Some(payload)
                }
                _ => None,
            })
            .unwrap();
        match (cmd, payload) {
            (Command::SetModel { .. }, CodexInput::SetModel { model }) => {
                assert_eq!(model, "model-b")
            }
            (Command::SetEffort { .. }, CodexInput::SetEffort { effort }) => {
                assert_eq!(effort, "medium")
            }
            (Command::SetPreset { .. }, CodexInput::SetPreset { approval, sandbox }) => {
                assert_eq!(approval, &ApprovalPolicy::OnRequest);
                assert_eq!(sandbox, &SandboxPolicy::ReadOnly);
            }
            other => panic!("wrong typed input: {other:?}"),
        }
        assert_eq!(
            facts(&model, agent_id(AGENT)),
            before,
            "a local selection is not a host fact"
        );
        assert_eq!(
            settings_gate(&model, agent_id(AGENT)),
            SettingsGate::Codex {
                reason: SendGate::InputInFlight
            }
        );
        update(
            &mut model,
            batch(
                AGENT,
                20,
                vec![json!({"type":"amux.input_result","input_id":op(1).0.as_bytes(),"ok":{}})],
            ),
        );
        assert_eq!(settings_gate(&model, agent_id(AGENT)), SettingsGate::Ready);
    }
}
#[test]
fn model_effort_pty_refuses_all_settings_with_a_named_gate() {
    for cmd in [
        change(),
        Command::SetEffort {
            agent: agent_id(AGENT),
            effort: "high".into(),
        },
        Command::SetPreset {
            agent: agent_id(AGENT),
            approval: ApprovalPolicy::Never,
            sandbox: SandboxPolicy::DangerFullAccess,
        },
    ] {
        let mut model = fold(chat_base(AGENT));
        assert_eq!(
            settings_gate(&model, agent_id(AGENT)),
            SettingsGate::PtySettingsUnavailable
        );
        assert!(facts(&model, agent_id(AGENT)).models.is_empty());
        assert!(update(&mut model, command(op(1), cmd)).is_empty());
        let OpOutcome::Error { error } = &model.finished_op(op(1)).unwrap().outcome else {
            panic!("refusal");
        };
        assert_eq!(
            error.message(),
            SettingsGate::PtySettingsUnavailable.refusal().unwrap()
        );
        println!("Claude PTY: {}", error.message());
    }
}
#[test]
fn model_effort_unknown_choices_and_stale_sessions_never_dispatch() {
    for cmd in [
        Command::SetModel {
            agent: agent_id(AGENT),
            model: "invented".into(),
        },
        Command::SetEffort {
            agent: agent_id(AGENT),
            effort: "high".into(),
        },
    ] {
        let mut model = fold(ready());
        assert!(update(&mut model, command(op(1), cmd)).is_empty());
        assert!(matches!(
            model.finished_op(op(1)).unwrap().outcome,
            OpOutcome::Error { .. }
        ));
    }
    for stale in [
        stream(AGENT, StreamMsg::Opened { truncated: false }),
        batch(
            AGENT,
            20,
            vec![json!({"type":"amux.codex_gap","reason":"connection_lost"})],
        ),
        batch(
            AGENT,
            20,
            vec![json!({"type":"turn/started","turn":{"id":"busy"}})],
        ),
    ] {
        let mut model = fold(ready());
        update(&mut model, stale);
        assert!(update(&mut model, command(op(1), change())).is_empty());
    }
    let mut model = fold(ready());
    update(&mut model, command(op(1), change()));
    update(
        &mut model,
        op_result(
            op(1),
            OpOutcome::Error {
                error: amux_ui::OpError::general("transport failed"),
            },
        ),
    );
    assert_eq!(settings_gate(&model, agent_id(AGENT)), SettingsGate::Ready);
}

#[test]
fn model_effort_observers_and_sessions_without_discovery_never_invent_choices() {
    let mut model = fold(ready());
    let mut agent = a_codex_agent(AGENT, "nova");
    agent.readonly = true;
    update(&mut model, agent_up(&agent));
    assert_eq!(
        settings_gate(&model, agent_id(AGENT)),
        SettingsGate::Codex {
            reason: SendGate::ObserverReadOnly
        }
    );
    assert!(update(&mut model, command(op(1), change())).is_empty());
    let mut initial = rows()[0].clone();
    initial["session"]["models"] = json!([]);
    let mut model = fold(seq([
        codex_base(AGENT),
        vec![batch(AGENT, 10, vec![initial])],
    ]));
    let facts = facts(&model, agent_id(AGENT));
    assert_eq!(facts.model.as_deref(), Some("model-a"));
    assert!(facts.models.is_empty());
    assert!(facts.efforts.is_empty());
    assert!(update(&mut model, command(op(1), change())).is_empty());
}
