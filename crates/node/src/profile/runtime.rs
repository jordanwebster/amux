//! Runtime ownership for one complete amux device profile.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use client::Client;
use host_api::{HostConfig, LocalAgentHost, LocalAgentHostFactory};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;

use super::status::{Observed, RuntimeStatus};
use crate::auth::CredentialProvider;
use crate::config::{Config, ConfigError, Keybinds, UiSettings};
use crate::identity;
use crate::server::ShutdownReason;
use crate::services::{
    CloudConnector, DeviceRuntimeSecurity, StartedUserServices, establish_cloud_connection,
    start_user_services,
};
use crate::subscription::SubscriptionReporter;
use crate::transport::InProcessConnection;
use crate::update::UpdateReporter;
use crate::user_state::ServerState;

const LINK_CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(200);

use crate::installation::ProfilePaths;

/// Settings that vary between profiles.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeConfig {
    pub(crate) cloud_url: String,
    pub(crate) tcp_port: Option<u16>,
}

/// Installation-owned settings shared by every profile runtime.
#[derive(Clone)]
pub struct InstallationSettings {
    pub repository_roots: Vec<PathBuf>,
    pub host_name: String,
    pub prevent_idle_sleep: Option<bool>,
    pub keybinds: Keybinds,
    pub ui: UiSettings,
    pub keymaps_dir: PathBuf,
    pub minimum_client_versions: HashMap<String, String>,
    pub update_manifest_url: String,
    pub status_reporters: crate::update::StatusReporters,
}

/// Which externally reachable listeners a runtime owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listeners {
    InProcessOnly,
    /// Expose the local profile socket without also binding the configured
    /// direct-TCP port. Test harnesses use this while supplying their own
    /// pre-bound, tracked TCP listener.
    ClientSocket,
    Sockets,
}

impl Listeners {
    pub(crate) fn has_sockets(self) -> bool {
        match self {
            Self::InProcessOnly => false,
            Self::ClientSocket | Self::Sockets => true,
        }
    }
}

#[derive(Clone)]
pub enum CloudFixtureAuth {
    Bearer(String),
    Refreshing(crate::routing::LinkConnectorAuth),
}

#[derive(Default)]
pub struct RuntimeFixtures {
    pub listener: Option<TcpListener>,
    pub tracked_tcp: Option<crate::dispatcher::TrackedTcpConnections>,
    pub host_factory: Option<Arc<dyn LocalAgentHostFactory>>,
    pub cloud: Option<(tonic::transport::Channel, CloudFixtureAuth)>,
    pub cloud_transport: Option<tonic::transport::Channel>,
}

pub struct ProfileRuntimeOptions {
    pub(crate) paths: ProfilePaths,
    pub(crate) config: RuntimeConfig,
    pub(crate) shared: Arc<InstallationSettings>,
    pub(crate) credentials: Option<Arc<dyn CredentialProvider>>,
    pub(crate) host_factory: Option<Arc<dyn LocalAgentHostFactory>>,

    pub(crate) listeners: Listeners,
    pub fixtures: RuntimeFixtures,
}

impl ProfileRuntimeOptions {
    pub fn from_legacy_config(
        config: Config,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
        subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
        listeners: Listeners,
        host_factory: Option<Arc<dyn LocalAgentHostFactory>>,
    ) -> Self {
        let paths = ProfilePaths {
            config_path: config.path.clone(),
            socket_path: config.socket_path.clone(),
            state_path: config.state_path.clone(),
            data_dir: config.data_dir.clone(),
            reports_dir: config.reports_dir(),
        };
        let profile = RuntimeConfig {
            cloud_url: config.cloud_url.clone(),
            tcp_port: config.tcp_port,
        };
        let shared = InstallationSettings {
            repository_roots: config.repository_roots,
            host_name: config.host_name,
            prevent_idle_sleep: config.prevent_idle_sleep,
            keybinds: config.keybinds,
            ui: config.ui,
            keymaps_dir: crate::keymap_dir(&config.data_dir),
            minimum_client_versions: config.minimum_client_versions,
            update_manifest_url: crate::InstallationConfig::default().update_manifest_url,
            status_reporters: crate::update::StatusReporters::Host {
                update: update_reporter,
                subscription: subscription_reporter,
            },
        };
        Self {
            paths,
            config: profile,
            shared: Arc::new(shared),
            credentials,
            host_factory,

            listeners,
            fixtures: RuntimeFixtures::default(),
        }
    }

