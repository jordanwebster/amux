//! Runtime ownership for one complete amux device profile.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, mpsc, watch};
use tokio::task::JoinHandle;

use super::status::{Observed, RuntimeStatus};
use crate::auth::CredentialProvider;
use crate::client::Client;
use crate::config::{Config, ConfigError, Keybinds, UiSettings};
use crate::identity;
use crate::protocol::{ProtocolError, wire};
use crate::server::ShutdownReason;
use crate::services::{
    CloudConnector, DeviceRuntimeSecurity, LocalAgentHost, StartedUserServices,
    establish_cloud_connection, start_user_services,
};
use crate::subscription::SubscriptionReporter;
use crate::transport::InProcessConnection;
use crate::update::UpdateReporter;
use crate::user_state::{ServerState, ShutdownRequest, new_local_agent_host};

const LINK_CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(200);

use crate::installation::ProfilePaths;

/// Settings that vary between profiles.
#[derive(Clone, Debug)]
pub(crate) struct ProfileConfig {
    pub(crate) cloud_url: String,
    pub(crate) tcp_port: Option<u16>,
}

/// Installation-owned settings shared by every profile runtime.
#[derive(Clone)]
pub struct InstallationSettings {
    pub host_name: String,
    pub prevent_idle_sleep: Option<bool>,
    pub keybinds: Keybinds,
    pub ui: UiSettings,
    pub keymaps_dir: PathBuf,
    pub minimum_client_versions: HashMap<String, String>,
    pub update_reporter: Option<Arc<dyn UpdateReporter>>,
    pub subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
}

/// Which externally reachable listeners a runtime owns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listeners {
    InProcessOnly,
    Sockets,
}

#[cfg(testnet)]
#[derive(Clone)]
pub(crate) enum CloudFixtureAuth {
    Bearer(String),
    Refreshing(crate::routing::LinkConnectorAuth),
}

#[cfg(testnet)]
#[derive(Default)]
pub(crate) struct RuntimeFixtures {
    pub(crate) listener: Option<TcpListener>,
    pub(crate) tracked_tcp: Option<crate::dispatcher::TrackedTcpConnections>,
    pub(crate) artifact_clock: Option<Arc<dyn amux_artifacts::Clock>>,
    pub(crate) cloud: Option<(tonic::transport::Channel, CloudFixtureAuth)>,
}

pub(crate) struct ProfileRuntimeOptions {
    pub(crate) paths: ProfilePaths,
    pub(crate) config: ProfileConfig,
    pub(crate) shared: Arc<InstallationSettings>,
    pub(crate) credentials: Option<Arc<dyn CredentialProvider>>,
    pub(crate) enable_cloud_mode: Option<bool>,
    pub(crate) listeners: Listeners,
    #[cfg(testnet)]
    pub(crate) fixtures: RuntimeFixtures,
}

impl ProfileRuntimeOptions {
    pub(crate) fn from_legacy_config(
        config: Config,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
        subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
        listeners: Listeners,
    ) -> Self {
        let paths = ProfilePaths {
            config_path: config.path.clone(),
            socket_path: config.socket_path.clone(),
            state_path: config.state_path.clone(),
            data_dir: config.data_dir.clone(),
            reports_dir: config.reports_dir(),
        };
        let profile = ProfileConfig {
            cloud_url: config.cloud_url.clone(),
            tcp_port: config.tcp_port,
        };
        let shared = InstallationSettings {
            host_name: config.host_name,
            prevent_idle_sleep: config.prevent_idle_sleep,
            keybinds: config.keybinds,
            ui: config.ui,
            keymaps_dir: crate::keymap_dir(&config.data_dir),
            minimum_client_versions: config.minimum_client_versions,
            update_reporter,
            subscription_reporter,
        };
        Self {
            paths,
            config: profile,
            shared: Arc::new(shared),
            credentials,
            enable_cloud_mode: config.enable_cloud_mode,
            listeners,
            #[cfg(testnet)]
            fixtures: RuntimeFixtures::default(),
        }
    }

