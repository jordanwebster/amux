//! Named complete-device profiles owned by a production installation.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};

use super::daemon::{CloudAttachment, DaemonInner, TestArtifactClock};
use super::{Daemon, NetInner};
use crate::installation::{
    BindError, BindRequest, BindTarget, CredentialSource, Installation, InstallationError,
    InstallationOptions, InstallationRoot, InstallationSettings, Listeners, OperationId,
    ProfileEvent, ProfileId, ProfilePaths, ProfileStatus, ProfileWatch,
};
use crate::profile::runtime::{ProfileRuntime, RuntimeFixtures};
use crate::test_fixtures::IdentityServer;

pub(super) struct InstallationSpec {
    pub name: String,
    pub persistent: bool,
    pub profiles: Vec<ProfileSpec>,
}

pub(super) struct ProfileSpec {
    pub name: String,
    pub cloud_user: Option<String>,
    pub cloud_only: bool,
}

struct ProfileFixture {
    tcp_addr: Option<SocketAddr>,
    tracked_tcp: crate::dispatcher::TrackedTcpConnections,
    clock: Arc<TestArtifactClock>,
}

#[derive(Default)]
struct FixturePlan {
    profiles: BTreeMap<ProfileId, ProfileFixture>,
    cloud_only: VecDeque<bool>,
}
type Fixtures = Arc<Mutex<FixturePlan>>;

pub(crate) struct ProfileOwner {
    installation: Weak<InstallationInner>,
    id: ProfileId,
    paths: ProfilePaths,
}

impl ProfileOwner {
    pub(crate) async fn runtime(
        &self,
    ) -> Option<tokio::sync::OwnedMutexGuard<Option<ProfileRuntime>>> {
        let owner = self.installation.upgrade()?;
        let installation = owner.current.read().unwrap().clone()?;
        installation.test_runtime(self.id).await
    }
}

struct InstallationInner {
    name: String,
    current: RwLock<Option<Arc<Installation>>>,
    profiles: BTreeMap<String, (ProfileId, Arc<DaemonInner>)>,
    identity: Arc<IdentityServer>,
    fixtures: Fixtures,
    cloud_addr: Option<SocketAddr>,
    root: PathBuf,
    persistent: bool,
    // Keep the root alive until the last handle and all runtimes are gone.
    _disk_root: Option<tempfile::TempDir>,
    lifecycle: tokio::sync::Mutex<()>,
}

impl Drop for InstallationInner {
    fn drop(&mut self) {
        let current = self.current.get_mut().unwrap().take();
        let root = self._disk_root.take();
        let identity = self.identity.clone();
        if let Some(current) = current
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                current.stop_for_test().await;
                // The filesystem and identity endpoint must outlive teardown,
                // including operations accepted just before the fixture dropped.
                drop(current);
                drop(identity);
                drop(root);
            });
        }
    }
}

impl InstallationInner {
    fn current(&self) -> Arc<Installation> {
        self.current
            .read()
            .unwrap()
            .as_ref()
            .expect("installation is running")
            .clone()
    }
}

/// The supervisor and identity provider behind a named fixture installation.
#[derive(Clone)]
pub struct InstallationHandle {
    inner: Arc<InstallationInner>,
    pub(super) net: Weak<NetInner>,
}

impl InstallationHandle {
    pub fn name(&self) -> &str {
        &self.inner.name
    }
    pub fn profile(&self, name: &str) -> Profile {
        let (id, inner) =
            self.inner.profiles.get(name).unwrap_or_else(|| {
                panic!("no profile '{name}' in installation '{}'", self.inner.name)
            });
        Profile {
            id: *id,
            daemon: Daemon {
                inner: inner.clone(),
                net: self.net.clone(),
            },
        }
    }

