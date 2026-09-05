use super::*;

#[test]
fn canonicalization_accepts_only_origins() {
    for (input, expected) in [
        ("HTTPS://Cloud.Example:443/", "https://cloud.example"),
        ("http://Cloud.Example:80", "http://cloud.example"),
        ("https://cloud.example:8443", "https://cloud.example:8443"),
        ("http://[::1]:8080/", "http://[::1]:8080"),
    ] {
        assert_eq!(
            CloudServiceId::canonicalize(input).unwrap().as_str(),
            expected
        );
    }
    for input in [
        "https://cloud.example/a",
        "https://cloud.example/a/..",
        "https://cloud.example//",
        "https://cloud.example?",
        "https://cloud.example#",
        "https://user@cloud.example",
        "https://@cloud.example",
        "ftp://cloud.example",
        "cloud.example",
    ] {
        assert!(CloudServiceId::canonicalize(input).is_err(), "{input}");
    }
}

use std::sync::Arc;

use crate::auth::{AuthError, CredentialProvider, oauth};
use crate::installation::credentials::{ProfileCredentialStore, ValidatedCredential};
use crate::installation::{
    CredentialSource, Installation, InstallationOptions, InstallationRoot, InstallationSettings,
    Intent, Listeners, OperationId,
};
use crate::server::ShutdownReason;
use crate::test_fixtures::{Fault, IdentityServer, TestAccount};

async fn identity() -> IdentityServer {
    IdentityServer::start(
        ["alice", "bob"]
            .into_iter()
            .map(|sub| TestAccount {
                sub: sub.into(),
                name: Some(format!("{sub} Example")),
                email: Some(format!("{sub}@example.test")),
            })
            .collect(),
        None,
    )
    .await
}
fn binding(identity: &IdentityServer, sub: &str) -> Binding {
    Binding {
        account: AccountId {
            service: CloudServiceId::canonicalize(&identity.url()).unwrap(),
            subject: sub.into(),
        },
        bound_at: Utc::now(),
    }
}
async fn validated(identity: &IdentityServer, sub: &str) -> ValidatedCredential {
    let token = identity.refresh_token_for(sub);
    let (access, rotated) = oauth::refresh_access_token(&identity.url(), &token)
        .await
        .unwrap();
    let userinfo = fetch_userinfo(&reqwest::Client::new(), &identity.url(), &access)
        .await
        .unwrap();
    ValidatedCredential {
        refresh_token: rotated.unwrap(),
        access,
        userinfo,
    }
}
fn accept(
    store: &ProfileCredentialStore,
    credential: ValidatedCredential,
    binding: &Binding,
) -> uuid::Uuid {
    let prepared = store
        .commit(store.stage(credential, store.epoch()), binding)
        .unwrap();
    let version = prepared.version;
    store.activate(prepared);
    version
}

#[tokio::test]
async fn staged_not_committed_preserves_accepted_credentials_across_reopen() {
    let identity = identity().await;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let path = root.path().join("credentials.yaml");
    let binding = binding(&identity, "alice");
    let store =
        ProfileCredentialStore::open(Some(path.clone()), reqwest::Client::new(), None, None)
            .unwrap();
    let original = validated(&identity, "alice").await;
    let version = accept(&store, original.clone(), &binding);
    let staged = validated(&identity, "alice").await;
    let prepared = store
        .commit(store.stage(staged.clone(), store.epoch()), &binding)
        .unwrap();
    assert_eq!(
        store.access_token().await.unwrap().bearer,
        original.access.bearer
    );
    let reopened = ProfileCredentialStore::open(
        Some(path.clone()),
        reqwest::Client::new(),
        Some(&binding),
        Some(version),
    )
    .unwrap();
    // Only the referenced old version is refreshed; the uncommitted candidate is still unused.
    reopened.access_token().await.unwrap();
    oauth::refresh_access_token(&identity.url(), &staged.refresh_token)
        .await
        .unwrap();
    assert_ne!(prepared.version, version);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    println!(
        "Uncommitted login preserves the accepted credential on disk and in memory (mode 600)"
    );
}

