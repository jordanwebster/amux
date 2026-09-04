//! Tier-2 integration: the Runtime shell against a real embedded server.
//!
//! The tier-1 spec suite proves the reducer; this proves the shell edges —
//! connection task, inventory pump, RPC effects — by asserting on the Model
//! after driving a live daemon.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use amux::{AgentIdentifier, ArtifactKind, Config, CreateAgentRequest, Server, claude_io};
use amux_artifacts::ARTIFACT_SIZE_CAP;
use amux_ui::{
    AgentPhase, AttachmentClient, AttachmentClientFuture, Attention, Command, DraftAttachment,
    InputPayload, Model, OpError, OpId, OpOutcome, Runtime, RuntimeOptions, execute_put_then_send,
};
use tempfile::tempdir;
use uuid::Uuid;

async fn wait_for(runtime: &mut Runtime, what: &str, predicate: impl Fn(&Model) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if predicate(runtime.model()) {
            return;
        }
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or_else(|| panic!("timed out waiting for {what}"));
        match tokio::time::timeout(remaining, runtime.next()).await {
            Ok(true) => {}
            Ok(false) => panic!("runtime shut down while waiting for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

async fn embedded_server_test_guard() -> tokio::sync::OwnedMutexGuard<()> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
        .lock_owned()
        .await
}

async fn create_test_agent(client: &amux::Client, working_dir: &Path) -> amux::Agent {
    client
        .create_agent(CreateAgentRequest {
            agent_id: Uuid::new_v4(),
            host_id: None,
            name: Some("attachment-stub".into()),
            agent_type: amux::AgentType::TestAgent {
                command: "cat".into(),
            },
            working_dir: working_dir.to_path_buf(),
            terminal_size: None,
            args: Vec::new(),
            parent: None,
            initial_prompt: None,
        })
        .await
        .expect("create attachment stub agent")
}

fn test_config(root: &Path) -> Config {
    Config {
        state_path: root.join("state.yaml"),
        socket_path: root.join("amux.sock"),
        data_dir: root.join("data"),
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    }
}

fn claude_input(text: &str) -> InputPayload {
    InputPayload::Claude {
        expected_seq: 0,
        intent: claude_io::Intent::Prompt {
            text: text.to_string(),
        },
        retry_stale: false,
    }
}

fn blob_path(root: &Path, agent: amux::AgentId, id: &amux::ArtifactId) -> PathBuf {
    root.join("data")
        .join("agents")
        .join(agent.to_string())
        .join("artifacts")
        .join("blobs")
        .join(id.as_str().strip_prefix("sha256:").expect("canonical id"))
}

#[derive(Default)]
struct AttachmentStub {
    calls: Mutex<Vec<String>>,
}

impl AttachmentClient for AttachmentStub {
    fn put_artifact<'a>(
        &'a self,
        _agent: AgentIdentifier,
        kind: ArtifactKind,
        name: &'a str,
        mime: &'a str,
        bytes: Vec<u8>,
    ) -> AttachmentClientFuture<'a, amux::ArtifactRef> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(format!("put:{name}"));
            if name == "oversized.bin" {
                return Err(amux::ClientError::Protocol(
                    amux::ProtocolError::AttachmentTooLarge {
                        size: ARTIFACT_SIZE_CAP + 1,
                        max: ARTIFACT_SIZE_CAP,
                    },
                ));
            }
            Ok(amux::ArtifactRef {
                id: amux_artifacts::id_of(&bytes),
                kind,
                name: name.to_string(),
                mime: mime.to_string(),
                size: u64::try_from(bytes.len()).unwrap(),
            })
        })
    }

    fn send_input(&self, request: amux::SendInputRequest) -> AttachmentClientFuture<'_, ()> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("send:{}", request.pin.join(",")));
            Ok(())
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    windows,
    ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
)]
async fn runtime_reflects_daemon_state_in_the_model() {
    let _guard = embedded_server_test_guard().await;
    let dir = tempdir().unwrap();
    let config = Config {
        state_path: dir.path().join("state.yaml"),
        socket_path: dir.path().join("amux.sock"),
        enable_cloud_mode: Some(false),
        prevent_idle_sleep: Some(false),
        ..Config::default()
    };
    let client = Server::builder()
        .config(config)
        .embedded()
        .open()
        .await
        .unwrap();

    let mut runtime = Runtime::start_with_client(client, RuntimeOptions::default());

    wait_for(&mut runtime, "snapshot synchronization", |model| {
        model.is_synchronized()
    })
    .await;
    assert!(runtime.model().host_count() >= 1, "the daemon lists itself");

    let op = runtime.dispatch(Command::CreateAgent {
        host: None,
        name: "ui-integration".to_string(),
        agent_type: amux::AgentType::TestAgent {
            command: "cat".to_string(),
        },
        working_dir: std::env::temp_dir(),
    });

    wait_for(&mut runtime, "create op to finish", move |model| {
        model.finished_op(op).is_some()
    })
    .await;
    let finished = runtime.model().finished_op(op).unwrap();
    assert!(
        !finished.outcome.is_error(),
        "create failed: {:?}",
        finished.outcome
    );

    wait_for(&mut runtime, "agent to appear via subscription", |model| {
        model
            .agents()
            .any(|card| card.agent.name.as_deref() == Some("ui-integration"))
    })
    .await;
    let card = runtime
        .model()
        .agents()
        .find(|card| card.agent.name.as_deref() == Some("ui-integration"))
        .unwrap();
    assert_eq!(card.attention, Attention::Unknown);
    assert_eq!(card.phase, AgentPhase::Running);
    let agent_id = card.agent.id;

    let delete = runtime.dispatch(Command::DeleteAgent { agent: agent_id });
    wait_for(&mut runtime, "delete op to finish", move |model| {
        model.finished_op(delete).is_some()
    })
    .await;
    wait_for(&mut runtime, "agent to disappear", move |model| {
        model.agent(agent_id).is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_puts_finish_before_one_send_and_a_failed_put_stops_the_operation() {
    let client = AttachmentStub::default();
    let agent = Uuid::new_v4();
    let draft = DraftAttachment::from_bytes(
        ArtifactKind::File,
        "note.txt",
        "text/plain",
        b"put before send".to_vec(),
    );
    let outcome = execute_put_then_send(
        &client,
        OpId(Uuid::new_v4()),
        agent,
        vec![draft.clone()],
        claude_input("stub send"),
        vec![draft.id.clone()],
    )
    .await;
    assert_eq!(outcome, OpOutcome::InputSent);
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![format!("put:{}", draft.name), format!("send:{}", draft.id)]
    );

    let too_large = DraftAttachment::from_bytes(
        ArtifactKind::File,
        "oversized.bin",
        "application/octet-stream",
        b"stub rejects by name".to_vec(),
    );
    let never_put = DraftAttachment::from_bytes(
        ArtifactKind::File,
        "later.txt",
        "text/plain",
        b"must not be stored".to_vec(),
    );
    let outcome = execute_put_then_send(
        &client,
        OpId(Uuid::new_v4()),
        agent,
        vec![too_large.clone(), never_put.clone()],
        claude_input("must not send"),
        vec![too_large.id.clone(), never_put.id.clone()],
    )
    .await;
    assert!(matches!(
        outcome,
        OpOutcome::Error {
            error: OpError::AttachmentTooLarge { name, size, max }
        } if name == "oversized.bin" && size == ARTIFACT_SIZE_CAP + 1 && max == ARTIFACT_SIZE_CAP
    ));
    assert_eq!(
        *client.calls.lock().unwrap(),
        vec![
            format!("put:{}", draft.name),
            format!("send:{}", draft.id),
            "put:oversized.bin".to_string(),
        ],
        "a failed put produces no later put or send call"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_open_uses_one_persistent_cache_and_refetches_tampering() {
    let _guard = embedded_server_test_guard().await;
    let dir = tempdir().unwrap();
    let config = test_config(dir.path());
    let client = Server::builder()
        .config(config)
        .embedded()
        .open()
        .await
        .unwrap();
    let agent = create_test_agent(&client, dir.path()).await;
    let bytes = b"cache me once".to_vec();
    let artifact = client
        .put_artifact(
            AgentIdentifier::Id(agent.id),
            ArtifactKind::File,
            "cached.txt",
            "text/plain",
            bytes.clone(),
        )
        .await
        .unwrap();
    let cache_root = dir.path().join("viewer-cache");
    let opened = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let opener = {
        let opened = opened.clone();
        Arc::new(move |meta: &amux_artifacts::ArtifactMeta, path: &Path| {
            assert_eq!(meta.name, "cached.txt");
            opened.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }) as amux_ui::AttachmentOpener
    };
    let options = || RuntimeOptions {
        artifact_cache: Some(cache_root.clone()),
        artifact_cache_bound: 1024 * 1024,
        attachment_opener: opener.clone(),
        ..RuntimeOptions::default()
    };

    let mut runtime = Runtime::start_with_client(client.clone(), options());
    wait_for(
        &mut runtime,
        "attachment runtime synchronization",
        |model| model.is_synchronized(),
    )
    .await;
    let first = runtime.dispatch(Command::OpenAttachment {
        agent: agent.id,
        id: artifact.id.clone(),
    });
    wait_for(&mut runtime, "first attachment open", move |model| {
        model.finished_op(first).is_some()
    })
    .await;
    assert!(matches!(
        runtime.model().finished_op(first).unwrap().outcome,
        OpOutcome::AttachmentOpened { .. }
    ));

    let owner_blob = blob_path(dir.path(), agent.id, &artifact.id);
    let held_owner_blob = dir.path().join("held-owner-blob");
    std::fs::rename(&owner_blob, &held_owner_blob).unwrap();
    let second = runtime.dispatch(Command::OpenAttachment {
        agent: agent.id,
        id: artifact.id.clone(),
    });
    wait_for(&mut runtime, "cached attachment open", move |model| {
        model.finished_op(second).is_some()
    })
    .await;
    assert!(matches!(
        runtime.model().finished_op(second).unwrap().outcome,
        OpOutcome::AttachmentOpened { .. }
    ));
    std::fs::rename(&held_owner_blob, &owner_blob).unwrap();

    let cache_blob = opened.lock().unwrap()[0].clone();
    std::fs::write(&cache_blob, b"tampered").unwrap();
    let third = runtime.dispatch(Command::OpenAttachment {
        agent: agent.id,
        id: artifact.id.clone(),
    });
    wait_for(&mut runtime, "tampered attachment refetch", move |model| {
        model.finished_op(third).is_some()
    })
    .await;
    assert!(matches!(
        runtime.model().finished_op(third).unwrap().outcome,
        OpOutcome::AttachmentOpened { .. }
    ));
    assert_eq!(std::fs::read(&cache_blob).unwrap(), bytes);

    std::fs::rename(&owner_blob, &held_owner_blob).unwrap();
    drop(runtime);
    let mut reopened = Runtime::start_with_client(client.clone(), options());
    wait_for(&mut reopened, "reopened runtime synchronization", |model| {
        model.is_synchronized()
    })
    .await;
    let fourth = reopened.dispatch(Command::OpenAttachment {
        agent: agent.id,
        id: artifact.id.clone(),
    });
    wait_for(&mut reopened, "persisted attachment open", move |model| {
        model.finished_op(fourth).is_some()
    })
    .await;
    assert!(matches!(
        reopened.model().finished_op(fourth).unwrap().outcome,
        OpOutcome::AttachmentOpened { .. }
    ));
    assert_eq!(opened.lock().unwrap().len(), 4);
    std::fs::rename(&held_owner_blob, &owner_blob).unwrap();
    client.delete_agent(agent.id).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_fetch_and_diff_preserve_typed_runtime_outcomes() {
    let _guard = embedded_server_test_guard().await;
    let dir = tempdir().unwrap();
    let client = Server::builder()
        .config(test_config(dir.path()))
        .embedded()
        .open()
        .await
        .unwrap();
    let agent = create_test_agent(&client, dir.path()).await;
    let patch = "diff --git a/a b/a\n+new\n";
    let artifact = client
        .put_artifact(
            AgentIdentifier::Id(agent.id),
            ArtifactKind::Diff,
            "review.diff",
            "text/x-diff",
            patch.as_bytes().to_vec(),
        )
        .await
        .unwrap();
    let mut runtime = Runtime::start_with_client(
        client.clone(),
        RuntimeOptions {
            artifact_cache: Some(dir.path().join("review-cache")),
            ..RuntimeOptions::default()
        },
    );
    wait_for(&mut runtime, "review runtime synchronization", |model| {
        model.is_synchronized()
    })
    .await;
    let fetch = runtime.dispatch(Command::FetchDiff {
        agent: agent.id,
        id: artifact.id.clone(),
    });
    wait_for(&mut runtime, "diff artifact fetch", move |model| {
        model.finished_op(fetch).is_some()
    })
    .await;
    assert!(matches!(
        &runtime.model().finished_op(fetch).unwrap().outcome,
        OpOutcome::DiffFetched { id, patch: fetched }
            if id == &artifact.id && fetched == patch
    ));

    let diff = runtime.dispatch(Command::RequestDiff {
        agent: agent.id,
        base: amux::DiffBase::WorkingTree,
    });
    wait_for(&mut runtime, "unavailable diff", move |model| {
        model.finished_op(diff).is_some()
    })
    .await;
    assert!(matches!(
        &runtime.model().finished_op(diff).unwrap().outcome,
        OpOutcome::Error {
            error: OpError::DiffUnavailable { .. }
        }
    ));
    client.delete_agent(agent.id).await.unwrap();
}
