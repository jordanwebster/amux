//! Owns complete profile runtimes independently of any connected client.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, RwLock, broadcast, watch};
use uuid::Uuid;

use super::binding::{BindError, BindRequest};
use super::credentials::ProfileCredentialStore;
use super::{
    InstallationError, InstallationRoot, InstallationSettings, Listeners, Observed, OperationGate,
    ProfileId, ProfileLabel, ProfilePaths, ProfileRecord, Registry,
};
use crate::HostId;
use crate::auth::CredentialProvider;
use crate::client::Client;
use crate::config::{ConfigError, InstallationConfig, ProfileConfig, check_path};
use crate::profile::runtime::{self, ProfileRuntime, ProfileRuntimeOptions, RuntimeConfig};
use crate::profile::status::RuntimeStatus;
use crate::server::ShutdownReason;

const WATCH_CAPACITY: usize = 256;
const LEDGER_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(pub Uuid);

impl OperationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    Unbound,
    Bound,
    LoggedOut,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileStatus {
    pub record: ProfileRecord,
    pub intent: Intent,
    pub observed: Observed,
    pub socket_path: Option<PathBuf>,
    /// Nil only when startup failed before an identity could be loaded.
    pub host_id: HostId,
    pub startup_error: Option<String>,
    /// False while starting, deleting, or after a startup/cleanup failure.
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileEvent {
    Upserted {
        sequence: u64,
        profile: Box<ProfileStatus>,
    },
    SnapshotComplete {
        sequence: u64,
    },
    Removed {
        sequence: u64,
        id: ProfileId,
    },
    Lagged,
}

/// A snapshot and a subscription are captured under the same lock. On overflow
/// the final item is Lagged; the caller must subscribe again for a fresh view.
pub struct ProfileWatch {
    snapshot: VecDeque<ProfileEvent>,
    receiver: broadcast::Receiver<ProfileEvent>,
    ended: bool,
}
impl ProfileWatch {
    pub async fn recv(&mut self) -> Option<ProfileEvent> {
        if self.ended {
            return None;
        }
        if let Some(event) = self.snapshot.pop_front() {
            return Some(event);
        }
        match self.receiver.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Lagged(_)) => {
                self.ended = true;
                Some(ProfileEvent::Lagged)
            }
            Err(broadcast::error::RecvError::Closed) => {
                self.ended = true;
                None
            }
        }
    }
}

#[derive(Clone)]
pub enum CredentialSource {
    #[cfg(feature = "local-agents")]
    ProfileFiles,
    /// The host owns secret storage and refresh. Tokens are checked against the
    /// profile binding before they can establish a cloud link.
    HostProvided(Arc<dyn Fn(ProfileId) -> Arc<dyn CredentialProvider> + Send + Sync>),
}

impl CredentialSource {
    fn uses_profile_files(&self) -> bool {
        match self {
            #[cfg(feature = "local-agents")]
            Self::ProfileFiles => true,
            Self::HostProvided(_) => false,
        }
    }
}

pub struct InstallationOptions {
    pub root: InstallationRoot,
    pub settings: InstallationSettings,
    pub listeners: Listeners,
    pub credentials: CredentialSource,
    pub identity_http: reqwest::Client,
}

pub struct Installation {
    pub(super) inner: Arc<Inner>,
    pub(crate) front_door_operations: Arc<crate::services::front_door::FrontDoorOperations>,
}

pub(super) struct Inner {
    state: Mutex<State>,
    /// Allows unrelated profile operations to run concurrently. Shutdown waits
    /// for accepted operations and prevents any new runtime from starting.
    lifecycle: RwLock<()>,
    root: PathBuf,
    _temporary_root: Option<tempfile::TempDir>,
    settings: Arc<InstallationSettings>,
    config: InstallationConfig,
    listeners: Listeners,
    credentials: CredentialSource,
    identity_http: reqwest::Client,
    binding: AsyncMutex<VecDeque<binding::PendingLogin>>,
    #[cfg(testnet)]
    fixtures: Option<RuntimeFixtureFactory>,
}

struct State {
    registry: Registry,
    profiles: BTreeMap<ProfileId, Entry>,
    deleted: HashSet<ProfileId>,
    operations: HashMap<OperationId, LedgerEntry>,
    completed: VecDeque<OperationId>,
    sequence: u64,
    events: Option<broadcast::Sender<ProfileEvent>>,
    stopped: bool,
    host_suspended: bool,
    update_active: bool,
    credential_clock: u64,
    revoked_accounts: HashMap<super::AccountId, u64>,
}