    pub(crate) fn service_config(&self) -> Config {
        Config {
            repository_roots: self.shared.repository_roots.clone(),
            host_name: self.shared.host_name.clone(),
            cloud_url: self.config.cloud_url.clone(),
            socket_path: self.paths.socket_path.clone(),
            tcp_port: self.config.tcp_port,
            state_path: self.paths.state_path.clone(),
            data_dir: self.paths.data_dir.clone(),
            reports_dir: Some(self.paths.reports_dir.clone()),

            prevent_idle_sleep: self.shared.prevent_idle_sleep,
            minimum_client_versions: self.shared.minimum_client_versions.clone(),
            keybinds: self.shared.keybinds.clone(),
            ui: self.shared.ui.clone(),
            claude: Default::default(),
            path: self.paths.config_path.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ProfileStartError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile state error: {0}")]
    State(String),
}

#[derive(Debug, Error)]
pub enum CloudStartError {
    #[error("profile has no cloud credentials")]
    MissingCredentials,
}

/// All state and tasks owned by one complete device profile.
pub struct ProfileRuntime {
    pub host_id: crate::HostId,
    paths: ProfilePaths,
    state: Arc<RwLock<ServerState>>,
    pub agent_host: Option<Arc<dyn LocalAgentHost>>,
    pub services: StartedUserServices,
    pub trust: crate::trust::SharedTrustStore,
    test_cloud: Option<(tonic::transport::Channel, CloudFixtureAuth)>,
    pub test_cloud_transport: Option<tonic::transport::Channel>,
    client: Client,
    pub client_channel: tonic::transport::Channel,
    in_process_connection: InProcessConnection,
    background_tasks: Vec<JoinHandle<()>>,
    cloud_connector: Mutex<Option<CloudConnector>>,
    /// The embedder obtains relay credentials from the configured cloud through
    /// its own account API. Stopping the profile must also stop this link.
    relay_task: Mutex<Option<JoinHandle<()>>>,
    status: RuntimeStatus,
    #[cfg(unix)]
    unix_accept_task: Option<JoinHandle<()>>,
    #[cfg(unix)]
    socket_ownership: Option<SocketOwnership>,
}

/// Start local services and listeners for one profile. Cloud attachment is
/// intentionally a separate operation.
pub async fn start(options: ProfileRuntimeOptions) -> Result<ProfileRuntime, ProfileStartError> {
    let reporters = options
        .shared
        .status_reporters
        .resolve(&options.paths.state_path);
    let status = RuntimeStatus::new(reporters.update, reporters.subscription);
    start_observed(options, status).await
}

pub(crate) async fn start_observed(
    options: ProfileRuntimeOptions,
    status: RuntimeStatus,
) -> Result<ProfileRuntime, ProfileStartError> {
    start_supervised(options, status, Arc::default()).await
}

pub(crate) async fn start_supervised(
    options: ProfileRuntimeOptions,
    status: RuntimeStatus,
    operations: Arc<crate::installation::OperationGate>,
) -> Result<ProfileRuntime, ProfileStartError> {
    let result = async {
        let device_files = identity::ensure_device_files_with_trust_in(&options.paths.data_dir)
            .map_err(|error| ProfileStartError::State(error.to_string()))?;
        let security = DeviceRuntimeSecurity::new(
            device_files.identity,
            device_files.trust_store,
            options.paths.data_dir.clone(),
        );
        build(
            options,
            security.with_operations(operations),
            status.clone(),
        )
        .await
    }
    .await;
    if result.is_err() {
        status.report(Observed::StartupFailed);
    }
    result
}

pub(crate) async fn start_with_security(
    options: ProfileRuntimeOptions,
    security: DeviceRuntimeSecurity,
) -> Result<ProfileRuntime, ProfileStartError> {
    let reporters = options
        .shared
        .status_reporters
        .resolve(&options.paths.state_path);
    let status = RuntimeStatus::new(reporters.update, reporters.subscription);
    let result = build(options, security, status.clone()).await;
    if result.is_err() {
        status.report(Observed::StartupFailed);
    }
    result
}

async fn build(
    options: ProfileRuntimeOptions,
    security: DeviceRuntimeSecurity,
    status: RuntimeStatus,
) -> Result<ProfileRuntime, ProfileStartError> {
    let mut options = options;
    let reporters = options
        .shared
        .status_reporters
        .resolve(&options.paths.state_path);
    let service_config = options.service_config();
    service_config.validate()?;

    let mut bound = BoundListeners::bind(&options).await?;

    let host_id = security.host_id();
    let state = Arc::new(RwLock::new(ServerState::new(
        service_config.clone(),
        host_id,
        options.credentials.clone(),
        reporters.update.clone(),
    )));
    state.write().await.subscription_reporter = reporters.subscription.clone();

    let host_factory: Option<Arc<dyn LocalAgentHostFactory>> = options
        .fixtures
        .host_factory
        .clone()
        .or_else(|| options.host_factory.clone());
    let agent_host = host_factory
        .as_ref()
        .map(|factory| {
            factory.create(HostConfig {
                host_id,
                data_dir: options.paths.data_dir.clone(),
                state_path: options.paths.state_path.clone(),
                runtime_dir: options
                    .paths
                    .socket_path
                    .parent()
                    .unwrap_or(&options.paths.data_dir)
                    .to_owned(),
                server_socket_path: options.paths.socket_path.clone(),
                executable: std::env::current_exe()?,
                profile_config_path: options.paths.config_path.clone(),
                claude_user_keymap_dir: options.shared.keymaps_dir.clone(),
                repository_roots: options.shared.repository_roots.clone(),
            })
        })
        .transpose()?;
    let trust = security.shared_trust_store();
    let mut services = start_user_services(state.clone(), agent_host.clone(), security)
        .await
        .map_err(|error| ProfileStartError::State(error.to_string()))?;

    #[cfg(unix)]
    let unix_accept_task = bound.unix_listener.take().map(|listener| {
        let task = services.serve_client_service_on_unix_listener(listener);
        tracing::info!(path = %options.paths.socket_path.display(), "listening on profile ClientService");
        task
    });
    if let Some(listener) = bound.tcp_listener.take() {
        let addr = listener.local_addr()?;
        services.serve_external_tcp_listener(listener);
        tracing::info!(addr = %addr, "listening on profile direct dispatcher TCP");
    }

    if let Some(tracked) = &options.fixtures.tracked_tcp {
        if let Some(listener) = options.fixtures.listener.take() {
            services.serve_external_tcp_listener_tracked(listener, tracked.clone());
        }
        services
            .reachability_link_connector()
            .track_dialed_tcp(tracked.clone());
    }

    let mut background_tasks = Vec::new();
    if options.listeners == Listeners::Sockets {
        background_tasks.extend(services.spawn_reachability_links());
        if let Some(task) = crate::server::spawn_periodic_update_check(
            reporters.update.clone(),
            options.shared.update_manifest_url.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
            Duration::from_secs(3600),
        ) {
            background_tasks.push(task);
        }
    }

    if options.listeners != Listeners::Sockets && options.fixtures.tracked_tcp.is_some() {
        background_tasks.extend(services.spawn_reachability_links());
    }

    let (client_channel, client_task, in_process_connection) =
        services.open_managed_in_process_client_channel();
    services.push_task(client_task);
    let client = Client::from_channel(client_channel.clone());
    status.report(Observed::Local);

    Ok(ProfileRuntime {
        host_id,
        paths: options.paths,
        state,
        agent_host,
        services,
        trust,
        test_cloud: options.fixtures.cloud,
        test_cloud_transport: options.fixtures.cloud_transport,
        client,
        client_channel,
        in_process_connection,
        background_tasks,
        cloud_connector: Mutex::new(None),
        relay_task: Mutex::new(None),
        status,
        #[cfg(unix)]
        unix_accept_task,
        #[cfg(unix)]
        socket_ownership: bound.disarm_socket_cleanup(),
    })
}

impl ProfileRuntime {
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub fn report_status_for_test(&self, observed: Observed) {
        self.status.report(observed);
    }

    #[allow(dead_code)]
    pub(crate) fn status(&self) -> watch::Receiver<Observed> {
        self.status.subscribe()
    }

    /// Attach an embedder's relay route without changing the configured cloud.
    /// One profile holds one relay; attaching a second replaces the first.
    pub(crate) async fn attach_relay(&self, relay: crate::EmbeddedRelay) {
        let task = relay.spawn(self.services.link_connector_ctx());
        if let Some(previous) = self.relay_task.lock().await.replace(task) {
            previous.abort();
            let _ = previous.await;
        }
    }

    async fn stop_relay(&self) {
        if let Some(task) = self.relay_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
    }

    pub(crate) async fn configure_credentials(
        &self,
        cloud_url: String,
        credentials: Option<Arc<dyn CredentialProvider>>,
    ) {
        let mut state = self.state.write().await;
        state.config.cloud_url = cloud_url;

        state.credentials = credentials;
    }

    /// Called while the supervisor holds the same gate as agent and trust mutations.
    pub(crate) async fn non_pristine(
        &self,
    ) -> Result<Option<crate::installation::NonPristine>, std::io::Error> {
        use crate::installation::NonPristine;
        let trust = self.trust.read().unwrap().entries().count();
        if trust > 0 {
            return Ok(Some(NonPristine::TrustEntries(trust)));
        }
        if let Some(host) = &self.agent_host {
            let inventory = host
                .resource_inventory(self.paths.state_path.clone())
                .await
                .map_err(std::io::Error::other)?;
            if inventory.agents > 0 {
                return Ok(Some(NonPristine::LocalAgents(inventory.agents)));
            }
            if inventory.retained_artifacts > 0 {
                return Ok(Some(NonPristine::RetainedArtifacts(
                    inventory.retained_artifacts,
                )));
            }
        }
        Ok(None)
    }

    pub async fn start_cloud(&self) -> Result<(), CloudStartError> {
        if let Some((channel, auth)) = &self.test_cloud {
            let mut connector = self.cloud_connector.lock().await;
            if connector
                .as_ref()
                .is_some_and(|connector| !connector.is_finished())
            {
                return Ok(());
            }
            if let Some(finished) = connector.take() {
                finished.stop().await;
            }
            let ctx = self.services.link_connector_ctx();
            *connector = Some(match auth {
                CloudFixtureAuth::Bearer(token) => CloudConnector::testnet_bearer(
                    ctx,
                    channel.clone(),
                    token.clone(),
                    self.status.clone(),
                ),
                CloudFixtureAuth::Refreshing(auth) => CloudConnector::testnet_with_auth(
                    ctx,
                    channel.clone(),
                    auth.clone(),
                    self.status.clone(),
                ),
            });
            return Ok(());
        }
        if self.state.read().await.credentials.is_none() {
            self.status.report(Observed::AuthenticationRequired);
            return Err(CloudStartError::MissingCredentials);
        }

        let mut connector = self.cloud_connector.lock().await;
        if connector
            .as_ref()
            .is_some_and(|connector| !connector.is_finished())
        {
            return Ok(());
        }
        if let Some(finished) = connector.take() {
            finished.stop().await;
        }

        let config = self.state.read().await.config.clone();
        self.status.report(Observed::Connecting);
        *connector = Some(establish_cloud_connection(
            config,
            self.state.clone(),
            self.services.link_connector_ctx(),
            self.status.clone(),
            self.test_cloud_transport.clone(),
        ));
        Ok(())
    }

    pub async fn set_test_cloud_auth(&mut self, auth: CloudFixtureAuth) {
        self.stop_cloud().await;
        self.test_cloud.as_mut().expect("test cloud configured").1 = auth;
    }

    pub async fn stop_cloud(&self) {
        let mut connector = self.cloud_connector.lock().await;
        if let Some(connector) = connector.take() {
            connector.stop().await;
        }
        self.status.report(Observed::Local);
    }

    pub async fn stop(mut self, reason: ShutdownReason) {
        self.quiesce(reason).await;
        self.finish_stop().await;
    }

    pub(crate) async fn quiesce(&mut self, reason: ShutdownReason) {
        self.stop_accepting_local_clients().await;
        self.stop_cloud().await;
        self.stop_relay().await;

        if let Some(host) = &self.agent_host {
            host.notify_shutdown(reason).await;
        }
        self.services
            .tunnels
            .link_registry()
            .send_link_close_to_all(link_close_reason(reason))
            .await;
        if let Some(host) = &self.agent_host {
            host.stop_all().await;
        }
    }

    /// Close and unlink the listener before acknowledging shutdown, so callers
    /// cannot reconnect to a dying server. Accepted connections stay alive to
    /// deliver the reply, and a replacement listener keeps its socket path.
    pub(crate) async fn stop_accepting_local_clients(&mut self) {
        #[cfg(unix)]
        {
            if let Some(task) = self.unix_accept_task.take() {
                task.abort();
                let _ = task.await;
            }
            if let Some(ownership) = self.socket_ownership.take() {
                ownership.remove_if_owned();
            }
        }
    }

    pub(crate) async fn finish_stop(mut self) {
        self.stop_accepting_local_clients().await;
        tokio::time::sleep(LINK_CLOSE_FLUSH_TIMEOUT).await;
        self.client.disconnect();
        self.in_process_connection.close();
        tokio::task::yield_now().await;
        stop_tasks(std::mem::take(&mut self.background_tasks)).await;
        self.services.stop_tasks().await;
    }

    #[cfg(test)]
    pub(crate) fn weak_state(&self) -> std::sync::Weak<RwLock<ServerState>> {
        Arc::downgrade(&self.state)
    }
}

impl Drop for ProfileRuntime {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(task) = &self.unix_accept_task {
            task.abort();
        }
        if let Ok(task) = self.relay_task.try_lock()
            && let Some(task) = task.as_ref()
        {
            task.abort();
        }
    }
}

async fn stop_tasks(tasks: Vec<JoinHandle<()>>) {
    for task in &tasks {
        task.abort();
    }
    for task in tasks {
        let _ = task.await;
    }
}

fn link_close_reason(reason: ShutdownReason) -> wire::pb::LinkCloseReason {
    match reason {
        ShutdownReason::UpdateRequired => wire::pb::LinkCloseReason::UpdateRequired,
        ShutdownReason::ProtocolError => wire::pb::LinkCloseReason::ProtocolError,
        ShutdownReason::UserRequested => wire::pb::LinkCloseReason::UserShutdown,
        ShutdownReason::Updating => wire::pb::LinkCloseReason::Updating,
        ShutdownReason::Suspending => wire::pb::LinkCloseReason::Suspending,
        ShutdownReason::Restarting => wire::pb::LinkCloseReason::Restarting,
        ShutdownReason::AuthExpired => wire::pb::LinkCloseReason::AuthExpired,
    }
}

struct BoundListeners {
    tcp_listener: Option<TcpListener>,
    #[cfg(unix)]
    unix_listener: Option<tokio::net::UnixListener>,
    #[cfg(unix)]
    socket_ownership: Option<SocketOwnership>,
}

impl BoundListeners {
    async fn bind(options: &ProfileRuntimeOptions) -> std::io::Result<Self> {
        if options.listeners == Listeners::InProcessOnly {
            return Ok(Self {
                tcp_listener: None,
                #[cfg(unix)]
                unix_listener: None,
                #[cfg(unix)]
                socket_ownership: None,
            });
        }

        #[cfg(unix)]
        let unix_listener = crate::transport::bind_unix_listener(&options.paths.socket_path)?;
        #[cfg(unix)]
        let socket_ownership = Some(SocketOwnership::capture(options.paths.socket_path.clone())?);

        let mut bound = Self {
            tcp_listener: None,
            #[cfg(unix)]
            unix_listener: Some(unix_listener),
            #[cfg(unix)]
            socket_ownership,
        };
        if options.listeners == Listeners::Sockets
            && let Some(port) = options.config.tcp_port
        {
            bound.tcp_listener =
                Some(TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?);
        }
        Ok(bound)
    }

