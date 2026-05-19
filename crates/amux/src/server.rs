//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts tonic services for local clients, host-to-host tunnels, and optional
//! cloud routing. Shared daemon state is kept in `Arc<RwLock<ServerState>>`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
#[cfg(unix)]
use crate::client::connect_existing_client_service;
use crate::client::{Client, ConnectError};
use crate::config::{Config, ConfigError};
use crate::identity;
use crate::protocol::{ProtocolError, wire};
use crate::routing::generate_server_link;
use crate::services::{
    CloudRoutingService, DeviceRuntimeSecurity, SharedAgentServiceState, StartedUserServices,
    commit_server_suspend, establish_cloud_connection, prepare_server_suspend, shutdown_server,
    start_user_services,
};
use crate::transport::{TransportError, create_tls_acceptor};
use crate::trust::TrustStore;
use crate::tunnel::TunnelPool;
use crate::update::{UpdateReporter, UpdateStatus};
use crate::user_state::{ServerState, ShutdownRequest, get_local_agent_service_state};

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_GOAWAY_DRAIN_TIMEOUT_MS: u32 = 200;

type Result<T> = std::result::Result<T, ServerError>;
type BuilderParts = (
    Config,
    Option<Arc<dyn CredentialProvider>>,
    bool,
    Option<Arc<dyn UpdateReporter>>,
);

enum PendingShutdownReply {
    Shutdown {
        reply: oneshot::Sender<std::result::Result<(), ProtocolError>>,
    },
    Suspend {
        reply: oneshot::Sender<std::result::Result<u64, ProtocolError>>,
        suspended_count: u64,
    },
}

impl PendingShutdownReply {
    fn send_success(self) {
        match self {
            Self::Shutdown { reply } => {
                let _ = reply.send(Ok(()));
            }
            Self::Suspend {
                reply,
                suspended_count,
            } => {
                let _ = reply.send(Ok(suspended_count));
            }
        }
    }
}

/// Reason for server shutdown notification.
pub(crate) const SHUTDOWN_REASON_METADATA_KEY: &str = "amux-shutdown-reason";

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    UpdateRequired,
    ProtocolError,
    UserRequested,
    Updating,
    Suspending,
    Restarting,
    AuthExpired,
}

impl ShutdownReason {
    pub(crate) fn as_wire_value(&self) -> &'static str {
        match self {
            ShutdownReason::UpdateRequired => "update_required",
            ShutdownReason::ProtocolError => "protocol_error",
            ShutdownReason::UserRequested => "user_requested",
            ShutdownReason::Updating => "updating",
            ShutdownReason::Suspending => "suspending",
            ShutdownReason::Restarting => "restarting",
            ShutdownReason::AuthExpired => "auth_expired",
        }
    }

    pub(crate) fn from_wire_value(value: &str) -> Option<Self> {
        match value {
            "update_required" => Some(ShutdownReason::UpdateRequired),
            "protocol_error" => Some(ShutdownReason::ProtocolError),
            "user_requested" => Some(ShutdownReason::UserRequested),
            "updating" => Some(ShutdownReason::Updating),
            "suspending" => Some(ShutdownReason::Suspending),
            "restarting" => Some(ShutdownReason::Restarting),
            "auth_expired" => Some(ShutdownReason::AuthExpired),
            _ => None,
        }
    }
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownReason::UpdateRequired => write!(f, "amux update required"),
            ShutdownReason::ProtocolError => write!(f, "protocol error"),
            ShutdownReason::UserRequested => write!(f, "server shutting down"),
            ShutdownReason::Updating => write!(f, "server updating"),
            ShutdownReason::Suspending => write!(f, "server suspending"),
            ShutdownReason::Restarting => write!(f, "server restarting"),
            ShutdownReason::AuthExpired => write!(f, "authentication expired"),
        }
    }
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("accept error: {0}")]
    Accept(String),
    #[error("connection error: {0}")]
    Connection(String),
    #[error("{0}")]
    State(String),
}

pub struct Server {
    state: Arc<RwLock<ServerState>>,
    shutdown_rx: Option<mpsc::Receiver<ShutdownRequest>>,
    mode: ServerMode,
}

enum ServerMode {
    CloudRelay,
    Device {
        identity: identity::DeviceIdentity,
        trust_store: TrustStore,
        data_dir: PathBuf,
    },
}