struct Entry {
    status: ProfileStatus,
    slot: Arc<Slot>,
    client: Option<Client>,
    deleting: bool,
}
struct Slot {
    operations: Arc<OperationGate>,
    runtime: Arc<AsyncMutex<Option<ProfileRuntime>>>,
    credentials: Mutex<Option<Arc<ProfileCredentialStore>>>,
    revoked_at: std::sync::atomic::AtomicU64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mutation {
    Create(Option<String>),
    Bind(BindRequest),
    Logout(ProfileId),
    Rename(ProfileId, u64, Option<String>),
    Pause(ProfileId),
    Resume(ProfileId),
    Delete(ProfileId, u64),
    SuspendAll(OperationId, SuspendReason),
    ResumeAll(OperationId),
}
#[derive(Clone)]
enum Outcome {
    Profile(Box<ProfileStatus>),
    Deleted,
    Suspended(SuspendReport),
    Resumed(ResumeReport),
    Bound(Result<Box<ProfileStatus>, BindError>),
}
type OperationResult = Result<Outcome, InstallationError>;
struct LedgerEntry {
    request: Mutation,
    result: watch::Receiver<Option<OperationResult>>,
}

impl State {
    fn entry(&self, id: ProfileId) -> Result<&Entry, InstallationError> {
        if self.deleted.contains(&id) {
            return Err(InstallationError::Deleted(id));
        }
        self.profiles
            .get(&id)
            .ok_or(InstallationError::UnknownProfile(id))
    }
    fn active(&self, id: ProfileId) -> Result<&Entry, InstallationError> {
        let entry = self.entry(id)?;
        if entry.deleting {
            return Err(InstallationError::Deleted(id));
        }
        Ok(entry)
    }
    fn publish(&mut self, id: ProfileId) {
        self.sequence += 1;
        let profile = Box::new(self.profiles[&id].status.clone());
        if let Some(events) = &self.events {
            let _ = events.send(ProfileEvent::Upserted {
                sequence: self.sequence,
                profile,
            });
        }
    }
    fn refresh_record(&mut self, id: ProfileId) {
        // Persistence can report a directory-sync failure after the rename
        // committed. Always publish the registry's actual visible record.
        if let Ok(record) = self.registry.get(id).cloned() {
            self.profiles.get_mut(&id).unwrap().status.record = record;
        }
    }
}

fn read_profile_config(
    installation: &InstallationConfig,
    id: ProfileId,
    path: &std::path::Path,
) -> Result<ProfileConfig, ConfigError> {
    let profile = ProfileConfig::from_file(path)?;
    check_path(
        "installation_config",
        &installation.file_path(),
        &profile.installation_config,
    )?;
    profile.check_paths(&installation.root, id)?;
    // A profile file never silently falls back to process defaults when its
    // explicitly referenced installation file is missing or invalid.
    let referenced = InstallationConfig::from_file(&profile.installation_config)?;
    check_path("root", &installation.root, &referenced.root)?;
    Ok(profile)
}

fn write_yaml(path: &std::path::Path, value: &impl Serialize) -> Result<(), InstallationError> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| InstallationError::InvalidPath(path.to_owned()))?;
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    let yaml = serde_yaml::to_string(value)
        .map_err(|error| InstallationError::Registry(error.to_string()))?;
    staged.write_all(yaml.as_bytes())?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

