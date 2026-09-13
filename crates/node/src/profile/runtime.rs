//! Runtime ownership for one complete amux device profile.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use client::Client;
use host_api::{HostConfig, LocalAgentHost, LocalAgentHostFactory};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;

use super::status::{Observed, RuntimeStatus};
use crate::auth::CredentialProvider;
use crate::config::{Config, ConfigError, Keybinds, LanConfig, UiSettings};
use crate::discovery::{Advertisement, Discovery, DiscoveryError, FoundHosts, local_pairing_addrs};
use crate::identity;
use crate::server::ShutdownReason;
use crate::services::{
    CloudLink, CloudTransport, DeviceRuntimeSecurity, StartedUserServices, UDP_BLOCKED_MEMORY,
    UdpBlockedMemory, establish_cloud_link, start_user_services,
};
use crate::transport::InProcessConnection;
use crate::update::UpdateReporter;
use crate::user_state::ServerState;

const LINK_CLOSE_FLUSH_TIMEOUT: Duration = Duration::from_millis(200);

pub(crate) fn platform_discovery() -> Result<Arc<dyn Discovery>, DiscoveryError> {
    #[cfg(all(debug_assertions, not(target_os = "ios")))]
    match std::env::var("AMUX_TEST_DISCOVERY_MODE").as_deref() {
        Ok("scripted" | "disabled") => {
            return Ok(Arc::new(crate::discovery::ScriptedDiscovery::new()));
        }
        Ok("mdns") | Err(std::env::VarError::NotPresent) => {}
        Ok(mode) => {
            return Err(DiscoveryError::Unavailable(format!(
                "unknown test discovery mode {mode:?}"
            )));
        }
        Err(error) => {
            return Err(DiscoveryError::Unavailable(format!(
                "invalid test discovery mode: {error}"
            )));
        }
    }
    #[cfg(any(test, target_os = "ios"))]
    {
        Ok(Arc::new(crate::discovery::ScriptedDiscovery::new()))
    }
    #[cfg(not(any(test, target_os = "ios")))]
    {
        Ok(Arc::new(crate::discovery::MdnsDiscovery::new()?))
    }
}

use crate::installation::ProfilePaths;

/// Settings that vary between profiles.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeConfig {
    pub(crate) cloud_url: String,
    pub(crate) lan: LanConfig,
    pub(crate) cloud_refresh_interval: Option<Duration>,
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
    Sockets,
}