pub struct ServerBuilder {
    config: Option<Config>,
    credentials: Option<Arc<dyn CredentialProvider>>,
    update_reporter: Option<Arc<dyn UpdateReporter>>,
    as_cloud_relay: bool,
}

pub struct EmbeddedBuilder {
    inner: ServerBuilder,
}

pub struct DaemonBuilder {
    inner: ServerBuilder,
}

struct EmbeddedServerGuard {
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    _started_services: StartedUserServices,
}

impl EmbeddedServerGuard {
    fn new(tasks: Arc<Mutex<Vec<JoinHandle<()>>>>, started_services: StartedUserServices) -> Self {
        Self {
            tasks,
            _started_services: started_services,
        }
    }

    fn abort_tasks(&self) {
        abort_embedded_tasks(&self.tasks);
    }
}

impl Drop for EmbeddedServerGuard {
    fn drop(&mut self) {
        self.abort_tasks();
    }
}

fn push_embedded_task(tasks: &Arc<Mutex<Vec<JoinHandle<()>>>>, task: JoinHandle<()>) {
    tasks
        .lock()
        .expect("embedded server task list mutex poisoned")
        .push(task);
}

fn abort_embedded_tasks(tasks: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    for task in tasks
        .lock()
        .expect("embedded server task list mutex poisoned")
        .drain(..)
    {
        task.abort();
    }
}