impl Installation {
    /// Serve a desktop installation until the host requests shutdown or a local
    /// administrator stops it through the front door. One sleep assertion covers
    /// every profile for the lifetime of the daemon.
    #[cfg(all(unix, feature = "local-agents"))]
    pub async fn serve(
        self,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<(), InstallationError> {
        if self.inner.listeners != Listeners::Sockets {
            return Err(InstallationError::Unavailable(
                "serving a desktop installation requires socket listeners".into(),
            ));
        }
        let _sleep = crate::sleep_inhibitor::SleepInhibitor::new(
            self.inner.config.prevent_idle_sleep.unwrap_or(false),
        );
        let installation = Arc::new(self);
        let front = super::FrontDoor::new(
            installation.clone(),
            Some(installation.inner.config.front_door_socket.clone()),
        );
        let listener = match front.listen() {
            Ok(listener) => listener,
            Err(error) => {
                installation.stop(ShutdownReason::UserRequested).await;
                return Err(error.into());
            }
        };
        let mut watch = installation.watch();
        tokio::select! {
            () = shutdown => installation.stop(ShutdownReason::UserRequested).await,
            () = async {
                while let Some(event) = watch.recv().await {
                    if matches!(event, ProfileEvent::Lagged) {
                        watch = installation.watch();
                    }
                }
            } => {}
        }
        drop(front);
        drop(installation);
        listener.stop().await;
        Ok(())
    }

    /// Own an installation and start its profiles independently. Keep this owner
    /// alive across screens; dropping clients never stops a profile.
    pub async fn open(options: InstallationOptions) -> Result<Self, InstallationError> {
        Self::open_inner(
            options,
            None,
            #[cfg(testnet)]
            None,
        )
        .await
    }

    /// Open a desktop installation using shared preferences from its config.
    #[cfg(feature = "local-agents")]
    pub async fn from_config(config: InstallationConfig) -> Result<Self, InstallationError> {
        config.validate()?;
        let options = InstallationOptions {
            root: InstallationRoot::OnDisk(config.root.clone()),
            settings: config.settings(),
            listeners: Listeners::Sockets,
            credentials: CredentialSource::ProfileFiles,
            identity_http: reqwest::Client::new(),
        };
        Self::open_inner(
            options,
            Some(config),
            #[cfg(testnet)]
            None,
        )
        .await
    }

    async fn open_inner(
        options: InstallationOptions,
        config: Option<InstallationConfig>,
        #[cfg(testnet)] fixtures: Option<RuntimeFixtureFactory>,
    ) -> Result<Self, InstallationError> {
        let registry = Registry::open(options.root)?;
        let temporary_root = if registry.path().is_none() {
            let parent = std::env::temp_dir();
            Some(tempfile::Builder::new().prefix("ai").tempdir_in(parent)?)
        } else {
            None
        };
        let root = registry
            .path()
            .map(PathBuf::from)
            .or_else(|| temporary_root.as_ref().map(|dir| dir.path().to_owned()))
            .unwrap();
        let root = std::fs::canonicalize(root)?;
        let mut config = config.unwrap_or_else(|| InstallationConfig {
            root: root.clone(),
            front_door_socket: root.join("amux.sock"),
            host_name: options.settings.host_name.clone(),
            prevent_idle_sleep: options.settings.prevent_idle_sleep,
            keybinds: options.settings.keybinds.clone(),
            ui: options.settings.ui.clone(),
            keymaps_dir: options.settings.keymaps_dir.clone(),
            minimum_client_versions: options.settings.minimum_client_versions.clone(),
            update_manifest_url: options.settings.update_manifest_url.clone(),
            ..InstallationConfig::default()
        });
        config.root = root.clone();
        let config_path = config.file_path();
        if !config_path.exists() {
            write_yaml(&config_path, &config)?;
        }
        config.path = Some(std::fs::canonicalize(config_path)?);
        // Refuse path disagreement before starting any profile. Other startup
        // failures remain per-profile so an unavailable device cannot block peers.
        for record in registry
            .profiles()
            .filter(|record| !registry.is_deleting(record.id))
        {
            let paths = ProfilePaths::allocated(&root, record.id);
            let path = paths.config_path.as_ref().unwrap();
            if path.exists()
                && let Err(error @ ConfigError::Disagreement { .. }) =
                    read_profile_config(&config, record.id, path)
            {
                return Err(error.into());
            }
        }
        let records: Vec<_> = registry.profiles().cloned().collect();
        let (events, _) = broadcast::channel(WATCH_CAPACITY);
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                registry,
                profiles: BTreeMap::new(),
                deleted: HashSet::new(),
                operations: HashMap::new(),
                completed: VecDeque::new(),
                sequence: 0,
                events: Some(events),
                stopped: false,
                host_suspended: false,
                update_active: update::Journal::load(&root)?
                    .is_some_and(|journal| journal.pending()),
                credential_clock: 0,
                revoked_accounts: HashMap::new(),
            }),
            lifecycle: RwLock::new(()),
            root,
            _temporary_root: temporary_root,
            settings: Arc::new(options.settings),
            config,
            listeners: options.listeners,
            credentials: options.credentials,
            identity_http: options.identity_http,
            binding: AsyncMutex::new(VecDeque::new()),
            #[cfg(testnet)]
            fixtures,
        });
        for record in records {
            inner.insert(record);
        }
        // Starting one profile never gates startup of another on its network.
        let ids: Vec<_> = inner
            .state
            .lock()
            .unwrap()
            .profiles
            .iter()
            .filter(|(_, entry)| !entry.deleting)
            .map(|(id, _)| *id)
            .collect();
        let installation = Self {
            inner,
            front_door_operations: Arc::default(),
        };
        let starts = ids.into_iter().map(|id| {
            let inner = installation.inner.clone();
            tokio::spawn(async move {
                let _lifecycle = inner.lifecycle.read().await;
                if !inner.state.lock().unwrap().stopped {
                    let _ = inner.start(id).await;
                }
            })
        });
        futures_util::future::join_all(starts).await;
        Ok(installation)
    }

    pub fn root(&self) -> &std::path::Path {
        &self.inner.root
    }

    #[cfg(test_fixtures)]
    pub(crate) async fn use_test_cloud_transport(
        &self,
        id: ProfileId,
        channel: tonic::transport::Channel,
    ) -> Result<(), InstallationError> {
        let slot = self.inner.state.lock().unwrap().active(id)?.slot.clone();
        let mut runtime = slot.runtime.lock().await;
        if self
            .inner
            .state
            .lock()
            .unwrap()
            .active(id)?
            .status
            .record
            .binding
            .is_some()
        {
            return Err(InstallationError::Unavailable(
                "install relay transport before binding".into(),
            ));
        }
        runtime
            .as_mut()
            .ok_or_else(|| InstallationError::Unavailable("profile is not running".into()))?
            .test_cloud_transport = Some(channel);
        Ok(())
    }

    /// Obtain pairing and trust administration for a running profile in process.
    pub async fn admin(&self, id: ProfileId) -> Result<super::ProfileAdmin, InstallationError> {
        self.admin_service(id)
            .await
            .map(|service| super::ProfileAdmin::new(service, id))
    }

    pub(crate) async fn admin_service(
        &self,
        id: ProfileId,
    ) -> Result<crate::services::client::ClientService, InstallationError> {
        let slot = self.inner.state.lock().unwrap().active(id)?.slot.clone();
        let runtime = slot.runtime.lock().await;
        self.inner.state.lock().unwrap().active(id)?;
        runtime
            .as_ref()
            .map(|runtime| runtime.services.client.clone())
            .ok_or_else(|| InstallationError::Unavailable("profile is not running".into()))
    }

    pub(crate) async fn stop(&self, reason: ShutdownReason) {
        let inner = self.inner.clone();
        let _ = tokio::spawn(async move { inner.shutdown(reason).await }).await;
    }

    pub fn profiles(&self) -> Vec<ProfileStatus> {
        self.inner
            .state
            .lock()
            .unwrap()
            .profiles
            .values()
            .map(|entry| entry.status.clone())
            .collect()
    }
    pub fn watch(&self) -> ProfileWatch {
        let state = self.inner.state.lock().unwrap();
        let mut snapshot: VecDeque<_> = state
            .profiles
            .values()
            .map(|entry| ProfileEvent::Upserted {
                sequence: state.sequence,
                profile: Box::new(entry.status.clone()),
            })
            .collect();
        snapshot.push_back(ProfileEvent::SnapshotComplete {
            sequence: state.sequence,
        });
        let receiver = match &state.events {
            Some(events) => events.subscribe(),
            None => broadcast::channel(1).1,
        };
        ProfileWatch {
            snapshot,
            receiver,
            ended: false,
        }
    }
    pub fn client(&self, id: ProfileId) -> Result<Client, InstallationError> {
        let state = self.inner.state.lock().unwrap();
        if state.stopped {
            return Err(InstallationError::Unavailable(
                "installation stopped".into(),
            ));
        }
        let entry = state.active(id)?;
        entry.client.clone().ok_or_else(|| {
            InstallationError::Unavailable(
                entry
                    .status
                    .startup_error
                    .clone()
                    .unwrap_or_else(|| "profile is starting".into()),
            )
        })
    }
    pub async fn create(
        &self,
        op: OperationId,
        label: Option<String>,
    ) -> Result<ProfileStatus, InstallationError> {
        self.profile_operation(op, Mutation::Create(label)).await
    }
    pub async fn bind(
        &self,
        op: OperationId,
        request: BindRequest,
    ) -> Result<ProfileStatus, BindError> {
        match self.inner.operate(op, Mutation::Bind(request)).await? {
            Outcome::Bound(result) => result.map(|status| *status),
            _ => unreachable!(),
        }
    }

    pub async fn logout(
        &self,
        op: OperationId,
        id: ProfileId,
    ) -> Result<ProfileStatus, InstallationError> {
        self.profile_operation(op, Mutation::Logout(id)).await
    }

    pub async fn rename(
        &self,
        op: OperationId,
        id: ProfileId,
        expected_revision: u64,
        override_name: Option<String>,
    ) -> Result<ProfileStatus, InstallationError> {
        self.profile_operation(op, Mutation::Rename(id, expected_revision, override_name))
            .await
    }
    pub async fn pause(
        &self,
        op: OperationId,
        id: ProfileId,
    ) -> Result<ProfileStatus, InstallationError> {
        self.profile_operation(op, Mutation::Pause(id)).await
    }
    pub async fn resume(
        &self,
        op: OperationId,
        id: ProfileId,
    ) -> Result<ProfileStatus, InstallationError> {
        self.profile_operation(op, Mutation::Resume(id)).await
    }
    pub async fn delete(
        &self,
        op: OperationId,
        id: ProfileId,
        confirm_revision: u64,
    ) -> Result<(), InstallationError> {
        match self
            .inner
            .operate(op, Mutation::Delete(id, confirm_revision))
            .await?
        {
            Outcome::Deleted => Ok(()),
            _ => unreachable!(),
        }
    }
    async fn profile_operation(
        &self,
        op: OperationId,
        request: Mutation,
    ) -> Result<ProfileStatus, InstallationError> {
        match self.inner.operate(op, request).await? {
            Outcome::Profile(status) => Ok(*status),
            _ => unreachable!(),
        }
    }
    /// Stop all cloud connectors while retaining local profiles, trust and clients.
    pub async fn host_suspend(&self) {
        let inner = self.inner.clone();
        let _ = tokio::spawn(async move { inner.set_host_suspended(true).await }).await;
    }

    /// Refresh and reconnect eligible profiles after the host becomes active.
    pub async fn host_resume(&self) {
        let inner = self.inner.clone();
        let _ = tokio::spawn(async move { inner.set_host_suspended(false).await }).await;
    }

    /// Finish runtime and transport teardown asynchronously, even if this future is dropped.
    pub async fn shutdown(self, reason: ShutdownReason) {
        let inner = self.inner.clone();
        // The owned teardown continues if a host drops the shutdown future.
        let _ = tokio::spawn(async move { inner.shutdown(reason).await }).await;
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        if !self.inner.state.lock().unwrap().stopped
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let inner = self.inner.clone();
            handle.spawn(async move {
                inner.shutdown(ShutdownReason::UserRequested).await;
            });
        }
    }
}