    #[cfg(unix)]
    fn disarm_socket_cleanup(&mut self) -> Option<SocketOwnership> {
        self.socket_ownership.take()
    }
}

#[cfg(unix)]
impl Drop for BoundListeners {
    fn drop(&mut self) {
        if let Some(ownership) = self.socket_ownership.take() {
            ownership.remove_if_owned();
        }
    }
}

#[cfg(unix)]
struct SocketOwnership {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl SocketOwnership {
    fn capture(path: PathBuf) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn remove_if_owned(self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
            && let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), error = %error, "failed to remove profile socket");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream};

    use model::ProtocolError;
    use tempfile::tempdir;

    use super::*;

    fn options(root: &std::path::Path, listeners: Listeners) -> ProfileRuntimeOptions {
        let data_dir = root.join("profile-data");
        ProfileRuntimeOptions {
            paths: ProfilePaths {
                config_path: None,
                socket_path: root.join("profile.sock"),
                state_path: root.join("profile-state.yaml"),
                reports_dir: data_dir.join("reports"),
                data_dir,
            },
            config: RuntimeConfig {
                cloud_url: "http://127.0.0.1:1".to_string(),
                tcp_port: None,
            },
            shared: Arc::new(InstallationSettings {
                repository_roots: Vec::new(),
                host_name: "profile-runtime-test".to_string(),
                prevent_idle_sleep: Some(false),
                keybinds: Keybinds::default(),
                ui: UiSettings::default(),
                keymaps_dir: root.join("installation-keymaps"),
                minimum_client_versions: HashMap::new(),
                update_manifest_url: "http://127.0.0.1:1/manifest.json".into(),
                status_reporters: Default::default(),
            }),
            credentials: None,
            host_factory: None,

            listeners,
            #[cfg(test)]
            fixtures: RuntimeFixtures::default(),
        }
    }

