use amux_ui::{Command as UiCommand, Draft, Runtime, RuntimeOptions, claude_sdk};
use serde_json::json;

use super::agents_tests::{succeeded, wait_for};
use super::*;

#[test]
fn testnet_sdk_topology_requires_a_valid_provider_initialization() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("topology.json");
    let mut topology = json!({"users":["personal"], "paired":[],
        "daemons":[{"name":"laptop","user":"personal","repository_roots":[]}],
        "agents":[{"name":"sdk","daemon":"laptop","working_dir":".",
            "provider":{"ClaudeSdk":{"model":"sonnet"}}}]});
    std::fs::write(&path, serde_json::to_vec(&topology).unwrap()).unwrap();
    assert!(
        Topology::load(&path)
            .unwrap_err()
            .to_string()
            .contains("needs its host's sdk_script")
    );
    topology["daemons"][0]["sdk_script"] = json!("sdk.json");
    std::fs::write(&path, serde_json::to_vec(&topology).unwrap()).unwrap();
    std::fs::write(
        root.path().join("sdk.json"),
        r#"{"initialization":{},"reply":"hello"}"#,
    )
    .unwrap();
    assert!(
        Topology::load(&path)
            .unwrap_err()
            .to_string()
            .contains("parse SDK script")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn testnet_sdk_creation_and_inputs_use_the_real_backend_over_the_relay() {
    tokio::time::timeout(Duration::from_secs(60), exercise())
        .await
        .unwrap();
}

async fn exercise() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../e2e-tests/topologies/claude-sessions.json");
    let topology = Topology::load(&path).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (net, ready, agents) = start(&topology, listener.local_addr().unwrap())
        .await
        .unwrap();
    let client =
        amux::testnet::connect_user(&ready.cloud_url, ready.relay, ready.users[0].token.clone())
            .await
            .unwrap();
    let mut runtime = Runtime::start_with_client(
        client.clone(),
        RuntimeOptions {
            host_inventory: Some(client.admin()),
            ..Default::default()
        },
    );
    let server = serve_net(net, listener, ["laptop".into()].into(), agents);
    let journey = async {
        let mut control = tests::ControlClient::connect(ready.control).await;
        let qr = control
            .ack(json!({"StartQrPairing":{"daemon":"laptop"}}))
            .await;
        let qr = amux::parse_qr_pairing_payload_for_cloud(
            qr["qr"].as_str().unwrap(),
            client.cloud_url(),
        )
        .unwrap();
        wait_for(
            &mut runtime,
            "relay host discovery before pairing",
            |model| model.host_online(qr.host_id),
        )
        .await;
        client
            .admin()
            .pair_qr_cloud_peer(qr.host_id, qr.secret)
            .await
            .unwrap();
        let sdk = ready
            .agents
            .iter()
            .find(|a| a.name == "existing-sdk")
            .unwrap()
            .agent_id;
        let pty = ready
            .agents
            .iter()
            .find(|a| a.name == "existing-pty")
            .unwrap()
            .agent_id;
        wait_for(&mut runtime, "paired inventory", |model| {
            model.agent(sdk).is_some() && model.agent(pty).is_some()
        })
        .await;
        let request = amux::CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: Some(ready.daemons[0].host_id),
            name: Some("created-sdk".into()),
            agent_type: amux::AgentType::Claude {
                driver: amux::ClaudeDriver::Sdk,
            },
            working_dir: topology.agents[0].working_dir.clone(),
            terminal_size: None,
            args: vec!["--model".into(), "sonnet".into()],
            parent: None,
            initial_prompt: None,
        };
        let created = client.create_agent(request.clone()).await.unwrap();
        let before = control.ack(json!({"Inventory":{"daemon":"laptop"}})).await;
        assert_eq!(before["agents"].as_array().unwrap().len(), 3);
        let created_inventory = before["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == created.id.to_string())
            .unwrap();
        assert_eq!(created_inventory["kind"], "claude");
        assert_eq!(created_inventory["driver"], "sdk");
        assert_eq!(
            before["agents"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["driver"] == "pty")
                .count(),
            1
        );

        for (agent, text) in [(created.id, "created prompt"), (sdk, "existing prompt")] {
            wait_for(&mut runtime, "SDK inventory", |model| {
                model.agent(agent).is_some()
            })
            .await;
            runtime.note_attached(agent);
            wait_for(&mut runtime, "SDK ready", |model| {
                claude_sdk::send_gate(model, agent) == claude_sdk::SendGate::Ready
            })
            .await;
            assert!(runtime.model().claude(agent).is_none());
            succeeded(
                &mut runtime,
                UiCommand::Send {
                    agent,
                    draft: Draft::plain(text, vec![]),
                },
            )
            .await;
            wait_for(&mut runtime, "SDK response", |model| {
                model.claude_sdk(agent).is_some_and(|layer| {
                    layer.entries().any(|row| {
                        matches!(&row.kind, claude_sdk::FeedEntryKind::Message(message)
                    if message.text == "The SDK session received your prompt.")
                    })
                })
            })
            .await;
            wait_for(&mut runtime, "SDK turn ended", |model| {
                claude_sdk::send_gate(model, agent) == claude_sdk::SendGate::Ready
            })
            .await;
            succeeded(
                &mut runtime,
                UiCommand::SetModel {
                    agent,
                    model: "haiku".into(),
                },
            )
            .await;
            wait_for(&mut runtime, "SDK observed model", |model| {
                amux_ui::provider::facts(model, agent).model.as_deref() == Some("haiku")
            })
            .await;
            let seen = control
                .ack(json!({"AgentObserve":{"agent":agent.to_string()}}))
                .await;
            let inputs = seen["sdk_inputs"].as_array().unwrap();
            assert_eq!(
                inputs
                    .iter()
                    .filter(|input| input["type"] == "user")
                    .count(),
                1
            );
            assert!(
                inputs
                    .iter()
                    .any(|input| input["message"]["content"] == text)
            );
            assert!(
                inputs
                    .iter()
                    .filter(|input| input["type"] == "user")
                    .all(|input| input["session_id"] == agent.to_string())
            );
            assert!(inputs.iter().any(|input| input["request"] == json!({"subtype":"set_model","model":"haiku"})));
            assert_eq!(seen["observed"], json!([]));
            if agent == sdk {
                assert_eq!(
                    control
                        .ack(json!({"AgentObserve":{"agent":"existing-sdk"}}))
                        .await,
                    seen
                );
            }
        }
        runtime.note_attached(pty);
        wait_for(&mut runtime, "PTY ready", |model| {
            // Replay can finish on the empty log before the live tailer has
            // published its readiness marker. Wait for both sources of
            // startup state before sending with the stream sequence guard.
            model.is_synchronized()
                && model
                    .claude(pty)
                    .is_some_and(|layer| layer.transcript_ready())
                && amux_ui::claude::send_gate(model, pty) == amux_ui::claude::SendGate::Ready
        })
        .await;
        assert!(runtime.model().claude_sdk(pty).is_none());
        succeeded(
            &mut runtime,
            UiCommand::Send {
                agent: pty,
                draft: Draft::plain("PTY prompt", vec![]),
            },
        )
        .await;
        wait_for(&mut runtime, "PTY response", |model| {
            model.claude(pty).unwrap().entries().any(|row| {
                matches!(&row.kind, amux_ui::claude::FeedEntryKind::Message(message)
                if message.segments.iter().any(|s| s == "The PTY session received your prompt."))
            })
        })
        .await;
        let pty_before = control
            .ack(json!({"AgentObserve":{"agent":"existing-pty"}}))
            .await;
        assert_eq!(
            control
                .ack(json!({"AgentObserve":{"agent":pty.to_string()}}))
                .await,
            pty_before
        );
        assert_eq!(pty_before["observed"].as_array().unwrap().len(), 1);
        assert_eq!(pty_before["observed"][0]["text"], "PTY prompt");
        assert_eq!(
            amux_ui::provider::settings_gate(runtime.model(), pty),
            amux_ui::provider::SettingsGate::PtySettingsUnavailable
        );
        let op = runtime.dispatch(UiCommand::SetModel {
            agent: pty,
            model: "haiku".into(),
        });
        wait_for(&mut runtime, "PTY model refused", |model| {
            model.finished_op(op).is_some()
        })
        .await;
        assert!(runtime.model().finished_op(op).unwrap().outcome.is_error());
        let amux_ui::OpOutcome::Error {
            error: amux_ui::OpError::General { message, .. },
        } = &runtime.model().finished_op(op).unwrap().outcome
        else {
            panic!("expected a named PTY gate refusal")
        };
        assert_eq!(
            message,
            "model, effort and preset changes are unavailable for Claude PTY sessions"
        );
        assert_eq!(
            control
                .ack(json!({"AgentObserve":{"agent":"existing-pty"}}))
                .await,
            pty_before
        );

        let mut refused = request;
        refused.agent_id = Uuid::new_v4();
        refused.name = Some("refused-sdk".into());
        refused.working_dir.push("does-not-exist-for-sdk-journey");
        assert!(client.create_agent(refused).await.is_err());
        let after = control.ack(json!({"Inventory":{"daemon":"laptop"}})).await;
        assert_eq!(after["agents"], before["agents"]);
        control.ack(json!("Shutdown")).await;
    };
    let (result, ()) = tokio::join!(server, journey);
    result.unwrap();
    assert!(TcpStream::connect(ready.relay).await.is_err());
}
