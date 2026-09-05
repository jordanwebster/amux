//! Server core: state management, listener orchestration, and session lifecycle.
//!
//! Starts tonic services for local clients, host-to-host tunnels, and optional
//! cloud routing. Shared daemon state is kept in `Arc<RwLock<ServerState>>`.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{RwLock, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::auth::CredentialProvider;
use crate::auth::jwt::JwtValidator;
#[cfg(unix)]
use crate::client::connect_existing_client_service;
use crate::client::{Client, ConnectError};
use crate::config::{Config, ConfigError};
use crate::identity;
use crate::profile::runtime::{
    Listeners, ProfileRuntime, ProfileRuntimeOptions, start_with_security,
};
use crate::protocol::wire;
use crate::services::{CloudLinkService, DeviceRuntimeSecurity};
use crate::subscription::SubscriptionReporter;
use crate::transport::{TransportError, create_tls_acceptor};
use crate::update::{UpdateReporter, UpdateStatus};
use crate::user_state::ServerState;

/// Maximum time allowed for a TLS handshake to complete.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Local grace before aborting routing tasks so queued LinkClose frames can
/// flush onto the sockets. Purely local; nothing on the wire mentions it.
const SERVER_LINK_CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(200);

type Result<T> = std::result::Result<T, ServerError>;
type BuilderParts = (
    Config,
    Option<Arc<dyn CredentialProvider>>,
    bool,
    Option<Arc<dyn UpdateReporter>>,
    Option<Arc<dyn SubscriptionReporter>>,
);

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
    mode: ServerMode,
}

enum ServerMode {
    CloudRelay,
    Device {
        security: Option<DeviceRuntimeSecurity>,
    },
}

pub struct ServerBuilder {
    config: Option<Config>,
    credentials: Option<Arc<dyn CredentialProvider>>,
    subscription_reporter: Option<Arc<dyn SubscriptionReporter>>,
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
    stop_tx: Option<oneshot::Sender<()>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl EmbeddedServerGuard {
    fn new(stop_tx: oneshot::Sender<()>, task: JoinHandle<()>) -> Self {
        Self {
            stop_tx: Some(stop_tx),
            task: Mutex::new(Some(task)),
        }
    }
}

impl Drop for EmbeddedServerGuard {
    fn drop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        // Dropping a JoinHandle detaches the task. The stop signal above owns
        // the orderly teardown; aborting here would skip cloud-link cleanup.
        let _ = self
            .task
            .lock()
            .expect("embedded server task mutex poisoned")
            .take();
    }
}

impl Server {
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            config: None,
            credentials: None,
            subscription_reporter: None,
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
        let data_dir = config.data_dir.clone();
        Self::with_config_and_credentials_in_data_dir(
            config,
            credentials,
            update_reporter,
            as_cloud_relay,
            &data_dir,
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
                    security: Some(DeviceRuntimeSecurity::new(
                        device_files.identity,
                        device_files.trust_store,
                        data_dir.to_path_buf(),
                    )),
                },
            )
        };

        Ok(Self {
            state: Arc::new(RwLock::new(ServerState::new(
                config,
                host_id,
                credentials,
                update_reporter,
            ))),
            mode,
        })
    }

    fn is_cloud_relay(&self) -> bool {
        matches!(self.mode, ServerMode::CloudRelay)
    }

    fn take_device_runtime_security(&mut self) -> DeviceRuntimeSecurity {
        let ServerMode::Device { security } = &mut self.mode else {
            unreachable!("cloud relay has no device runtime security");
        };
        security.take().expect("device server run twice")
    }

    /// Run the server
    ///
    /// If this server was constructed as a cloud relay:
    /// - TCP connections use TLS
    /// - All connections require valid JWT tokens
    pub(crate) async fn run(&mut self) -> Result<()> {
        let is_cloud_server = self.is_cloud_relay();
        let (tcp_port, cloud_url, prevent_idle_sleep) = {
            let state = self.state.read().await;
            (
                state.config.tcp_port,
                state.config.cloud_url.clone(),
                state.config.prevent_idle_sleep.unwrap_or(false),
            )
        };

        if is_cloud_server && tcp_port.is_none() {
            return Err(ConfigError::Invalid("cloud relay requires tcp_port".into()).into());
        }

        // Validate shared settings before creating runtime services.
        {
            let state = self.state.read().await;
            state.config.validate()?;
        }

        let _sleep_inhibitor = crate::sleep_inhibitor::SleepInhibitor::new(prevent_idle_sleep);

        if !is_cloud_server {
            let (
                config,
                credentials,
                update_reporter,
                subscription_reporter,
                has_cloud_credentials,
            ) = {
                let state = self.state.read().await;
                (
                    state.config.clone(),
                    state.credentials.clone(),
                    state.update_reporter.clone(),
                    state.subscription_reporter.clone(),
                    state.credentials.is_some(),
                )
            };
            let options = ProfileRuntimeOptions::from_legacy_config(
                config,
                credentials,
                update_reporter,
                subscription_reporter,
                Listeners::Sockets,
            );
            let security = self.take_device_runtime_security();
            let runtime = start_with_security(options, security)
                .await
                .map_err(|error| ServerError::State(error.to_string()))?;
            if has_cloud_credentials {
                runtime
                    .start_cloud()
                    .await
                    .map_err(|error| ServerError::State(error.to_string()))?;
            }

            tokio::signal::ctrl_c().await?;
            runtime.stop(ShutdownReason::UserRequested).await;
            tracing::info!("server exiting");
            return Ok(());
        }

        // Configure cloud server: enable JWT validation and TLS.
        let tls_acceptor = {
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
            acceptor
        };

        let cloud_routing = CloudLinkService::new(self.state.clone());
        let Some(port) = tcp_port else {
            unreachable!("cloud server config validation requires tcp_port");
        };
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(addr = %addr, "listening on cloud TLS LinkService");
        let cloud_routing_task =
            cloud_routing.serve_on_tls_tcp_listener(listener, tls_acceptor, TLS_HANDSHAKE_TIMEOUT);

        tokio::signal::ctrl_c().await?;
        cloud_routing
            .send_link_close_to_all(link_close_reason_for_shutdown(
                ShutdownReason::UserRequested,
            ))
            .await;
        tokio::time::sleep(SERVER_LINK_CLOSE_FLUSH_TIMEOUT).await;
        cloud_routing_task.abort();
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

    pub fn subscription_reporter(mut self, reporter: Arc<dyn SubscriptionReporter>) -> Self {
        self.subscription_reporter = Some(reporter);
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
        let (config, credentials, as_cloud_relay, update_reporter, subscription_reporter) =
            self.into_parts()?;
        let mut server = Server::with_config_and_credentials(
            config,
            credentials,
            update_reporter,
            as_cloud_relay,
        )?;
        server.state.write().await.subscription_reporter = subscription_reporter;
        server.run().await
    }
}