impl Server {
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            config: None,
            credentials: None,
            update_reporter: None,
            as_cloud_relay: false,
        }
    }

    pub(crate) fn with_config_and_credentials(
        config: Config,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
        as_cloud_relay: bool,
    ) -> Result<Self> {
        Self::with_config_and_credentials_in_data_dir(
            config,
            credentials,
            update_reporter,
            as_cloud_relay,
            &crate::default_data_dir(),
        )
    }

    fn with_config_and_credentials_in_data_dir(
        config: Config,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
        as_cloud_relay: bool,
        data_dir: &Path,
    ) -> Result<Self> {
        let (host_id, mode) = if as_cloud_relay {
            (Uuid::new_v4(), ServerMode::CloudRelay)
        } else {
            let device_files = identity::ensure_device_files_with_trust_in(data_dir)
                .map_err(|error| ServerError::State(error.to_string()))?;
            (
                device_files.identity.host_id,
                ServerMode::Device {
                    identity: device_files.identity,
                    trust_store: device_files.trust_store,
                    data_dir: data_dir.to_path_buf(),
                },
            )
        };
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        Ok(Self {
            state: Arc::new(RwLock::new(ServerState::new(
                config,
                host_id,
                shutdown_tx,
                credentials,
                update_reporter,
            ))),
            shutdown_rx: Some(shutdown_rx),
            mode,
        })
    }

    fn is_cloud_relay(&self) -> bool {
        matches!(self.mode, ServerMode::CloudRelay)
    }

    fn device_runtime_security(&self) -> DeviceRuntimeSecurity {
        let ServerMode::Device {
            identity,
            trust_store,
            data_dir,
        } = &self.mode
        else {
            unreachable!("cloud relay has no device runtime security");
        };
        DeviceRuntimeSecurity::new(identity.clone(), trust_store.clone(), data_dir.clone())
    }

    /// Run the server
    ///
    /// If this server was constructed as a cloud relay:
    /// - TCP connections use TLS
    /// - All connections require valid JWT tokens
    pub(crate) async fn run(&mut self) -> Result<()> {
        let is_cloud_server = self.is_cloud_relay();
        let (socket_path, tcp_port, cloud_url, prevent_idle_sleep) = {
            let state = self.state.read().await;
            (
                state.config.socket_path.clone(),
                state.config.tcp_port,
                state.config.cloud_url.clone(),
                state.config.prevent_idle_sleep.unwrap_or(false),
            )
        };

        // Validate server-specific config (cloud relay mode requires TCP).
        {
            let state = self.state.read().await;
            state.config.validate(is_cloud_server)?;
        }

        let _sleep_inhibitor = crate::sleep_inhibitor::SleepInhibitor::new(prevent_idle_sleep);

        // Configure cloud server: enable JWT validation and TLS.
        let tls_acceptor = if is_cloud_server {
            let mut state = self.state.write().await;
            state.is_cloud_server = true;
            state.jwt_validator = Some(Arc::new(JwtValidator::new(&cloud_url)));

            // Cloud mode requires TLS certificates via environment variables.
            let cert_path = std::env::var("AMUX_TLS_CERT").map_err(|_| {
                ServerError::Config(ConfigError::Invalid(
                    "AMUX_TLS_CERT environment variable required for cloud mode".into(),
                ))
            })?;
            let key_path = std::env::var("AMUX_TLS_KEY").map_err(|_| {
                ServerError::Config(ConfigError::Invalid(
                    "AMUX_TLS_KEY environment variable required for cloud mode".into(),
                ))
            })?;

            let cert_pem = std::fs::read(&cert_path).map_err(|e| {
                ServerError::Config(ConfigError::Invalid(format!(
                    "Failed to read TLS cert from {}: {}",
                    cert_path, e
                )))
            })?;
            let key_pem = std::fs::read(&key_path).map_err(|e| {
                ServerError::Config(ConfigError::Invalid(format!(
                    "Failed to read TLS key from {}: {}",
                    key_path, e
                )))
            })?;

            let acceptor = create_tls_acceptor(&cert_pem, &key_pem)?;
            tracing::info!("TLS configured for cloud mode");
            Some(acceptor)
        } else {
            None
        };

        let cloud_routing = is_cloud_server.then(|| CloudRoutingService::new(self.state.clone()));
        let mut cloud_routing_task = None;
        let mut local_agent_state = None;
        let mut started_services = None;
        let mut background_tasks: Vec<JoinHandle<()>> = Vec::new();

        if is_cloud_server {
            let Some(port) = tcp_port else {
                unreachable!("cloud server config validation requires tcp_port");
            };
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = TcpListener::bind(addr).await?;
            tracing::info!(addr = %addr, "listening on cloud TLS RoutingService");
            let service = cloud_routing
                .as_ref()
                .expect("cloud server should have cloud routing service");
            let acceptor = tls_acceptor.expect("cloud TLS mode should have TLS acceptor");
            cloud_routing_task =
                Some(service.serve_on_tls_tcp_listener(listener, acceptor, TLS_HANDSHAKE_TIMEOUT));
        } else {
            let agent_state = get_local_agent_service_state(&self.state).await;
            let mut services = start_user_services(
                self.state.clone(),
                agent_state.clone(),
                self.device_runtime_security(),
            )
            .await
            .map_err(|error| ServerError::State(error.to_string()))?;

            if let Some(port) = tcp_port {
                let addr = SocketAddr::from(([0, 0, 0, 0], port));
                let listener = TcpListener::bind(addr).await?;
                tracing::info!(addr = %addr, "listening on direct dispatcher TCP");
                services.serve_external_tcp_listener(listener);
            }

            #[cfg(unix)]
            {
                services.serve_client_service_on_unix_socket(&socket_path)?;
                tracing::info!(path = %socket_path.display(), "listening on local ClientService");
            }

            background_tasks
                .extend(spawn_local_background_tasks(self.state.clone(), &services).await);
            local_agent_state = Some(agent_state);
            started_services = Some(services);
        }

        let mut shutdown_rx = self.shutdown_rx.take().expect("run() called twice");
        let state_path = {
            let state = self.state.read().await;
            state.config.state_path.clone()
        };

        let pending_shutdown_reply = loop {
            let Some(req) = shutdown_rx.recv().await else {
                tracing::warn!("shutdown request channel closed before shutdown");
                return Ok(());
            };
            if let Some(reply) = process_shutdown_request(
                req,
                local_agent_state.as_ref(),
                &state_path,
                started_services
                    .as_ref()
                    .map(|services| services.tunnels.as_ref()),
                cloud_routing.as_ref(),
            )
            .await
            {
                break reply;
            }
        };

        // Remove the local socket before replying so clients can't reconnect to
        // the old server after receiving the response. Keep routing tasks alive
        // briefly so queued GoAway frames can flush.
        #[cfg(unix)]
        if !is_cloud_server {
            let _ = std::fs::remove_file(&socket_path);
        }
        pending_shutdown_reply.send_success();

        tokio::time::sleep(Duration::from_millis(SERVER_GOAWAY_DRAIN_TIMEOUT_MS as u64)).await;
        if let Some(task) = cloud_routing_task.take() {
            task.abort();
        }
        for task in background_tasks {
            task.abort();
        }
        tracing::info!("server exiting");

        Ok(())
    }
}