    pub(crate) fn service_config(&self) -> Config {
        Config {
            host_name: self.shared.host_name.clone(),
            cloud_url: self.config.cloud_url.clone(),
            socket_path: self.paths.socket_path.clone(),
            tcp_port: self.config.tcp_port,
            state_path: self.paths.state_path.clone(),
            data_dir: self.paths.data_dir.clone(),
            reports_dir: Some(self.paths.reports_dir.clone()),
            enable_cloud_mode: self.enable_cloud_mode,
            prevent_idle_sleep: self.shared.prevent_idle_sleep,
            minimum_client_versions: self.shared.minimum_client_versions.clone(),
            keybinds: self.shared.keybinds.clone(),
            ui: self.shared.ui.clone(),
            path: self.paths.config_path.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ProfileStartError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("profile state error: {0}")]
    State(String),
}

#[derive(Debug, Error)]
pub(crate) enum CloudStartError {
    #[error("profile has no cloud credentials")]
    MissingCredentials,
}

/// All state and tasks owned by one complete device profile.
pub(crate) struct ProfileRuntime {
    pub(crate) host_id: crate::HostId,
    paths: ProfilePaths,
    state: Arc<RwLock<ServerState>>,
    shutdown_rx: Option<mpsc::Receiver<ShutdownRequest>>,
    agent_host: Option<Arc<dyn LocalAgentHost>>,
    pub(crate) services: StartedUserServices,
    #[cfg(testnet)]
    pub(crate) test_agent_host: Arc<crate::services::PtyAgentHost>,
    pub(crate) trust: crate::trust::SharedTrustStore,
    #[cfg(testnet)]
    test_cloud: Option<(tonic::transport::Channel, CloudFixtureAuth)>,
    client: Client,
    #[cfg(testnet)]
    pub(crate) client_channel: tonic::transport::Channel,
    in_process_connection: InProcessConnection,
    background_tasks: Vec<JoinHandle<()>>,
    cloud_connector: Mutex<Option<CloudConnector>>,
    status: RuntimeStatus,
    prepared_suspend: bool,
    #[cfg(unix)]
    unix_accept_task: Option<JoinHandle<()>>,
    #[cfg(unix)]
    socket_ownership: Option<SocketOwnership>,
}

/// Start local services and listeners for one profile. Cloud attachment is
/// intentionally a separate operation.
pub(crate) async fn start(
    options: ProfileRuntimeOptions,
) -> Result<ProfileRuntime, ProfileStartError> {
    let status = RuntimeStatus::new(
        options.shared.update_reporter.clone(),
        options.shared.subscription_reporter.clone(),
    );
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
    let status = RuntimeStatus::new(
        options.shared.update_reporter.clone(),
        options.shared.subscription_reporter.clone(),
    );
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
    #[cfg(testnet)]
    let mut options = options;
    let service_config = options.service_config();
    service_config.validate(false)?;

    let mut bound = BoundListeners::bind(&options).await?;
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
    let host_id = security.host_id();
    let state = Arc::new(RwLock::new(ServerState::new(
        service_config.clone(),
        host_id,
        shutdown_tx,
        options.credentials.clone(),
        options.shared.update_reporter.clone(),
    )));
    state.write().await.subscription_reporter = options.shared.subscription_reporter.clone();

    let agent_host = new_local_agent_host(
        host_id,
        &service_config,
        options.shared.keymaps_dir.clone(),
        options.paths.data_dir.clone(),
    )?;
    #[cfg(testnet)]
    let test_agent_host = agent_host.clone().expect("testnet requires local agents");
    let agent_host = agent_host.map(|host| host as Arc<dyn LocalAgentHost>);
    let trust = security.shared_trust_store();
    #[cfg(not(testnet))]
    let mut services = start_user_services(state.clone(), agent_host.clone(), security)
        .await
        .map_err(|error| ProfileStartError::State(error.to_string()))?;

    #[cfg(testnet)]
    let mut services = match options.fixtures.artifact_clock.take() {
        Some(clock) => {
            crate::services::start_user_services_with_artifact_clock(
                state.clone(),
                agent_host.clone(),
                security,
                clock,
            )
            .await
        }
        None => start_user_services(state.clone(), agent_host.clone(), security).await,
    }
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

    #[cfg(testnet)]
    if let Some(tracked) = &options.fixtures.tracked_tcp {
        if let Some(listener) = options.fixtures.listener.take() {
            services.serve_external_tcp_listener_tracked(listener, tracked.clone());
        }
        services
            .reachability_link_connector()
            .track_dialed_tcp(tracked.clone());
    }

    let mut background_tasks = vec![crate::agents::spawn_artifact_sweeper(
        services.artifact_owners.clone(),
    )];
    if options.listeners == Listeners::Sockets {
        background_tasks.extend(services.spawn_reachability_links());
        if let Some(task) = crate::server::spawn_periodic_update_check(
            options.shared.update_reporter.clone(),
            options.config.cloud_url.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
            Duration::from_secs(3600),
        ) {
            background_tasks.push(task);
        }
    }

    #[cfg(testnet)]
    if options.listeners == Listeners::InProcessOnly && options.fixtures.tracked_tcp.is_some() {
        background_tasks.extend(services.spawn_reachability_links());
    }

    let (client_channel, client_task, in_process_connection) =
        services.open_managed_in_process_client_channel();
    services.push_task(client_task);
    let client = Client::from_client_service_channel(client_channel.clone(), None);
    status.report(Observed::Local);

    Ok(ProfileRuntime {
        host_id,
        paths: options.paths,
        state,
        shutdown_rx: Some(shutdown_rx),
        agent_host,
        services,
        #[cfg(testnet)]
        test_agent_host,
        trust,
        #[cfg(testnet)]
        test_cloud: options.fixtures.cloud,
        client,
        #[cfg(testnet)]
        client_channel,
        in_process_connection,
        background_tasks,
        cloud_connector: Mutex::new(None),
        status,
        prepared_suspend: false,
        #[cfg(unix)]
        unix_accept_task,
        #[cfg(unix)]
        socket_ownership: bound.disarm_socket_cleanup(),
    })
}

impl ProfileRuntime {
    pub(crate) fn client(&self) -> Client {
        self.client.clone()
    }