impl EmbeddedBuilder {
    pub async fn open(self) -> Result<Client> {
        let (config, credentials, as_cloud_relay, update_reporter, subscription_reporter) =
            self.inner.into_parts()?;
        if as_cloud_relay {
            return Err(ServerError::Config(ConfigError::Invalid(
                "embedded cloud relays are not supported; run a daemon cloud relay instead"
                    .to_string(),
            )));
        }
        config.validate()?;
        let has_cloud_credentials = credentials.is_some();
        let options = ProfileRuntimeOptions::from_legacy_config(
            config,
            credentials,
            update_reporter,
            subscription_reporter,
            Listeners::InProcessOnly,
        );
        let runtime = crate::profile::runtime::start(options)
            .await
            .map_err(|error| ServerError::State(error.to_string()))?;
        if has_cloud_credentials {
            runtime
                .start_cloud()
                .await
                .map_err(|error| ServerError::State(error.to_string()))?;
        }
        Ok(spawn_embedded_runtime(runtime))
    }
}

pub(crate) fn spawn_embedded_runtime(runtime: ProfileRuntime) -> Client {
    let client = runtime.client();
    let (stop_tx, stop_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let _ = stop_rx.await;
        runtime.stop(ShutdownReason::UserRequested).await;
    });
    let guard = Arc::new(EmbeddedServerGuard::new(stop_tx, task));
    client.with_guard(guard)
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
        Ok((
            config,
            self.credentials,
            self.as_cloud_relay,
            self.update_reporter,
            self.subscription_reporter,
        ))
    }
}

pub(crate) fn spawn_periodic_update_check(
    reporter: Option<Arc<dyn UpdateReporter>>,
    manifest_url: String,
    current_version: String,
    interval: Duration,
) -> Option<JoinHandle<()>> {
    let reporter = reporter?;
    Some(tokio::spawn(async move {
        loop {
            match crate::update::check_for_update(&manifest_url, &current_version).await {
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

fn link_close_reason_for_shutdown(reason: ShutdownReason) -> wire::pb::LinkCloseReason {
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Server, ShutdownReason, link_close_reason_for_shutdown};
    use crate::Config;
    use crate::protocol::wire;

    #[test]
    fn shutdown_reason_maps_to_link_close_reason() {
        assert_eq!(
            link_close_reason_for_shutdown(ShutdownReason::UserRequested),
            wire::pb::LinkCloseReason::UserShutdown
        );
        assert_eq!(
            link_close_reason_for_shutdown(ShutdownReason::Updating),
            wire::pb::LinkCloseReason::Updating
        );
        assert_eq!(
            link_close_reason_for_shutdown(ShutdownReason::Suspending),
            wire::pb::LinkCloseReason::Suspending
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

    #[tokio::test]
    async fn update_checker_is_spawned_with_reporter() {
        use std::sync::Arc;

        use crate::update::{UpdateReporter, UpdateStatus};

        struct NoopReporter;
        impl UpdateReporter for NoopReporter {
            fn report(&self, _status: UpdateStatus) {}
        }

        // The daemon path wires the poll whenever a reporter is configured.
        let task = super::spawn_periodic_update_check(
            Some(Arc::new(NoopReporter)),
            "http://127.0.0.1:1".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            Duration::from_secs(3600),
        );

        assert!(task.is_some());
        if let Some(task) = task {
            task.abort();
        }
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
