use amux::claude_io::{AskAnswer, PermissionAnswer};
use amux_ui::attachments::DraftAttachment;
use amux_ui::claude::{ClaudeCommand, FeedEntryKind, SendGate};
use amux_ui::{Command as UiCommand, Model, Runtime, RuntimeOptions};
use serde_json::json;

use super::*;

pub(super) async fn wait_for(
    runtime: &mut Runtime,
    what: &str,
    predicate: impl Fn(&Model) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate(runtime.model()) {
            assert!(runtime.next().await, "runtime closed waiting for {what}");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", runtime.model()));
}

fn has_text(model: &Model, agent: Uuid, text: &str) -> bool {
    model.claude(agent).is_some_and(|layer| layer.entries().any(|entry| {
        matches!(&entry.kind, FeedEntryKind::Message(message) if message.segments.iter().any(|s| s == text))
    }))
}

pub(super) async fn succeeded(runtime: &mut Runtime, command: UiCommand) {
    let op = runtime.dispatch(command);
    wait_for(runtime, "input outcome", |model| {
        model.finished_op(op).is_some()
    })
    .await;
    assert!(
        !runtime.model().finished_op(op).unwrap().outcome.is_error(),
        "{:?}",
        runtime.model().finished_op(op)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn testnet_agents_controls_and_runtime_over_authenticated_relay() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../e2e-tests/topologies/scripted-agents.json");
    let topology = Topology::load(&path).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let (net, ready, agents) = start(&topology, listener.local_addr().unwrap())
        .await
        .unwrap();
    eprintln!("readiness {}", serde_json::to_string(&ready).unwrap());
    let agent = ready.agents[0].agent_id;
    let client = amux::testnet::connect_user(ready.relay, ready.users[0].token.clone())
        .await
        .unwrap();
    let outsider = amux::testnet::connect_user(ready.relay, ready.users[1].token.clone())
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
        client
            .admin()
            .pair_qr_cloud_peer(qr.host_id, qr.secret)
            .await
            .unwrap();
        wait_for(&mut runtime, "paired agent inventory", |model| {
            model.agent(agent).is_some()
        })
        .await;
        runtime.note_attached(agent);
        // The transcript marker alone does not open the composer: the kernel
        // stream can still be replaying, and a prompt sent then is refused.
        // Wait for the gate a person would see before typing.
        wait_for(&mut runtime, "agent stream", |model| {
            model.is_synchronized()
                && model
                    .claude(agent)
                    .is_some_and(|layer| layer.transcript_ready())
                && amux_ui::claude::send_gate(model, agent) == SendGate::Ready
        })
        .await;
        assert!(
            outsider.list_agents().await.unwrap().is_empty(),
            "another account cannot see this host's agent"
        );
        assert_eq!(
            control
                .ack(json!({"AgentObserve":{"agent":"helper"}}))
                .await["observed"],
            json!([])
        );
        succeeded(
            &mut runtime,
            UiCommand::Claude(ClaudeCommand::SendPrompt {
                agent,
                text: "Please check the workspace".into(),
            }),
        )
        .await;
        wait_for(&mut runtime, "scripted response", |model| {
            has_text(model, agent, "I received the prompt through the relay.")
        })
        .await;
        let stale = client
            .send_input(amux::SendInputRequest {
                agent: agent.into(),
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: amux::claude_io::PTY_TRANSCRIPT_V1.into(),
                pin: vec![],
                payload: amux::claude_io::encode_pty_transcript_v1_input(
                    amux::claude_io::ClaudePtyTranscriptV1Input {
                        expected_seq: u64::MAX,
                        intent: amux::claude_io::Intent::Prompt {
                            text: "This stale input must not reach the provider".into(),
                        },
                    },
                )
                .into(),
            })
            .await;
        assert!(matches!(
            stale,
            Err(amux::ClientError::Protocol(
                amux::ProtocolError::SequenceNumberMismatch { .. }
            ))
        ));
        control.ack(json!({"AgentEmit":{"agent":"helper","rows":[{
            "type":"assistant", "uuid":Uuid::new_v4(),
            "message":{"id":"control-message","role":"assistant","content":[{"type":"text","text":"A row emitted over the control socket."}]}
        }]}})).await;
        wait_for(&mut runtime, "control row", |model| {
            has_text(model, agent, "A row emitted over the control socket.")
        })
        .await;
        control.ack(json!({"AgentRaiseAsk":{"agent":"helper","ask":{"Permission":{
            "tool":"Bash", "invocation":{"command":"pwd"}, "scoped_directories":["/workspace"]
        }}}})).await;
        // Not just the ask, but the transcript row that announces the tool it
        // is about: an ask reaches a reader on the hook and the row follows,
        // and an answer sent in between raced a session that had moved on and
        // is refused. Waiting for the pairing is waiting for the client to be
        // level with the host.
        wait_for(&mut runtime, "permission ask", |model| {
            let layer = model.claude(agent).unwrap();
            layer.ask_count() == 1
                && layer
                    .ask_head()
                    .is_some_and(|head| head.tool_use_id.is_some())
        })
        .await;
        let ask = runtime
            .model()
            .claude(agent)
            .unwrap()
            .ask_head()
            .unwrap()
            .clone();
        succeeded(
            &mut runtime,
            UiCommand::Claude(ClaudeCommand::AnswerAsk {
                agent,
                ask: ask.id,
                answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
            }),
        )
        .await;
        wait_for(&mut runtime, "answer response", |model| {
            has_text(model, agent, "Permission received on the host.")
        })
        .await;
        // A second ask in the same session, answered like the first.
        //
        // A permission is a tool the agent asked to use, and answering it has
        // to finish that tool: an ask that never closes stays at the head of
        // the queue, and every answer after it is refused as queued behind a
        // menu nobody can see. One ask per session is not a session.
        control.ack(json!({"AgentRaiseAsk":{"agent":"helper","ask":{"Permission":{
            "tool":"Bash", "invocation":{"command":"ls"}, "scoped_directories":["/workspace"]
        }}}})).await;
        let answered = ask.session_ask_id.clone();
        wait_for(&mut runtime, "the first ask to close and a second to arrive", |model| {
            let layer = model.claude(agent).unwrap();
            layer.ask_count() == 1
                && layer.ask_head().is_some_and(|head| {
                    head.session_ask_id != answered && head.tool_use_id.is_some()
                })
        })
        .await;
        let second = runtime
            .model()
            .claude(agent)
            .unwrap()
            .ask_head()
            .unwrap()
            .clone();
        succeeded(
            &mut runtime,
            UiCommand::Claude(ClaudeCommand::AnswerAsk {
                agent,
                ask: second.id,
                answer: AskAnswer::Permission(PermissionAnswer::AllowOnce),
            }),
        )
        .await;
        control
            .ack(json!({"AgentEndTurn":{"agent":"helper"}}))
            .await;
        wait_for(&mut runtime, "turn end", |model| {
            model
                .claude(agent)
                .unwrap()
                .entries()
                .any(|e| matches!(e.kind, FeedEntryKind::Turn(_)))
        })
        .await;
        let mut expected = json!([
            {"seq":1,"intent":"prompt","text":"Please check the workspace","ask_id":null,"answer":null,"pins":[]},
            {"seq":2,"intent":"answer","text":null,"ask_id":ask.session_ask_id,"answer":{"answer":"permission","permission":"allow_once"},"pins":[]},
            {"seq":3,"intent":"answer","text":null,"ask_id":second.session_ask_id,"answer":{"answer":"permission","permission":"allow_once"},"pins":[]}
        ]);
        assert_eq!(
            control
                .ack(json!({"AgentObserve":{"agent":"helper"}}))
                .await["observed"],
            expected
        );
        let unused = client
            .put_artifact(
                agent.into(),
                amux::ArtifactKind::File,
                "unused.txt",
                "text/plain",
                b"This stored draft is never attached".to_vec(),
            )
            .await
            .unwrap();
        let mut drafts = ["first", "second"].map(|name| {
            DraftAttachment::from_bytes(
                amux::ArtifactKind::File,
                format!("{name}.txt"),
                "text/plain",
                name.as_bytes().to_vec(),
            )
        });
        drafts.sort_by(|a, b| b.id.cmp(&a.id));
        for (index, attachments) in [vec![drafts[0].clone()], drafts.to_vec(), vec![]]
            .into_iter()
            .enumerate()
        {
            let text = format!("Attachment observation prompt {index}");
            let pins: Vec<_> = attachments
                .iter()
                .map(|draft| draft.id.to_string())
                .collect();
            succeeded(
                &mut runtime,
                UiCommand::SendPromptWithAttachments {
                    agent,
                    text: text.clone(),
                    attachments,
                },
            )
            .await;
            wait_for(&mut runtime, "attachment prompt turn end", |model| {
                model
                    .claude(agent)
                    .unwrap()
                    .entries()
                    .filter(|entry| matches!(entry.kind, FeedEntryKind::Turn(_)))
                    .count()
                    == index + 2
            })
            .await;
            // Numbered by what the host has already been told, not by this
            // loop: every input before this one counts, and an answer added
            // to the run above would otherwise shift each of these by one.
            let seq = expected.as_array().unwrap().len() + 1;
            expected.as_array_mut().unwrap().push(json!({
                "seq": seq, "intent": "prompt", "text": text,
                "ask_id": null, "answer": null, "pins": pins,
            }));
            let observed = control
                .ack(json!({"AgentObserve":{"agent":"helper"}}))
                .await;
            eprintln!(
                "attachment input observation {}",
                serde_json::to_string(&observed).unwrap()
            );
            assert_eq!(observed["observed"], expected);
            assert!(
                observed["observed"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|input| {
                        !input["pins"]
                            .as_array()
                            .unwrap()
                            .contains(&json!(unused.id))
                    })
            );
        }
        control
            .ack(json!({"AgentSpawnChild":{"agent":"helper","child":"child"}}))
            .await;
        wait_for(&mut runtime, "child inventory", |model| {
            model
                .agents()
                .any(|card| card.agent.name.as_deref() == Some("child"))
        })
        .await;
        let child = runtime
            .model()
            .agents()
            .find(|card| card.agent.name.as_deref() == Some("child"))
            .unwrap()
            .agent
            .clone();
        assert_eq!(
            child.parent,
            Some(amux::AgentParent {
                agent_id: agent,
                host_id: child.host_id
            })
        );
        assert_eq!(
            child.working_dir,
            runtime.model().agent(agent).unwrap().agent.working_dir
        );
        runtime.note_attached(child.id);
        control.ack(json!({"AgentRaiseAsk":{"agent":"child","ask":{"Plan":{"markdown":"Review this child plan."}}}})).await;
        wait_for(&mut runtime, "child ask on its own session", |model| {
            model.claude(child.id).is_some_and(|l| l.ask_count() == 1)
        })
        .await;
        assert_eq!(
            control.ack(json!({"AgentObserve":{"agent":"child"}})).await["observed"],
            json!([])
        );
        for request in [
            json!({"AgentObserve":{"agent":"missing"}}),
            json!({"AgentSpawnChild":{"agent":"helper","child":"child"}}),
            json!({"AgentSpawnChild":{"agent":"helper","child":"../invalid"}}),
            json!({"AgentExit":{"agent":"helper","code":-1}}),
        ] {
            assert!(control.request(request).await.get("Error").is_some());
        }
        eprintln!(
            "projected transcript {}",
            serde_json::to_string(
                &runtime
                    .model()
                    .claude(agent)
                    .unwrap()
                    .entries()
                    .collect::<Vec<_>>()
            )
            .unwrap()
        );
        control
            .ack(json!({"AgentExit":{"agent":"helper","code":7}}))
            .await;
        wait_for(&mut runtime, "exited session", |model| {
            matches!(
                model.agent(agent).unwrap().phase,
                amux_ui::AgentPhase::Exited { .. }
            )
        })
        .await;
        // The code the agent was asked to exit with, all the way through. The
        // end of the output stream is how a subscriber learns the agent has
        // gone and it carries no code, so this is the assertion that keeps the
        // daemon completing the close reason from the backend instead of
        // sending an empty one every reader has to guess at.
        assert_eq!(
            runtime.model().agent(agent).unwrap().phase,
            amux_ui::AgentPhase::Exited { exit_code: Some(7) }
        );
        assert_eq!(
            control
                .ack(json!({"AgentObserve":{"agent":"helper"}}))
                .await["observed"],
            expected
        );
        assert!(
            control
                .request(json!({"AgentEmit":{"agent":"helper","rows":[]}}))
                .await
                .get("Error")
                .is_some()
        );
        control.ack(json!({"RestartDaemon":{"name":"host"}})).await;
        assert!(
            control
                .request(json!({"AgentObserve":{"agent":"child"}}))
                .await
                .get("Error")
                .is_some()
        );
        control.ack(json!("Shutdown")).await;
    };
    let (result, ()) = tokio::join!(server, exercise);
    result.unwrap();
    assert!(TcpStream::connect(ready.relay).await.is_err());
    assert!(TcpStream::connect(ready.control).await.is_err());
    eprintln!("Claude journey cleanup verified: relay and control refuse connections");
}