impl Listeners {
    pub(crate) fn has_sockets(self) -> bool {
        match self {
            Self::InProcessOnly => false,
            Self::Sockets => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectDialPolicy {
    Never,
    OnStart,
    WhileForeground,
}

#[derive(Clone)]
pub enum CloudFixtureAuth {
    Refreshing(crate::routing::LinkConnectorAuth),
    RefreshingQuic {
        auth: crate::routing::LinkConnectorAuth,
        client_config: quinn::ClientConfig,
        server_name: String,
        quic_addr: SocketAddr,
    },
    RefreshingAuto {
        auth: crate::routing::LinkConnectorAuth,
        client_config: quinn::ClientConfig,
        server_name: String,
        quic_addr: SocketAddr,
    },
}

#[derive(Default)]
pub struct RuntimeFixtures {
    pub listener: Option<std::net::UdpSocket>,
    pub quic_client_socket: Option<std::net::UdpSocket>,
    pub advertised_addr: Option<SocketAddr>,
    pub quic_transport: Option<Arc<quinn::TransportConfig>>,
    pub discovery: Option<Arc<dyn Discovery>>,
    pub host_factory: Option<Arc<dyn LocalAgentHostFactory>>,
    pub cloud: Option<(std::net::SocketAddr, CloudFixtureAuth)>,
    pub cloud_transport: Option<std::net::SocketAddr>,
    pub cloud_refresh_interval: Option<Duration>,
    pub udp_blocked_memory: Option<Duration>,
}

pub struct ProfileRuntimeOptions {
    pub(crate) paths: ProfilePaths,
    pub(crate) config: RuntimeConfig,
    pub(crate) shared: Arc<InstallationSettings>,
    pub(crate) credentials: Option<Arc<dyn CredentialProvider>>,
    pub(crate) discovery: Arc<dyn Discovery>,
    pub(crate) dial: DirectDialPolicy,
    pub(crate) host_factory: Option<Arc<dyn LocalAgentHostFactory>>,

    pub(crate) listeners: Listeners,
    pub fixtures: RuntimeFixtures,
}

impl ProfileRuntimeOptions {
    pub fn from_legacy_config(
        config: Config,
        credentials: Option<Arc<dyn CredentialProvider>>,
        update_reporter: Option<Arc<dyn UpdateReporter>>,
        listeners: Listeners,
        discovery: Arc<dyn Discovery>,
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
            lan: config.lan,
            cloud_refresh_interval: None,
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
            },
        };
        Self {
            paths,
            config: profile,
            shared: Arc::new(shared),
            credentials,
            discovery,
            dial: DirectDialPolicy::OnStart,
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
            tcp_port: None,
            udp_port: None,
            lan: self.config.lan,
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
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    #[error("profile state error: {0}")]
    State(String),
}

#[derive(Debug, Error)]
pub enum CloudStartError {
    #[error("profile has no cloud credentials")]
    MissingCredentials,
}

/// All state and tasks owned by one complete device profile.
/// The parts of an embedder's relay a profile keeps: who can obtain a token
/// for this account, and the live link's own account of itself.
#[derive(Clone)]
struct AttachedRelay {
    credentials: Arc<dyn CredentialProvider>,
    retry: Arc<crate::RelayRetry>,
}

pub struct ProfileRuntime {
    pub host_id: crate::HostId,
    paths: ProfilePaths,
    state: Arc<RwLock<ServerState>>,
    pub agent_host: Option<Arc<dyn LocalAgentHost>>,
    pub services: StartedUserServices,
    pub trust: crate::trust::SharedTrustStore,
    test_cloud: Option<(std::net::SocketAddr, CloudFixtureAuth)>,
    pub test_cloud_transport: Option<std::net::SocketAddr>,
    pub test_cloud_refresh_interval: Option<Duration>,
    client: Client,
    pub client_channel: tonic::transport::Channel,
    in_process_connection: InProcessConnection,
    discovery: Arc<dyn Discovery>,
    dial: DirectDialPolicy,
    background_tasks: Vec<JoinHandle<()>>,
    cloud_link: Mutex<Option<CloudLink>>,
    udp_blocked: Arc<UdpBlockedMemory>,
    /// The embedder obtains relay credentials from the configured cloud through
    /// its own account API. Stopping the profile must also stop this link.
    relay_task: Mutex<Option<JoinHandle<()>>>,
    /// What the attached relay left behind, so this profile can ask the same
    /// account service for a fresh entitlement without a cloud link.
    attached_relay: Mutex<Option<AttachedRelay>>,
    status: RuntimeStatus,
    #[cfg(unix)]
    unix_accept_task: Option<JoinHandle<()>>,
    #[cfg(unix)]
    link_accept_task: Option<JoinHandle<()>>,
    #[cfg(unix)]
    socket_ownership: Option<SocketOwnership>,
    #[cfg(unix)]
    link_socket_ownership: Option<SocketOwnership>,
}

/// Start local services and listeners for one profile. Cloud attachment is
/// intentionally a separate operation.
pub async fn start(options: ProfileRuntimeOptions) -> Result<ProfileRuntime, ProfileStartError> {
    let reporters = options
        .shared
        .status_reporters
        .resolve(&options.paths.state_path);
    let status = RuntimeStatus::new(reporters.update);
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
    let status = RuntimeStatus::new(reporters.update);
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
    let discovery = options
        .fixtures
        .discovery
        .take()
        .unwrap_or_else(|| options.discovery.clone());

    let mut service_config = options.service_config();
    service_config.validate()?;

    let mut quic_server_config = security
        .quic_server_config()
        .map_err(|error| ProfileStartError::State(error.to_string()))?;
    if let Some(transport) = options.fixtures.quic_transport.clone() {
        quic_server_config.transport_config(transport);
    }
    let mut bound = BoundListeners::bind(&options, quic_server_config.clone()).await?;
    let mut lan_endpoint = bound.quic_endpoint.take();
    if let Some(socket) = options.fixtures.listener.take() {
        socket.set_nonblocking(true)?;
        lan_endpoint = Some(quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(quic_server_config),
            socket,
            Arc::new(quinn::TokioRuntime),
        )?);
    }
    let lan_addr = lan_endpoint
        .as_ref()
        .map(quinn::Endpoint::local_addr)
        .transpose()?;
    let advertised_lan_addr = options.fixtures.advertised_addr.or(lan_addr);
    if let Some(addr) = advertised_lan_addr {
        service_config.lan.port = addr.port();
    }

    let host_id = security.host_id();
    let state = Arc::new(RwLock::new(ServerState::new(
        service_config.clone(),
        host_id,
        options.credentials.clone(),
        reporters.update.clone(),
    )));

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

    let found_hosts = Arc::new(FoundHosts::default());
    let direct_endpoint = match &lan_endpoint {
        Some(endpoint) => endpoint.clone(),
        None => {
            if let Some(socket) = options.fixtures.quic_client_socket.take() {
                socket.set_nonblocking(true)?;
                quinn::Endpoint::new(
                    quinn::EndpointConfig::default(),
                    None,
                    socket,
                    Arc::new(quinn::TokioRuntime),
                )?
            } else {
                quinn::Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))?
            }
        }
    };
    services.configure_reachability(
        options.paths.data_dir.clone(),
        discovery.clone(),
        found_hosts,
        direct_endpoint,
    );
    services.set_test_quic_transport(options.fixtures.quic_transport.clone());