    #[tokio::test]
    async fn profile_runtime_outlives_clients() {
        let root = tempdir().unwrap();
        let runtime = start(options(root.path(), Listeners::InProcessOnly))
            .await
            .unwrap();
        let client = runtime.client();
        client.list_agents().await.unwrap();
        let other = client.clone();
        drop(client);
        drop(other);
        tokio::task::yield_now().await;

        runtime.client().list_agents().await.unwrap();
        let weak_state = runtime.weak_state();
        assert!(weak_state.upgrade().is_some());
        runtime.stop(ShutdownReason::UserRequested).await;
        assert!(weak_state.upgrade().is_none());
    }

    struct StaticCredentials;

    #[async_trait::async_trait]
    impl CredentialProvider for StaticCredentials {
        async fn access_token(&self) -> Result<crate::auth::AccessToken, crate::auth::AuthError> {
            Ok(crate::auth::AccessToken {
                bearer: "test-token".into(),
                expires_at: None,
            })
        }
        fn invalidate(&self, _token: &crate::auth::AccessToken) {}
    }

    async fn wait_for_status(runtime: &ProfileRuntime, expected: Observed) {
        let mut status = runtime.status();
        tokio::time::timeout(Duration::from_secs(3), async {
            status.wait_for(|value| *value == expected).await.unwrap();
        })
        .await
        .unwrap_or_else(|_| panic!("expected {expected:?}, got {:?}", *status.borrow()));
    }