impl ServerBuilder {
    pub fn config(mut self, config: Config) -> Self {
        self.config = Some(config);
        self
    }

    pub fn credentials(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = Some(provider);
        self
    }

    pub fn update_reporter(mut self, reporter: Arc<dyn UpdateReporter>) -> Self {
        self.update_reporter = Some(reporter);
        self
    }

    pub fn as_cloud_relay(mut self) -> Self {
        self.as_cloud_relay = true;
        self
    }

    pub fn embedded(self) -> EmbeddedBuilder {
        EmbeddedBuilder { inner: self }
    }

    pub fn daemon(self) -> DaemonBuilder {
        DaemonBuilder { inner: self }
    }

    pub async fn run(self) -> Result<()> {
        let (config, credentials, as_cloud_relay, update_reporter) = self.into_parts()?;
        let mut server = Server::with_config_and_credentials(
            config,
            credentials,
            update_reporter,
            as_cloud_relay,
        )?;
        server.run().await
    }
}

impl EmbeddedBuilder {
    pub async fn open(self) -> Result<Client> {
        let (config, credentials, as_cloud_relay, update_reporter) = self.inner.into_parts()?;
        if as_cloud_relay {
            return Err(ServerError::Config(ConfigError::Invalid(
                "embedded cloud relays are not supported; run a daemon cloud relay instead"
                    .to_string(),
            )));
        }
        config.validate(as_cloud_relay)?;
        let mut server = Server::with_config_and_credentials(
            config,
            credentials,
            update_reporter,
            as_cloud_relay,
        )?;

        let tasks = Arc::new(Mutex::new(Vec::new()));
        let agent_state = get_local_agent_service_state(&server.state).await;
        let started_services = start_user_services(
            server.state.clone(),
            agent_state.clone(),
            server.device_runtime_security(),
        )
        .await
        .map_err(|error| ServerError::State(error.to_string()))?;

        for task in spawn_local_background_tasks(server.state.clone(), &started_services).await {
            push_embedded_task(&tasks, task);
        }

        let (client_channel, client_service_task) =
            started_services.open_in_process_client_channel();
        push_embedded_task(&tasks, client_service_task);

        let mut shutdown_rx = server
            .shutdown_rx
            .take()
            .expect("open() called after run()");
        let shutdown_agent_state = agent_state.clone();
        let shutdown_tasks = tasks.clone();
        let shutdown_tunnels = started_services.tunnels.clone();
        let state_path = {
            let state = server.state.read().await;
            state.config.state_path.clone()
        };
        push_embedded_task(
            &tasks,
            tokio::spawn(async move {
                while let Some(req) = shutdown_rx.recv().await {
                    if handle_embedded_shutdown(
                        req,
                        shutdown_agent_state.clone(),
                        state_path.clone(),
                        shutdown_tasks.clone(),
                        shutdown_tunnels.clone(),
                    )
                    .await
                    {
                        break;
                    }
                }
            }),
        );

        let guard = Arc::new(EmbeddedServerGuard::new(tasks, started_services));
        Ok(Client::from_client_service_channel(
            client_channel,
            Some(guard),
        ))
    }
}

impl DaemonBuilder {
    pub async fn open(self) -> std::result::Result<Client, ConnectError> {
        let config = self
            .inner
            .config
            .ok_or_else(|| ConfigError::Invalid("server config is required".to_string()))
            .map_err(ConnectError::Config)?;
        #[cfg(unix)]
        {
            let channel = connect_existing_client_service(&config).await?;
            Ok(Client::from_client_service_channel(channel, None))
        }
        #[cfg(not(unix))]
        Err(ConnectError::Start(
            "local ClientService is only available on Unix sockets and embedded in-process channels".to_string(),
        ))
    }
}

impl ServerBuilder {
    fn into_parts(self) -> std::result::Result<BuilderParts, ConfigError> {
        let config = self
            .config
            .ok_or_else(|| ConfigError::Invalid("server config is required".to_string()))?;
        if self.as_cloud_relay && self.credentials.is_some() {
            return Err(ConfigError::Invalid(
                "credentials() and as_cloud_relay() are mutually exclusive".to_string(),
            ));
        }
        if !self.as_cloud_relay
            && crate::setup::cloud_enabled(&config)
            && self.credentials.is_none()
        {
            return Err(ConfigError::Invalid(
                "credentials provider is required when cloud mode is enabled".to_string(),
            ));
        }
        Ok((
            config,
            self.credentials,
            self.as_cloud_relay,
            self.update_reporter,
        ))
    }
}