    #[cfg(unix)]
    let unix_accept_task = bound.unix_listener.take().map(|listener| {
        let task = services.serve_client_service_on_unix_listener(listener);
        tracing::info!(path = %options.paths.socket_path.display(), "listening on profile ClientService");
        task
    });
    #[cfg(unix)]
    let link_accept_task = bound.link_listener.take().map(|listener| {
        let task = services.serve_link_on_unix_listener(listener);
        tracing::info!(
            path = %crate::installation::adjacent_link_socket_path(&options.paths.socket_path).display(),
            "listening on profile native links"
        );
        task
    });
    if let Some(endpoint) = lan_endpoint {
        let addr = advertised_lan_addr.expect("LAN listener address captured before serving");
        let addrs = if addr.ip().is_unspecified() {
            local_pairing_addrs(addr.port())
        } else {
            vec![addr]
        };
        discovery.advertise(Advertisement {
            host_id,
            name: options.shared.host_name.clone(),
            version: crate::PROTOCOL_VERSION,
            addrs,
        })?;
        services.serve_external_quic_endpoint(endpoint);
        tracing::info!(addr = %addr, "listening on profile direct dispatcher QUIC");
    }

    let mut background_tasks = Vec::new();
    if options.dial != DirectDialPolicy::Never {
        let events = discovery.browse();
        background_tasks.push(services.spawn_dial_on_found(events));
        background_tasks.extend(services.spawn_reachability_links());
        discovery.requery();
    }
    if options.listeners.has_sockets()
        && let Some(task) = crate::server::spawn_periodic_update_check(
            reporters.update.clone(),
            options.shared.update_manifest_url.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
            Duration::from_secs(3600),
        )
    {
        background_tasks.push(task);
    }

    let (client_channel, client_task, in_process_connection) =
        services.open_managed_in_process_client_channel();
    services.push_task(client_task);
    let client = Client::from_channel(client_channel.clone());
    status.report(Observed::Local);

    #[cfg(unix)]
    let (socket_ownership, link_socket_ownership) = bound.disarm_socket_cleanup();

    let udp_blocked = Arc::new(UdpBlockedMemory::new(
        options
            .fixtures
            .udp_blocked_memory
            .unwrap_or(UDP_BLOCKED_MEMORY),
    ));

    Ok(ProfileRuntime {
        host_id,
        paths: options.paths,
        state,
        agent_host,
        services,
        trust,
        test_cloud: options.fixtures.cloud,
        test_cloud_transport: options.fixtures.cloud_transport,
        test_cloud_refresh_interval: options
            .fixtures
            .cloud_refresh_interval
            .or(options.config.cloud_refresh_interval),
        client,
        client_channel,
        in_process_connection,
        discovery,
        dial: options.dial,
        background_tasks,
        cloud_link: Mutex::new(None),
        udp_blocked,
        relay_task: Mutex::new(None),
        attached_relay: Mutex::new(None),
        status,
        #[cfg(unix)]
        unix_accept_task,
        #[cfg(unix)]
        link_accept_task,
        #[cfg(unix)]
        socket_ownership,
        #[cfg(unix)]
        link_socket_ownership,
    })
}