    /// The production in-process administrative API. Socket RPCs are tested
    /// separately from this supervisor fixture.
    pub fn front_door(&self) -> Arc<Installation> {
        self.inner.current()
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn identity(&self) -> &IdentityServer {
        &self.inner.identity
    }

    /// Fully stop and reopen a persistent installation at the same root.
    pub async fn restart(&self) {
        assert!(
            self.inner.persistent,
            "restart requires .persistent() on the installation builder"
        );
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.stop().await;
        let installation = Installation::open_for_test(
            options(
                &self.inner.name,
                InstallationRoot::OnDisk(self.inner.root.clone()),
            ),
            fixture_factory(self.inner.fixtures.clone(), self.inner.cloud_addr),
        )
        .await
        .expect("reopen installation");
        *self.inner.current.write().unwrap() = Some(Arc::new(installation));
    }

    pub async fn stop(&self) {
        let current = self.inner.current.write().unwrap().take();
        if let Some(current) = current {
            current.stop_for_test().await;
            for (_, inner) in self.inner.profiles.values() {
                Daemon {
                    inner: inner.clone(),
                    net: self.net.clone(),
                }
                .wait_until_peers_see_us_down()
                .await;
            }
        }
    }

    pub async fn login(&self, profile: &str, user: &str) -> Result<ProfileStatus, BindError> {
        self.front_door()
            .bind(
                OperationId::new(),
                BindRequest {
                    target: BindTarget::Explicit(self.profile(profile).id),
                    cloud_url: self.inner.identity.url(),
                    staged_refresh_token: self.inner.identity.refresh_token_for(user),
                    adopt_non_pristine: false,
                },
            )
            .await
    }

    pub async fn logout(&self, profile: &str) -> ProfileStatus {
        self.front_door()
            .logout(OperationId::new(), self.profile(profile).id)
            .await
            .unwrap()
    }

    pub async fn pause(&self, profile: &str) -> ProfileStatus {
        self.front_door()
            .pause(OperationId::new(), self.profile(profile).id)
            .await
            .unwrap()
    }

    pub async fn resume(&self, profile: &str) -> ProfileStatus {
        self.front_door()
            .resume(OperationId::new(), self.profile(profile).id)
            .await
            .unwrap()
    }

    pub async fn delete(&self, profile: &str) {
        let profile = self.profile(profile);
        let status = profile.status();
        self.front_door()
            .delete(OperationId::new(), profile.id, status.record.revision)
            .await
            .unwrap();
    }

    pub async fn watch(&self) -> WatchProbe {
        WatchProbe {
            watch: self.front_door().watch(),
            sequence: None,
        }
    }

    /// Try the real root-lock boundary without disturbing the serving owner.
    pub async fn try_second_supervisor(&self) -> Result<Installation, InstallationError> {
        assert!(
            self.inner.persistent,
            "root ownership requires .persistent()"
        );
        Installation::open(options(
            &self.inner.name,
            InstallationRoot::OnDisk(self.inner.root.clone()),
        ))
        .await
    }

    pub(super) fn daemon_inners(&self) -> impl Iterator<Item = Arc<DaemonInner>> + '_ {
        self.inner.profiles.values().map(|(_, inner)| inner.clone())
    }
}

/// A complete device, with exactly the same assertion and pairing verbs as a
/// standalone daemon. Runtime ownership remains with its installation.
#[derive(Clone)]
pub struct Profile {
    pub id: ProfileId,
    daemon: Daemon,
}

impl std::ops::Deref for Profile {
    type Target = Daemon;
    fn deref(&self) -> &Daemon {
        &self.daemon
    }
}

impl Profile {
    /// Force refresh through the runtime's installed credential provider and
    /// await its commit or refusal. No credential material leaves the fixture.
    pub async fn refresh_credentials(&self) -> Result<(), crate::auth::AuthError> {
        let owner = self.daemon.inner.installation.as_ref().unwrap();
        owner
            .installation
            .upgrade()
            .unwrap()
            .current()
            .refresh_for_test(self.id)
            .await
    }

    pub async fn reaches_status(&self, observed: crate::installation::Observed) {
        super::assertions::eventually(
            &format!("{} reaches {observed:?}", self.name()),
            async || self.status().observed == observed,
            self.daemon.failure_dump(),
        )
        .await;
    }

