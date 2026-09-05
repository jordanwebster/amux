use std::collections::HashMap;
use std::time::Duration;

use super::*;

fn options(root: InstallationRoot, listeners: Listeners) -> InstallationOptions {
    InstallationOptions {
        root,
        listeners,
        credentials: CredentialSource::ProfileFiles,
        identity_http: reqwest::Client::new(),
        settings: InstallationSettings {
            host_name: "installation-test".into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: PathBuf::new(),
            minimum_client_versions: HashMap::new(),
            status_reporters: Default::default(),
        },
    }
}
async fn installation() -> Installation {
    Installation::open(options(
        InstallationRoot::InMemory,
        Listeners::InProcessOnly,
    ))
    .await
    .unwrap()
}
async fn create(installation: &Installation, label: &str) -> ProfileStatus {
    let profile = installation
        .create(OperationId::new(), Some(label.into()))
        .await
        .unwrap();
    assert!(profile.available, "{profile:?}");
    profile
}
async fn event(watch: &mut ProfileWatch) -> ProfileEvent {
    tokio::time::timeout(Duration::from_secs(5), watch.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn replayed_create_returns_the_original_result_even_after_rename() {
    let installation = installation().await;
    let op = OperationId::new();
    let (first, replay) = tokio::join!(
        installation.create(op, Some("work".into())),
        installation.create(op, Some("work".into()))
    );
    let first = first.unwrap();
    assert_eq!(first, replay.unwrap());
    installation
        .rename(
            OperationId::new(),
            first.record.id,
            first.record.revision,
            Some("renamed".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        installation.create(op, Some("work".into())).await.unwrap(),
        first
    );
    assert_eq!(installation.profiles().len(), 1);
    assert!(
        installation
            .create(op, Some("different".into()))
            .await
            .is_err()
    );
    println!("Concurrent create and replay after rename: one profile, original result retained");
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn stale_revision_rename_and_replayed_error_leave_newer_name_intact() {
    let installation = installation().await;
    let first = create(&installation, "work").await;
    let renamed = installation
        .rename(
            OperationId::new(),
            first.record.id,
            1,
            Some("office".into()),
        )
        .await
        .unwrap();
    assert_eq!(renamed.record.revision, 2);
    let stale = OperationId::new();
    for _ in 0..2 {
        assert!(matches!(
            installation.rename(stale, first.record.id, 1, None).await,
            Err(InstallationError::RevisionMismatch {
                expected: 1,
                actual: 2
            })
        ));
    }
    assert_eq!(
        installation.profiles()[0]
            .record
            .label
            .override_name
            .as_deref(),
        Some("office")
    );
    println!("Stale rename refused: expected revision 1, actual 2; office remains");
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn delete_closes_open_clients_and_rejects_late_mutations() {
    let installation = installation().await;
    let first = create(&installation, "personal").await;
    let second = create(&installation, "work").await;
    let client = installation.client(first.record.id).unwrap();
    let cloned = client.clone();
    client.list_agents().await.unwrap();
    let other = installation.client(second.record.id).unwrap();
    assert!(matches!(
        installation
            .delete(OperationId::new(), first.record.id, 2)
            .await,
        Err(InstallationError::RevisionMismatch { .. })
    ));
    client.list_agents().await.unwrap();
    let op = OperationId::new();
    installation.delete(op, first.record.id, 1).await.unwrap();
    installation.delete(op, first.record.id, 1).await.unwrap();
    assert!(client.list_agents().await.is_err());
    assert!(cloned.list_agents().await.is_err());
    assert!(matches!(
        installation.client(first.record.id),
        Err(InstallationError::Deleted(_))
    ));
    assert!(matches!(
        installation
            .resume(OperationId::new(), first.record.id)
            .await,
        Err(InstallationError::Deleted(_))
    ));
    assert!(
        !installation
            .inner
            .root
            .join("profiles")
            .join(first.record.id.to_string())
            .exists()
    );
    other.list_agents().await.unwrap();
    println!(
        "Delete: both open clients close, late resume returns Deleted, work still lists agents"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
    assert!(other.list_agents().await.is_err());
}

#[tokio::test]
async fn watch_snapshot_ordered_changes_removed_and_lagged() {
    let installation = installation().await;
    let profile = create(&installation, "work").await;
    let mut watch = installation.watch();
    let sequence = match event(&mut watch).await {
        ProfileEvent::Upserted {
            sequence,
            profile: snapshot,
        } => {
            assert_eq!(*snapshot, profile);
            sequence
        }
        event => panic!("unexpected {event:?}"),
    };
    assert_eq!(
        event(&mut watch).await,
        ProfileEvent::SnapshotComplete { sequence }
    );
    installation
        .rename(
            OperationId::new(),
            profile.record.id,
            1,
            Some("office".into()),
        )
        .await
        .unwrap();
    installation
        .delete(OperationId::new(), profile.record.id, 2)
        .await
        .unwrap();
    let mut previous = sequence;
    loop {
        let change = event(&mut watch).await;
        println!("{change:?}");
        match change {
            ProfileEvent::Upserted { sequence, .. } => {
                assert_eq!(sequence, previous + 1);
                previous = sequence;
            }
            ProfileEvent::Removed { sequence, id } => {
                assert_eq!(id, profile.record.id);
                assert_eq!(sequence, previous + 1);
                break;
            }
            event => panic!("unexpected {event:?}"),
        }
    }
    let profile = create(&installation, "personal").await;
    let mut slow = installation.watch();
    event(&mut slow).await;
    event(&mut slow).await;
    // Runtime observations use the same ordered publication path as intent.
    for _ in 0..WATCH_CAPACITY + 1 {
        installation
            .inner
            .state
            .lock()
            .unwrap()
            .publish(profile.record.id);
    }
    assert_eq!(event(&mut slow).await, ProfileEvent::Lagged);
    assert!(slow.recv().await.is_none());
    let mut fresh = installation.watch();
    assert!(matches!(
        event(&mut fresh).await,
        ProfileEvent::Upserted { .. }
    ));
    assert!(matches!(
        event(&mut fresh).await,
        ProfileEvent::SnapshotComplete { .. }
    ));
    println!("Slow watcher ends with Lagged; resubscription gets a fresh snapshot");
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn pause_survives_restart_and_preserves_identity_and_local_calls() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let open = || {
        Installation::open(options(
            InstallationRoot::OnDisk(root.path().into()),
            Listeners::InProcessOnly,
        ))
    };
    let installation = open().await.unwrap();
    let profile = create(&installation, "work").await;
    let paused = installation
        .pause(OperationId::new(), profile.record.id)
        .await
        .unwrap();
    assert_eq!(paused.intent, Intent::Paused);
    installation
        .client(profile.record.id)
        .unwrap()
        .list_agents()
        .await
        .unwrap();
    installation.shutdown(ShutdownReason::UserRequested).await;
    let installation = open().await.unwrap();
    assert_eq!(installation.profiles()[0].intent, Intent::Paused);
    assert_eq!(installation.profiles()[0].host_id, profile.host_id);
    let resumed = installation
        .resume(OperationId::new(), profile.record.id)
        .await
        .unwrap();
    assert_eq!(resumed.intent, Intent::Unbound);
    assert_eq!(resumed.observed, Observed::Local);
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[cfg(unix)]
#[tokio::test]
async fn startup_failure_leaves_other_profile_serving_and_delete_closes_socket_clients() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let first = ProfileId::new();
    let second = ProfileId::new();
    {
        let mut registry = Registry::open(InstallationRoot::OnDisk(root.path().into())).unwrap();
        registry.create(first, ProfileLabel::default()).unwrap();
        registry.create(second, ProfileLabel::default()).unwrap();
    }
    let bad = ProfilePaths::for_id(root.path(), first).unwrap();
    let existing = std::os::unix::net::UnixListener::bind(&bad.socket_path).unwrap();
    let installation = Installation::open(options(
        InstallationRoot::OnDisk(root.path().into()),
        Listeners::Sockets,
    ))
    .await
    .unwrap();
    let failed = installation
        .profiles()
        .into_iter()
        .find(|p| p.record.id == first)
        .unwrap();
    assert_eq!(failed.observed, Observed::StartupFailed);
    assert!(failed.startup_error.is_some());
    assert!(installation.client(first).is_err());
    std::os::unix::net::UnixStream::connect(&bad.socket_path).unwrap();
    let good = installation
        .profiles()
        .into_iter()
        .find(|p| p.record.id == second)
        .unwrap();
    let config = crate::config::Config {
        socket_path: good.socket_path.clone().unwrap(),
        ..Default::default()
    };
    let channel = crate::client::connect_existing_client_service(&config)
        .await
        .unwrap();
    let client = Client::from_client_service_channel(channel, None);
    client.list_agents().await.unwrap();
    installation
        .delete(OperationId::new(), second, 1)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), client.list_agents())
            .await
            .unwrap()
            .is_err()
    );
    println!(
        "Occupied personal socket: StartupFailed; work socket lists agents; delete closes accepted gRPC client"
    );
    drop(existing);
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn cancelling_a_caller_does_not_cancel_or_duplicate_its_mutation() {
    let installation = installation().await;
    let profile = create(&installation, "work").await;
    let slot = installation.inner.state.lock().unwrap().profiles[&profile.record.id]
        .slot
        .clone();
    let guard = slot.operations.lock().await;
    let op = OperationId::new();
    let mut rename = Box::pin(installation.rename(op, profile.record.id, 1, Some("office".into())));
    assert!(futures_util::poll!(rename.as_mut()).is_pending());
    drop(rename);
    assert!(
        installation
            .inner
            .state
            .lock()
            .unwrap()
            .operations
            .contains_key(&op)
    );
    let other = create(&installation, "personal").await;
    installation
        .client(other.record.id)
        .unwrap()
        .list_agents()
        .await
        .unwrap();
    drop(guard);
    let replay = installation
        .rename(op, profile.record.id, 1, Some("office".into()))
        .await
        .unwrap();
    assert_eq!(replay.record.revision, 2);
    assert_eq!(installation.profiles().len(), 2);
    println!(
        "Dropped rename caller: retry receives its completed revision 2; another profile serves throughout"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn failed_delete_remains_unavailable_after_restart_and_cleanup_is_retryable() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let open = || {
        Installation::open(options(
            InstallationRoot::OnDisk(root.path().into()),
            Listeners::InProcessOnly,
        ))
    };
    let installation = open().await.unwrap();
    let profile = create(&installation, "work").await;
    let directory = installation
        .inner
        .root
        .join("profiles")
        .join(profile.record.id.to_string());
    let retained = installation.inner.root.join("retained");
    // Inject a real cleanup error after identity creation. The persisted deletion
    // marker, rather than the contents of the directory, determines availability.
    std::fs::rename(&directory, &retained).unwrap();
    std::fs::write(&directory, "cleanup obstacle").unwrap();
    assert!(
        installation
            .delete(OperationId::new(), profile.record.id, 1)
            .await
            .is_err()
    );
    assert!(!installation.profiles()[0].available);
    assert!(
        installation.profiles()[0]
            .startup_error
            .as_ref()
            .unwrap()
            .contains("cleanup failed")
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(&retained, &directory).unwrap();
    let installation = open().await.unwrap();
    assert!(!installation.profiles()[0].available);
    assert!(matches!(
        installation.client(profile.record.id),
        Err(InstallationError::Deleted(_))
    ));
    assert!(matches!(
        installation
            .rename(OperationId::new(), profile.record.id, 1, None)
            .await,
        Err(InstallationError::Deleted(_))
    ));
    installation
        .delete(OperationId::new(), profile.record.id, 1)
        .await
        .unwrap();
    assert!(!directory.exists());
    assert!(installation.profiles().is_empty());
    println!(
        "Interrupted delete stays unavailable after reopen; retry removes retained identity and registry entry"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn lifecycle_waits_for_profile_mutations_and_closed_gate_rejects_late_agent_work() {
    let installation = installation().await;
    let profile = create(&installation, "work").await;
    let slot = installation.inner.state.lock().unwrap().profiles[&profile.record.id]
        .slot
        .clone();
    let agent = slot
        .runtime
        .lock()
        .await
        .as_ref()
        .unwrap()
        .services
        .agent
        .clone();
    let guard = slot.operations.lock().await;
    let op = OperationId::new();
    let mut delete = Box::pin(installation.delete(op, profile.record.id, 1));
    assert!(futures_util::poll!(delete.as_mut()).is_pending());
    let other = create(&installation, "personal").await;
    installation
        .client(other.record.id)
        .unwrap()
        .list_agents()
        .await
        .unwrap();
    let mut rename = Box::pin(agent.rename(crate::agents::RenameAgentRequest {
        agent_id: Uuid::new_v4(),
        name: "late".into(),
    }));
    assert!(futures_util::poll!(rename.as_mut()).is_pending());
    drop(rename);
    drop(guard);
    delete.await.unwrap();
    let error = agent
        .rename(crate::agents::RenameAgentRequest {
            agent_id: Uuid::new_v4(),
            name: "late".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::protocol::ProtocolError::FailedPrecondition { .. }
    ));
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn bound_profiles_start_independently_and_pause_cancels_only_its_connector() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[derive(Default)]
    struct Credentials {
        active: Arc<AtomicUsize>,
        calls: AtomicUsize,
    }
    struct Active(Arc<AtomicUsize>);
    impl Drop for Active {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    #[async_trait::async_trait]
    impl CredentialProvider for Credentials {
        async fn access_token(&self) -> Result<crate::auth::AccessToken, crate::auth::AuthError> {
            self.active.fetch_add(1, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _active = Active(self.active.clone());
            std::future::pending().await
        }
        fn invalidate(&self, _: &crate::auth::AccessToken) {}
    }
    async fn wait_for_calls(credentials: &Credentials, count: usize) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while credentials.calls.load(Ordering::SeqCst) < count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let ids = [ProfileId::new(), ProfileId::new()];
    {
        let mut registry = Registry::open(InstallationRoot::OnDisk(root.path().into())).unwrap();
        for (id, subject) in ids.into_iter().zip(["personal", "work"]) {
            let mut record = registry.create(id, ProfileLabel::default()).unwrap();
            record.binding = Some(super::super::Binding {
                account: super::super::registry::AccountId {
                    service: super::super::CloudServiceId::canonicalize("http://127.0.0.1:1")
                        .unwrap(),
                    subject: subject.into(),
                },
                bound_at: chrono::Utc::now(),
            });
            registry.replace(record).unwrap();
        }
    }
    let personal = Arc::new(Credentials::default());
    let work = Arc::new(Credentials::default());
    let providers = HashMap::from([(ids[0], personal.clone()), (ids[1], work.clone())]);
    let mut opts = options(
        InstallationRoot::OnDisk(root.path().into()),
        Listeners::InProcessOnly,
    );
    opts.credentials = CredentialSource::HostProvided(Arc::new(move |id| providers[&id].clone()));
    let installation = Installation::open(opts).await.unwrap();
    wait_for_calls(&personal, 1).await;
    wait_for_calls(&work, 1).await;
    assert!(
        installation
            .profiles()
            .iter()
            .all(|profile| profile.available && profile.observed == Observed::Connecting)
    );
    let paused = installation
        .pause(OperationId::new(), ids[0])
        .await
        .unwrap();
    assert_eq!(paused.observed, Observed::Local);
    assert_eq!(personal.active.load(Ordering::SeqCst), 0);
    assert_eq!(work.active.load(Ordering::SeqCst), 1);
    installation
        .client(ids[0])
        .unwrap()
        .list_agents()
        .await
        .unwrap();
    installation
        .resume(OperationId::new(), ids[0])
        .await
        .unwrap();
    wait_for_calls(&personal, 2).await;
    installation
        .resume(OperationId::new(), ids[0])
        .await
        .unwrap();
    assert_eq!(personal.calls.load(Ordering::SeqCst), 2);
    assert_eq!(personal.active.load(Ordering::SeqCst), 1);
    println!(
        "Two bound profiles start despite hung token providers; pause cancels personal only; repeated resume keeps one connector"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
    assert_eq!(personal.active.load(Ordering::SeqCst), 0);
    assert_eq!(work.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn deletion_that_wins_startup_cannot_be_undone_by_the_late_start() {
    let installation = installation().await;
    let id = ProfileId::new();
    let record = installation
        .inner
        .state
        .lock()
        .unwrap()
        .registry
        .create(id, ProfileLabel::default())
        .unwrap();
    installation.inner.insert(record);
    let slot = installation.inner.state.lock().unwrap().profiles[&id]
        .slot
        .clone();
    let guard = slot.operations.lock().await;
    let mut delete = Box::pin(installation.delete(OperationId::new(), id, 1));
    assert!(futures_util::poll!(delete.as_mut()).is_pending());
    // The delete worker queues behind the held profile operation before start.
    tokio::task::yield_now().await;
    let mut start = Box::pin(installation.inner.start(id));
    assert!(futures_util::poll!(start.as_mut()).is_pending());
    drop(guard);
    let (deleted, started) = tokio::join!(delete, start);
    deleted.unwrap();
    assert!(matches!(started, Err(InstallationError::Deleted(_))));
    assert!(
        !installation
            .inner
            .root
            .join("profiles")
            .join(id.to_string())
            .exists()
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}