    #[tokio::test]
    async fn profile_runtime_reports_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Real /api/connect responses drive the production preparation and
        // retry loop. Subscribe only after startup to prove retained state.
        for (response, expected) in [
            ("401 Unauthorized", Observed::AuthenticationRequired),
            ("403 Forbidden", Observed::SubscriptionRequired),
            ("503 Service Unavailable", Observed::Retrying),
        ] {
            let root = tempdir().unwrap();
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let mut options = options(root.path(), Listeners::InProcessOnly);
            options.config.cloud_url = format!("http://{}", listener.local_addr().unwrap());
            options.credentials = Some(Arc::new(StaticCredentials));

            let server = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 1024];
                    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        let read = stream.read(&mut buffer).await.unwrap();
                        assert!(read > 0, "cloud request closed before its headers arrived");
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let body = r#"{"error":"payment_required"}"#;
                    stream.write_all(format!("HTTP/1.1 {response}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                }
            });
            let runtime = start(options).await.unwrap();
            assert_eq!(*runtime.status().borrow(), Observed::Local);
            runtime.start_cloud().await.unwrap();
            assert_eq!(*runtime.status().borrow(), Observed::Connecting);
            wait_for_status(&runtime, expected.clone()).await;
            runtime.client().list_agents().await.unwrap();
            println!("cloud {response}: {expected:?}; local calls remain available");
            runtime.stop_cloud().await;
            assert_eq!(*runtime.status().borrow(), Observed::Local);
            runtime.stop(ShutdownReason::UserRequested).await;
            server.abort();
            let _ = server.await;
        }
    }

    #[cfg(test)]
    #[tokio::test]
    async fn profile_runtime_reports_status_from_relay_and_outlives_cloud_clients() {
        use crate::routing::{AuthenticatedLinkUser, LinkTokenAuthenticator};
        use crate::services::CloudLinkService;

        struct RelayAuth {
            user: uuid::Uuid,
            rejection: std::sync::Mutex<Option<tonic::Status>>,
        }
        #[tonic::async_trait]
        impl LinkTokenAuthenticator for RelayAuth {
            async fn authenticate_token(
                &self,
                _: &str,
            ) -> Result<AuthenticatedLinkUser, tonic::Status> {
                if let Some(error) = self.rejection.lock().unwrap().clone() {
                    return Err(error);
                }
                Ok(AuthenticatedLinkUser {
                    user_id: self.user,
                    client_id: "runtime-test".into(),
                    expires_at: std::time::SystemTime::now() + Duration::from_secs(3600),
                })
            }
        }

        let auth = Arc::new(RelayAuth {
            user: uuid::Uuid::new_v4(),
            rejection: std::sync::Mutex::new(None),
        });

        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            uuid::Uuid::new_v4(),
            None,
            None,
        )));
        state.write().await.is_cloud_server = true;
        let relay = CloudLinkService::with_authenticator(state, auth.clone());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let channel = tonic::transport::Endpoint::from_shared(format!(
            "http://{}",
            listener.local_addr().unwrap()
        ))
        .unwrap()
        .connect_lazy();
        let server = relay.serve_on_tcp_listener(listener);
        let root = tempdir().unwrap();
        let mut options = options(root.path(), Listeners::InProcessOnly);
        options.fixtures.cloud = Some((channel, CloudFixtureAuth::Bearer("runtime-token".into())));

        let runtime = start(options).await.unwrap();
        let host_id = runtime.state.read().await.host_id();
        let wait_for_relay_detach = || async {
            tokio::time::timeout(Duration::from_secs(3), async {
                while relay.user_has_link_to(auth.user, host_id).await {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("relay did not observe connector teardown");
        };
        runtime.start_cloud().await.unwrap();
        assert_eq!(*runtime.status().borrow(), Observed::Connecting);
        wait_for_status(&runtime, Observed::Connected).await;
        let client = runtime.client();
        client.list_agents().await.unwrap();
        drop(client);
        tokio::task::yield_now().await;
        assert!(relay.user_has_link_to(auth.user, host_id).await);
        runtime.client().list_agents().await.unwrap();
        assert_eq!(*runtime.status().borrow(), Observed::Connected);
        println!("last cloud client dropped: Connected; relay link and local calls survive");
        runtime.stop_cloud().await;
        wait_for_relay_detach().await;

        for (error, expected) in [
            (
                tonic::Status::unauthenticated("expired"),
                Observed::AuthenticationRequired,
            ),
            (
                wire::protocol_status(ProtocolError::PaymentRequired),
                Observed::SubscriptionRequired,
            ),
            (
                wire::protocol_status(ProtocolError::UpdateRequired {
                    minimum_version: "99.0.0".into(),
                    client_version: "0.6.0".into(),
                }),
                Observed::UpdateRequired {
                    minimum_version: Some("99.0.0".into()),
                },
            ),
            (
                tonic::Status::failed_precondition("amux update required"),
                Observed::UpdateRequired {
                    minimum_version: None,
                },
            ),
            (tonic::Status::unavailable("try again"), Observed::Retrying),
        ] {
            *auth.rejection.lock().unwrap() = Some(error);
            runtime.start_cloud().await.unwrap();
            wait_for_status(&runtime, expected.clone()).await;
            runtime.client().list_agents().await.unwrap();
            println!("relay: {expected:?}; local calls remain available");
            runtime.stop_cloud().await;
            assert_eq!(*runtime.status().borrow(), Observed::Local);
            wait_for_relay_detach().await;
        }
        *auth.rejection.lock().unwrap() = None;
        runtime.start_cloud().await.unwrap();
        wait_for_status(&runtime, Observed::Connected).await;
        let weak_state = runtime.weak_state();
        runtime.stop(ShutdownReason::UserRequested).await;
        assert!(weak_state.upgrade().is_none());
        wait_for_relay_detach().await;
        println!("runtime stopped: service tasks released and relay link removed");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn config_split_daemon_without_credentials_serves_locally() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let root = tempdir().unwrap();
            let runtime_options = options(root.path(), Listeners::Sockets);
            let config = runtime_options.service_config();
            let runtime = start(runtime_options).await.unwrap();
            let channel = client::connect_socket(&config.socket_path).await.unwrap();
            let client = Client::from_channel(channel);
            let dump = client
                .debug_dump(crate::debug::DebugFormat::Json)
                .await
                .unwrap();
            let debug: serde_json::Value = serde_json::from_str(&dump).unwrap();
            assert_eq!(debug["has_credential_provider"], false);
            assert!(debug["config"].get("enable_cloud_mode").is_none());

            crate::installation::ProfileAdmin::for_test(runtime.services.client.clone())
                .start_qr_pairing()
                .await
                .unwrap();
            println!(
                "Credential-free profile serves local calls and can prepare a pairing QR:\n{dump}"
            );
            runtime.stop(ShutdownReason::UserRequested).await;
        })
        .await
        .expect("local daemon test timed out");
    }

    #[tokio::test]
    async fn profile_runtime_owned_stop_releases_socket() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let root = tempdir().unwrap();
            let opts = options(root.path(), Listeners::Sockets);
            let config = opts.service_config();
            let runtime = start(opts).await.unwrap();
            let channel = client::connect_socket(&config.socket_path).await.unwrap();
            let client = Client::from_channel(channel);
            client.list_agents().await.unwrap();
            runtime.stop(ShutdownReason::UserRequested).await;
            assert!(client.list_agents().await.is_err());
            assert!(UnixStream::connect(&config.socket_path).is_err());
            assert!(!config.socket_path.exists());
            let replacement = crate::transport::bind_unix_listener(&config.socket_path).unwrap();
            UnixStream::connect(&config.socket_path).unwrap();
            println!("Owned stop completed: client closed and a fresh socket bind succeeds");
            drop(replacement);
        })
        .await
        .expect("owned stop timed out");
    }

    #[tokio::test]
    async fn profile_runtime_drop_closes_the_unix_listener() {
        let root = tempdir().unwrap();
        let socket_path = root.path().join("profile.sock");
        let runtime = start(options(root.path(), Listeners::Sockets))
            .await
            .unwrap();
        let accept_task = runtime.unix_accept_task.as_ref().unwrap().abort_handle();
        UnixStream::connect(&socket_path).unwrap();

        drop(runtime);

        tokio::time::timeout(Duration::from_secs(1), async {
            while !accept_task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(UnixStream::connect(&socket_path).is_err());
    }

    #[tokio::test]
    async fn profile_runtime_stop_removes_the_socket_it_owns() {
        let root = tempdir().unwrap();
        let socket_path = root.path().join("profile.sock");
        let runtime = start(options(root.path(), Listeners::Sockets))
            .await
            .unwrap();

        UnixStream::connect(&socket_path).unwrap();
        runtime.stop(ShutdownReason::UserRequested).await;

        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn profile_runtime_stop_refuses_to_unlink_a_replacement_listener() {
        let root = tempdir().unwrap();
        let socket_path = root.path().join("profile.sock");
        let runtime = start(options(root.path(), Listeners::Sockets))
            .await
            .unwrap();
        std::fs::remove_file(&socket_path).unwrap();
        let replacement = StdUnixListener::bind(&socket_path).unwrap();

        runtime.stop(ShutdownReason::UserRequested).await;

        UnixStream::connect(&socket_path).unwrap();
        drop(replacement);
    }

    #[tokio::test]
    async fn profile_runtime_start_failure_removes_its_bound_socket() {
        let root = tempdir().unwrap();
        let socket_path = root.path().join("profile.sock");
        let occupied = TcpListener::bind(("0.0.0.0", 0)).await.unwrap();
        let mut options = options(root.path(), Listeners::Sockets);
        options.config.tcp_port = Some(occupied.local_addr().unwrap().port());

        let status = RuntimeStatus::new(None, None);
        let result = start_observed(options, status.clone()).await;

        assert!(result.is_err());
        assert_eq!(*status.subscribe().borrow(), Observed::StartupFailed);
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn profile_runtime_stop_leaves_no_spawned_service_task() {
        let root = tempdir().unwrap();
        let runtime = start(options(root.path(), Listeners::InProcessOnly))
            .await
            .unwrap();
        let weak_state = runtime.weak_state();
        let client = runtime.client();

        runtime.stop(ShutdownReason::UserRequested).await;

        assert!(client.list_agents().await.is_err());
        drop(client);
        tokio::task::yield_now().await;
        assert!(weak_state.upgrade().is_none());
    }

    #[test]
    fn profile_runtime_paths_are_profile_scoped_and_keymaps_are_installation_scoped() {
        let root = tempdir().unwrap();
        let options = options(root.path(), Listeners::Sockets);
        let config = options.service_config();

        assert_eq!(config.socket_path, options.paths.socket_path);
        assert_eq!(config.state_path, options.paths.state_path);
        assert_eq!(config.data_dir, options.paths.data_dir);
        assert_eq!(config.reports_dir(), options.paths.reports_dir);
        assert_ne!(
            options.shared.keymaps_dir,
            crate::keymap_dir(&config.data_dir)
        );
    }
}