#[tokio::test]
async fn rotated_refresh_is_serialized_and_checked_against_binding() {
    let identity = identity().await;
    let binding = binding(&identity, "alice");
    let store =
        Arc::new(ProfileCredentialStore::open(None, reqwest::Client::new(), None, None).unwrap());
    let initial = validated(&identity, "alice").await;
    accept(&store, initial.clone(), &binding);
    store.invalidate(&initial.access);
    let tokens = futures_util::future::join_all((0..12).map(|_| store.access_token())).await;
    let first = tokens[0].as_ref().unwrap().bearer.clone();
    assert!(tokens.iter().all(|t| t.as_ref().unwrap().bearer == first));
    let token = tokens.into_iter().next().unwrap().unwrap();
    store.invalidate(&token);
    let rotated = store.access_token().await.unwrap();
    assert_ne!(rotated.bearer, token.bearer);
    store.invalidate(&rotated);
    identity.inject(Fault::SwapSubject {
        from: "alice".into(),
        to: "bob".into(),
    });
    assert!(matches!(
        store.access_token().await,
        Err(AuthError::AccountMismatch)
    ));
    println!(
        "Concurrent refresh spends one token; rotation works; a changed subject cannot authenticate"
    );
}

#[tokio::test]
async fn clear_invalidates_stages_and_in_flight_refresh_without_recreating_files() {
    let identity = identity().await;
    let binding = binding(&identity, "alice");
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let path = root.path().join("credentials.yaml");
    let store = Arc::new(
        ProfileCredentialStore::open(Some(path.clone()), reqwest::Client::new(), None, None)
            .unwrap(),
    );
    let initial = validated(&identity, "alice").await;
    accept(&store, initial.clone(), &binding);
    let stage = store.stage(validated(&identity, "alice").await, store.epoch());
    store.invalidate(&initial.access);
    let mut hold = identity.hold_next_userinfo();
    let worker = {
        let store = store.clone();
        tokio::spawn(async move { store.access_token().await })
    };
    hold.entered().await;
    store.clear().unwrap();
    hold.release();
    assert!(matches!(
        worker.await.unwrap(),
        Err(AuthError::Unauthenticated)
    ));
    assert!(store.commit(stage, &binding).is_err());
    assert!(!path.exists());
    assert!(!store.has_credential());
    println!(
        "Logout during refresh prevents the late result and old stages from restoring credentials"
    );
}

#[tokio::test]
async fn cancelled_refresh_caller_does_not_burn_rotation() {
    let identity = identity().await;
    let binding = binding(&identity, "alice");
    let store =
        Arc::new(ProfileCredentialStore::open(None, reqwest::Client::new(), None, None).unwrap());
    let initial = validated(&identity, "alice").await;
    accept(&store, initial.clone(), &binding);
    store.invalidate(&initial.access);
    let mut hold = identity.hold_next_userinfo();
    let caller = {
        let store = store.clone();
        tokio::spawn(async move { store.access_token().await })
    };
    hold.entered().await;
    caller.abort();
    let _ = caller.await;
    hold.release();
    let refreshed = store.access_token().await.unwrap();
    store.invalidate(&refreshed);
    store.access_token().await.unwrap();
}

fn options(root: InstallationRoot) -> InstallationOptions {
    InstallationOptions {
        root,
        listeners: Listeners::InProcessOnly,
        credentials: CredentialSource::ProfileFiles,
        identity_http: reqwest::Client::new(),
        settings: InstallationSettings {
            host_name: "binding-test".into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: Default::default(),
            minimum_client_versions: Default::default(),
            status_reporters: Default::default(),
        },
    }
}
fn request(identity: &IdentityServer, sub: &str, target: BindTarget) -> BindRequest {
    BindRequest {
        target,
        cloud_url: identity.url(),
        staged_refresh_token: identity.refresh_token_for(sub),
        adopt_non_pristine: false,
    }
}
async fn create(installation: &Installation) -> crate::installation::ProfileStatus {
    let status = installation.create(OperationId::new(), None).await.unwrap();
    assert!(status.available, "{status:?}");
    status
}