impl Inner {
    fn cloud_eligible(&self, id: ProfileId) -> bool {
        let state = self.state.lock().unwrap();
        !state.stopped
            && !state.host_suspended
            && state
                .active(id)
                .is_ok_and(|entry| self.intent(&entry.status.record, &entry.slot) == Intent::Bound)
    }

    async fn set_host_suspended(&self, suspended: bool) {
        // A host transition excludes create, bind, pause, delete and shutdown.
        // Profile connectors transition concurrently so one account cannot delay another.
        let _lifecycle = self.lifecycle.write().await;
        let slots = {
            let mut state = self.state.lock().unwrap();
            if state.stopped || state.host_suspended == suspended {
                return;
            }
            state.host_suspended = suspended;
            state
                .profiles
                .iter()
                .filter(|(_, entry)| !entry.deleting)
                .map(|(id, entry)| (*id, entry.slot.clone()))
                .collect::<Vec<_>>()
        };
        futures_util::future::join_all(slots.into_iter().map(|(id, slot)| async move {
            let runtime = slot.runtime.lock().await;
            if let Some(runtime) = runtime.as_ref() {
                if suspended {
                    runtime.stop_cloud().await;
                } else if self.cloud_eligible(id) {
                    let store = slot.credentials.lock().unwrap().clone();
                    if let Some(store) = store {
                        store.refresh_after_host_resume().await;
                    }
                    let _ = runtime.start_cloud().await;
                }
            }
        }))
        .await;
    }