    pub async fn socket_client(&self) -> crate::Client {
        let config = crate::config::Config {
            socket_path: self.paths().socket_path,
            ..Default::default()
        };
        let channel = crate::client::connect_existing_client_service(&config)
            .await
            .unwrap();
        crate::Client::from_client_service_channel(channel, None)
    }

    pub fn status(&self) -> ProfileStatus {
        let owner = self.daemon.inner.installation.as_ref().unwrap();
        owner
            .installation
            .upgrade()
            .expect("installation dropped")
            .current()
            .profiles()
            .into_iter()
            .find(|status| status.record.id == self.id)
            .expect("profile no longer exists")
    }

    pub fn client(&self) -> crate::Client {
        let owner = self.daemon.inner.installation.as_ref().unwrap();
        owner
            .installation
            .upgrade()
            .expect("installation dropped")
            .current()
            .client(self.id)
            .unwrap()
    }

    pub fn paths(&self) -> ProfilePaths {
        self.daemon
            .inner
            .installation
            .as_ref()
            .unwrap()
            .paths
            .clone()
    }
}

/// Ordered events, including the initial snapshot and its completion marker.
pub struct WatchProbe {
    watch: ProfileWatch,
    sequence: Option<u64>,
}

impl WatchProbe {
    pub async fn next(&mut self) -> ProfileEvent {
        let event = tokio::time::timeout(super::assertions::DEFAULT_TIMEOUT, self.watch.recv())
            .await
            .expect("profile watch timed out")
            .expect("profile watch closed");
        match &event {
            ProfileEvent::SnapshotComplete { sequence } => self.sequence = Some(*sequence),
            ProfileEvent::Upserted { sequence, .. } | ProfileEvent::Removed { sequence, .. } => {
                if let Some(previous) = self.sequence {
                    assert_eq!(
                        *sequence,
                        previous + 1,
                        "profile watch lost or reordered an event"
                    );
                    self.sequence = Some(*sequence);
                }
            }
            ProfileEvent::Lagged => panic!("profile watch lagged"),
        }
        event
    }

    pub async fn snapshot(&mut self) -> Vec<ProfileStatus> {
        let mut profiles = Vec::new();
        loop {
            match self.next().await {
                ProfileEvent::Upserted { profile, .. } => profiles.push(*profile),
                ProfileEvent::SnapshotComplete { .. } => return profiles,
                event => panic!("unexpected snapshot event: {event:?}"),
            }
        }
    }
}

fn options(name: &str, root: InstallationRoot) -> InstallationOptions {
    InstallationOptions {
        root,
        listeners: Listeners::Sockets,
        credentials: CredentialSource::ProfileFiles,
        identity_http: reqwest::Client::new(),
        settings: InstallationSettings {
            host_name: name.into(),
            prevent_idle_sleep: Some(false),
            keybinds: Default::default(),
            ui: Default::default(),
            keymaps_dir: PathBuf::new(),
            minimum_client_versions: Default::default(),
            update_reporter: None,
            subscription_reporter: None,
        },
    }
}

fn fixture_factory(
    fixtures: Fixtures,
    cloud_addr: Option<SocketAddr>,
) -> Arc<dyn Fn(ProfileId) -> RuntimeFixtures + Send + Sync> {
    Arc::new(move |id| {
        let mut fixtures = fixtures.lock().unwrap();
        let cloud_only = if let Some(fixture) = fixtures.profiles.get(&id) {
            fixture.tcp_addr.is_none()
        } else {
            fixtures.cloud_only.pop_front().unwrap_or(false)
        };
        let listener = if cloud_only {
            None
        } else {
            let addr = fixtures
                .profiles
                .get(&id)
                .and_then(|fixture| fixture.tcp_addr)
                .unwrap_or_else(|| "127.0.0.1:0".parse().unwrap());
            let listener =
                std::net::TcpListener::bind(addr).expect("bind profile fixture LAN listener");
            listener.set_nonblocking(true).unwrap();
            Some(listener)
        };
        let fixture = fixtures
            .profiles
            .entry(id)
            .or_insert_with(|| ProfileFixture {
                tcp_addr: listener
                    .as_ref()
                    .map(|listener| listener.local_addr().unwrap()),
                tracked_tcp: Default::default(),
                clock: Arc::new(TestArtifactClock::new()),
            });
        RuntimeFixtures {
            listener: listener.map(|listener| tokio::net::TcpListener::from_std(listener).unwrap()),
            tracked_tcp: Some(fixture.tracked_tcp.clone()),
            artifact_clock: Some(fixture.clock.clone()),
            cloud: None,
            cloud_transport: cloud_addr.map(|addr| {
                tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
                    .unwrap()
                    .connect_lazy()
            }),
        }
    })
}