    #[allow(dead_code)]
    pub(crate) fn status(&self) -> watch::Receiver<Observed> {
        self.status.subscribe()
    }

    pub(crate) async fn configure_credentials(
        &self,
        cloud_url: String,
        credentials: Option<Arc<dyn CredentialProvider>>,
    ) {
        let mut state = self.state.write().await;
        state.config.cloud_url = cloud_url;
        state.config.enable_cloud_mode = Some(true);
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
            let count = host.agent_count().await;
            if count > 0 {
                return Ok(Some(NonPristine::LocalAgents(count)));
            }
        }
        let suspended = crate::suspend::load_suspended(&self.paths.state_path)
            .map_err(std::io::Error::other)?;
        if !suspended.agents.is_empty() {
            return Ok(Some(NonPristine::LocalAgents(suspended.agents.len())));
        }
        for (path, agents) in [
            (self.paths.data_dir.join("agents"), true),
            (self.paths.data_dir.join("cache/artifacts"), false),
        ] {
            let count = std::fs::read_dir(path)?
                .collect::<Result<Vec<_>, _>>()?
                .len();
            if count > 0 {
                return Ok(Some(if agents {
                    NonPristine::LocalAgents(count)
                } else {
                    NonPristine::RetainedArtifacts(count)
                }));
            }
        }
        Ok(None)
    }