    fn insert(&self, record: ProfileRecord) {
        let id = record.id;
        let intent = if record.paused {
            Intent::Paused
        } else if record.binding.is_none() {
            Intent::Unbound
        } else {
            Intent::LoggedOut
        };
        let mut entry = Entry {
            status: ProfileStatus {
                record,
                intent,
                observed: Observed::Local,
                socket_path: None,
                host_id: Uuid::nil(),
                startup_error: None,
                available: false,
            },
            slot: Arc::new(Slot {
                operations: Arc::default(),
                runtime: Arc::new(AsyncMutex::new(None)),
                credentials: Mutex::new(None),
                revoked_at: std::sync::atomic::AtomicU64::new(0),
            }),
            client: None,
            deleting: false,
        };
        let mut state = self.state.lock().unwrap();
        if state.update_active {
            entry.slot.operations.freeze();
        }
        if state.registry.is_deleting(id) {
            entry.deleting = true;
            entry.slot.operations.close();
            entry.status.startup_error =
                Some("delete cleanup is incomplete; retry deletion".into());
        }
        state.profiles.insert(id, entry);
        state.publish(id);
    }

    async fn start(self: &Arc<Self>, id: ProfileId) -> Result<ProfileStatus, InstallationError> {
        let slot = {
            let state = self.state.lock().unwrap();
            let entry = state.active(id)?;
            entry.slot.clone()
        };
        let _operation = slot.operations.lock().await;
        let record = self.state.lock().unwrap().active(id)?.status.record.clone();
        let result = async {
            let mut paths = ProfilePaths::for_id(&self.root, id)?;
            if let Some(reports_dir) = &self.config.reports_dir {
                paths.reports_dir = reports_dir.clone();
            }
            let version = self.state.lock().unwrap().registry.credential_version(id);
            let store = Arc::new(ProfileCredentialStore::open(
                self.credentials
                    .uses_profile_files()
                    .then(|| paths.credentials_path().unwrap()),
                self.identity_http.clone(),
                record.binding.as_ref(),
                if self.credentials.uses_profile_files() {
                    version
                } else {
                    None
                },
            )?);
            if let (CredentialSource::HostProvided(provider), Some(binding)) =
                (&self.credentials, &record.binding)
                && !self.state.lock().unwrap().registry.is_logged_out(id)
            {
                store.use_host(binding, provider(id));
            }
            let credentials: Option<Arc<dyn CredentialProvider>> = Some(store.clone());
            *slot.credentials.lock().unwrap() = Some(store);
            let cloud_url = record
                .binding
                .as_ref()
                .map(|binding| binding.account.service.to_string())
                .unwrap_or_else(|| crate::config::Config::default().cloud_url);
            let mut options = ProfileRuntimeOptions {
                paths: paths.clone(),
                config: RuntimeConfig {
                    cloud_url,
                    tcp_port: None,
                },
                shared: self.settings.clone(),
                credentials,

                listeners: self.listeners,
                #[cfg(testnet)]
                fixtures: self
                    .fixtures
                    .as_ref()
                    .map(|factory| factory(id))
                    .unwrap_or_default(),
            };
            let config_path = paths.config_path.as_ref().unwrap();
            if config_path.exists() {
                let config = read_profile_config(&self.config, id, config_path)?;
                if record.binding.is_none() {
                    options.config.cloud_url = config.cloud_url;
                }
                options.config.tcp_port = config.tcp_port;
            } else {
                let config = ProfileConfig {
                    installation_config: self.config.file_path(),
                    socket_path: paths.socket_path.clone(),
                    data_dir: paths.data_dir.clone(),
                    state_path: paths.state_path.clone(),
                    cloud_url: options.config.cloud_url.clone(),
                    tcp_port: options.config.tcp_port,
                };
                write_yaml(config_path, &config)?;
            }
            let weak = Arc::downgrade(self);
            let reporters = options.shared.status_reporters.resolve(&paths.state_path);
            let status = RuntimeStatus::new(reporters.update, reporters.subscription)
                .with_observer(move |observed| {
                    if let Some(inner) = weak.upgrade() {
                        let mut state = inner.state.lock().unwrap();
                        if let Some(entry) = state.profiles.get_mut(&id)
                            && !entry.deleting
                            && entry.status.observed != observed
                        {
                            entry.status.observed = observed;
                            state.publish(id);
                        }
                    }
                });
            let runtime = runtime::start_supervised(options, status, slot.operations.clone())
                .await
                .map_err(|error| InstallationError::Unavailable(error.to_string()))?;
            if self.cloud_eligible(id) {
                let _ = runtime.start_cloud().await;
            }
            Ok::<_, InstallationError>((runtime, paths))
        }
        .await;
        match result {
            Ok((runtime, paths)) => {
                let client = runtime.client();
                let host_id = runtime.host_id;
                *slot.runtime.lock().await = Some(runtime);
                let mut state = self.state.lock().unwrap();
                let entry = state.profiles.get_mut(&id).unwrap();
                entry.client = Some(client);
                entry.status.host_id = host_id;
                entry.status.available = true;
                entry.status.intent = self.intent(&entry.status.record, &slot);
                entry.status.socket_path =
                    self.listeners.has_sockets().then_some(paths.socket_path);
                state.publish(id);
            }
            Err(error) => {
                let mut state = self.state.lock().unwrap();
                let entry = state.profiles.get_mut(&id).unwrap();
                entry.status.observed = Observed::StartupFailed;
                entry.status.startup_error = Some(error.to_string());
                entry.status.host_id = crate::identity::stored_host_id_in(
                    &self.root.join("profiles").join(id.to_string()).join("data"),
                )
                .unwrap_or_default();
                state.publish(id);
            }
        }
        Ok(self.state.lock().unwrap().entry(id)?.status.clone())
    }