pub(super) async fn start(
    spec: InstallationSpec,
    identity: Arc<IdentityServer>,
    cloud: Option<&super::net::CloudRelay>,
) -> InstallationHandle {
    let disk_root = spec
        .persistent
        .then(|| tempfile::tempdir_in("/tmp").unwrap());
    let root = disk_root
        .as_ref()
        .map(|root| InstallationRoot::OnDisk(root.path().into()))
        .unwrap_or(InstallationRoot::InMemory);
    let fixtures = Arc::new(Mutex::new(FixturePlan {
        profiles: BTreeMap::new(),
        cloud_only: spec
            .profiles
            .iter()
            .map(|profile| profile.cloud_only)
            .collect(),
    }));
    let installation = Installation::open_for_test(
        options(&spec.name, root),
        fixture_factory(fixtures.clone(), cloud.map(|cloud| cloud.addr)),
    )
    .await
    .expect("start production installation");
    let root = installation.test_root();
    let mut records = Vec::new();
    for profile in &spec.profiles {
        let record = installation
            .create(OperationId::new(), Some(profile.name.clone()))
            .await
            .unwrap();
        assert!(record.available, "profile startup failed: {record:?}");
        if let Some(user) = &profile.cloud_user {
            installation
                .bind(
                    OperationId::new(),
                    BindRequest {
                        target: BindTarget::Explicit(record.record.id),
                        cloud_url: identity.url(),
                        staged_refresh_token: identity.refresh_token_for(user),
                        adopt_non_pristine: false,
                    },
                )
                .await
                .expect("bind fixture profile");
        }
        records.push(record);
    }
    let inner = Arc::new_cyclic(|weak| InstallationInner {
        profiles: spec
            .profiles
            .iter()
            .zip(records)
            .map(|(profile, record)| {
                let id = record.record.id;
                let fixtures = fixtures.lock().unwrap();
                let fixture = &fixtures.profiles[&id];
                let paths = ProfilePaths::for_id(&root, id).unwrap();
                let daemon = Arc::new(DaemonInner {
                    name: format!("{}/{}", spec.name, profile.name),
                    host_id: record.host_id,
                    data_dir: paths.data_dir.clone(),
                    artifact_clock: fixture.clock.clone(),
                    tcp_addr: fixture.tcp_addr,
                    cloud: profile.cloud_user.as_ref().map(|user| {
                        let cloud = cloud.expect("cloud_user requires .cloud()");
                        let (user_id, token) = cloud.credentials_for_user(user);
                        CloudAttachment {
                            addr: cloud.addr,
                            user_id,
                            token,
                        }
                    }),
                    runtime: tokio::sync::Mutex::new(None),
                    installation: Some(ProfileOwner {
                        installation: weak.clone(),
                        id,
                        paths,
                    }),
                    tracked_tcp: fixture.tracked_tcp.clone(),
                });
                (profile.name.clone(), (id, daemon))
            })
            .collect(),
        name: spec.name,
        current: RwLock::new(Some(Arc::new(installation))),
        identity,
        fixtures,
        cloud_addr: cloud.map(|cloud| cloud.addr),
        root,
        persistent: spec.persistent,
        _disk_root: disk_root,
        lifecycle: tokio::sync::Mutex::new(()),
    });
    InstallationHandle {
        inner,
        net: Weak::new(),
    }
}