#[tokio::test]
async fn explicit_target_never_substitutes_and_refusals_keep_credential_and_label() {
    let identity = identity().await;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let installation = Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
        .await
        .unwrap();
    let first = create(&installation).await;
    let login = request(&identity, "alice", BindTarget::Explicit(first.record.id));
    let op = OperationId::new();
    let bound = installation.bind(op, login.clone()).await.unwrap();
    assert_eq!(bound.record.id, first.record.id);
    assert_eq!(bound.host_id, first.host_id);
    println!(
        "Accepted profile record:\n{}",
        serde_json::to_string_pretty(&bound.record).unwrap()
    );
    assert_eq!(
        bound.record.label.account_name.as_deref(),
        Some("alice Example")
    );
    assert_eq!(
        bound.record.label.email.as_deref(),
        Some("alice@example.test")
    );
    assert_eq!(installation.bind(op, login).await.unwrap(), bound);
    let path = root
        .path()
        .join("profiles")
        .join(first.record.id.to_string())
        .join("credentials.yaml");
    let before = std::fs::read(&path).unwrap();
    assert!(
        matches!(installation.bind(OperationId::new(), request(&identity, "bob", BindTarget::Explicit(first.record.id))).await,
        Err(BindError::ProfileBoundToOtherAccount { profile }) if profile == first.record.id)
    );
    let second = create(&installation).await;
    assert!(
        matches!(installation.bind(OperationId::new(), request(&identity, "alice", BindTarget::Explicit(second.record.id))).await,
        Err(BindError::AccountAlreadyBound { profile }) if profile == first.record.id)
    );
    assert!(matches!(
        installation
            .bind(
                OperationId::new(),
                request(&identity, "bob", BindTarget::Explicit(ProfileId::new()))
            )
            .await,
        Err(BindError::Installation(InstallationError::UnknownProfile(
            _
        )))
    ));
    assert_eq!(std::fs::read(path).unwrap(), before);
    println!(
        "Explicit targets never fall back; account mismatch and duplicate account preserve accepted credentials and labels"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn by_account_chooses_sole_pristine_then_bound_and_concurrent_logins_share_one_profile() {
    let identity = identity().await;
    let installation = Installation::open(options(InstallationRoot::InMemory))
        .await
        .unwrap();
    let initial = create(&installation).await;
    let (a, b) = tokio::join!(
        installation.bind(
            OperationId::new(),
            request(&identity, "alice", BindTarget::ByAccount)
        ),
        installation.bind(
            OperationId::new(),
            request(&identity, "alice", BindTarget::ByAccount)
        ),
    );
    assert_eq!(a.unwrap().record.id, initial.record.id);
    assert_eq!(b.unwrap().record.id, initial.record.id);
    assert_eq!(installation.profiles().len(), 1);
    let bob = installation
        .bind(
            OperationId::new(),
            request(&identity, "bob", BindTarget::ByAccount),
        )
        .await
        .unwrap();
    assert_ne!(bob.record.id, initial.record.id);
    assert_eq!(installation.profiles().len(), 2);
    println!(
        "Simultaneous logins select one profile; another account gets another complete device"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn logout_reserves_account_across_restart_and_relogin_preserves_device() {
    let identity = identity().await;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let installation = Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
        .await
        .unwrap();
    let bound = installation
        .bind(
            OperationId::new(),
            request(&identity, "alice", BindTarget::ByAccount),
        )
        .await
        .unwrap();
    let logged_out = installation
        .logout(OperationId::new(), bound.record.id)
        .await
        .unwrap();
    assert_eq!(logged_out.intent, Intent::LoggedOut);
    assert_eq!(logged_out.record.binding, bound.record.binding);
    installation
        .client(bound.record.id)
        .unwrap()
        .list_agents()
        .await
        .unwrap();
    installation.shutdown(ShutdownReason::UserRequested).await;
    let installation = Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
        .await
        .unwrap();
    assert_eq!(installation.profiles()[0].intent, Intent::LoggedOut);
    let relogin = installation
        .bind(
            OperationId::new(),
            request(&identity, "alice", BindTarget::ByAccount),
        )
        .await
        .unwrap();
    assert_eq!(relogin.record.id, bound.record.id);
    assert_eq!(relogin.host_id, bound.host_id);
    assert_eq!(relogin.record.binding, bound.record.binding);
    println!(
        "Logout survives restart and reserves the account; re-login keeps the profile and host identity"
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn logout_and_delete_cancel_pending_login_before_it_can_commit_or_connect() {
    for delete in [false, true] {
        let identity = identity().await;
        let installation = Arc::new(
            Installation::open(options(InstallationRoot::InMemory))
                .await
                .unwrap(),
        );
        let first = create(&installation).await;
        let mut hold = identity.hold_next_userinfo();
        let login = request(&identity, "alice", BindTarget::Explicit(first.record.id));
        let worker = {
            let installation = installation.clone();
            tokio::spawn(async move { installation.bind(OperationId::new(), login).await })
        };
        hold.entered().await;
        if delete {
            installation
                .delete(OperationId::new(), first.record.id, first.record.revision)
                .await
                .unwrap();
        } else {
            installation
                .logout(OperationId::new(), first.record.id)
                .await
                .unwrap();
        }
        hold.release();
        let error = worker.await.unwrap().unwrap_err();
        assert!(
            matches!(
                error,
                BindError::Cancelled | BindError::Installation(InstallationError::Deleted(_))
            ),
            "{error}"
        );
        assert!(
            installation
                .profiles()
                .iter()
                .all(|p| p.record.binding.is_none())
        );
        Arc::try_unwrap(installation)
            .ok()
            .unwrap()
            .shutdown(ShutdownReason::UserRequested)
            .await;
    }
    println!(
        "Logout and deletion invalidate an in-flight login before any binding, credential, or connector is installed"
    );
}

#[tokio::test]
async fn adoption_confirmation_reuses_staged_rotation_and_rechecks_local_state() {
    let identity = identity().await;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let installation = Arc::new(
        Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
            .await
            .unwrap(),
    );
    let first = create(&installation).await;
    let mut hold = identity.hold_next_userinfo();
    let login = request(&identity, "alice", BindTarget::Explicit(first.record.id));
    let worker = {
        let installation = installation.clone();
        let login = login.clone();
        tokio::spawn(async move { installation.bind(OperationId::new(), login).await })
    };
    hold.entered().await;
    let artifact = root
        .path()
        .join("profiles")
        .join(first.record.id.to_string())
        .join("data/cache/artifacts/retained");
    std::fs::write(artifact, "retained data").unwrap();
    hold.release();
    assert!(
        matches!(worker.await.unwrap(), Err(BindError::AdoptionNeedsConfirmation { profile, reason: NonPristine::RetainedArtifacts(1) }) if profile == first.record.id)
    );
    let bound = installation
        .bind(
            OperationId::new(),
            BindRequest {
                adopt_non_pristine: true,
                ..login
            },
        )
        .await
        .unwrap();
    assert_eq!(bound.record.id, first.record.id);
    println!(
        "State created during login requires adoption confirmation; confirmation reuses the rotated staged token"
    );
    Arc::try_unwrap(installation)
        .ok()
        .unwrap()
        .shutdown(ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn userinfo_failure_retries_the_rotated_token_without_spending_it_again() {
    let identity = identity().await;
    let store = ProfileCredentialStore::open(None, reqwest::Client::new(), None, None).unwrap();
    let initial = validated(&identity, "alice").await;
    accept(&store, initial.clone(), &binding(&identity, "alice"));
    store.invalidate(&initial.access);
    identity.inject(Fault::MissingSubject);
    assert!(matches!(
        store.access_token().await,
        Err(AuthError::Provider(_))
    ));
    let recovered = store.access_token().await.unwrap();
    store.invalidate(&recovered);
    store.access_token().await.unwrap();
}

#[tokio::test]
async fn host_credentials_are_subject_checked_and_logout_stays_logged_out_on_reopen() {
    struct HostToken(crate::auth::AccessToken);
    #[async_trait::async_trait]
    impl CredentialProvider for HostToken {
        async fn access_token(&self) -> Result<crate::auth::AccessToken, AuthError> {
            Ok(self.0.clone())
        }
        fn invalidate(&self, _: &crate::auth::AccessToken) {}
    }
    let identity = identity().await;
    let wrong = Arc::new(HostToken(validated(&identity, "bob").await.access));
    let store = ProfileCredentialStore::open(None, reqwest::Client::new(), None, None).unwrap();
    store.use_host(&binding(&identity, "alice"), wrong.clone());
    assert!(matches!(
        store.access_token().await,
        Err(AuthError::AccountMismatch)
    ));
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let id = ProfileId::new();
    {
        let mut registry =
            crate::installation::Registry::open(InstallationRoot::OnDisk(root.path().into()))
                .unwrap();
        let mut record = registry.create(id, Default::default()).unwrap();
        record.binding = Some(binding(&identity, "alice"));
        registry.replace(record).unwrap();
    }
    let make_options = || {
        let mut options = options(InstallationRoot::OnDisk(root.path().into()));
        let wrong = wrong.clone();
        options.credentials = CredentialSource::HostProvided(Arc::new(move |_| wrong.clone()));
        options
    };
    let installation = Installation::open(make_options()).await.unwrap();
    installation.logout(OperationId::new(), id).await.unwrap();
    installation.shutdown(ShutdownReason::UserRequested).await;
    let installation = Installation::open(make_options()).await.unwrap();
    assert_eq!(installation.profiles()[0].intent, Intent::LoggedOut);
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn registry_write_failure_does_not_activate_staged_login() {
    let identity = identity().await;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let installation = Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
        .await
        .unwrap();
    let first = installation
        .bind(
            OperationId::new(),
            request(&identity, "alice", BindTarget::ByAccount),
        )
        .await
        .unwrap();
    let registry_path = root.path().join("registry.yaml");
    let registry_bytes = std::fs::read(&registry_path).unwrap();
    std::fs::remove_file(&registry_path).unwrap();
    std::fs::create_dir(&registry_path).unwrap();
    assert!(
        installation
            .bind(
                OperationId::new(),
                request(&identity, "alice", BindTarget::Explicit(first.record.id))
            )
            .await
            .is_err()
    );
    assert_eq!(installation.profiles()[0].record, first.record);
    std::fs::remove_dir(&registry_path).unwrap();
    std::fs::write(&registry_path, registry_bytes).unwrap();
    installation.shutdown(ShutdownReason::UserRequested).await;
    let installation = Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
        .await
        .unwrap();
    let reopened = &installation.profiles()[0];
    assert!(reopened.available, "{reopened:?}");
    assert_eq!(reopened.record, first.record);
    assert_eq!(reopened.intent, Intent::Bound);
    installation.shutdown(ShutdownReason::UserRequested).await;
}

#[tokio::test]
async fn suspended_agents_require_explicit_adoption_confirmation() {
    use crate::suspend::{SuspendedAgent, SuspendedLocalAgentNameSource, SuspendedServerState};
    let identity = identity().await;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let installation = Installation::open(options(InstallationRoot::OnDisk(root.path().into())))
        .await
        .unwrap();
    let first = create(&installation).await;
    let paths = crate::installation::ProfilePaths::for_id(root.path(), first.record.id).unwrap();
    crate::suspend::save_suspended(
        &paths.state_path,
        &SuspendedServerState {
            agents: vec![SuspendedAgent::Claude {
                driver: crate::agents::ClaudeDriver::Pty,
                agent_id: uuid::Uuid::new_v4(),
                name: None,
                name_source: SuspendedLocalAgentNameSource::Unset,
                working_dir: root.path().into(),
                terminal_size: None,
                args: Vec::new(),
                session_id: uuid::Uuid::new_v4(),
                created_at: Utc::now(),
                parent: None,
                working_on: None,
            }],
        },
    )
    .unwrap();
    assert!(matches!(
        installation
            .bind(
                OperationId::new(),
                request(&identity, "alice", BindTarget::Explicit(first.record.id))
            )
            .await,
        Err(BindError::AdoptionNeedsConfirmation {
            reason: NonPristine::LocalAgents(1),
            ..
        })
    ));
    installation.shutdown(ShutdownReason::UserRequested).await;
}