    async fn operate(self: &Arc<Self>, op: OperationId, request: Mutation) -> OperationResult {
        let mut result = {
            let mut state = self.state.lock().unwrap();
            if let Some(entry) = state.operations.get(&op) {
                if entry.request != request {
                    return Err(InstallationError::Registry(
                        "operation id reused with a different request".into(),
                    ));
                }
                entry.result.clone()
            } else {
                if state.stopped {
                    return Err(InstallationError::Unavailable(
                        "installation stopped".into(),
                    ));
                }
                let (sender, receiver) = watch::channel(None);
                state.operations.insert(
                    op,
                    LedgerEntry {
                        request: request.clone(),
                        result: receiver.clone(),
                    },
                );
                let inner = self.clone();
                tokio::spawn(async move {
                    let updating =
                        matches!(request, Mutation::SuspendAll(..) | Mutation::ResumeAll(..));
                    let (_shared, _exclusive) = if updating {
                        (None, Some(inner.lifecycle.write().await))
                    } else {
                        (Some(inner.lifecycle.read().await), None)
                    };
                    let result = if inner.state.lock().unwrap().stopped {
                        Err(InstallationError::Unavailable(
                            "installation stopped".into(),
                        ))
                    } else if updating {
                        inner.update(request).await
                    } else if inner.state.lock().unwrap().update_active {
                        Err(InstallationError::Unavailable(
                            "installation update is in progress".into(),
                        ))
                    } else {
                        inner.mutate(request).await
                    };
                    let mut state = inner.state.lock().unwrap();
                    sender.send_replace(Some(result));
                    state.completed.push_back(op);
                    while state.completed.len() > LEDGER_CAPACITY {
                        let expired = state.completed.pop_front().unwrap();
                        state.operations.remove(&expired);
                    }
                });
                receiver
            }
        };
        loop {
            if let Some(result) = result.borrow_and_update().clone() {
                return result;
            }
            if result.changed().await.is_err() {
                return Err(InstallationError::Unavailable(
                    "operation worker stopped".into(),
                ));
            }
        }
    }