fn spawn_periodic_update_check(
    reporter: Option<Arc<dyn UpdateReporter>>,
    cloud_url: String,
    current_version: String,
    interval: Duration,
) -> Option<JoinHandle<()>> {
    let reporter = reporter?;
    Some(tokio::spawn(async move {
        loop {
            match crate::update::check_for_update(&cloud_url, &current_version).await {
                Some(info) => {
                    tracing::info!(
                        current = %info.current_version,
                        latest = %info.update_version,
                        "update available"
                    );
                    reporter.report(UpdateStatus::Available(Some(info)));
                }
                None => {
                    reporter.report(UpdateStatus::Available(None));
                }
            }
            tokio::time::sleep(interval).await;
        }
    }))
}

async fn handle_embedded_shutdown(
    req: ShutdownRequest,
    agent_state: SharedAgentServiceState,
    state_path: std::path::PathBuf,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    tunnels: Arc<TunnelPool>,
) -> bool {
    if let Some(reply) =
        process_shutdown_request(req, Some(&agent_state), &state_path, Some(&tunnels), None).await
    {
        reply.send_success();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(SERVER_GOAWAY_DRAIN_TIMEOUT_MS as u64)).await;
            abort_embedded_tasks(&tasks);
        });
        true
    } else {
        false
    }
}

async fn spawn_local_background_tasks(
    state: Arc<RwLock<ServerState>>,
    started_services: &StartedUserServices,
) -> Vec<JoinHandle<()>> {
    let config = {
        let state = state.read().await;
        state.config.clone()
    };
    let cloud_url = config.cloud_url.clone();
    let mut tasks = Vec::new();
    if crate::setup::cloud_enabled(&config) {
        let connector_ctx = started_services.routing_connector_ctx(generate_server_link(
            &config.host_name,
            config.randomise_link_name,
        ));
        tasks.push(establish_cloud_connection(
            config.clone(),
            state.clone(),
            connector_ctx,
        ));
    }
    tasks.extend(
        started_services.spawn_reachability_links(&config.host_name, config.randomise_link_name),
    );

    let update_reporter = {
        let state = state.read().await;
        state.update_reporter.clone()
    };
    if let Some(task) = spawn_periodic_update_check(
        update_reporter,
        cloud_url,
        env!("CARGO_PKG_VERSION").to_string(),
        Duration::from_secs(3600),
    ) {
        tasks.push(task);
    }
    tasks
}

async fn process_shutdown_request(
    req: ShutdownRequest,
    agent_state: Option<&SharedAgentServiceState>,
    state_path: &Path,
    tunnels: Option<&TunnelPool>,
    cloud_routing: Option<&CloudRoutingService>,
) -> Option<PendingShutdownReply> {
    match req {
        ShutdownRequest::Shutdown { reply } => {
            if let Some(agent_state) = agent_state {
                notify_local_clients(agent_state, ShutdownReason::UserRequested).await;
                shutdown_server(agent_state).await;
            }
            notify_routing_peers(tunnels, cloud_routing, ShutdownReason::UserRequested).await;
            Some(PendingShutdownReply::Shutdown { reply })
        }
        ShutdownRequest::Suspend { reason, reply } => {
            let Some(agent_state) = agent_state else {
                notify_routing_peers(tunnels, cloud_routing, reason).await;
                return Some(PendingShutdownReply::Suspend {
                    reply,
                    suspended_count: 0,
                });
            };
            let (suspended, errors) = prepare_server_suspend(agent_state).await;
            let suspended_count = suspended.agents.len();
            if !errors.is_empty() {
                let _ = reply.send(Err(ProtocolError::ServerError {
                    message: errors.join("; "),
                }));
                return None;
            }
            if !suspended.agents.is_empty()
                && let Err(error) = crate::suspend::save_suspended(state_path, &suspended)
            {
                tracing::error!(error = %error, "failed to save suspended agents");
                let _ = reply.send(Err(ProtocolError::ServerError {
                    message: format!("failed to save state: {error}"),
                }));
                return None;
            }
            notify_local_clients(agent_state, reason).await;
            notify_routing_peers(tunnels, cloud_routing, reason).await;
            commit_server_suspend(agent_state).await;
            Some(PendingShutdownReply::Suspend {
                reply,
                suspended_count: suspended_count as u64,
            })
        }
    }
}

