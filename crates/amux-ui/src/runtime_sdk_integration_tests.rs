//! Client effects cross the daemon decoder and real SDK session over scripted provider IO.
use amux::derived_rows_test_support::ClaudeSdkBackendHarness;
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, duplex};
use uuid::Uuid;

use crate::{
    Agent, Command, Draft, DraftSegment, Effect, HostEntry, InputPayload, Model, Msg, OpId,
    OpOutcome, QueueCommand, ServerMsg, StreamEntry, StreamMsg, update,
};

const AGENT: Uuid = Uuid::from_u128(900);
const HOST: Uuid = Uuid::from_u128(901);
fn now() -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(0, 0).unwrap()
}
fn model() -> Model {
    let mut model = Model::default();
    for msg in [
        Msg::Server(ServerMsg::Connected {
            local_host_id: Some(HOST),
        }),
        Msg::Server(ServerMsg::HostUpserted {
            host: HostEntry {
                id: HOST,
                name: "sdk-host".into(),
                online: true,
                version: None,
                capabilities: None,
                trust_status: amux::HostTrustStatus::Trusted,
                last_dial_error: None,
            },
        }),
        Msg::Server(ServerMsg::AgentUpserted {
            agent: Agent {
                id: AGENT,
                host_id: HOST,
                name: Some("sdk".into()),
                command: "claude".into(),
                working_dir: "/work".into(),
                kind: amux::AgentKind::Claude {
                    driver: amux::ClaudeDriver::Sdk,
                },
                readonly: false,
                args: vec![],
                created_at: now(),
                parent: None,
                working_on: None,
            },
        }),
        Msg::Server(ServerMsg::HostsSynchronized),
        Msg::Server(ServerMsg::AgentsSynchronized),
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::ReplayComplete,
        },
    ] {
        update(&mut model, msg);
    }
    model
}
fn issue(model: &mut Model, n: u128, command: Command) -> Vec<Effect> {
    update(
        model,
        Msg::Command {
            op: OpId(Uuid::from_u128(n)),
            command,
        },
    )
}
fn pump(model: &mut Model, host: &ClaudeSdkBackendHarness, consumed: &mut usize) -> Vec<Effect> {
    let entries = host
        .rows()
        .iter()
        .enumerate()
        .skip(*consumed)
        .map(|(index, payload)| StreamEntry {
            seq: index as u64 + 1,
            payload: payload.clone(),
        })
        .collect();
    *consumed = host.rows().len();
    update(
        model,
        Msg::Stream {
            agent: AGENT,
            event: StreamMsg::Batch { at: now(), entries },
        },
    )
}
async fn send(host: &ClaudeSdkBackendHarness, model: &mut Model, effects: Vec<Effect>) {
    let [
        Effect::SendInput {
            op,
            input_id,
            payload: InputPayload::ClaudeSdk { payload },
            ..
        },
    ] = effects.as_slice()
    else {
        panic!("one SDK effect expected: {effects:?}");
    };
    let wire = super::encode_claude_sdk_input(payload.clone()).unwrap();
    host.send_encoded(input_id, &wire).await.unwrap();
    update(
        model,
        Msg::OpResult {
            op: *op,
            outcome: OpOutcome::InputSent,
        },
    );
}
async fn read(stdin: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    assert!(stdin.read_line(&mut line).await.unwrap() > 0);
    serde_json::from_str(&line).unwrap()
}
async fn write(stdout: &mut DuplexStream, value: Value) {
    stdout
        .write_all(format!("{value}\n").as_bytes())
        .await
        .unwrap();
    stdout.flush().await.unwrap();
}
async fn ack(stdout: &mut DuplexStream, request: &Value, response: Value) {
    write(
        stdout,
        json!({"type":"control_response", "response":{
            "subtype":"success", "request_id":request["request_id"], "response":response
        }}),
    )
    .await;
}
fn recorded(kind: &str) -> Value {
    include_str!("../../amux/tests/fixtures/rows/claude-sdk/streamed_turn.rows.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|row| row["type"] == kind)
        .unwrap()
}

#[tokio::test]
async fn sdk_integration_runtime_round_trips_queue_settings_and_command_through_sdk() {
    tokio::time::timeout(std::time::Duration::from_secs(20), journey(true))
        .await
        .unwrap();
}
#[tokio::test]
async fn sdk_integration_runtime_keeps_missing_model_catalogue_unknown() {
    tokio::time::timeout(std::time::Duration::from_secs(20), journey(false))
        .await
        .unwrap();
}
async fn journey(with_catalogue: bool) {
    let (sdk_stdin, provider_stdin) = duplex(65536);
    let (mut stdout, sdk_stdout) = duplex(65536);
    let (release, held) = tokio::sync::oneshot::channel();
    let (finish, finished) = tokio::sync::oneshot::channel();
    let provider = tokio::spawn(async move {
        let mut stdin = BufReader::new(provider_stdin);
        let init = read(&mut stdin).await;
        let models = if with_catalogue {
            json!([
                {"value":"sonnet", "resolvedModel":"sonnet-resolved", "displayName":"Sonnet", "description":"Balanced", "supportedEffortLevels":["low", "high"]},
                {"value":"haiku", "displayName":"Haiku", "description":"Fast", "supportedEffortLevels":["medium"]},
                {"value":"provider-default", "displayName":"Provider default", "description":"No advertised effort choices"}
            ])
        } else {
            json!([])
        };
        ack(&mut stdout, &init, json!({"commands":[{"name":"compact","description":"Compact","argumentHint":"[instructions]"}],
            "agents":[],"models":models,"account":{},"output_style":"default","available_output_styles":[]})).await;
        let mut observed = vec![];
        let first = read(&mut stdin).await;
        assert_eq!(first["message"]["content"], "first");
        observed.push(first);
        write(&mut stdout, recorded("assistant")).await;
        for line in include_str!("../tests/fixtures/todos/sdk-rows.jsonl")
            .lines()
            .take(2)
        {
            write(&mut stdout, serde_json::from_str(line).unwrap()).await;
        }
        held.await.unwrap();
        write(&mut stdout, recorded("result")).await;
        let next = read(&mut stdin).await;
        assert_eq!(next["message"]["content"], "next");
        observed.push(next);
        write(&mut stdout, recorded("result")).await;
        for expected in [
            json!({"subtype":"set_model","model":"sonnet"}),
            json!({"subtype":"apply_flag_settings","settings":{"effortLevel":"high"}}),
            json!({"subtype":"set_permission_mode","mode":"plan"}),
        ] {
            let request = read(&mut stdin).await;
            assert_eq!(request["request"], expected);
            ack(&mut stdout, &request, json!({})).await;
            observed.push(request);
        }
        for model in [
            "sonnet-resolved",
            "haiku",
            "provider-default",
            "unknown-model",
            "sonnet",
        ] {
            let request = read(&mut stdin).await;
            assert_eq!(
                request["request"],
                json!({"subtype":"set_model", "model":model})
            );
            ack(&mut stdout, &request, json!({})).await;
            observed.push(request);
        }
        let command = read(&mut stdin).await;
        assert_eq!(command["message"]["content"], "/compact keep decisions");
        observed.push(command);
        write(&mut stdout, recorded("result")).await;
        finished.await.unwrap();
        observed
    });
    let session = claude::sdk::from_io(
        BufReader::new(sdk_stdout),
        sdk_stdin,
        claude::sdk::QueryOptions {
            session_id: Some(AGENT.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut host = ClaudeSdkBackendHarness::with_session(session)
        .await
        .unwrap();
    host.wait_for_type("amux.claude_sdk.session_facts")
        .await
        .unwrap();
    let mut model = model();
    let mut consumed = 0;
    assert!(pump(&mut model, &host, &mut consumed).is_empty());
    assert_eq!(
        crate::provider::facts(&model, AGENT).commands[0].name,
        "compact"
    );
    let initialized = crate::provider::facts(&model, AGENT);
    assert!(
        initialized.efforts.is_empty(),
        "no current model is known before the first turn"
    );
    if with_catalogue {
        assert_eq!(
            initialized.models,
            vec![
                crate::provider::ModelInfo {
                    id: "sonnet".into(),
                    name: "Sonnet".into(),
                    efforts: vec!["low".into(), "high".into()],
                    default_effort: None
                },
                crate::provider::ModelInfo {
                    id: "haiku".into(),
                    name: "Haiku".into(),
                    efforts: vec!["medium".into()],
                    default_effort: None
                },
                crate::provider::ModelInfo {
                    id: "provider-default".into(),
                    name: "Provider default".into(),
                    efforts: vec![],
                    default_effort: None
                },
            ]
        );
    } else {
        assert!(initialized.models.is_empty());
    }
    let effects = issue(
        &mut model,
        1,
        Command::Send {
            agent: AGENT,
            draft: Draft::plain("first", vec![]),
        },
    );
    send(&host, &mut model, effects).await;
    host.wait_for_type("assistant").await.unwrap();
    host.wait_for_type("user").await.unwrap();
    pump(&mut model, &host, &mut consumed);
    assert!(
        issue(
            &mut model,
            2,
            Command::Queue(QueueCommand::Hold {
                agent: AGENT,
                draft: Draft::plain("next", vec![])
            })
        )
        .is_empty()
    );
    assert!(model.queued(AGENT).is_some());
    let todos = crate::provider::facts(&model, AGENT).todos.unwrap();
    assert_eq!(
        (todos.done, todos.total, todos.current.as_deref()),
        (1, 3, Some("Building the app"))
    );
    assert!(!model.claude_sdk(AGENT).unwrap().entries().any(|entry| matches!(&entry.kind, crate::claude_sdk::FeedEntryKind::Tool(tool) if tool.name == "TodoWrite")));
    release.send(()).unwrap();
    host.wait_for_type("result").await.unwrap();
    let effects = pump(&mut model, &host, &mut consumed);
    send(&host, &mut model, effects).await;
    host.wait_for_type("result").await.unwrap();
    assert!(pump(&mut model, &host, &mut consumed).is_empty());
    assert!(model.queued(AGENT).is_none());
    for (n, cmd) in [
        (
            3,
            Command::SetModel {
                agent: AGENT,
                model: "sonnet".into(),
            },
        ),
        (
            4,
            Command::SetEffort {
                agent: AGENT,
                effort: "high".into(),
            },
        ),
        (
            5,
            Command::ClaudeSdk(crate::ClaudeSdkCommand::SetPermissionMode {
                agent: AGENT,
                mode: "plan".into(),
            }),
        ),
    ] {
        let effects = issue(&mut model, n, cmd);
        send(&host, &mut model, effects).await;
        host.wait_for_type("amux.claude_sdk.input_result")
            .await
            .unwrap();
        pump(&mut model, &host, &mut consumed);
    }
    let facts = crate::provider::facts(&model, AGENT);
    assert_eq!(facts.model.as_deref(), Some("sonnet"));
    assert_eq!(facts.effort.as_deref(), Some("high"));
    assert_eq!(
        facts.permission,
        crate::provider::PermissionFacts::Claude {
            mode: Some("plan".into())
        }
    );
    assert_eq!(facts.models, initialized.models);
    assert_eq!(
        facts.efforts,
        if with_catalogue {
            vec!["low", "high"]
        } else {
            vec![]
        }
    );
    let mut choices = vec![];
    for (n, (selection, efforts)) in [
        ("sonnet-resolved", vec!["low", "high"]),
        ("haiku", vec!["medium"]),
        ("provider-default", vec![]),
        ("unknown-model", vec![]),
        ("sonnet", vec!["low", "high"]),
    ]
    .into_iter()
    .enumerate()
    {
        let effects = issue(
            &mut model,
            10 + n as u128,
            Command::SetModel {
                agent: AGENT,
                model: selection.into(),
            },
        );
        send(&host, &mut model, effects).await;
        host.wait_for_type("amux.claude_sdk.input_result")
            .await
            .unwrap();
        pump(&mut model, &host, &mut consumed);
        let current = crate::provider::facts(&model, AGENT);
        assert_eq!(current.model.as_deref(), Some(selection));
        assert_eq!(current.models, initialized.models);
        assert_eq!(
            current.efforts,
            if with_catalogue { efforts } else { vec![] }
        );
        // Reopening only the latest facts snapshot must recover the same choices.
        let snapshot = host
            .rows()
            .iter()
            .rev()
            .find(|row| row["type"] == "amux.claude_sdk.session_facts")
            .unwrap()
            .clone();
        let mut reopened = self::model();
        update(
            &mut reopened,
            Msg::Stream {
                agent: AGENT,
                event: StreamMsg::Batch {
                    at: now(),
                    entries: vec![StreamEntry {
                        seq: 1,
                        payload: snapshot,
                    }],
                },
            },
        );
        let replayed = crate::provider::facts(&reopened, AGENT);
        assert_eq!(replayed.models, current.models);
        assert_eq!(replayed.efforts, current.efforts);
        choices.push(current);
    }
    let effects = issue(
        &mut model,
        6,
        Command::Send {
            agent: AGENT,
            draft: Draft {
                segments: vec![
                    DraftSegment::CommandToken {
                        name: "compact".into(),
                    },
                    DraftSegment::Text {
                        text: " keep decisions".into(),
                    },
                ],
                attachments: vec![],
            },
        },
    );
    send(&host, &mut model, effects).await;
    host.wait_for_type("result").await.unwrap();
    pump(&mut model, &host, &mut consumed);
    assert_eq!(
        crate::claude_sdk::send_gate(&model, AGENT),
        crate::claude_sdk::SendGate::Ready
    );
    assert!(model.check_invariants().is_empty());
    finish.send(()).unwrap();
    let observed = provider.await.unwrap();
    assert_eq!(
        observed.len(),
        11,
        "the provider receives the queued prompt once"
    );
    let rows = host.finish().await.unwrap();
    if let Some(path) = std::env::var_os("SDK_INTEGRATION_EVIDENCE") {
        let path = std::path::PathBuf::from(path);
        let path = if with_catalogue {
            path
        } else {
            path.join("no-catalogue")
        };
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("provider-observed-inputs.json"),
            serde_json::to_string_pretty(&observed).unwrap(),
        )
        .unwrap();
        std::fs::write(
            path.join("daemon-rows.jsonl"),
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
        std::fs::write(
            path.join("model-choice-facts.json"),
            serde_json::to_string_pretty(&choices).unwrap(),
        )
        .unwrap();
        std::fs::write(
            path.join("runtime-provider-facts.json"),
            serde_json::to_string_pretty(&facts).unwrap(),
        )
        .unwrap();
    }
}