    async fn mutate(self: &Arc<Self>, request: Mutation) -> OperationResult {
        if let Mutation::Bind(request) = request {
            return Ok(Outcome::Bound(
                self.bind_request(request).await.map(Box::new),
            ));
        }
        if let Mutation::Create(label) = request {
            let id = ProfileId::new();
            let (result, record) = {
                let mut state = self.state.lock().unwrap();
                let result = state.registry.create(
                    id,
                    ProfileLabel {
                        override_name: label,
                        ..Default::default()
                    },
                );
                let record = state.registry.get(id).ok().cloned();
                (result, record)
            };
            let started = if let Some(record) = record {
                self.insert(record);
                self.start(id).await
            } else {
                Err(InstallationError::UnknownProfile(id))
            };
            result?;
            return started.map(|status| Outcome::Profile(Box::new(status)));
        }

        let id = match request {
            Mutation::Rename(id, ..)
            | Mutation::Logout(id)
            | Mutation::Pause(id)
            | Mutation::Resume(id)
            | Mutation::Delete(id, _) => id,
            Mutation::Create(_)
            | Mutation::Bind(_)
            | Mutation::SuspendAll(..)
            | Mutation::ResumeAll(..) => unreachable!(),
        };
        let slot = self.state.lock().unwrap().entry(id)?.slot.clone();
        let _operation = slot.operations.lock().await;
        if let Mutation::Delete(_, revision) = request {
            return self
                .delete(id, revision, &slot)
                .await
                .map(|()| Outcome::Deleted);
        }
        if matches!(request, Mutation::Logout(_)) {
            return self
                .logout_profile(id, &slot)
                .await
                .map(|status| Outcome::Profile(Box::new(status)));
        }
        let persist_result = {
            let mut state = self.state.lock().unwrap();
            let entry = state.active(id)?;
            let mut record = entry.status.record.clone();
            match &request {
                Mutation::Rename(_, expected, name) => {
                    if record.revision != *expected {
                        return Err(InstallationError::RevisionMismatch {
                            expected: *expected,
                            actual: record.revision,
                        });
                    }
                    record.label.override_name = name.clone();
                }
                Mutation::Pause(_) => record.paused = true,
                Mutation::Resume(_) => record.paused = false,
                _ => unreachable!(),
            }
            let result = state.registry.replace(record);
            state.refresh_record(id);
            let entry = state.profiles.get_mut(&id).unwrap();
            entry.status.intent = self.intent(&entry.status.record, &slot);
            state.publish(id);
            result
        };
        let runtime = slot.runtime.lock().await;
        if let Some(runtime) = runtime.as_ref() {
            match request {
                Mutation::Pause(_) => {
                    let paused = self.state.lock().unwrap().profiles[&id]
                        .status
                        .record
                        .paused;
                    if paused {
                        runtime.stop_cloud().await;
                    }
                }
                Mutation::Resume(_) => {
                    let bound = self.cloud_eligible(id);
                    if bound {
                        let _ = runtime.start_cloud().await;
                    }
                }
                _ => {}
            }
        }
        persist_result?;
        Ok(Outcome::Profile(Box::new(
            self.state.lock().unwrap().profiles[&id].status.clone(),
        )))
    }

