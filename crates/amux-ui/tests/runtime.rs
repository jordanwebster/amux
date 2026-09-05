//! Tier-2 integration: the Runtime shell against a real embedded server.
//!
//! The tier-1 spec suite proves the reducer; this proves the shell edges —
//! connection task, inventory pump, RPC effects — by asserting on the Model
//! after driving a live daemon.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use amux::{AgentIdentifier, ArtifactKind, CreateAgentRequest, claude_io};
use amux_artifacts::ARTIFACT_SIZE_CAP;
use amux_ui::{
    AgentPhase, AttachmentClient, AttachmentClientFuture, Attention, Command, DraftAttachment,
    InputPayload, Model, Msg, OpError, OpId, OpOutcome, Runtime, RuntimeOptions,
    execute_put_then_send,
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

async fn installation_client() -> (amux::Installation, amux::Client, PathBuf, tempfile::TempDir) {
    let disk_root = amux::test_fixtures::short_installation_root();
    let installation = amux::Installation::open(amux::InstallationOptions {
        root: amux::InstallationRoot::OnDisk(disk_root.path().into()),
        settings: amux::InstallationSettings {
            host_name: "ui-test".into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: PathBuf::new(),
            minimum_client_versions: Default::default(),
            update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
            status_reporters: Default::default(),
        },
        listeners: amux::Listeners::InProcessOnly,
        credentials: amux::CredentialSource::ProfileFiles,
        identity_http: Default::default(),
    })
    .await
    .unwrap();
    let id = installation
        .create(amux::OperationId::new(), None)
        .await
        .unwrap()
        .record
        .id;
    let client = installation.client(id).unwrap();
    let root = installation.root().join("profiles").join(id.to_string());
    (installation, client, root, disk_root)
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
    let (installation, client, _, _root) = installation_client().await;

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
    installation
        .shutdown(amux::ShutdownReason::UserRequested)
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
    let (installation, client, profile_root, _root) = installation_client().await;
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

    let owner_blob = blob_path(&profile_root, agent.id, &artifact.id);
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
    installation
        .shutdown(amux::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attachments_fetch_and_diff_preserve_typed_runtime_outcomes() {
    let _guard = embedded_server_test_guard().await;
    let dir = tempdir().unwrap();
    let (installation, client, _, _root) = installation_client().await;
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
    installation
        .shutdown(amux::ShutdownReason::UserRequested)
        .await;
}

/// Switching accounts is not reconnecting: everything the previous profile
/// was still saying has to be dropped, not folded into the account the user
/// moved to.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    windows,
    ignore = "agent PTY teardown hangs under ConPTY, like the disabled Windows e2e leg"
)]
async fn switcher_rejects_late_results() {
    use amux::installation::FrontDoor;
    use amux_ui::{LateResult, ProfileDirectory, ServerMsg, StreamMsg};

    let _guard = embedded_server_test_guard().await;
    let dir = tempdir().unwrap();
    // The artifact cache reports canonical paths; on macOS the temporary root
    // is reached through a symlink, so compare against the resolved root.
    let root = dir.path().canonicalize().unwrap();
    let (installation, _root) = socketed_installation().await;
    let installation = Arc::new(installation);
    let personal = new_profile(&installation, "Personal").await;
    let work = new_profile(&installation, "Work").await;

    // The switcher reads the directory through the front door. A profile's
    // own client API knows nothing about its neighbours.
    let front_socket = installation.root().join("amux.sock");
    let front = FrontDoor::new(installation.clone(), Some(front_socket.clone()));
    let front_listener = front.listen().unwrap();
    let entries = ProfileDirectory::connect(&front_socket)
        .await
        .unwrap()
        .list()
        .await
        .unwrap();
    let entry = |id: amux::ProfileId| {
        entries
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("{id:?} is missing from the switcher directory"))
            .clone()
    };
    let personal_entry = entry(personal);
    let work_entry = entry(work);
    assert_eq!(personal_entry.label, "Personal");
    assert_eq!(work_entry.label, "Work");
    assert!(personal_entry.socket != work_entry.socket);

    let personal_client = installation.client(personal).unwrap();
    let work_client = installation.client(work).unwrap();
    let personal_agent = create_test_agent(&personal_client, dir.path()).await;
    let work_agent = create_test_agent(&work_client, dir.path()).await;
    let personal_artifact = personal_client
        .put_artifact(
            AgentIdentifier::Id(personal_agent.id),
            ArtifactKind::File,
            "personal.txt",
            "text/plain",
            b"belongs to the personal account".to_vec(),
        )
        .await
        .unwrap();
    let work_artifact = work_client
        .put_artifact(
            AgentIdentifier::Id(work_agent.id),
            ArtifactKind::File,
            "work.txt",
            "text/plain",
            b"belongs to the work account".to_vec(),
        )
        .await
        .unwrap();

    let opened = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let bindings = ProfileBindings::new(&root, opened.clone());
    let mut runtime = Runtime::start(
        socket_connector(&personal_entry.socket),
        bindings.options("personal", false),
    );
    wait_for(&mut runtime, "personal synchronization", |model| {
        model.is_synchronized()
    })
    .await;
    wait_for(&mut runtime, "the personal account's agent", move |model| {
        model.agent(personal_agent.id).is_some()
    })
    .await;
    assert!(!runtime.model().cloud_subscription_required());

    // Real work for the personal account, left in flight across the switch:
    // an operation, an attachment fetch, and the inventory event the extra
    // agent raises. None of it is folded before the switch.
    let late_command = runtime.dispatch(Command::CreateAgent {
        host: None,
        name: "late-personal-agent".to_string(),
        agent_type: amux::AgentType::TestAgent {
            command: "cat".to_string(),
        },
        working_dir: dir.path().to_path_buf(),
    });
    let late_attachment = runtime.dispatch(Command::OpenAttachment {
        agent: personal_agent.id,
        id: personal_artifact.id.clone(),
    });
    let personal_edge = runtime.shell_edge();

    let mut runtime = runtime.switch(&work_entry, bindings.options("work", true));
    assert_eq!(runtime.generation(), amux_ui::Generation(1));
    // A new selection starts from nothing: no agents, not synchronized, no
    // record of any operation the previous account was running.
    assert_eq!(runtime.model().agent_count(), 0);
    assert!(!runtime.model().is_synchronized());
    assert!(runtime.model().finished_op(late_command).is_none());
    assert!(runtime.model().finished_op(late_attachment).is_none());

    // The personal account's edge outlives its runtime, exactly as an
    // in-flight task's does. Every kind of result it can still report has to
    // be dropped.
    let late = [
        Msg::Server(ServerMsg::AgentUpserted {
            agent: personal_agent.clone(),
        }),
        Msg::Stream {
            agent: personal_agent.id,
            event: StreamMsg::Opened { truncated: false },
        },
        Msg::OpResult {
            op: late_attachment,
            outcome: OpOutcome::AttachmentOpened {
                id: personal_artifact.id.clone(),
            },
        },
        Msg::OpResult {
            op: late_command,
            outcome: OpOutcome::AgentCreated {
                agent: personal_agent.clone(),
            },
        },
    ];
    for msg in late {
        personal_edge.report(msg).await.unwrap();
    }

    wait_for(&mut runtime, "the work account's agent", move |model| {
        model.agent(work_agent.id).is_some()
    })
    .await;
    for _ in 0..40 {
        runtime.drain();
        if runtime.discarded_late_results() >= 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert_eq!(
        runtime.discarded_late_kinds(),
        vec![
            LateResult::Inventory,
            LateResult::Session,
            LateResult::Attachment,
            LateResult::Command,
        ],
        "every shell edge of the personal account reported after the switch"
    );
    assert!(
        runtime.model().agent(personal_agent.id).is_none(),
        "the personal account's agent reached the work account's model"
    );
    assert!(
        runtime.model().stream(personal_agent.id).is_none(),
        "the personal account's session reached the work account's model"
    );
    assert!(runtime.model().finished_op(late_command).is_none());
    assert!(runtime.model().finished_op(late_attachment).is_none());
    assert!(
        runtime
            .model()
            .agents()
            .all(|card| card.agent.name.as_deref() != Some("late-personal-agent")),
        "an agent created for the personal account appeared in the work fleet"
    );

    // The new runtime is bound to the selected profile throughout, not only
    // in its connection: its subscription status, report directory and
    // artifact cache are the work account's.
    assert!(runtime.model().cloud_subscription_required());
    let report = runtime.report(amux_ui::DumpReason::UserRequested).unwrap();
    assert!(
        report.starts_with(root.join("work").join("reports")),
        "report landed outside the work account: {}",
        report.display()
    );
    let work_open = runtime.dispatch(Command::OpenAttachment {
        agent: work_agent.id,
        id: work_artifact.id.clone(),
    });
    wait_for(
        &mut runtime,
        "the work account's attachment",
        move |model| model.finished_op(work_open).is_some(),
    )
    .await;
    let cached = opened.lock().unwrap().last().cloned().unwrap();
    assert!(
        cached.starts_with(root.join("work").join("cache")),
        "attachment cached outside the work account: {}",
        cached.display()
    );

    drop(runtime);
    front_listener.stop().await;
    drop(front);
    Arc::try_unwrap(installation)
        .ok()
        .expect("the front door released the installation")
        .shutdown(amux::ShutdownReason::UserRequested)
        .await;
}

#[cfg(unix)]
/// Per-profile runtime bindings, laid out under one root so a report or a
/// cached attachment names the profile it belongs to.
struct ProfileBindings {
    root: PathBuf,
    opened: Arc<Mutex<Vec<PathBuf>>>,
}

#[cfg(unix)]
impl ProfileBindings {
    fn new(root: &Path, opened: Arc<Mutex<Vec<PathBuf>>>) -> Self {
        Self {
            root: root.to_path_buf(),
            opened,
        }
    }

    fn options(&self, profile: &str, subscription_required: bool) -> RuntimeOptions {
        let opened = self.opened.clone();
        let profile_root = self.root.join(profile);
        std::fs::create_dir_all(profile_root.join("reports")).unwrap();
        RuntimeOptions {
            local_host_id: None,
            report_dir: Some(profile_root.join("reports")),
            artifact_cache: Some(profile_root.join("cache")),
            artifact_cache_bound: 1024 * 1024,
            subscription_status_provider: Some(Arc::new(move || subscription_required)),
            attachment_opener: Arc::new(
                move |_meta: &amux_artifacts::ArtifactMeta, path: &Path| {
                    opened.lock().unwrap().push(path.to_path_buf());
                    Ok(())
                },
            ),
            ..RuntimeOptions::default()
        }
    }
}

#[cfg(unix)]
fn socket_connector(socket: &Path) -> amux_ui::Connector {
    let socket = socket.to_path_buf();
    Box::new(move || {
        let socket = socket.clone();
        Box::pin(async move {
            amux::Client::connect_socket(&socket)
                .await
                .map_err(|error| amux_ui::ConnectFailure {
                    message: error.to_string(),
                    auth_required: false,
                    subscription_required: false,
                })
        })
    })
}

#[cfg(unix)]
async fn socketed_installation() -> (amux::Installation, tempfile::TempDir) {
    let disk_root = amux::test_fixtures::short_installation_root();
    let installation = amux::Installation::open(amux::InstallationOptions {
        root: amux::InstallationRoot::OnDisk(disk_root.path().into()),
        settings: amux::InstallationSettings {
            host_name: "ui-switcher-test".into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: PathBuf::new(),
            minimum_client_versions: Default::default(),
            update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
            status_reporters: Default::default(),
        },
        listeners: amux::Listeners::Sockets,
        credentials: amux::CredentialSource::ProfileFiles,
        identity_http: Default::default(),
    })
    .await
    .unwrap();
    (installation, disk_root)
}

#[cfg(unix)]
async fn new_profile(installation: &amux::Installation, label: &str) -> amux::ProfileId {
    installation
        .create(amux::OperationId::new(), Some(label.to_string()))
        .await
        .unwrap()
        .record
        .id
}