async fn notify_local_clients(agent_state: &SharedAgentServiceState, reason: ShutdownReason) {
    agent_state.write().await.local_shutdown_events.emit(reason);
}

async fn notify_routing_peers(
    tunnels: Option<&TunnelPool>,
    cloud_routing: Option<&CloudRoutingService>,
    reason: ShutdownReason,
) {
    let goaway_reason = goaway_reason_for_shutdown(reason);
    if let Some(tunnels) = tunnels {
        tunnels
            .link_registry()
            .send_goaway_to_all(goaway_reason, SERVER_GOAWAY_DRAIN_TIMEOUT_MS)
            .await;
    }
    if let Some(cloud_routing) = cloud_routing {
        cloud_routing
            .send_goaway_to_all(goaway_reason, SERVER_GOAWAY_DRAIN_TIMEOUT_MS)
            .await;
    }
}

fn goaway_reason_for_shutdown(reason: ShutdownReason) -> wire::pb::GoAwayReason {
    match reason {
        ShutdownReason::UpdateRequired => wire::pb::GoAwayReason::UpdateRequired,
        ShutdownReason::ProtocolError => wire::pb::GoAwayReason::ProtocolError,
        ShutdownReason::UserRequested => wire::pb::GoAwayReason::UserShutdown,
        ShutdownReason::Updating => wire::pb::GoAwayReason::Updating,
        ShutdownReason::Suspending => wire::pb::GoAwayReason::Suspending,
        ShutdownReason::Restarting => wire::pb::GoAwayReason::Restarting,
        ShutdownReason::AuthExpired => wire::pb::GoAwayReason::AuthExpired,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Server, ShutdownReason, goaway_reason_for_shutdown};
    use crate::Config;
    use crate::protocol::wire;

    #[test]
    fn shutdown_reason_maps_to_goaway_reason() {
        assert_eq!(
            goaway_reason_for_shutdown(ShutdownReason::UserRequested),
            wire::pb::GoAwayReason::UserShutdown
        );
        assert_eq!(
            goaway_reason_for_shutdown(ShutdownReason::Updating),
            wire::pb::GoAwayReason::Updating
        );
        assert_eq!(
            goaway_reason_for_shutdown(ShutdownReason::Suspending),
            wire::pb::GoAwayReason::Suspending
        );
    }

    #[test]
    fn update_checker_is_not_spawned_without_reporter() {
        let task = super::spawn_periodic_update_check(
            None,
            "https://example.com".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            Duration::from_secs(3600),
        );

        assert!(task.is_none());
    }

    #[test]
    fn device_server_uses_persisted_host_id() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            path: Some(dir.path().join("config.yaml")),
            state_path: dir.path().join("state.yaml"),
            ..Config::default()
        };

        let first = Server::with_config_and_credentials_in_data_dir(
            config.clone(),
            None,
            None,
            false,
            dir.path(),
        )
        .unwrap();
        let second =
            Server::with_config_and_credentials_in_data_dir(config, None, None, false, dir.path())
                .unwrap();

        assert_eq!(
            first.state.blocking_read().host_id(),
            second.state.blocking_read().host_id()
        );
        assert!(dir.path().join("device.key").exists());
        assert!(dir.path().join("host_id").exists());
        assert!(dir.path().join("trust.json").exists());
    }

    #[test]
    fn cloud_relay_does_not_create_device_identity_files() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            path: Some(dir.path().join("config.yaml")),
            state_path: dir.path().join("state.yaml"),
            tcp_port: Some(0),
            ..Config::default()
        };

        let first = Server::with_config_and_credentials_in_data_dir(
            config.clone(),
            None,
            None,
            true,
            dir.path(),
        )
        .unwrap();
        let second =
            Server::with_config_and_credentials_in_data_dir(config, None, None, true, dir.path())
                .unwrap();

        assert_ne!(
            first.state.blocking_read().host_id(),
            second.state.blocking_read().host_id()
        );
        assert!(!dir.path().join("device.key").exists());
        assert!(!dir.path().join("host_id").exists());
        assert!(!dir.path().join("trust.json").exists());
    }
}