    async fn delete(
        &self,
        id: ProfileId,
        revision: u64,
        slot: &Slot,
    ) -> Result<(), InstallationError> {
        {
            let mut state = self.state.lock().unwrap();
            let entry = state.entry(id)?;
            if entry.status.record.revision != revision {
                return Err(InstallationError::RevisionMismatch {
                    expected: revision,
                    actual: entry.status.record.revision,
                });
            }
            let intent = state.registry.mark_deleting(id, revision);
            if !state.registry.is_deleting(id) {
                intent?;
            }
            state.revoke_credentials(id);
            let entry = state.profiles.get_mut(&id).unwrap();
            entry.deleting = true;
            entry.client = None;
            entry.status.available = false;
            slot.operations.close();
            if let Some(store) = slot.credentials.lock().unwrap().as_ref() {
                store.invalidate_pending();
            }
            state.publish(id);
        }
        if let Some(runtime) = slot.runtime.lock().await.take() {
            runtime.stop(ShutdownReason::UserRequested).await;
        }
        let directory = self.root.join("profiles").join(id.to_string());
        let cleanup = match std::fs::remove_dir_all(&directory) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                Err(InstallationError::Io(error))
            }
            _ => Ok(()),
        };
        let mut state = self.state.lock().unwrap();
        let cleanup = cleanup.and_then(|()| state.registry.remove(id, revision));
        if state.registry.get(id).is_err() {
            state.profiles.remove(&id);
            state.deleted.insert(id);
            state.sequence += 1;
            if let Some(events) = &state.events {
                let _ = events.send(ProfileEvent::Removed {
                    sequence: state.sequence,
                    id,
                });
            }
        } else if let Err(error) = &cleanup {
            let entry = state.profiles.get_mut(&id).unwrap();
            entry.status.startup_error = Some(format!("delete cleanup failed: {error}"));
            state.publish(id);
        }
        cleanup
    }

    async fn shutdown(&self, reason: ShutdownReason) {
        let _lifecycle = self.lifecycle.write().await;
        let slots = {
            let mut state = self.state.lock().unwrap();
            if state.stopped {
                return;
            }
            state.stopped = true;
            let ids: Vec<_> = state.profiles.keys().copied().collect();
            let mut slots = Vec::new();
            for id in ids {
                let entry = state.profiles.get_mut(&id).unwrap();
                entry.client = None;
                entry.status.available = false;
                entry.deleting = true;
                slots.push(entry.slot.clone());
                state.publish(id);
            }
            slots
        };
        futures_util::future::join_all(slots.into_iter().map(|slot| async move {
            let _operation = slot.operations.lock().await;
            slot.operations.close();
            if let Some(store) = slot.credentials.lock().unwrap().as_ref() {
                store.invalidate_pending();
            }
            if let Some(runtime) = slot.runtime.lock().await.take() {
                runtime.stop(reason).await;
            }
        }))
        .await;
        self.state.lock().unwrap().events.take();
    }
}

mod binding;
mod update;
pub use update::{
    AgentResumeResult, AgentResumeStatus, ProfileResumeResult, ProfileSuspendResult, ResumeReport,
    SuspendReason, SuspendReport,
};

#[cfg(all(test, feature = "local-agents"))]
mod tests;

#[cfg(testnet)]
mod testnet;
#[cfg(testnet)]
use testnet::RuntimeFixtureFactory;
