use amux_ui::codex::{AskContext, FeedEntryKind};
use amux_ui::{CodexCommand, CodexDecision, Command as UiCommand, Runtime, RuntimeOptions};
use serde_json::json;

use super::agents_tests::{succeeded, wait_for};
use super::*;

const PROMPT: &str =
    "Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE.";

async fn journey(wrong_prompt: bool, wrong_answer: bool) {
    let topology = Topology::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../e2e-tests/topologies/codex-recording.json"),
    )
    .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (net, ready, agents) = start(&topology, listener.local_addr().unwrap())
        .await
        .unwrap();
    let agent = ready.agents[0].agent_id;
    let AgentProvider::Codex(recorded) = &agents["codex"].provider else {
        panic!("Codex recording")
    };
    let replay = recorded.controller.clone();
    let client = amux::testnet::connect_user(ready.relay, ready.users[0].token.clone())
        .await
        .unwrap();
    let mut runtime = Runtime::start_with_client(client.clone(), RuntimeOptions::default());
    let server = serve_net(net, listener, ["host".into()].into(), agents);
    let exercise = async {
        let mut control = tests::ControlClient::connect(ready.control).await;
        let qr = control
            .ack(json!({"StartQrPairing":{"daemon":"host"}}))
            .await["qr"]
            .as_str()
            .unwrap()
            .to_owned();
        let qr = amux::parse_qr_pairing_payload_for_cloud(&qr, &format!("http://{}", ready.relay))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match client
                    .pair_qr_cloud_peer(qr.host_id, qr.secret.clone())
                    .await
                {
                    Ok(_) => break,
                    Err(amux::ClientError::Protocol(amux::ProtocolError::Unreachable {
                        ..
                    })) => {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(error) => panic!("pair recorded host: {error}"),
                }
            }
        })
        .await
        .expect("client relay route becomes ready");
        wait_for(&mut runtime, "Codex inventory", |model| {
            model.agent(agent).is_some()
        })
        .await;
        assert_eq!(
            runtime.model().agent(agent).unwrap().agent.kind,
            amux::AgentKind::Codex
        );
        runtime.note_attached(agent);
        wait_for(&mut runtime, "Codex ready", |model| {
            amux_ui::codex::allows_prompt(model, agent)
        })
        .await;
        let incomplete = control
            .request(json!({"AgentVerifyReplay":{"agent":"codex"}}))
            .await;
        assert!(
            incomplete["Error"]["message"]
                .as_str()
                .unwrap()
                .contains("replay incomplete")
        );
        let mutation = control
            .request(json!({"AgentEmit":{"agent":"codex","rows":[]}}))
            .await;
        assert!(
            mutation["Error"]["message"]
                .as_str()
                .unwrap()
                .contains("only recorded")
        );
        succeeded(
            &mut runtime,
            UiCommand::Codex(CodexCommand::Prompt {
                agent,
                text: if wrong_prompt {
                    "An unrecorded prompt"
                } else {
                    PROMPT
                }
                .into(),
            }),
        )
        .await;
        if !wrong_prompt {
            wait_for(&mut runtime, "recorded approval", |model| {
                model
                    .codex(agent)
                    .is_some_and(|layer| layer.ask_count() == 1)
            })
            .await;
            let ask = runtime
                .model()
                .codex(agent)
                .unwrap()
                .ask_head()
                .unwrap()
                .clone();
            assert!(
                matches!(&ask.context, AskContext::Command { command, .. } if command.contains("/usr/bin/touch"))
            );
            println!(
                "Recorded approval: {}",
                serde_json::to_string(&ask).unwrap()
            );
            succeeded(
                &mut runtime,
                UiCommand::Codex(CodexCommand::Answer {
                    agent,
                    request_id: ask.request_id,
                    decision: if wrong_answer {
                        CodexDecision::Cancel
                    } else {
                        CodexDecision::Accept
                    },
                }),
            )
            .await;
        }
        if wrong_prompt || wrong_answer {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if replay
                        .finish()
                        .is_err_and(|error| !error.report.write_mismatches.is_empty())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("unrecorded write must fail promptly");
            // Requests waiting for a response must receive their input result
            // even though the failed transport can never return that response.
            wait_for(&mut runtime, "failed replay input settles", |model| {
                model
                    .codex(agent)
                    .is_some_and(|layer| layer.in_flight_inputs().next().is_none())
            })
            .await;
            let failure = control
                .request(json!({"AgentVerifyReplay":{"agent":"codex"}}))
                .await;
            let message = failure["Error"]["message"].as_str().unwrap();
            assert!(message.starts_with("ReplayWriteMismatch:"), "{failure}");
            let report = replay.finish().unwrap_err().report;
            assert_eq!(report.write_mismatches.len(), 1);
            let actual: serde_json::Value =
                serde_json::from_str(&report.write_mismatches[0].actual).unwrap();
            if wrong_prompt {
                assert_eq!(actual["params"]["input"][0]["text"], "An unrecorded prompt");
            } else {
                assert_eq!(actual["result"]["decision"], "cancel");
            }
            println!("Unrecorded interaction rejected: {failure}");
        } else {
            wait_for(&mut runtime, "recorded DONE and turn completion", |model| model.codex(agent).is_some_and(|layer| {
                layer.entries().any(|entry| matches!(&entry.kind, FeedEntryKind::Message(message) if message.text == "DONE"))
                    && layer.entries().any(|entry| matches!(entry.kind, FeedEntryKind::Turn(_)))
                    && layer.ask_count() == 0
            })).await;
            println!(
                "Projected Codex transcript: {}",
                serde_json::to_string(runtime.model().codex(agent).unwrap()).unwrap()
            );
            let verified = control
                .ack(json!({"AgentVerifyReplay":{"agent":"codex"}}))
                .await;
            let report = replay.finish().unwrap();
            assert!(report.is_complete());
            assert_eq!(report.validated_writes, 5);
            println!("Strict replay verified: {report:?}; control: {verified}");
        }
        control.ack(json!("Shutdown")).await;
    };
    let (result, ()) = tokio::join!(server, exercise);
    result.unwrap();
    assert!(TcpStream::connect(ready.relay).await.is_err());
    assert!(TcpStream::connect(ready.control).await.is_err());
    eprintln!("Codex journey cleanup verified: relay and control refuse connections");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn testnet_codex_recording_approval_and_rows_over_relay_verify_strictly() {
    journey(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn testnet_codex_recording_unrecorded_prompt_fails_without_hanging() {
    journey(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn testnet_codex_recording_unrecorded_answer_fails_without_hanging() {
    journey(false, true).await;
}