    pub(crate) async fn start_cloud(&self) -> Result<(), CloudStartError> {
        #[cfg(testnet)]
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
        ));
        Ok(())
    }

    #[cfg(testnet)]
    pub(crate) async fn set_test_cloud_auth(&mut self, auth: CloudFixtureAuth) {
        self.stop_cloud().await;
        self.test_cloud.as_mut().expect("test cloud configured").1 = auth;
    }

    pub(crate) async fn stop_cloud(&self) {
        let mut connector = self.cloud_connector.lock().await;
        if let Some(connector) = connector.take() {
            connector.stop().await;
        }
        self.status.report(Observed::Local);
    }

    pub(crate) async fn stop(mut self, reason: ShutdownReason) {
        self.quiesce(reason).await;
        self.finish_stop().await;
    }

    pub(crate) async fn quiesce(&mut self, reason: ShutdownReason) {
        self.stop_accepting_local_clients().await;
        self.stop_cloud().await;

        if let Some(host) = &self.agent_host {
            host.notify_shutdown(reason).await;
        }
        self.services
            .tunnels
            .link_registry()
            .send_link_close_to_all(link_close_reason(reason))
            .await;
        if let Some(host) = &self.agent_host {
            if self.prepared_suspend {
                host.commit_suspend().await;
            } else {
                host.stop_all().await;
            }
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
        self.client.close();
        self.in_process_connection.close();
        tokio::task::yield_now().await;
        stop_tasks(std::mem::take(&mut self.background_tasks)).await;
        self.services.stop_tasks().await;
    }

    pub(crate) fn take_shutdown_receiver(&mut self) -> mpsc::Receiver<ShutdownRequest> {
        self.shutdown_rx
            .take()
            .expect("profile runtime shutdown receiver taken twice")
    }

    pub(crate) async fn prepare_suspend(&mut self) -> Result<u64, ProtocolError> {
        let Some(host) = &self.agent_host else {
            self.prepared_suspend = true;
            return Ok(0);
        };
        let count = host.prepare_suspend(self.paths.state_path.clone()).await?;
        self.prepared_suspend = true;
        Ok(count)
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
        if let Some(port) = options.config.tcp_port {
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
            config: ProfileConfig {
                cloud_url: "http://127.0.0.1:1".to_string(),
                tcp_port: None,
            },
            shared: Arc::new(InstallationSettings {
                host_name: "profile-runtime-test".to_string(),
                prevent_idle_sleep: Some(false),
                keybinds: Keybinds::default(),
                ui: UiSettings::default(),
                keymaps_dir: root.join("installation-keymaps"),
                minimum_client_versions: HashMap::new(),
                update_reporter: None,
                subscription_reporter: None,
            }),
            credentials: None,
            enable_cloud_mode: Some(false),
            listeners,
            #[cfg(testnet)]
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

    #[tokio::test]
    async fn profile_runtime_embedded_guard_stops_services_on_last_drop() {
        let root = tempdir().unwrap();
        let runtime = start(options(root.path(), Listeners::InProcessOnly))
            .await
            .unwrap();
        let weak_state = runtime.weak_state();
        // An independent transport client observes shutdown without keeping the
        // embedding guard alive.
        let observer = runtime.client();
        let client = crate::server::spawn_embedded_runtime(runtime);
        let other = client.clone();
        drop(client);
        other.list_agents().await.unwrap();
        drop(other);

        tokio::time::timeout(Duration::from_secs(3), async {
            while weak_state.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("embedded guard did not finish runtime teardown");
        assert!(observer.list_agents().await.is_err());
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
            options.enable_cloud_mode = Some(true);
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

    #[cfg(testnet)]
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
        let (shutdown_tx, _shutdown_rx) = mpsc::channel(1);
        let state = Arc::new(RwLock::new(ServerState::new(
            Config::default(),
            uuid::Uuid::new_v4(),
            shutdown_tx,
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
        options.enable_cloud_mode = Some(true);
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
                crate::protocol::protocol_status(ProtocolError::PaymentRequired),
                Observed::SubscriptionRequired,
            ),
            (
                crate::protocol::protocol_status(ProtocolError::UpdateRequired {
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
        // The same embedding owner must also tear down an established cloud
        // link when its final guarded client is dropped.
        let weak_state = runtime.weak_state();
        let client = crate::server::spawn_embedded_runtime(runtime);
        drop(client);
        tokio::time::timeout(Duration::from_secs(3), async {
            while weak_state.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        wait_for_relay_detach().await;
        println!("embedded guard dropped: service tasks released and relay link removed");
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn profile_runtime_daemon_preserves_disabled_cloud_mode_with_credentials() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let root = tempdir().unwrap();
            let config = options(root.path(), Listeners::Sockets).service_config();
            let server_config = config.clone();
            let server = tokio::spawn(async move {
                crate::server::Server::builder()
                    .config(server_config)
                    .credentials(Arc::new(StaticCredentials))
                    .run()
                    .await
                    .unwrap();
            });
            let channel = loop {
                match crate::client::connect_existing_client_service(&config).await {
                    Ok(channel) => break channel,
                    Err(_) => {
                        assert!(
                            !server.is_finished(),
                            "daemon exited before serving clients"
                        );
                        tokio::task::yield_now().await;
                    }
                }
            };
            let client = Client::from_client_service_channel(channel, None);
            let dump = client
                .debug_dump(crate::debug::DebugFormat::Json)
                .await
                .unwrap();
            let debug: serde_json::Value = serde_json::from_str(&dump).unwrap();
            assert_eq!(debug["use_cloud_mode"], false);
            assert_eq!(debug["config"]["enable_cloud_mode"], false);
            let error = client.start_qr_pairing().await.unwrap_err();
            assert!(error.to_string().contains("QR pairing requires cloud mode"));
            println!(
                "Daemon with credentials and cloud mode disabled:\n{dump}\nQR pairing: {error}"
            );
            client.shutdown().await.unwrap();
            server.await.unwrap();
        })
        .await
        .expect("daemon cloud-mode test timed out");
    }

    #[test]
    fn profile_runtime_service_config_preserves_cloud_mode_with_credentials() {
        let root = tempdir().unwrap();
        for enable_cloud_mode in [None, Some(false), Some(true)] {
            let mut config = options(root.path(), Listeners::InProcessOnly).service_config();
            config.enable_cloud_mode = enable_cloud_mode;
            let options = ProfileRuntimeOptions::from_legacy_config(
                config,
                Some(Arc::new(StaticCredentials)),
                None,
                None,
                Listeners::InProcessOnly,
            );
            assert_eq!(options.enable_cloud_mode, enable_cloud_mode);
            assert_eq!(
                options.service_config().enable_cloud_mode,
                enable_cloud_mode
            );
        }
    }

    #[tokio::test]
    async fn profile_runtime_daemon_shutdown_reply_releases_socket() {
        assert_daemon_reply_releases_socket(false).await;
    }

    #[tokio::test]
    async fn profile_runtime_daemon_suspend_reply_releases_socket() {
        assert_daemon_reply_releases_socket(true).await;
    }

    async fn assert_daemon_reply_releases_socket(suspend: bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let root = tempdir().unwrap();
            let config = options(root.path(), Listeners::Sockets).service_config();
            let socket_path = config.socket_path.clone();
            let server_config = config.clone();
            let server = tokio::spawn(async move {
                crate::server::Server::builder()
                    .config(server_config)
                    .run()
                    .await
                    .unwrap();
            });
            let channel = loop {
                match crate::client::connect_existing_client_service(&config).await {
                    Ok(channel) => break channel,
                    Err(_) => {
                        assert!(
                            !server.is_finished(),
                            "daemon exited before serving clients"
                        );
                        tokio::task::yield_now().await;
                    }
                }
            };
            let client = Client::from_client_service_channel(channel, None);
            if suspend {
                client.suspend().await.unwrap();
            } else {
                client.shutdown().await.unwrap();
            }

            assert!(UnixStream::connect(&socket_path).is_err());
            assert!(!socket_path.exists());
            let replacement = crate::transport::bind_unix_listener(&socket_path).unwrap();
            println!(
                "{} reply received: reconnect fails and a fresh socket bind succeeds",
                if suspend { "Suspend" } else { "Shutdown" }
            );
            server.await.unwrap();
            UnixStream::connect(&socket_path).unwrap();
            drop(replacement);
        })
        .await
        .expect("daemon shutdown timed out");
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
