//! Public embedding contract, exercised through the same handles a host retains.

use std::sync::Arc;
use std::time::Duration;

use crate::{
    AccessToken, AuthError, CredentialProvider, CredentialSource, Installation,
    InstallationOptions, InstallationRoot, InstallationSettings, Listeners, OperationId,
    ProfileAdmin, ProfileEvent, ShutdownReason,
};

struct NoCredentials;

#[async_trait::async_trait]
impl CredentialProvider for NoCredentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        Err(AuthError::Unauthenticated)
    }
    fn invalidate(&self, _: &AccessToken) {}
}

fn options(root: &std::path::Path) -> InstallationOptions {
    InstallationOptions {
        root: InstallationRoot::OnDisk(root.into()),
        settings: InstallationSettings {
            repository_roots: Vec::new(),
            claude: crate::ClaudeSettings::default(),
            host_name: "embedded-host".into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: Default::default(),
            minimum_client_versions: Default::default(),
            update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
            status_reporters: Default::default(),
        },
        listeners: Listeners::InProcessOnly,
        credentials: CredentialSource::HostProvided(Arc::new(|_| Arc::new(NoCredentials))),
        identity_http: reqwest::Client::new(),
    }
}

#[tokio::test]
async fn background_profiles_outlive_every_screen_client() {
    let root = crate::test_fixtures::short_installation_root();
    let installation = Installation::open(options(root.path())).await.unwrap();
    let saved =
        crate::InstallationConfig::from_file(&installation.root().join("config.yaml")).unwrap();
    assert_eq!(
        saved.update_manifest_url,
        "http://127.0.0.1:1/manifest.json"
    );
    let mut ids = Vec::new();
    for name in ["personal", "work"] {
        let profile = installation
            .create(OperationId::new(), Some(name.into()))
            .await
            .unwrap();
        assert!(profile.available, "{profile:?}");
        assert!(profile.socket_path.is_none());
        let client = installation.client(profile.record.id).unwrap();
        let other = client.clone();
        client.list_agents().await.unwrap();
        drop(client);
        drop(other);
        ids.push((profile.record.id, profile.host_id));
    }
    assert_ne!(ids[0].1, ids[1].1);
    println!("Profile directory: {:?}", installation.profiles());
    installation.host_suspend().await;
    installation.host_suspend().await;
    for (id, host_id) in ids {
        let client = installation.client(id).unwrap();
        assert!(client.list_agents().await.unwrap().is_empty());
        let admin: ProfileAdmin = installation.admin(id).await.unwrap();
        assert!(admin.list_peers().await.unwrap().is_empty());
        assert_eq!(
            installation
                .profiles()
                .iter()
                .find(|p| p.record.id == id)
                .unwrap()
                .host_id,
            host_id
        );
    }
    installation.host_resume().await;
    installation.host_resume().await;
    println!(
        "Two distinct profile identities remain available after every screen client drops and repeated host suspension/resumption."
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[cfg(testnet)]
#[tokio::test]
async fn shutdown_yields_and_finishes_after_its_caller_is_cancelled() {
    let root = crate::test_fixtures::short_installation_root();
    let installation = Installation::open(options(root.path())).await.unwrap();
    let profile = installation.create(OperationId::new(), None).await.unwrap();
    let client = installation.client(profile.record.id).unwrap();
    let mut watch = installation.watch();
    while !matches!(
        watch.recv().await,
        Some(ProfileEvent::SnapshotComplete { .. })
    ) {}
    // Hold a runtime operation so teardown must yield on this single-thread executor.
    let runtime = installation.test_runtime(profile.record.id).await.unwrap();
    let weak_state = runtime.as_ref().unwrap().weak_state();
    let shutdown = tokio::spawn(installation.shutdown(ShutdownReason::UserRequested));
    let event = tokio::time::timeout(Duration::from_secs(3), watch.recv())
        .await
        .unwrap();
    assert!(matches!(event, Some(ProfileEvent::Upserted { profile, .. }) if !profile.available));
    assert!(!shutdown.is_finished());
    shutdown.abort();
    assert!(shutdown.await.unwrap_err().is_cancelled());
    drop(runtime);
    tokio::time::timeout(Duration::from_secs(3), async {
        while watch.recv().await.is_some() {}
    })
    .await
    .expect("owned teardown must finish after caller cancellation");
    assert!(weak_state.upgrade().is_none());
    assert!(client.list_agents().await.is_err());
    println!(
        "Shutdown yields while an operation is held, then closes clients and releases service state even after its caller is cancelled."
    );
}

#[cfg(test_fixtures)]
#[tokio::test]
async fn login_and_profile_resume_cannot_wake_a_suspended_host() {
    use crate::installation::{BindTarget, Intent, Observed};
    use crate::test_fixtures::{IdentityServer, TestAccount};

    let identity = IdentityServer::start(
        vec![TestAccount {
            sub: "work".into(),
            name: Some("Work".into()),
            email: Some("work@example.test".into()),
        }],
        None,
    )
    .await;
    let root = crate::test_fixtures::short_installation_root();
    let installation = Installation::open(options(root.path())).await.unwrap();
    installation.host_suspend().await;
    let profile = installation.create(OperationId::new(), None).await.unwrap();
    let id = profile.record.id;
    installation
        .bind(
            OperationId::new(),
            crate::BindRequest {
                target: BindTarget::Explicit(id),
                cloud_url: identity.url(),
                staged_refresh_token: identity.refresh_token_for("work"),
                adopt_non_pristine: false,
            },
        )
        .await
        .unwrap();
    installation.pause(OperationId::new(), id).await.unwrap();
    installation.resume(OperationId::new(), id).await.unwrap();
    let profile = installation.profiles().pop().unwrap();
    assert_eq!(profile.intent, Intent::Bound);
    assert_eq!(profile.observed, Observed::Local);
    println!("Suspended profile after login and profile resume: {profile:?}");
    installation
        .client(id)
        .unwrap()
        .list_agents()
        .await
        .unwrap();
    installation.host_resume().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if installation.profiles()[0].observed == Observed::AuthenticationRequired {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resume must ask the host provider for fresh credentials");
    assert_eq!(installation.profiles()[0].record.id, id);
    println!(
        "Login and per-profile resume retain a local-only profile while the host is suspended; host resume obtains fresh host credentials."
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}