impl ProfileRuntime {
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    pub fn rebind_direct_quic(&self, socket: std::net::UdpSocket) -> std::io::Result<()> {
        socket.set_nonblocking(true)?;
        self.services.rebind_direct_quic(socket)
    }

    /// Hands this profile the machines an outside browser resolved. What the
    /// discovery makes of it is its own: a profile that browses for itself
    /// ignores a set somebody else found.
    pub fn hand_over_discovered(&self, found: Vec<crate::discovery::Advertisement>) {
        self.discovery.hand_over(found);
    }

    pub async fn suspend_direct_links(&self) {
        if self.dial == DirectDialPolicy::WhileForeground {
            self.services.close_direct_links().await;
        }
    }

    pub fn resume_direct_links(&mut self) {
        if self.dial == DirectDialPolicy::WhileForeground {
            self.background_tasks
                .extend(self.services.resume_direct_links());
        }
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
    /// What a relay link of this profile dials on: the profile's own QUIC
    /// endpoint and its memory of networks that ate UDP, so the relay races the
    /// same two carriers the configured cloud link does.
    pub(crate) fn relay_transport(&self) -> crate::transport::RelayTransport {
        crate::transport::RelayTransport {
            quic_endpoint: self.services.quic_endpoint(),
            udp_blocked: self.udp_blocked.clone(),
        }
    }

    pub(crate) async fn attach_relay(&self, relay: crate::EmbeddedRelay) {
        let attached = AttachedRelay {
            credentials: relay.credentials.clone(),
            retry: relay.retry.clone(),
        };
        let task = relay.spawn(
            self.services.link_connector_ctx(),
            self.relay_transport(),
            self.status.clone(),
        );
        *self.attached_relay.lock().await = Some(attached);
        // A relay route belongs to an account, so a profile that has one is
        // signed in as far as anything it greets is concerned: that is what
        // tells a machine on the other side this device can be reached when
        // it is not on the same network.
        self.services.set_signed_in(true);
        if let Some(previous) = self.relay_task.lock().await.replace(task) {
            previous.abort();
            let _ = previous.await;
        }
    }

    async fn stop_relay(&self) {
        if let Some(task) = self.relay_task.lock().await.take() {
            task.abort();
            let _ = task.await;
            self.status.report(Observed::Local);
        }
        if self.attached_relay.lock().await.take().is_some() {
            self.services.set_signed_in(false);
        }
    }

    pub(crate) async fn configure_credentials(
        &self,
        cloud_url: String,
        credentials: Option<Arc<dyn CredentialProvider>>,
    ) {
        let signed_in = credentials.is_some();
        let mut state = self.state.write().await;
        state.config.cloud_url = cloud_url;

        state.credentials = credentials;
        self.services.set_signed_in(signed_in);
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
        let signed_in = self.state.read().await.credentials.is_some();
        if let Some((address, auth)) = &self.test_cloud {
            let mut connector = self.cloud_link.lock().await;
            if connector
                .as_ref()
                .is_some_and(|connector| !connector.is_finished())
            {
                return Ok(());
            }
            if let Some(finished) = connector.take() {
                finished.stop().await;
            }
            let ctx = self.services.link_connector_ctx_with_signed_in(signed_in);
            *connector = Some(match auth {
                CloudFixtureAuth::Refreshing(auth) => CloudLink::testnet_with_auth(
                    ctx,
                    *address,
                    auth.clone(),
                    self.status.clone(),
                    self.services.quic_endpoint(),
                    crate::services::TestCloudTransport::Tcp,
                    self.udp_blocked.clone(),
                ),
                CloudFixtureAuth::RefreshingQuic {
                    auth,
                    client_config,
                    server_name,
                    quic_addr,
                } => CloudLink::testnet_with_auth(
                    ctx,
                    *address,
                    auth.clone(),
                    self.status.clone(),
                    self.services.quic_endpoint(),
                    crate::services::TestCloudTransport::Quic {
                        client_config: client_config.clone(),
                        server_name: server_name.clone(),
                        quic_addr: *quic_addr,
                    },
                    self.udp_blocked.clone(),
                ),
                CloudFixtureAuth::RefreshingAuto {
                    auth,
                    client_config,
                    server_name,
                    quic_addr,
                } => CloudLink::testnet_with_auth(
                    ctx,
                    *address,
                    auth.clone(),
                    self.status.clone(),
                    self.services.quic_endpoint(),
                    crate::services::TestCloudTransport::Auto {
                        client_config: client_config.clone(),
                        server_name: server_name.clone(),
                        quic_addr: *quic_addr,
                    },
                    self.udp_blocked.clone(),
                ),
            });
            return Ok(());
        }
        if !signed_in {
            self.status.report(Observed::AuthenticationRequired);
            return Err(CloudStartError::MissingCredentials);
        }

        let mut connector = self.cloud_link.lock().await;
        if connector
            .as_ref()
            .is_some_and(|connector| !connector.is_finished())
        {
            return Ok(());
        }
        if let Some(finished) = connector.take() {
            finished.stop().await;
        }

        let state = self.state.read().await;
        let config = state.config.clone();
        drop(state);
        self.status.report(Observed::Connecting);
        *connector = Some(establish_cloud_link(
            config,
            self.state.clone(),
            self.services.link_connector_ctx_with_signed_in(signed_in),
            self.status.clone(),
            CloudTransport::new(
                self.services.quic_endpoint(),
                self.udp_blocked.clone(),
                self.test_cloud_transport,
                self.test_cloud_refresh_interval,
            ),
        ));
        Ok(())
    }

    /// Ask the account service what this account buys, now.
    ///
    /// A daemon asks its own cloud link. A rich client has none — its relay
    /// route was resolved by the application, which is also the only thing
    /// that can obtain a token — so the question goes back out through that
    /// application, and the answer is re-reported so a screen already showing
    /// the old tier follows without waiting for the link to be rebuilt.
    pub async fn refresh_entitlement(&self) -> Result<crate::Tier, crate::auth::cloud::CloudError> {
        if let Some(connector) = self.cloud_link.lock().await.as_ref() {
            return connector.refresh_entitlement().await;
        }
        let attached = self.attached_relay.lock().await.clone().ok_or_else(|| {
            crate::auth::cloud::CloudError::Connection("no relay is attached".into())
        })?;
        let token = attached
            .credentials
            .access_token()
            .await
            .map_err(|error| crate::auth::cloud::CloudError::Connection(error.to_string()))?;
        let tier = token.tier.unwrap_or(crate::Tier::Free);
        // Only a live link has a carrier to name, and a state that claimed one
        // while nothing was connected would be a worse answer than silence.
        if let Some(carrier) = attached.retry.carrier() {
            self.status.report(Observed::Connected { tier, carrier });
        }
        Ok(tier)
    }

    pub async fn set_test_cloud_auth(&mut self, auth: CloudFixtureAuth) {
        self.stop_cloud().await;
        self.test_cloud.as_mut().expect("test cloud configured").1 = auth;
    }

    pub async fn stop_cloud(&self) {
        let mut connector = self.cloud_link.lock().await;
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
        self.discovery.withdraw();
        self.services.stop_accepting_external_links().await;
        self.stop_accepting_local_clients().await;
        self.services
            .channels
            .link_registry()
            .send_link_close_to_all(link_close_reason(reason))
            .await;
        self.services.close_direct_links().await;
        self.stop_cloud().await;
        self.stop_relay().await;

        if let Some(host) = &self.agent_host {
            host.notify_shutdown(reason).await;
        }
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
            if let Some(task) = self.link_accept_task.take() {
                task.abort();
                let _ = task.await;
            }
            if let Some(ownership) = self.socket_ownership.take() {
                ownership.remove_if_owned();
            }
            if let Some(ownership) = self.link_socket_ownership.take() {
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
        self.discovery.withdraw();
        #[cfg(unix)]
        if let Some(task) = &self.unix_accept_task {
            task.abort();
        }
        #[cfg(unix)]
        if let Some(task) = &self.link_accept_task {
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
    quic_endpoint: Option<quinn::Endpoint>,
    #[cfg(unix)]
    unix_listener: Option<tokio::net::UnixListener>,
    #[cfg(unix)]
    link_listener: Option<tokio::net::UnixListener>,
    #[cfg(unix)]
    socket_ownership: Option<SocketOwnership>,
    #[cfg(unix)]
    link_socket_ownership: Option<SocketOwnership>,
}

impl BoundListeners {
    async fn bind(
        options: &ProfileRuntimeOptions,
        quic_server_config: quinn::ServerConfig,
    ) -> std::io::Result<Self> {
        if options.listeners == Listeners::InProcessOnly {
            return Ok(Self {
                quic_endpoint: None,
                #[cfg(unix)]
                unix_listener: None,
                #[cfg(unix)]
                link_listener: None,
                #[cfg(unix)]
                socket_ownership: None,
                #[cfg(unix)]
                link_socket_ownership: None,
            });
        }

        #[cfg(unix)]
        let unix_listener = crate::transport::bind_unix_listener(&options.paths.socket_path)?;
        #[cfg(unix)]
        let socket_ownership = Some(SocketOwnership::capture(options.paths.socket_path.clone())?);
        #[cfg(unix)]
        let link_path = crate::installation::adjacent_link_socket_path(&options.paths.socket_path);
        #[cfg(unix)]
        let link_listener = match crate::transport::bind_unix_listener(&link_path) {
            Ok(listener) => listener,
            Err(error) => {
                if let Some(ownership) = socket_ownership {
                    ownership.remove_if_owned();
                }
                return Err(error);
            }
        };
        #[cfg(unix)]
        let link_socket_ownership = match SocketOwnership::capture(link_path.clone()) {
            Ok(ownership) => Some(ownership),
            Err(error) => {
                if let Some(ownership) = socket_ownership {
                    ownership.remove_if_owned();
                }
                let _ = std::fs::remove_file(link_path);
                return Err(error);
            }
        };

        let mut bound = Self {
            quic_endpoint: None,
            #[cfg(unix)]
            unix_listener: Some(unix_listener),
            #[cfg(unix)]
            link_listener: Some(link_listener),
            #[cfg(unix)]
            socket_ownership,
            #[cfg(unix)]
            link_socket_ownership,
        };
        if options.config.lan.listen {
            let port = options.config.lan.port;
            bound.quic_endpoint = Some(quinn::Endpoint::server(
                quic_server_config,
                SocketAddr::from(([0, 0, 0, 0], port)),
            )?);
        }
        Ok(bound)
    }

    #[cfg(unix)]
    fn disarm_socket_cleanup(&mut self) -> (Option<SocketOwnership>, Option<SocketOwnership>) {
        (
            self.socket_ownership.take(),
            self.link_socket_ownership.take(),
        )
    }
}

#[cfg(unix)]
impl Drop for BoundListeners {
    fn drop(&mut self) {
        if let Some(ownership) = self.socket_ownership.take() {
            ownership.remove_if_owned();
        }
        if let Some(ownership) = self.link_socket_ownership.take() {
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
    use tokio::net::TcpListener;

    use super::*;

    fn connected() -> Observed {
        Observed::Connected {
            tier: crate::Tier::Pro,
            carrier: crate::profile::status::RelayCarrier::Tcp,
        }
    }

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
                lan: LanConfig::default(),
                cloud_refresh_interval: None,
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
            discovery: Arc::new(crate::discovery::ScriptedDiscovery::new()),
            dial: DirectDialPolicy::Never,
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

    #[tokio::test]
    async fn profile_reports_legacy_direct_tcp_trust_file_by_name() {
        let root = tempdir().unwrap();
        let runtime_options = options(root.path(), Listeners::InProcessOnly);
        std::fs::create_dir_all(&runtime_options.paths.data_dir).unwrap();
        let peer = crate::HostId::from_u128(2);
        let json = format!(
            r#"{{
  "{peer}": {{
    "pubkey": [{}],
    "name": "old peer",
    "paired_at": "1970-01-01T00:00:00Z",
    "reachabilities": [{{ "type": "direct_tcp", "addr": "127.0.0.1:9000" }}]
  }}
}}"#,
            std::iter::repeat_n("7", 32).collect::<Vec<_>>().join(", ")
        );
        crate::identity::create_private_file(
            &runtime_options.paths.data_dir.join("trust.json"),
            json.as_bytes(),
        )
        .unwrap();

        let error = match start(runtime_options).await {
            Ok(_) => panic!("legacy direct_tcp trust unexpectedly loaded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("trust.json"), "{error}");
    }

    #[tokio::test]
    async fn profile_runtime_advertises_interface_addresses_not_the_unspecified_listener() {
        let root = tempdir().unwrap();
        let discovery = Arc::new(crate::discovery::ScriptedDiscovery::new());
        let mut browser = discovery.browse();
        let mut runtime_options = options(root.path(), Listeners::Sockets);
        runtime_options.discovery = discovery;

        let runtime = start(runtime_options).await.unwrap();
        let advert = match browser.recv().await.unwrap() {
            crate::discovery::DiscoveryEvent::Found(advert) => advert,
            crate::discovery::DiscoveryEvent::Lost { host_id } => {
                panic!("listener unexpectedly withdrew {host_id}")
            }
        };
        let port = runtime.state.read().await.config.lan.port;

        assert!(!advert.addrs.is_empty());
        assert!(advert.addrs.iter().all(|addr| addr.port() == port));
        assert!(advert.addrs.iter().all(|addr| !addr.ip().is_unspecified()));

        runtime.stop(ShutdownReason::UserRequested).await;
    }

    struct StaticCredentials;

    #[async_trait::async_trait]
    impl CredentialProvider for StaticCredentials {
        async fn access_token(&self) -> Result<crate::auth::AccessToken, crate::auth::AuthError> {
            Ok(crate::auth::AccessToken {
                bearer: "test-token".into(),
                expires_at: None,
                tier: None,
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
            ("403 Forbidden", Observed::AuthenticationRequired),
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
        use crate::routing::{
            AuthenticatedLinkUser, LinkConnectorAuth, LinkConnectorToken,
            LinkConnectorTokenRefresher, LinkTokenAuthenticator,
        };

        struct StaticRelayToken;
        #[tonic::async_trait]
        impl LinkConnectorTokenRefresher for StaticRelayToken {
            async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
                Ok(LinkConnectorToken {
                    token: "runtime-token".into(),
                    expires_at: std::time::SystemTime::now() + Duration::from_secs(3600),
                    tier: crate::Tier::Pro,
                })
            }
        }
        use crate::services::CloudLinkServer;

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
                    tier: crate::Tier::Pro,
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
        let relay = CloudLinkServer::with_authenticator(state, auth.clone());
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let relay_addr = listener.local_addr().unwrap();
        let server = relay.serve_on_tcp_listener(listener);
        let root = tempdir().unwrap();
        let mut options = options(root.path(), Listeners::InProcessOnly);
        options.fixtures.cloud = Some((
            relay_addr,
            CloudFixtureAuth::Refreshing(LinkConnectorAuth::new(
                LinkConnectorToken {
                    token: "runtime-token".into(),
                    expires_at: std::time::SystemTime::now() + Duration::from_secs(3600),
                    tier: crate::Tier::Pro,
                },
                Arc::new(StaticRelayToken),
            )),
        ));

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
        wait_for_status(&runtime, connected()).await;
        let client = runtime.client();
        client.list_agents().await.unwrap();
        drop(client);
        tokio::task::yield_now().await;
        assert!(relay.user_has_link_to(auth.user, host_id).await);
        runtime.client().list_agents().await.unwrap();
        assert_eq!(*runtime.status().borrow(), connected());
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
                Observed::AuthenticationRequired,
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
                Observed::AuthenticationRequired,
            ),
            (
                tonic::Status::unavailable("try again"),
                Observed::AuthenticationRequired,
            ),
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
        wait_for_status(&runtime, connected()).await;
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
            let link_socket = crate::installation::adjacent_link_socket_path(&config.socket_path);
            let runtime = start(opts).await.unwrap();
            let channel = client::connect_socket(&config.socket_path).await.unwrap();
            let client = Client::from_channel(channel);
            client.list_agents().await.unwrap();
            UnixStream::connect(&link_socket).unwrap();
            runtime.stop(ShutdownReason::UserRequested).await;
            assert!(client.list_agents().await.is_err());
            assert!(UnixStream::connect(&config.socket_path).is_err());
            assert!(!config.socket_path.exists());
            assert!(UnixStream::connect(&link_socket).is_err());
            assert!(!link_socket.exists());
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
        let occupied = std::net::UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        let mut options = options(root.path(), Listeners::Sockets);
        options.config.lan.port = occupied.local_addr().unwrap().port();

        let status = RuntimeStatus::new(None);
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
