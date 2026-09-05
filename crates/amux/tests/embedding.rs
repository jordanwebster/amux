//! Embedded installation behavior through the public host and client APIs.
#![cfg(test_fixtures)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use amux::installation::{BindTarget, Observed, ProfileStatus};
use amux::test_fixtures::{Fault, IdentityServer, TestAccount, TestRelay};
use amux::{
    AccessToken, AuthError, BindRequest, Client, CredentialProvider, CredentialSource, HostId,
    HostTrustStatus, Installation, InstallationOptions, InstallationRoot, InstallationSettings,
    Listeners, OAuthError, OperationId, PairingSecret, ProfileId, ShutdownReason,
    refresh_access_token,
};

struct HostCredentials {
    url: String,
    refresh: tokio::sync::Mutex<String>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl CredentialProvider for HostCredentials {
    async fn access_token(&self) -> Result<AccessToken, AuthError> {
        let mut refresh = self.refresh.lock().await;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (access, rotated) =
            refresh_access_token(&self.url, &refresh)
                .await
                .map_err(|error| match error {
                    OAuthError::RefreshTokenExpired => AuthError::Unauthenticated,
                    other => AuthError::Provider(other.to_string()),
                })?;
        *refresh = rotated.expect("fixture rotates every successful refresh");
        Ok(access)
    }

    // This test host always refreshes when asked; it holds no access-token cache.
    fn invalidate(&self, _: &AccessToken) {}
}

type Providers = Arc<Mutex<HashMap<ProfileId, Arc<HostCredentials>>>>;

fn options(name: &str, providers: Providers, root: InstallationRoot) -> InstallationOptions {
    InstallationOptions {
        root,
        settings: InstallationSettings {
            host_name: name.into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: Default::default(),
            minimum_client_versions: Default::default(),
            update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
            status_reporters: Default::default(),
        },
        listeners: Listeners::InProcessOnly,
        credentials: CredentialSource::HostProvided(Arc::new(move |id| {
            providers.lock().unwrap()[&id].clone()
        })),
        identity_http: reqwest::Client::new(),
    }
}

async fn bind(
    installation: &Installation,
    providers: &Providers,
    identity: &IdentityServer,
    relay: &TestRelay,
    subject: &str,
) -> (ProfileStatus, Arc<HostCredentials>) {
    let profile = installation.create(OperationId::new(), None).await.unwrap();
    assert!(profile.available, "{profile:?}");
    assert!(profile.socket_path.is_none());
    relay
        .use_for_profile(installation, profile.record.id)
        .await
        .unwrap();
    let credentials = Arc::new(HostCredentials {
        url: identity.url(),
        refresh: tokio::sync::Mutex::new(identity.refresh_token_for(subject)),
        calls: AtomicUsize::new(0),
    });
    providers
        .lock()
        .unwrap()
        .insert(profile.record.id, credentials.clone());
    let bound = installation
        .bind(
            OperationId::new(),
            BindRequest {
                target: BindTarget::Explicit(profile.record.id),
                cloud_url: identity.url(),
                staged_refresh_token: identity.refresh_token_for(subject),
                adopt_non_pristine: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(bound.host_id, profile.host_id);
    assert_eq!(
        bound.record.binding.as_ref().unwrap().account.subject,
        subject
    );
    assert_eq!(
        bound.record.label.email.as_deref(),
        Some(format!("{subject}@example.test").as_str())
    );
    (bound, credentials)
}

async fn wait_status(installation: &Installation, expected: &[(ProfileId, Observed)]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let profiles = installation.profiles();
            if expected.iter().all(|(id, state)| {
                profiles
                    .iter()
                    .any(|p| p.record.id == *id && p.observed == *state)
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("expected {expected:?}, got {:?}", installation.profiles()));
}

async fn wait_presence(client: &Client, peer: HostId, online: bool, forbidden: &[HostId]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let hosts = client.list_hosts().await.unwrap();
            assert!(
                hosts.iter().all(|host| !forbidden.contains(&host.id)),
                "cross-tenant presence: {hosts:?}"
            );
            if hosts
                .iter()
                .any(|host| host.id == peer && host.online == online)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("peer {peer} did not become online={online}"));
}

async fn wait_pairing_host(installation: &Installation, id: ProfileId, peer: HostId) {
    let admin = installation.admin(id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let hosts = admin.list_pairing_hosts().await.unwrap();
            assert!(
                hosts.iter().all(|host| host.id == peer),
                "cross-tenant discovery: {hosts:?}"
            );
            if hosts.len() == 1 {
                println!("Profile {id} pairing discovery: {hosts:?}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("same-tenant peer must be discoverable before pairing");
}

#[tokio::test]
async fn embedded_accounts_stay_isolated_and_recover_without_screen_clients() {
    #[cfg(feature = "local-agents")]
    assert!(
        std::env::var_os("AMUX_EMBED_TEST").is_none(),
        "embedding recipe must compile out local agents"
    );
    println!(
        "Local agents compiled out: {}",
        !cfg!(feature = "local-agents")
    );
    let relay = TestRelay::start().await;
    let personal_user = relay.register_user("personal");
    let work_user = relay.register_user("work");
    assert_ne!(personal_user.user_id, work_user.user_id);
    let mut identities = Vec::new();
    for subject in ["personal", "work"] {
        identities.push(
            IdentityServer::start(
                vec![TestAccount {
                    sub: subject.into(),
                    name: Some(subject.into()),
                    email: Some(format!("{subject}@example.test")),
                }],
                Some(relay.addr),
            )
            .await,
        );
    }
    let providers = Providers::default();
    #[cfg(feature = "local-agents")]
    let roots = [
        amux::test_fixtures::short_installation_root(),
        amux::test_fixtures::short_installation_root(),
    ];
    #[cfg(feature = "local-agents")]
    let [phone_root, peer_root] = roots
        .each_ref()
        .map(|root| InstallationRoot::OnDisk(root.path().into()));
    #[cfg(not(feature = "local-agents"))]
    let [phone_root, peer_root] = [InstallationRoot::Ephemeral, InstallationRoot::Ephemeral];
    let installation = Installation::open(options("phone", providers.clone(), phone_root))
        .await
        .unwrap();
    let witnesses = Installation::open(options("peer", providers.clone(), peer_root))
        .await
        .unwrap();
    let mut profiles = Vec::new();
    let mut peers = Vec::new();
    let mut credentials = Vec::new();
    for (index, subject) in ["personal", "work"].into_iter().enumerate() {
        let (profile, provider) = bind(
            &installation,
            &providers,
            &identities[index],
            &relay,
            subject,
        )
        .await;
        profiles.push(profile);
        credentials.push(provider);
        peers.push(
            bind(&witnesses, &providers, &identities[index], &relay, subject)
                .await
                .0,
        );
    }
    let ids = [profiles[0].record.id, profiles[1].record.id];
    let hosts = [profiles[0].host_id, profiles[1].host_id];
    assert_ne!(hosts[0], hosts[1]);
    wait_status(
        &installation,
        &[(ids[0], Observed::Connected), (ids[1], Observed::Connected)],
    )
    .await;
    wait_status(
        &witnesses,
        &peers
            .iter()
            .map(|p| (p.record.id, Observed::Connected))
            .collect::<Vec<_>>(),
    )
    .await;
    println!("Host profile directory: {:?}", installation.profiles());

    let mut pairings = Vec::new();
    for id in ids {
        let screen = installation.client(id).unwrap();
        let last_screen = screen.clone();
        assert!(screen.list_agents().await.unwrap().is_empty());
        pairings.push(
            installation
                .admin(id)
                .await
                .unwrap()
                .start_pin_pairing()
                .await
                .unwrap(),
        );
        drop(screen);
        drop(last_screen);
    }
    assert_ne!(pairings[0].identity.pubkey, pairings[1].identity.pubkey);
    let mut peer_clients = Vec::new();
    for index in 0..2 {
        let peer_client = witnesses.client(peers[index].record.id).unwrap();
        wait_pairing_host(&witnesses, peers[index].record.id, hosts[index]).await;
        wait_pairing_host(&installation, ids[index], peers[index].host_id).await;
        let PairingSecret::Pin(pin) = &pairings[index].secret else {
            panic!("expected PIN")
        };
        let paired = witnesses
            .admin(peers[index].record.id)
            .await
            .unwrap()
            .pair_pin_cloud_peer(hosts[index], pin.clone())
            .await
            .unwrap();
        assert_eq!(paired, pairings[index].identity);
        let trust = installation
            .admin(ids[index])
            .await
            .unwrap()
            .list_peers()
            .await
            .unwrap();
        assert_eq!(trust.len(), 1);
        assert_eq!(trust[0].host_id, peers[index].host_id);
        assert!(
            peer_client.list_hosts().await.unwrap().iter().any(
                |host| host.id == hosts[index] && host.trust_status == HostTrustStatus::Trusted
            )
        );
        println!(
            "Paired {} after dropping every screen client; profile trust: {trust:?}",
            ["personal", "work"][index]
        );
        peer_clients.push(peer_client);
    }

    for cycle in 0..2 {
        installation.host_suspend().await;
        wait_status(
            &installation,
            &[(ids[0], Observed::Local), (ids[1], Observed::Local)],
        )
        .await;
        for index in 0..2 {
            wait_presence(
                &peer_clients[index],
                hosts[index],
                false,
                &[hosts[1 - index], peers[1 - index].host_id],
            )
            .await;
            let screen = installation.client(ids[index]).unwrap();
            assert!(screen.list_agents().await.unwrap().is_empty());
            let trust = installation
                .admin(ids[index])
                .await
                .unwrap()
                .list_peers()
                .await
                .unwrap();
            assert_eq!(trust.len(), 1);
            assert_eq!(trust[0].host_id, peers[index].host_id);
        }
        if cycle == 1 {
            identities[1].inject(Fault::RejectRefresh("work refresh revoked".into()));
        }
        installation.host_resume().await;
        let work_status = if cycle == 0 {
            Observed::Connected
        } else {
            Observed::AuthenticationRequired
        };
        wait_status(
            &installation,
            &[(ids[0], Observed::Connected), (ids[1], work_status)],
        )
        .await;
        for index in 0..2 {
            assert_eq!(credentials[index].calls.load(Ordering::SeqCst), cycle + 1);
            wait_presence(
                &peer_clients[index],
                hosts[index],
                index == 0 || cycle == 0,
                &[hosts[1 - index], peers[1 - index].host_id],
            )
            .await;
        }
        println!("Host resume {}: {:?}", cycle + 1, installation.profiles());
    }
    installation
        .pause(OperationId::new(), ids[1])
        .await
        .unwrap();
    installation
        .resume(OperationId::new(), ids[1])
        .await
        .unwrap();
    wait_status(
        &installation,
        &[(ids[0], Observed::Connected), (ids[1], Observed::Connected)],
    )
    .await;
    assert_eq!(credentials[0].calls.load(Ordering::SeqCst), 2);
    assert_eq!(credentials[1].calls.load(Ordering::SeqCst), 3);
    for index in 0..2 {
        wait_presence(
            &peer_clients[index],
            hosts[index],
            true,
            &[hosts[1 - index], peers[1 - index].host_id],
        )
        .await;
        let profile = installation
            .profiles()
            .into_iter()
            .find(|p| p.record.id == ids[index])
            .unwrap();
        assert_eq!(profile.host_id, hosts[index]);
        let screen = installation.client(ids[index]).unwrap();
        wait_presence(
            &screen,
            peers[index].host_id,
            true,
            &[hosts[1 - index], peers[1 - index].host_id],
        )
        .await;
        println!(
            "Recovered {} fleet: {:?}",
            ["personal", "work"][index],
            screen.list_hosts().await.unwrap()
        );
    }
    println!(
        "Both accounts reconnect with their original identities and trust; rejected work refresh leaves personal connected, and work recovery does not refresh personal."
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
    for index in 0..2 {
        wait_presence(
            &peer_clients[index],
            hosts[index],
            false,
            &[hosts[1 - index], peers[1 - index].host_id],
        )
        .await;
    }
    witnesses.shutdown(ShutdownReason::UserRequested).await;
}

#[cfg(all(unix, not(feature = "local-agents")))]
#[test]
fn embedded_ephemeral_storage_stays_in_container_tmpdir() {
    use std::os::unix::ffi::OsStrExt;

    const CHILD: &str = "AMUX_EPHEMERAL_ROOT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let temporary = tempfile::tempdir().unwrap();
        let container_tmp = temporary.path().join("app-container-tmp-".repeat(8));
        std::fs::create_dir(&container_tmp).unwrap();
        // Give only this child a container-shaped TMPDIR; other tests may be
        // allocating temporary files concurrently in the parent process.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "embedded_ephemeral_storage_stays_in_container_tmpdir",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("TMPDIR", &container_tmp)
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(
            output.status.success(),
            "ephemeral storage child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_dir(container_tmp).unwrap().count(), 0);
        return;
    }

    let container_tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    assert!(container_tmp.as_os_str().as_bytes().len() > 104);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let installation = Installation::open(options(
                "phone",
                Providers::default(),
                InstallationRoot::Ephemeral,
            ))
            .await
            .unwrap();
            let root = installation.root().to_owned();
            assert_eq!(root.parent(), Some(container_tmp.as_path()));
            let profile = installation
                .create(OperationId::new(), Some("personal".into()))
                .await
                .unwrap();
            assert!(profile.available, "{profile:?}");
            assert!(profile.socket_path.is_none());
            let client = installation.client(profile.record.id).unwrap();
            assert!(client.list_agents().await.unwrap().is_empty());
            let identity = installation
                .admin(profile.record.id)
                .await
                .unwrap()
                .start_pin_pairing()
                .await
                .unwrap()
                .identity;
            installation
                .admin(profile.record.id)
                .await
                .unwrap()
                .cancel_pairing()
                .await
                .unwrap();
            drop(client);

            // Reopen the profile as a new screen would, keeping its device
            // identity while the ephemeral installation owner remains alive.
            let reopened = installation.client(profile.record.id).unwrap();
            assert!(reopened.list_agents().await.unwrap().is_empty());
            assert_eq!(installation.profiles(), vec![profile.clone()]);
            assert_eq!(
                installation
                    .admin(profile.record.id)
                    .await
                    .unwrap()
                    .start_pin_pairing()
                    .await
                    .unwrap()
                    .identity,
                identity
            );
            drop(reopened);

            let profile_root = root.join("profiles").join(profile.record.id.to_string());
            let config_path = profile_root.join("config.yaml");
            let config = amux::load_profile_config(&config_path).unwrap();
            let paths = amux::installation::ProfilePaths::for_id(&root, profile.record.id)
                .unwrap();
            let cache = config.artifact_cache_dir();
            let reports = config.reports_dir();
            for path in [
                &profile_root,
                &config_path,
                &config.installation.root,
                &config.installation.front_door_socket,
                &config.installation.keymaps_dir,
                config.installation.path.as_ref().unwrap(),
                &config.profile.installation_config,
                &config.profile.socket_path,
                &config.profile.data_dir,
                &config.profile.state_path,
                &cache,
                &reports,
                paths.config_path.as_ref().unwrap(),
                &paths.socket_path,
                &paths.state_path,
                &paths.data_dir,
                &paths.reports_dir,
                &paths.credentials_path().unwrap(),
            ] {
                assert!(path.starts_with(&root), "path escaped root: {path:?}");
                if path.exists() {
                    assert!(std::fs::canonicalize(path).unwrap().starts_with(&root));
                }
                println!("Container-owned path: {}", path.display());
            }
            assert!(cache.is_dir());
            assert!(reports.is_dir());
            assert!(config.profile.data_dir.join("device.key").is_file());
            assert!(!root.join("registry.yaml").exists());
            assert!(!config.installation.front_door_socket.exists());
            assert!(!config.profile.socket_path.exists());
            println!(
                "Created and reopened embedded profile {} under {}-byte TMPDIR {}; registry is unpersisted, profile files stay on disk beneath {}",
                profile.record.id,
                container_tmp.as_os_str().as_bytes().len(),
                container_tmp.display(),
                root.display()
            );
            installation.shutdown(ShutdownReason::UserRequested).await;
            assert!(!root.exists(), "ephemeral root must be removed on drop");
            println!("Ephemeral installation shutdown and drop removed its root and all profile files.");
        });
}

#[cfg(all(unix, not(feature = "local-agents")))]
#[tokio::test]
async fn embedded_storage_reopens_long_roots_and_refuses_symlink_redirection() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    use amux::installation::{InstallationError, ProfilePaths};

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("embedded-app-storage-".repeat(8));
    let make_options = || {
        options(
            "phone",
            Providers::default(),
            InstallationRoot::OnDisk(root.clone()),
        )
    };
    let installation = Installation::open(make_options()).await.unwrap();
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    assert!(installation.root().as_os_str().as_bytes().len() > address.sun_path.len());
    let profile = installation
        .create(OperationId::new(), Some("personal".into()))
        .await
        .unwrap();
    assert!(profile.available, "{profile:?}");
    assert!(profile.socket_path.is_none());
    let paths = ProfilePaths::for_id(installation.root(), profile.record.id).unwrap();
    let namespace = installation
        .root()
        .join("profiles")
        .join(profile.record.id.to_string());
    assert!(paths.data_dir.starts_with(&namespace));
    assert!(paths.state_path.starts_with(&namespace));
    assert!(!paths.socket_path.exists());
    let identity = installation
        .admin(profile.record.id)
        .await
        .unwrap()
        .start_pin_pairing()
        .await
        .unwrap()
        .identity;
    installation.shutdown(ShutdownReason::UserRequested).await;

    let installation = Installation::open(make_options()).await.unwrap();
    let reopened = installation.profiles();
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].record, profile.record);
    assert_eq!(reopened[0].host_id, profile.host_id);
    assert!(reopened[0].available, "{reopened:?}");
    assert!(reopened[0].socket_path.is_none());
    assert_eq!(
        installation
            .admin(profile.record.id)
            .await
            .unwrap()
            .start_pin_pairing()
            .await
            .unwrap()
            .identity,
        identity
    );
    assert!(
        installation
            .client(profile.record.id)
            .unwrap()
            .list_agents()
            .await
            .unwrap()
            .is_empty()
    );
    println!(
        "Reopened embedded profile beneath a {}-byte root (Unix socket buffer: {} bytes): {:?}",
        installation.root().as_os_str().as_bytes().len(),
        address.sun_path.len(),
        reopened[0]
    );
    installation.shutdown(ShutdownReason::UserRequested).await;

    let foreign = temporary.path().join("foreign-trust.json");
    std::fs::write(&foreign, "foreign data must stay untouched").unwrap();
    let trust = paths.data_dir.join("trust.json");
    if trust.exists() {
        std::fs::remove_file(&trust).unwrap();
    }
    symlink(&foreign, &trust).unwrap();
    assert!(
        matches!(ProfilePaths::for_id(&root, profile.record.id), Err(InstallationError::InvalidPath(path)) if path == trust)
    );
    let installation = Installation::open(make_options()).await.unwrap();
    let rejected = installation.profiles();
    assert_eq!(rejected.len(), 1);
    assert!(!rejected[0].available);
    assert_eq!(rejected[0].observed, Observed::StartupFailed);
    assert_eq!(
        std::fs::read_to_string(&foreign).unwrap(),
        "foreign data must stay untouched"
    );
    println!(
        "Symlink redirection remains refused on the long root: {:?}",
        rejected[0]
    );
    installation.shutdown(ShutdownReason::UserRequested).await;
}
