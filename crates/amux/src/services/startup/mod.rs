//! Startup wiring for one user's runtime services.

mod cloud;

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

#[cfg(testnet)]
pub(crate) use cloud::TestCloudTransport;
pub(crate) use cloud::{
    CloudLink, CloudTransport, FREE_TIER_REFRESH_INTERVAL, UDP_BLOCKED_MEMORY, UdpBlockedMemory,
    establish_cloud_link,
};
use futures_util::{Stream, StreamExt, stream};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, Semaphore, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::HostId;
use crate::agents::ArtifactOwners;
use crate::connection::ConnectionManager;
use crate::dispatcher::TunnelDispatcher;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::link::{
    CarrierKind, ChannelPool, MuxCarrier, MuxRole, QuicCarrier, run_link, serve_inbound_streams,
};
use crate::pairing::PairMode;
use crate::protocol::wire;
use crate::resource_limits::{
    EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY, EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::routing::{
    AuthenticatedLinkContextProvider, AuthenticatedLinkUser, HostReachabilityEvent,
    LinkConnectorCtx, LinkCtx, LinkTokenAuthenticator, LiveLocalHost, RoutingCore, local_host,
};
use crate::services::client::{ClientService, PairingTrustAccess};
use crate::services::{
    AgentServiceCtx, LocalAgentHost, LocalPairingIdentity, PairingService,
    ReachabilityLinkConnector,
};
#[cfg(test)]
use crate::transport::PreTrustPairingReachability;
#[cfg(test)]
use crate::transport::in_process_transport_pair;
#[cfg(any(test, test_fixtures))]
use crate::transport::tcp_incoming;
#[cfg(unix)]
use crate::transport::unix_incoming;
use crate::transport::{
    BoxedGrpcIo, InProcessConnection, in_process_channel, managed_in_process_transport_pair,
};
use crate::trust::{SharedTrustStore, TrustStore};
use crate::user_state::ServerState;

const DEVICE_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const CLOUD_TLS_HANDSHAKE_CONCURRENCY: usize = 128;
type CloudTlsTransport = tokio_rustls::server::TlsStream<TcpStream>;

#[derive(Clone)]
pub(crate) struct JwtCloudLinkAuthenticator {
    state: Arc<RwLock<ServerState>>,
}

impl JwtCloudLinkAuthenticator {
    pub(crate) fn new(state: Arc<RwLock<ServerState>>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl LinkTokenAuthenticator for JwtCloudLinkAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        let (validator, host_name, tcp_port) = {
            let state = self.state.read().await;
            let validator = state.jwt_validator().ok_or_else(|| {
                tonic::Status::failed_precondition("cloud routing auth is not configured")
            })?;
            let tcp_port = state.tcp_port().ok_or_else(|| {
                tonic::Status::failed_precondition("cloud routing tcp_port is not configured")
            })?;
            (validator, state.host_name().to_string(), tcp_port)
        };

        let claims = validator
            .validate(token, &host_name, tcp_port)
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "routing token validation failed");
                tonic::Status::unauthenticated("invalid routing authorization")
            })?;
        let user_id = claims.sub.parse::<Uuid>().map_err(|_| {
            tracing::warn!(sub = %claims.sub, "routing token has invalid user id");
            tonic::Status::unauthenticated("invalid routing authorization")
        })?;
        let expires_at = UNIX_EPOCH
            .checked_add(Duration::from_secs(claims.exp))
            .ok_or_else(|| tonic::Status::unauthenticated("invalid routing authorization"))?;
        Ok(AuthenticatedLinkUser {
            user_id,
            client_id: claims.client_id,
            expires_at,
            tier: claims.tier,
        })
    }
}

#[derive(Clone)]
pub(crate) struct CloudLinkServer {
    inner: Arc<CloudLinkServerInner>,
}

struct CloudLinkServerInner {
    state: Arc<RwLock<ServerState>>,
    authenticator: Arc<dyn LinkTokenAuthenticator>,
    users: RwLock<HashMap<Uuid, StartedRoutingServices>>,
}

impl CloudLinkServer {
    pub(crate) fn new(state: Arc<RwLock<ServerState>>) -> Self {
        Self::with_authenticator(
            state.clone(),
            Arc::new(JwtCloudLinkAuthenticator::new(state)),
        )
    }

    pub(crate) fn with_authenticator(
        state: Arc<RwLock<ServerState>>,
        authenticator: Arc<dyn LinkTokenAuthenticator>,
    ) -> Self {
        Self {
            inner: Arc::new(CloudLinkServerInner {
                state,
                authenticator,
                users: RwLock::new(HashMap::new()),
            }),
        }
    }

    #[cfg(any(test, test_fixtures))]
    pub(crate) fn serve_on_tcp_listener(&self, listener: TcpListener) -> JoinHandle<()> {
        spawn_cloud_carrier_server(self.clone(), tcp_incoming(listener))
    }

    /// Serves the relay on an arbitrary accepted-transport stream. Used by
    /// the testnet harness to keep kill-switch handles on accepted sockets.
    #[cfg(testnet)]
    pub(crate) fn serve_on_incoming<I, IO>(&self, incoming: I) -> JoinHandle<()>
    where
        I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        spawn_cloud_carrier_server(self.clone(), incoming)
    }

    pub(crate) fn serve_on_tls_tcp_listener(
        &self,
        listener: TcpListener,
        acceptor: TlsAcceptor,
        handshake_timeout: Duration,
    ) -> JoinHandle<()> {
        let incoming = cloud_tls_incoming(listener, acceptor, handshake_timeout);
        spawn_cloud_carrier_server(self.clone(), incoming)
    }

    pub(crate) fn serve_on_quic_endpoint(
        &self,
        endpoint: quinn::Endpoint,
        handshake_timeout: Duration,
    ) -> JoinHandle<()> {
        self.serve_on_quic_endpoint_with_admission(
            endpoint,
            handshake_timeout,
            Arc::new(Semaphore::new(EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY)),
        )
    }

    fn serve_on_quic_endpoint_with_admission(
        &self,
        endpoint: quinn::Endpoint,
        handshake_timeout: Duration,
        slots: Arc<Semaphore>,
    ) -> JoinHandle<()> {
        let service = self.clone();
        let limiter = Arc::new(tokio::sync::Mutex::new(SlidingWindowRateLimiter::new(
            EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
            EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW,
        )));
        tokio::spawn(async move {
            let mut connection_tasks = tokio::task::JoinSet::new();
            loop {
                let incoming = tokio::select! {
                    incoming = endpoint.accept() => incoming,
                    completed = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                        if let Some(Err(error)) = completed
                            && !error.is_cancelled()
                        {
                            tracing::warn!(error = %error, "cloud QUIC connection task failed");
                        }
                        continue;
                    }
                };
                let Some(incoming) = incoming else { break };
                let addr = incoming.remote_address();

                if !incoming.remote_address_validated() {
                    if !limiter.lock().await.allow(addr.ip()) {
                        tracing::warn!(peer = %addr, "cloud QUIC handshake rate limit exceeded");
                        incoming.ignore();
                        continue;
                    }
                    if let Err(error) = incoming.retry() {
                        tracing::debug!(peer = %addr, error = %error, "cloud QUIC address was already validated");
                        error.into_incoming().ignore();
                    }
                    continue;
                }
                let permit = match slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(peer = %addr, "cloud QUIC handshake concurrency limit exceeded");
                        incoming.refuse();
                        continue;
                    }
                };
                let service = service.clone();
                connection_tasks.spawn(async move {
                    let _permit = permit;
                    let connection = match tokio::time::timeout(handshake_timeout, incoming).await {
                        Ok(Ok(connection)) => connection,
                        Ok(Err(error)) => {
                            tracing::warn!(peer = %addr, error = %error, "cloud QUIC handshake failed");
                            return;
                        }
                        Err(_) => {
                            tracing::warn!(peer = %addr, "cloud QUIC handshake timed out");
                            return;
                        }
                    };
                    drop(_permit);
                    let ctx = service.accepting_link_ctx().await;
                    let carrier = Arc::new(QuicCarrier::from_accepted_with_kind(
                        connection,
                        CarrierKind::RelayQuic,
                    ));
                    if let Err(error) =
                        run_link(ctx, carrier, crate::routing::ConnectRole::Acceptor).await
                    {
                        tracing::warn!(peer = %addr, error = %error, "cloud QUIC link exited with error");
                    }
                });
            }
            connection_tasks.detach_all();
        })
    }

    async fn link_ctx_for_user(&self, user_id: Uuid) -> LinkCtx {
        if let Some(ctx) = self
            .inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(StartedRoutingServices::link_ctx)
        {
            return ctx;
        }

        let started = start_routing_services(self.inner.state.clone()).await;

        let mut users = self.inner.users.write().await;
        users.entry(user_id).or_insert(started).link_ctx()
    }

    async fn accepting_link_ctx(&self) -> LinkCtx {
        let host = {
            let state = self.inner.state.read().await;
            local_host(
                state.host_id(),
                state.host_name(),
                state.is_cloud_server(),
                state.credentials.is_some(),
            )
        };
        LinkCtx::new(
            host,
            Arc::new(RoutingCore::new()),
            Arc::new(crate::routing::LinkRegistry::default()),
        )
        .with_token_authenticator(self.inner.authenticator.clone(), None)
        .with_authenticated_context_provider(Arc::new(self.clone()))
    }

    /// Testnet observation seam: the relay-side `ConnectionManager` serving
    /// `user_id`, if that user has attached. Lets spec tests assert what the
    /// relay can (not) do with the traffic it forwards.
    #[cfg(testnet)]
    pub(crate) async fn user_routing_connections(
        &self,
        user_id: Uuid,
    ) -> Option<Arc<crate::connection::ConnectionManager>> {
        self.inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(|services| services.connections.clone())
    }

    #[cfg(testnet)]
    pub(crate) async fn user_has_link_to(&self, user_id: Uuid, host_id: HostId) -> bool {
        let tunnels = self
            .inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(|services| services.channels.clone());
        match tunnels {
            Some(tunnels) => tunnels
                .link_registry()
                .link_to_peer(host_id)
                .await
                .is_some(),
            None => false,
        }
    }

    pub(crate) async fn send_link_close_to_all(&self, reason: wire::pb::LinkCloseReason) {
        let tunnels = {
            let users = self.inner.users.read().await;
            users
                .values()
                .map(|services| services.channels.clone())
                .collect::<Vec<_>>()
        };
        for tunnels in tunnels {
            tunnels.link_registry().send_link_close_to_all(reason).await;
        }
    }
}

#[tonic::async_trait]
impl AuthenticatedLinkContextProvider for CloudLinkServer {
    async fn context_for(&self, user: &AuthenticatedLinkUser) -> (LinkCtx, Option<String>) {
        let minimum_client_version = self
            .inner
            .state
            .read()
            .await
            .minimum_client_version(&user.client_id);
        let ctx = self
            .link_ctx_for_user(user.user_id)
            .await
            .with_link_role(crate::routing::LinkRole::CloudRelay);
        (ctx, minimum_client_version)
    }
}

fn cloud_tls_incoming(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    handshake_timeout: Duration,
) -> impl Stream<Item = Result<CloudTlsTransport, std::io::Error>> + Send + 'static {
    let (tx, rx) = mpsc::channel(CLOUD_TLS_HANDSHAKE_CONCURRENCY);
    let slots = Arc::new(Semaphore::new(CLOUD_TLS_HANDSHAKE_CONCURRENCY));

    tokio::spawn(async move {
        loop {
            let permit = tokio::select! {
                permit = slots.clone().acquire_owned() => {
                    match permit {
                        Ok(permit) => permit,
                        Err(_) => break,
                    }
                }
                _ = tx.closed() => break,
            };

            let (stream, addr) = tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            if tx.send(Err(error)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                    }
                }
                _ = tx.closed() => break,
            };

            if let Err(error) = stream.set_nodelay(true) {
                tracing::warn!(error = %error, "failed to set TCP_NODELAY");
            }
            crate::transport::configure_relay_tcp_keepalive(&stream);
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(tls_stream)) => {
                        let _ = tx.send(Ok(tls_stream)).await;
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(peer = %addr, error = %error, "TLS handshake failed");
                    }
                    Err(_) => {
                        tracing::warn!(peer = %addr, "TLS handshake timed out");
                    }
                }
            });
        }
    });

    stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

pub(crate) struct StartedRoutingServices {
    pub(crate) routing: Arc<RoutingCore>,
    pub(crate) channels: Arc<ChannelPool>,
    pub(crate) connections: Arc<ConnectionManager>,
    local_host: LiveLocalHost,
    incoming_streams_tx: mpsc::Sender<(HostId, crate::link::ByteStream)>,
    tasks: Vec<JoinHandle<()>>,
}

struct StartedRoutingParts {
    runtime: StartedRoutingServices,
    incoming_streams_rx: mpsc::Receiver<(HostId, crate::link::ByteStream)>,
}

async fn start_routing_services_parts(
    state: Arc<RwLock<ServerState>>,
    device_security: Option<DeviceRuntimeSecurity>,
) -> StartedRoutingParts {
    let (host_id, host_name, is_cloud_server, signed_in) = {
        let state = state.read().await;
        (
            state.host_id(),
            state.host_name().to_string(),
            state.is_cloud_server(),
            state.credentials.is_some(),
        )
    };
    let host = LiveLocalHost::new(local_host(host_id, &host_name, is_cloud_server, signed_in));

    let routing = Arc::new(match device_security.as_ref() {
        Some(security) => RoutingCore::with_persisted_trust_store(
            security.trust_store.clone(),
            security.data_dir.clone(),
        ),
        None => RoutingCore::new(),
    });
    let links = Arc::new(crate::routing::LinkRegistry::default());
    let (incoming_streams_tx, incoming_streams_rx) = mpsc::channel(64);
    let channels = Arc::new(match device_security {
        Some(security) => ChannelPool::with_device_tls(
            links,
            security.identity.clone(),
            security.trust_store.clone(),
        ),
        None => ChannelPool::new(links),
    });
    let connections = Arc::new(ConnectionManager::new(routing.clone(), channels.clone()));

    let tasks = vec![connections.clone().attach_routing_events().await];

    StartedRoutingParts {
        runtime: StartedRoutingServices {
            routing,
            channels,
            connections,
            local_host: host,
            incoming_streams_tx,
            tasks,
        },
        incoming_streams_rx,
    }
}

pub(crate) async fn start_routing_services(
    state: Arc<RwLock<ServerState>>,
) -> StartedRoutingServices {
    let mut parts = start_routing_services_parts(state, None).await;
    let mut incoming_streams_rx = parts.incoming_streams_rx;
    parts.runtime.tasks.push(tokio::spawn(async move {
        while incoming_streams_rx.recv().await.is_some() {}
    }));
    parts.runtime
}

pub(crate) struct StartedUserServices {
    runtime: StartedRoutingServices,
    pub(crate) artifact_owners: Arc<ArtifactOwners>,
    #[cfg(any(test, testnet))]
    pub(crate) agent: AgentServiceCtx,
    pub(crate) client: ClientService,
    trusted_incoming_tx: mpsc::Sender<BoxedGrpcIo>,
    #[cfg(test)]
    pairing_incoming_tx: mpsc::Sender<BoxedGrpcIo>,
    #[cfg(any(test, testnet))]
    pub(crate) pair_mode: Arc<PairMode>,
    reachability_links: ReachabilityLinkConnector,
    dispatcher: TunnelDispatcher,
    external_accept_task: Option<JoinHandle<()>>,
    external_links_shutdown: watch::Sender<bool>,
    connections_closed: tokio_util::sync::CancellationToken,
}

#[derive(Clone)]
pub(crate) struct DeviceRuntimeSecurity {
    identity: DeviceIdentity,
    trust_store: SharedTrustStore,
    data_dir: PathBuf,
    operations: Arc<crate::installation::OperationGate>,
}

impl DeviceRuntimeSecurity {
    pub(crate) fn new(
        identity: DeviceIdentity,
        trust_store: TrustStore,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            identity,
            trust_store: Arc::new(std::sync::RwLock::new(trust_store)),
            data_dir,
            operations: Arc::default(),
        }
    }

    pub(crate) fn with_operations(
        mut self,
        operations: Arc<crate::installation::OperationGate>,
    ) -> Self {
        self.operations = operations;
        self
    }

    pub(crate) fn host_id(&self) -> HostId {
        self.identity.host_id
    }

    pub(crate) fn shared_trust_store(&self) -> SharedTrustStore {
        self.trust_store.clone()
    }

    pub(crate) fn quic_server_config(&self) -> Result<quinn::ServerConfig, IdentityError> {
        self.identity.quic_server_config(self.trust_store.clone())
    }
}

pub(crate) async fn start_user_services(
    state: Arc<RwLock<ServerState>>,
    agent_host: Option<Arc<dyn LocalAgentHost>>,
    device_security: DeviceRuntimeSecurity,
) -> Result<StartedUserServices, IdentityError> {
    start_user_services_with_clock(
        state,
        agent_host,
        device_security,
        Arc::new(amux_artifacts::SystemClock),
    )
    .await
}

#[cfg(testnet)]
pub(crate) async fn start_user_services_with_artifact_clock(
    state: Arc<RwLock<ServerState>>,
    agent_host: Option<Arc<dyn LocalAgentHost>>,
    device_security: DeviceRuntimeSecurity,
    clock: Arc<dyn amux_artifacts::Clock>,
) -> Result<StartedUserServices, IdentityError> {
    start_user_services_with_clock(state, agent_host, device_security, clock).await
}

async fn start_user_services_with_clock(
    state: Arc<RwLock<ServerState>>,
    agent_host: Option<Arc<dyn LocalAgentHost>>,
    device_security: DeviceRuntimeSecurity,
    artifact_clock: Arc<dyn amux_artifacts::Clock>,
) -> Result<StartedUserServices, IdentityError> {
    // ClientService and the debug serializer must observe the same concrete
    // local runtime. Production startup already seeded this slot lazily;
    // explicit hosts supplied by testnet and other embedders need it too.
    state.write().await.local_agent_host = agent_host.clone();
    let mut parts =
        start_routing_services_parts(state.clone(), Some(device_security.clone())).await;
    let host_id = parts.runtime.local_host.id();
    let is_cloud_server = {
        let state = state.read().await;
        state.is_cloud_server()
    };

    let artifact_owners = Arc::new(
        ArtifactOwners::open(device_security.data_dir.clone(), artifact_clock)
            .map_err(|error| IdentityError::Io(std::io::Error::other(error)))?,
    );
    let agent = AgentServiceCtx::new(agent_host.clone(), host_id, is_cloud_server)
        .with_artifact_owners(artifact_owners.clone())
        .with_operations(device_security.operations.clone());
    let trust_commit_lock = device_security.operations.clone();
    let pair_mode = Arc::new(PairMode::new());
    let reachability_links = ReachabilityLinkConnector::new(
        device_security.identity.clone(),
        device_security.trust_store.clone(),
        parts.runtime.local_host.clone(),
        parts.runtime.routing.clone(),
        parts.runtime.channels.clone(),
        parts.runtime.connections.clone(),
        parts.runtime.incoming_streams_tx.clone(),
    );
    let client = ClientService::new(
        agent.clone(),
        state,
        parts.runtime.connections.clone(),
        PairingTrustAccess::new(
            device_security.identity.public_key().to_vec(),
            device_security.trust_store.clone(),
            trust_commit_lock.clone(),
            device_security.data_dir.clone(),
        ),
        pair_mode.clone(),
        reachability_links.clone(),
    );
    client
        .apply_host_event(HostReachabilityEvent::Added {
            host: parts.runtime.local_host.snapshot(),
        })
        .await;

    let (trusted_incoming_tx, trusted_incoming_rx) = mpsc::channel(64);
    let (pairing_incoming_tx, pairing_incoming_rx) = mpsc::channel(64);

    let connections_closed = tokio_util::sync::CancellationToken::new();
    let (external_links_shutdown, _) = watch::channel(false);
    parts.runtime.tasks.push(spawn_trusted_service_server(
        client.clone(),
        agent.clone(),
        trusted_incoming_rx,
        connections_closed.clone(),
    ));
    parts.runtime.tasks.push(spawn_pairing_service_server(
        PairingService::new(
            pair_mode.clone(),
            LocalPairingIdentity::from_device_identity(&device_security.identity),
            parts.runtime.local_host.snapshot().name,
            device_security.trust_store.clone(),
            trust_commit_lock,
            parts.runtime.connections.clone(),
            device_security.data_dir.clone(),
        ),
        pairing_incoming_rx,
        connections_closed.clone(),
    ));
    let dispatcher = TunnelDispatcher::new(
        &device_security.identity,
        device_security.trust_store.clone(),
        pair_mode.clone(),
        parts.runtime.connections.trusted_connections(),
        trusted_incoming_tx.clone(),
        pairing_incoming_tx.clone(),
        DEVICE_TLS_HANDSHAKE_TIMEOUT,
    )?
    .with_link_ctx(parts.runtime.link_ctx());
    parts.runtime.tasks.push(serve_inbound_streams(
        Arc::new(dispatcher.clone()),
        parts.incoming_streams_rx,
    ));
    parts.runtime.tasks.push(
        client
            .attach_routing_events(parts.runtime.routing.clone())
            .await,
    );
    if !parts
        .runtime
        .local_host
        .snapshot()
        .capabilities
        .supported_agent_types
        .is_empty()
    {
        match client.attach_local_agent_events(agent.clone()).await {
            Ok(task) => parts.runtime.tasks.push(task),
            Err(error) => {
                tracing::warn!(error = %error, "failed to attach local agent events");
            }
        }
        match client.attach_local_agent_messages(agent.clone()).await {
            Ok(task) => parts.runtime.tasks.push(task),
            Err(error) => {
                tracing::warn!(error = %error, "failed to attach local agent messages");
            }
        }
    }

    Ok(StartedUserServices {
        connections_closed,
        runtime: parts.runtime,
        artifact_owners,
        #[cfg(any(test, testnet))]
        agent,
        client,
        trusted_incoming_tx,
        #[cfg(test)]
        pairing_incoming_tx,
        #[cfg(any(test, testnet))]
        pair_mode,
        reachability_links,
        dispatcher,
        external_accept_task: None,
        external_links_shutdown,
    })
}

impl Deref for StartedUserServices {
    type Target = StartedRoutingServices;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl DerefMut for StartedUserServices {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.runtime
    }
}

impl Drop for StartedUserServices {
    fn drop(&mut self) {
        self.external_links_shutdown.send_replace(true);
    }
}

impl StartedUserServices {
    pub(crate) fn quic_endpoint(&self) -> quinn::Endpoint {
        self.reachability_links
            .quic_endpoint()
            .expect("profile QUIC endpoint is configured")
    }

    pub(crate) fn configure_reachability(
        &self,
        data_dir: PathBuf,
        discovery: Arc<dyn crate::discovery::Discovery>,
        found_hosts: Arc<crate::discovery::FoundHosts>,
        quic_endpoint: quinn::Endpoint,
    ) {
        self.reachability_links
            .configure(data_dir, discovery, found_hosts, quic_endpoint);
    }

    #[cfg(testnet)]
    pub(crate) fn set_test_quic_transport(&self, transport: Option<Arc<quinn::TransportConfig>>) {
        self.reachability_links.set_test_quic_transport(transport);
    }

    pub(crate) fn spawn_dial_on_found(
        &self,
        events: tokio::sync::broadcast::Receiver<crate::discovery::DiscoveryEvent>,
    ) -> JoinHandle<()> {
        self.reachability_links.spawn_dial_on_found(events)
    }

    pub(crate) async fn close_direct_links(&self) {
        self.reachability_links.close_direct_links().await;
    }

    pub(crate) fn resume_direct_links(&self) -> Vec<JoinHandle<()>> {
        self.reachability_links.resume_direct_links()
    }

    #[cfg(test)]
    pub(crate) fn open_in_process_client_channel(&self) -> (Channel, JoinHandle<()>) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let trusted_tx = self.trusted_incoming_tx.clone();
        let task = tokio::spawn(async move {
            if trusted_tx
                .send(BoxedGrpcIo::local_trusted(server_transport))
                .await
                .is_err()
            {
                tracing::warn!("trusted server closed before in-process stream was accepted");
            }
        });
        (in_process_channel(client_transport), task)
    }

    pub(crate) fn open_managed_in_process_client_channel(
        &self,
    ) -> (Channel, JoinHandle<()>, InProcessConnection) {
        let (client_transport, server_transport, connection) = managed_in_process_transport_pair();
        let trusted_tx = self.trusted_incoming_tx.clone();
        let task = tokio::spawn(async move {
            if trusted_tx
                .send(BoxedGrpcIo::local_trusted(server_transport))
                .await
                .is_err()
            {
                tracing::warn!("trusted server closed before in-process stream was accepted");
            }
        });
        (in_process_channel(client_transport), task, connection)
    }

    #[cfg(unix)]
    pub(crate) fn serve_client_service_on_unix_listener(
        &self,
        listener: tokio::net::UnixListener,
    ) -> JoinHandle<()> {
        let incoming = unix_incoming(listener);
        let trusted_tx = self.trusted_incoming_tx.clone();
        spawn_forward_to_trusted(incoming, trusted_tx, "Unix socket")
    }

    #[cfg(unix)]
    pub(crate) fn serve_link_on_unix_listener(
        &self,
        listener: tokio::net::UnixListener,
    ) -> JoinHandle<()> {
        let ctx = self
            .link_ctx()
            .with_carrier(crate::routing::LinkCarrier::Ssh)
            .with_shutdown(self.external_links_shutdown.subscribe());
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let carrier =
                            Arc::new(MuxCarrier::new(stream, MuxRole::Acceptor, CarrierKind::Ssh));
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            if let Err(error) =
                                run_link(ctx, carrier, crate::routing::ConnectRole::Acceptor).await
                            {
                                tracing::warn!(error = %error, "local SSH link exited with error");
                            }
                        });
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "local SSH link accept failed");
                        break;
                    }
                }
            }
        })
    }

    pub(crate) fn serve_external_quic_endpoint(&mut self, endpoint: quinn::Endpoint) {
        let task = self
            .dispatcher
            .serve_quic_endpoint(endpoint, self.external_links_shutdown.subscribe());
        self.external_accept_task = Some(task);
    }

    pub(crate) fn spawn_reachability_links(&self) -> Vec<JoinHandle<()>> {
        self.reachability_links.spawn_startup_links()
    }

    pub(crate) fn push_task(&mut self, task: JoinHandle<()>) {
        self.tasks.push(task);
    }

    pub(crate) async fn stop_accepting_external_links(&mut self) {
        self.external_links_shutdown.send_replace(true);
        if let Some(task) = self.external_accept_task.take() {
            let _ = task.await;
        }
    }

    pub(crate) async fn stop_tasks(&mut self) {
        self.stop_accepting_external_links().await;
        self.connections_closed.cancel();
        let tasks = std::mem::take(&mut self.tasks);
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    #[cfg(testnet)]
    pub(crate) fn reachability_link_connector(&self) -> &ReachabilityLinkConnector {
        &self.reachability_links
    }

    #[cfg(testnet)]
    pub(crate) fn rebind_direct_quic(&self, socket: std::net::UdpSocket) -> std::io::Result<()> {
        self.reachability_links.rebind_quic(socket)
    }
}

impl StartedRoutingServices {
    pub(crate) fn set_signed_in(&self, signed_in: bool) {
        self.local_host.set_signed_in(signed_in);
    }

    pub(crate) fn link_connector_ctx(&self) -> LinkConnectorCtx {
        LinkConnectorCtx::new_live(
            self.local_host.clone(),
            self.routing.clone(),
            self.channels.link_registry(),
        )
        .with_incoming_streams(self.incoming_streams_tx.clone())
    }

    pub(crate) fn link_connector_ctx_with_signed_in(&self, signed_in: bool) -> LinkConnectorCtx {
        let mut host = self.local_host.snapshot();
        host.signed_in = Some(signed_in);
        LinkConnectorCtx::new(host, self.routing.clone(), self.channels.link_registry())
            .with_incoming_streams(self.incoming_streams_tx.clone())
    }

    pub(crate) fn link_ctx(&self) -> LinkCtx {
        LinkCtx::new_live(
            self.local_host.clone(),
            self.routing.clone(),
            self.channels.link_registry(),
        )
        .with_incoming_streams(self.incoming_streams_tx.clone())
    }
}

impl Drop for StartedRoutingServices {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn spawn_trusted_service_server(
    client: ClientService,
    agent: AgentServiceCtx,
    incoming_rx: mpsc::Receiver<BoxedGrpcIo>,
    connections_closed: tokio_util::sync::CancellationToken,
) -> JoinHandle<()> {
    let incoming = stream::unfold(
        (incoming_rx, connections_closed),
        |(mut rx, closed)| async move {
            rx.recv().await.map(|transport| {
                let transport = crate::transport::ShutdownIo::new(transport, closed.clone());
                (Ok::<_, std::io::Error>(transport), (rx, closed))
            })
        },
    );

    tokio::spawn(async move {
        if let Err(error) = crate::transport::tonic_server_builder()
            .add_service(wire::client_service_server(client))
            .add_service(wire::agent_service_server(agent))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::warn!(error = %error, "Trusted Server exited with error");
        }
    })
}

fn spawn_pairing_service_server(
    pairing: PairingService,
    incoming_rx: mpsc::Receiver<BoxedGrpcIo>,
    connections_closed: tokio_util::sync::CancellationToken,
) -> JoinHandle<()> {
    let incoming = stream::unfold(
        (incoming_rx, connections_closed),
        |(mut rx, closed)| async move {
            rx.recv().await.map(|transport| {
                let transport = crate::transport::ShutdownIo::new(transport, closed.clone());
                (Ok::<_, std::io::Error>(transport), (rx, closed))
            })
        },
    );

    tokio::spawn(async move {
        if let Err(error) = crate::transport::tonic_server_builder()
            .add_service(wire::pairing_service_server::PairingServiceServer::new(
                pairing,
            ))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::warn!(error = %error, "Pairing Server exited with error");
        }
    })
}

fn spawn_forward_to_trusted<I, IO>(
    incoming: I,
    trusted_tx: mpsc::Sender<BoxedGrpcIo>,
    label: &'static str,
) -> JoinHandle<()>
where
    I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut incoming = Box::pin(incoming);
        while let Some(item) = incoming.next().await {
            match item {
                Ok(io) => {
                    if trusted_tx
                        .send(BoxedGrpcIo::local_trusted(io))
                        .await
                        .is_err()
                    {
                        tracing::warn!(source = label, "Trusted Server channel closed");
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(source = label, error = %error, "trusted stream accept failed");
                }
            }
        }
    })
}

fn spawn_cloud_carrier_server<I, IO>(service: CloudLinkServer, incoming: I) -> JoinHandle<()>
where
    I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut incoming = Box::pin(incoming);
        while let Some(item) = incoming.next().await {
            match item {
                Ok(io) => {
                    let service = service.clone();
                    tokio::spawn(async move {
                        let ctx = service.accepting_link_ctx().await;
                        let carrier = Arc::new(MuxCarrier::new(
                            io,
                            MuxRole::Acceptor,
                            CarrierKind::RelayTcp,
                        ));
                        if let Err(error) =
                            run_link(ctx, carrier, crate::routing::ConnectRole::Acceptor).await
                        {
                            tracing::warn!(error = %error, "cloud link exited with error");
                        }
                    });
                }
                Err(error) => tracing::warn!(error = %error, "cloud stream accept failed"),
            }
        }
    })
}

#[cfg(all(test, feature = "local-agents"))]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use std::{fmt, io};

    use futures_util::StreamExt;
    use hyper_util::rt::TokioIo;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{
        WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
    };
    use rustls::pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
    };
    use rustls::{
        ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme, version,
    };
    use tonic::codegen::http::Uri;
    use tonic::transport::{Channel, Endpoint};
    use tower::service_fn;

    use super::*;
    use crate::agents::{
        CreateAgentConfig, CreateAgentRpcRequest, TEST_ECHO_COMMAND, TEST_ECHO_V1,
    };
    use crate::config::Config;
    use crate::identity::DeviceIdentity;
    use crate::protocol::ProtocolError;
    use crate::routing::{Capabilities, Host, Route, SupportedAgentType};
    use crate::trust::{Reachability, TrustEntry};
    use crate::{HostId, SessionCloseReason, SubscribeSessionEvent};

    fn test_state(host_id: Uuid) -> Arc<RwLock<ServerState>> {
        let config = Config {
            host_name: "local".to_string(),
            ..Config::default()
        };
        Arc::new(RwLock::new(ServerState::new(config, host_id, None, None)))
    }

    async fn test_started_services() -> StartedUserServices {
        test_started_services_with_host_id(Uuid::from_u128(1)).await
    }

    async fn test_started_services_with_host_id(host_id: Uuid) -> StartedUserServices {
        let identity = DeviceIdentity::for_test(host_id);
        test_started_services_with_identity_and_trust(identity, TrustStore::default()).await
    }

    async fn test_started_services_with_identity_and_trust(
        identity: DeviceIdentity,
        trust_store: TrustStore,
    ) -> StartedUserServices {
        let state = test_state(identity.host_id);
        let agent_host: Option<Arc<dyn LocalAgentHost>> =
            Some(crate::services::PtyAgentHost::new(identity.host_id));
        let data_dir = tempfile::tempdir().unwrap();
        start_user_services(
            state,
            agent_host,
            DeviceRuntimeSecurity::new(identity, trust_store, data_dir.keep()),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn daemon_startup_preloads_every_existing_artifact_owner() {
        let host_id = Uuid::new_v4();
        let data_dir = tempfile::tempdir().unwrap();
        for agent_id in [Uuid::new_v4(), Uuid::new_v4()] {
            let owner = amux_artifacts::Owner::open(
                data_dir
                    .path()
                    .join("agents")
                    .join(agent_id.to_string())
                    .join("artifacts"),
                Arc::new(amux_artifacts::SystemClock),
            )
            .unwrap();
            owner
                .put(
                    amux_artifacts::ArtifactKind::File,
                    "existing.txt",
                    "text/plain",
                    agent_id.as_bytes(),
                )
                .unwrap();
        }
        let identity = DeviceIdentity::for_test(host_id);
        let agent_host: Option<Arc<dyn LocalAgentHost>> =
            Some(crate::services::PtyAgentHost::new(host_id));

        let services = start_user_services(
            test_state(host_id),
            agent_host,
            DeviceRuntimeSecurity::new(
                identity,
                TrustStore::default(),
                data_dir.path().to_path_buf(),
            ),
        )
        .await
        .unwrap();

        assert_eq!(services.artifact_owners.loaded_count(), 2);
    }

    fn trust_store_for(peers: &[&DeviceIdentity]) -> TrustStore {
        let mut trust_store = TrustStore::default();
        for peer in peers {
            trust_store.insert_for_test(peer.host_id, trust_entry(peer, vec![Reachability::Cloud]));
        }
        trust_store
    }

    fn trust_entry(peer: &DeviceIdentity, reachabilities: Vec<Reachability>) -> TrustEntry {
        TrustEntry {
            pubkey: peer.public_key().to_vec(),
            name: format!("peer-{}", peer.host_id),
            paired_at: chrono::Utc::now(),
            reachabilities,
            signed_in: None,
        }
    }

    fn serve_test_quic(
        services: &mut StartedUserServices,
        identity: &DeviceIdentity,
        trust: SharedTrustStore,
    ) -> (quinn::Endpoint, std::net::SocketAddr) {
        let endpoint = quinn::Endpoint::server(
            identity.quic_server_config(trust).unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let addr = endpoint.local_addr().unwrap();
        services.configure_reachability(
            tempfile::tempdir().unwrap().keep(),
            Arc::new(crate::discovery::ScriptedDiscovery::new()),
            Arc::new(crate::discovery::FoundHosts::default()),
            endpoint.clone(),
        );
        services.serve_external_quic_endpoint(endpoint.clone());
        (endpoint, addr)
    }

    fn remote_host(id: u128) -> Host {
        Host {
            id: Uuid::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: vec![SupportedAgentType {
                    agent_type: "test-agent".to_string(),
                }],
            },
            signed_in: Some(true),
        }
    }

    #[derive(Clone)]
    struct StaticCloudLinkAuthenticator {
        token_users: Arc<HashMap<String, AuthenticatedLinkUser>>,
    }

    impl StaticCloudLinkAuthenticator {
        fn new(token: &str, user_id: Uuid) -> Self {
            Self {
                token_users: Arc::new(HashMap::from([(
                    token.to_string(),
                    AuthenticatedLinkUser {
                        user_id,
                        client_id: "test-client".to_string(),
                        expires_at: std::time::SystemTime::now() + Duration::from_secs(3600),
                        tier: crate::Tier::Pro,
                    },
                )])),
            }
        }
    }

    #[tonic::async_trait]
    impl LinkTokenAuthenticator for StaticCloudLinkAuthenticator {
        async fn authenticate_token(
            &self,
            token: &str,
        ) -> Result<AuthenticatedLinkUser, tonic::Status> {
            self.token_users
                .get(token)
                .cloned()
                .ok_or_else(|| tonic::Status::unauthenticated("unknown token"))
        }
    }

    async fn test_cloud_carrier_server(
        host_id: Uuid,
        token: &str,
        user_id: Uuid,
    ) -> CloudLinkServer {
        let state = test_state(host_id);
        state.write().await.is_cloud_server = true;
        CloudLinkServer::with_authenticator(
            state,
            Arc::new(StaticCloudLinkAuthenticator::new(token, user_id)),
        )
    }

    async fn create_test_agent(services: &StartedUserServices, agent_id: Uuid) {
        services
            .agent
            .create(CreateAgentRpcRequest {
                agent_id,
                name: Some("echo".to_string()),
                parent: None,
                initial_prompt: None,
                agent: CreateAgentConfig::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    terminal_size: None,
                },
            })
            .await
            .unwrap();
    }

    async fn cloud_test_carrier(addr: std::net::SocketAddr) -> Arc<MuxCarrier> {
        let stream = TcpStream::connect(addr).await.unwrap();
        Arc::new(MuxCarrier::new(
            stream,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ))
    }

    #[tokio::test]
    async fn cloud_tls_incoming_accepts_new_socket_while_first_handshake_stalls() {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let acceptor_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert.der().as_ref().to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der())),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(acceptor_config));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut incoming = Box::pin(cloud_tls_incoming(
            listener,
            acceptor,
            Duration::from_secs(30),
        ));

        let _slow_client = TcpStream::connect(addr).await.unwrap();
        let fast_client = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            let connector = tokio_rustls::TlsConnector::from(Arc::new(no_verify_client_config()));
            let server_name = ServerName::try_from("localhost").unwrap();
            connector.connect(server_name, stream).await.unwrap()
        });

        let accepted = tokio::time::timeout(Duration::from_secs(1), incoming.next())
            .await
            .expect("second TLS handshake should not wait for the first handshake timeout")
            .expect("incoming stream should yield")
            .expect("second TLS handshake should succeed");
        let fast_client = fast_client.await.unwrap();

        drop(accepted);
        drop(fast_client);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cloud_quic_handshake_timeout_releases_the_slot_after_cap_refusal() {
        const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(500);

        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let server_config = crate::transport::relay_quic_server_config_from_der(
            vec![CertificateDer::from(cert.der().as_ref().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(signing_key.serialize_der())),
        )
        .unwrap();
        let server_endpoint =
            quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();
        let service = CloudLinkServer::new(test_state(Uuid::new_v4()));
        let slots = Arc::new(Semaphore::new(1));
        let server_task = service.serve_on_quic_endpoint_with_admission(
            server_endpoint.clone(),
            HANDSHAKE_TIMEOUT,
            slots.clone(),
        );

        let regular_config = relay_quic_test_client_config(Arc::new(no_server_verification()));
        let (proxy_addr, second_initial_rx, proxy_task) =
            quic_stall_after_retry_proxy(server_addr).await;
        let stalled_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let stalled_config = regular_config.clone();
        let stalled = tokio::spawn(async move {
            stalled_endpoint
                .connect_with(stalled_config, proxy_addr, "localhost")
                .unwrap()
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), second_initial_rx)
            .await
            .expect("stalled client did not answer the relay's Retry")
            .expect("stalled-client proxy dropped its readiness signal");
        tokio::time::timeout(Duration::from_secs(1), async {
            while slots.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stalled client never occupied the handshake slot");
        let slot_occupied_at = tokio::time::Instant::now();

        let capped_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let capped = tokio::time::timeout(
            Duration::from_secs(1),
            capped_endpoint
                .connect_with(regular_config.clone(), server_addr, "localhost")
                .unwrap(),
        )
        .await
        .expect("connection at the handshake cap did not get a prompt refusal");
        assert!(
            capped.is_err(),
            "connection at the handshake cap was admitted"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while slots.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stalled handshake did not release its slot at the timeout");
        assert!(
            slot_occupied_at.elapsed()
                >= HANDSHAKE_TIMEOUT.saturating_sub(Duration::from_millis(50))
        );

        let healthy_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        let healthy = tokio::time::timeout(
            Duration::from_secs(1),
            healthy_endpoint
                .connect_with(regular_config, server_addr, "localhost")
                .unwrap(),
        )
        .await
        .expect("accept loop stopped serving after the handshake cap")
        .expect("healthy connection was refused after the stalled handshake timed out");
        healthy.close(0_u32.into(), b"test complete");

        let stalled = tokio::time::timeout(Duration::from_secs(1), stalled)
            .await
            .expect("stalled client task did not resolve")
            .expect("stalled client task panicked")
            .expect("client side did not receive the relay handshake");
        tokio::time::timeout(Duration::from_secs(1), stalled.closed())
            .await
            .expect("relay kept the stalled connection after its handshake timeout");
        proxy_task.abort();
        server_endpoint.close(0_u32.into(), b"test complete");
        server_task.abort();
        let _ = server_task.await;
    }

    async fn quic_stall_after_retry_proxy(
        server_addr: std::net::SocketAddr,
    ) -> (
        std::net::SocketAddr,
        tokio::sync::oneshot::Receiver<()>,
        JoinHandle<()>,
    ) {
        let socket = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr = socket.local_addr().unwrap();
        let (second_initial_tx, second_initial_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut client_addr = None;
            let mut retry_forwarded = false;
            let mut second_initial_tx = Some(second_initial_tx);
            let mut buffer = vec![0_u8; 65_535];
            loop {
                let (len, source) = socket.recv_from(&mut buffer).await.unwrap();
                if source == server_addr {
                    if let Some(client_addr) = client_addr {
                        socket.send_to(&buffer[..len], client_addr).await.unwrap();
                        retry_forwarded = true;
                    }
                    continue;
                }

                client_addr = Some(source);
                if !retry_forwarded || second_initial_tx.is_some() {
                    socket.send_to(&buffer[..len], server_addr).await.unwrap();
                    if retry_forwarded && let Some(ready) = second_initial_tx.take() {
                        let _ = ready.send(());
                    }
                }
            }
        });
        (addr, second_initial_rx, task)
    }

    fn relay_quic_test_client_config(verifier: Arc<dyn ServerCertVerifier>) -> quinn::ClientConfig {
        let mut tls = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        tls.alpn_protocols = vec![crate::identity::QUIC_ALPN.to_vec()];
        tls.resumption = rustls::client::Resumption::disabled();
        tls.enable_early_data = false;
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        quinn::ClientConfig::new(Arc::new(crypto))
    }

    fn no_server_verification() -> NoServerVerification {
        NoServerVerification {
            supported_algs: rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        }
    }

    fn no_verify_client_config() -> ClientConfig {
        let verifier = Arc::new(no_server_verification());
        let mut config = ClientConfig::builder_with_protocol_versions(&[&version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec()];
        config
    }

    #[derive(Clone)]
    struct NoServerVerification {
        supported_algs: WebPkiSupportedAlgorithms,
    }

    impl fmt::Debug for NoServerVerification {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("NoServerVerification").finish()
        }
    }

    impl ServerCertVerifier for NoServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> std::result::Result<ServerCertVerified, TlsError> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
            verify_tls12_signature(message, cert, dss, &self.supported_algs)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
            verify_tls13_signature(message, cert, dss, &self.supported_algs)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported_algs.supported_schemes()
        }
    }

    fn client_create_request(agent_id: Uuid, name: &str) -> wire::ClientCreateAgentRequest {
        wire::ClientCreateAgentRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            name: Some(name.to_string()),
            host_id: None,
            parent: None,
            initial_prompt: None,
            agent: Some(wire::client_create_agent_request::Agent::TestAgent(
                wire::TestAgentCreateConfig {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: "/tmp".to_string(),
                    initial_terminal_size: None,
                },
            )),
        }
    }

    #[tokio::test]
    async fn started_services_seeds_client_and_attaches_startup_events() {
        let services = test_started_services().await;

        let hosts = services.client.list_hosts().await;
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].id, Uuid::from_u128(1));

        let mut agent_events = services.client.subscribe_agents().await;
        let agent_id = Uuid::from_u128(10);
        create_test_agent(&services, agent_id).await;

        let event = tokio::time::timeout(Duration::from_secs(1), agent_events.recv())
            .await
            .expect("timed out waiting for local agent event")
            .expect("agent event stream closed");
        assert!(matches!(
            event,
            crate::agents::AgentEvent::AgentUp { agent }
                if agent.id == agent_id && agent.host_id == Uuid::from_u128(1)
        ));
        assert_eq!(services.client.list_agents().await.len(), 1);

        let mut host_events = services.client.subscribe_hosts().await;
        services
            .routing
            .apply_claim_up(HostId::from_u128(9), remote_host(2))
            .await;
        tokio::time::timeout(Duration::from_secs(1), host_events.recv())
            .await
            .expect("timed out waiting for remote host event")
            .expect("host event stream closed");
        let hosts = services.client.list_hosts().await;
        assert_eq!(hosts.len(), 2);
        assert!(hosts.iter().any(|host| host.id == Uuid::from_u128(2)));
    }

    #[tokio::test]
    async fn started_services_serves_agent_service_on_trusted_ingress() {
        let services = test_started_services().await;
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        services
            .trusted_incoming_tx
            .send(BoxedGrpcIo::local_trusted(server_io))
            .await
            .unwrap();

        let channel = channel_from_transport(client_io);
        let mut client = wire::agent_service_client(channel);
        let mut stream = client
            .subscribe_agent_events(wire::SubscribeAgentEventsRequest::default())
            .await
            .unwrap()
            .into_inner();

        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            first.event,
            Some(wire::subscribe_agent_events_response::Event::SnapshotComplete(_))
        ));
    }

    #[tokio::test]
    async fn pubkey_replacement_revokes_open_tls_trusted_server_connections() {
        let local = DeviceIdentity::for_test(Uuid::from_u128(1));
        let peer = DeviceIdentity::for_test(Uuid::from_u128(2));
        let services =
            test_started_services_with_identity_and_trust(local, trust_store_for(&[&peer])).await;
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        services
            .trusted_incoming_tx
            .send(
                BoxedGrpcIo::tls_trusted(server_io, peer.host_id)
                    .track_trusted_peer(&services.connections.trusted_connections()),
            )
            .await
            .unwrap();

        let channel = channel_from_transport(client_io);
        let mut client = wire::agent_service_client(channel);
        let mut stream = client
            .subscribe_agent_events(wire::SubscribeAgentEventsRequest::default())
            .await
            .unwrap()
            .into_inner();
        let first = stream.next().await.unwrap().unwrap();
        assert!(matches!(
            first.event,
            Some(wire::subscribe_agent_events_response::Event::SnapshotComplete(_))
        ));

        let connections = services.connections.clone();
        tokio::time::timeout(Duration::from_secs(1), async move {
            connections.teardown_host(peer.host_id).await;
        })
        .await
        .expect("timed out waiting for trusted transport teardown");

        let next = tokio::time::timeout(Duration::from_secs(1), stream.message())
            .await
            .expect("timed out waiting for trusted stream revocation");
        assert!(next.is_err() || next.unwrap().is_none());
    }

    #[tokio::test]
    async fn pairing_service_is_only_on_pairing_ingress() {
        let services = test_started_services().await;

        let (trusted_client_io, trusted_server_io) = tokio::io::duplex(64 * 1024);
        services
            .trusted_incoming_tx
            .send(BoxedGrpcIo::local_trusted(trusted_server_io))
            .await
            .unwrap();
        let trusted_channel = channel_from_transport(trusted_client_io);
        let mut trusted_pairing_client =
            wire::pairing_service_client::PairingServiceClient::new(trusted_channel);
        let trusted_error = trusted_pairing_client
            .pair(futures_util::stream::empty::<wire::pb::PairMessage>())
            .await
            .unwrap_err();
        assert_eq!(trusted_error.code(), tonic::Code::Unimplemented);

        let (pairing_client_io, pairing_server_io) = tokio::io::duplex(64 * 1024);
        services
            .pairing_incoming_tx
            .send(BoxedGrpcIo::pre_trust_pairing(
                pairing_server_io,
                PreTrustPairingReachability::Cloud,
            ))
            .await
            .unwrap();
        let pairing_channel = channel_from_transport(pairing_client_io);
        let mut pairing_client =
            wire::pairing_service_client::PairingServiceClient::new(pairing_channel);
        let pairing_error = pairing_client
            .pair(futures_util::stream::empty::<wire::pb::PairMessage>())
            .await
            .unwrap_err();
        assert_eq!(pairing_error.code(), tonic::Code::FailedPrecondition);

        services
            .pair_mode
            .start_qr_secret_for_duration([1_u8; 32], Duration::from_secs(60))
            .unwrap();
        let (active_client_io, active_server_io) = tokio::io::duplex(64 * 1024);
        services
            .pairing_incoming_tx
            .send(BoxedGrpcIo::pre_trust_pairing(
                active_server_io,
                PreTrustPairingReachability::Cloud,
            ))
            .await
            .unwrap();
        let active_pairing_channel = channel_from_transport(active_client_io);
        let mut active_pairing_client =
            wire::pairing_service_client::PairingServiceClient::new(active_pairing_channel);
        let active_pairing_stream = active_pairing_client
            .pair(futures_util::stream::empty::<wire::pb::PairMessage>())
            .await;
        assert!(
            active_pairing_stream.is_ok(),
            "an active pairing window must admit the Pair stream on pairing ingress"
        );
    }

    #[tokio::test]
    async fn direct_pin_pairing_over_quic_updates_both_trust_stores() {
        let initiator_dir = tempfile::tempdir().unwrap();
        let responder_dir = tempfile::tempdir().unwrap();
        let initiator_identity =
            crate::identity::load_or_create_device_identity_in(initiator_dir.path()).unwrap();
        let responder_identity =
            crate::identity::load_or_create_device_identity_in(responder_dir.path()).unwrap();

        let initiator_security = DeviceRuntimeSecurity::new(
            initiator_identity.clone(),
            TrustStore::default(),
            initiator_dir.path().to_path_buf(),
        );
        let initiator_trust = initiator_security.trust_store.clone();
        let responder_security = DeviceRuntimeSecurity::new(
            responder_identity.clone(),
            TrustStore::default(),
            responder_dir.path().to_path_buf(),
        );
        let responder_trust = responder_security.trust_store.clone();
        let initiator = start_user_services(
            test_state(initiator_identity.host_id),
            Some(crate::services::PtyAgentHost::new(
                initiator_identity.host_id,
            )),
            initiator_security,
        )
        .await
        .unwrap();
        let mut responder = start_user_services(
            test_state(responder_identity.host_id),
            Some(crate::services::PtyAgentHost::new(
                responder_identity.host_id,
            )),
            responder_security,
        )
        .await
        .unwrap();

        responder
            .pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (_responder_endpoint, addr) =
            serve_test_quic(&mut responder, &responder_identity, responder_trust.clone());
        let initiator_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        initiator.configure_reachability(
            initiator_dir.path().to_path_buf(),
            Arc::new(crate::discovery::ScriptedDiscovery::new()),
            Arc::new(crate::discovery::FoundHosts::default()),
            initiator_endpoint,
        );

        let paired_peer = crate::pair_via_pin_direct_quic(
            initiator_dir.path(),
            "initiator",
            addr,
            "123456",
            &crate::installation::ProfileAdmin::for_test(initiator.client.clone()),
        )
        .await
        .unwrap();

        assert_eq!(paired_peer.host_id, responder_identity.host_id);
        {
            let initiator_live = initiator_trust.read().unwrap();
            assert_eq!(
                initiator_live
                    .entry(responder_identity.host_id)
                    .unwrap()
                    .reachabilities,
                vec![Reachability::Direct { addrs: vec![addr] }]
            );
        }
        {
            let responder_live = responder_trust.read().unwrap();
            assert!(
                responder_live
                    .entry(initiator_identity.host_id)
                    .unwrap()
                    .reachabilities
                    .is_empty()
            );
        }

        wait_for_host_entry(&initiator.routing, responder_identity.host_id).await;
    }

    #[tokio::test]
    async fn direct_quic_reachability_establishes_runtime_link_from_trust_store() {
        let identity_a = DeviceIdentity::for_test(Uuid::from_u128(21));
        let identity_b = DeviceIdentity::for_test(Uuid::from_u128(22));
        let mut trust_a = TrustStore::default();
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap();
        trust_a.insert_for_test(
            identity_b.host_id,
            trust_entry(
                &identity_b,
                vec![Reachability::Direct { addrs: vec![addr] }],
            ),
        );
        let mut trust_b = TrustStore::default();
        trust_b.insert_for_test(identity_a.host_id, trust_entry(&identity_a, Vec::new()));

        let host_a =
            test_started_services_with_identity_and_trust(identity_a.clone(), trust_a).await;
        let mut host_b =
            test_started_services_with_identity_and_trust(identity_b.clone(), trust_b).await;
        let endpoint_b = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(
                identity_b
                    .quic_server_config(host_b.reachability_links.trust_store().unwrap())
                    .unwrap(),
            ),
            socket,
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap();
        host_b.configure_reachability(
            tempfile::tempdir().unwrap().keep(),
            Arc::new(crate::discovery::ScriptedDiscovery::new()),
            Arc::new(crate::discovery::FoundHosts::default()),
            endpoint_b.clone(),
        );
        host_b.serve_external_quic_endpoint(endpoint_b);
        host_a.configure_reachability(
            tempfile::tempdir().unwrap().keep(),
            Arc::new(crate::discovery::ScriptedDiscovery::new()),
            Arc::new(crate::discovery::FoundHosts::default()),
            quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap(),
        );

        let tasks = host_a.spawn_reachability_links();
        wait_for_host_entry(&host_a.routing, identity_b.host_id).await;
        wait_for_active_direct_route(&host_a.connections, identity_b.host_id).await;

        let channel = host_a
            .connections
            .channel_to(identity_b.host_id)
            .await
            .unwrap();
        let mut client = wire::client_service_client(channel);
        let response = client
            .list_hosts(wire::ListHostsRequest {})
            .await
            .unwrap()
            .into_inner();
        assert!(
            response
                .hosts
                .iter()
                .any(|host| { host.host_id == identity_b.host_id.as_bytes().to_vec() })
        );

        host_a
            .channels
            .link_registry()
            .close_host(identity_b.host_id)
            .await;
        wait_for_host_entry_removed(&host_a.routing, identity_b.host_id).await;
        assert!(
            host_a
                .connections
                .channel_to(identity_b.host_id)
                .await
                .is_err()
        );
        drop(host_b);

        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn direct_quic_reachabilities_on_both_peers_establish_two_outbound_links() {
        let identity_a = DeviceIdentity::for_test(Uuid::from_u128(23));
        let identity_b = DeviceIdentity::for_test(Uuid::from_u128(24));
        let socket_a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr_a = socket_a.local_addr().unwrap();
        let socket_b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr_b = socket_b.local_addr().unwrap();

        let mut trust_a = TrustStore::default();
        trust_a.insert_for_test(
            identity_b.host_id,
            trust_entry(
                &identity_b,
                vec![Reachability::Direct {
                    addrs: vec![addr_b],
                }],
            ),
        );
        let mut trust_b = TrustStore::default();
        trust_b.insert_for_test(
            identity_a.host_id,
            trust_entry(
                &identity_a,
                vec![Reachability::Direct {
                    addrs: vec![addr_a],
                }],
            ),
        );

        let mut host_a =
            test_started_services_with_identity_and_trust(identity_a.clone(), trust_a).await;
        let mut host_b =
            test_started_services_with_identity_and_trust(identity_b.clone(), trust_b).await;
        let endpoint_a = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(
                identity_a
                    .quic_server_config(host_a.reachability_links.trust_store().unwrap())
                    .unwrap(),
            ),
            socket_a,
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap();
        let endpoint_b = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(
                identity_b
                    .quic_server_config(host_b.reachability_links.trust_store().unwrap())
                    .unwrap(),
            ),
            socket_b,
            Arc::new(quinn::TokioRuntime),
        )
        .unwrap();
        host_a.configure_reachability(
            tempfile::tempdir().unwrap().keep(),
            Arc::new(crate::discovery::ScriptedDiscovery::new()),
            Arc::new(crate::discovery::FoundHosts::default()),
            endpoint_a.clone(),
        );
        host_b.configure_reachability(
            tempfile::tempdir().unwrap().keep(),
            Arc::new(crate::discovery::ScriptedDiscovery::new()),
            Arc::new(crate::discovery::FoundHosts::default()),
            endpoint_b.clone(),
        );
        host_a.serve_external_quic_endpoint(endpoint_a);
        host_b.serve_external_quic_endpoint(endpoint_b);

        let tasks_a = host_a.spawn_reachability_links();
        let tasks_b = host_b.spawn_reachability_links();
        wait_for_host_entry(&host_a.routing, identity_b.host_id).await;
        wait_for_host_entry(&host_b.routing, identity_a.host_id).await;

        wait_for_active_direct_route(&host_a.connections, identity_b.host_id).await;
        wait_for_active_direct_route(&host_b.connections, identity_a.host_id).await;

        assert!(
            host_a
                .connections
                .channel_to(identity_b.host_id)
                .await
                .is_ok()
        );
        assert!(
            host_b
                .connections
                .channel_to(identity_a.host_id)
                .await
                .is_ok()
        );

        for task in tasks_a.into_iter().chain(tasks_b) {
            task.abort();
        }
    }

    #[tokio::test]
    async fn ssh_relay_runtime_link_establishes_route_over_trusted_ingress() {
        let identity_a = DeviceIdentity::for_test(Uuid::from_u128(31));
        let identity_b = DeviceIdentity::for_test(Uuid::from_u128(32));

        let mut trust_a = TrustStore::default();
        trust_a.insert_for_test(
            identity_b.host_id,
            trust_entry(
                &identity_b,
                vec![Reachability::Ssh {
                    target: "workstation".to_string(),
                    profile: crate::installation::ProfileId(uuid::Uuid::from_u128(42)),
                }],
            ),
        );
        // SSH pairing commits trust on both sides; the responder must pin
        // the initiator because every call now runs pinned mTLS inside its
        // tunnel — SSH links no longer inherit transport-level trust (D4).
        let mut trust_b = TrustStore::default();
        trust_b.insert_for_test(identity_a.host_id, trust_entry(&identity_a, Vec::new()));
        let host_a =
            test_started_services_with_identity_and_trust(identity_a.clone(), trust_a).await;
        let host_b =
            test_started_services_with_identity_and_trust(identity_b.clone(), trust_b).await;

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let acceptor_carrier = Arc::new(MuxCarrier::new(
            server_io,
            MuxRole::Acceptor,
            CarrierKind::Ssh,
        ));
        let acceptor_task = tokio::spawn(run_link(
            host_b
                .link_ctx()
                .with_carrier(crate::routing::LinkCarrier::Ssh),
            acceptor_carrier,
            crate::routing::ConnectRole::Acceptor,
        ));
        let connector_carrier = Arc::new(MuxCarrier::new(
            client_io,
            MuxRole::Connector,
            CarrierKind::Ssh,
        ));
        let (connector_task, established_rx) = crate::routing::spawn_connector_with_establishment(
            host_a
                .link_connector_ctx()
                .with_expected_peer(identity_b.host_id)
                .with_carrier(crate::routing::LinkCarrier::Ssh),
            connector_carrier,
        );

        let established = tokio::time::timeout(Duration::from_secs(1), established_rx)
            .await
            .expect("timed out waiting for SSH relay Link establishment")
            .expect("SSH relay establishment channel closed")
            .expect("SSH relay Link establishment failed");
        assert_eq!(established.id, identity_b.host_id);
        wait_for_host_entry(&host_a.routing, identity_b.host_id).await;

        let channel = host_a
            .connections
            .channel_to(identity_b.host_id)
            .await
            .unwrap();
        let mut client = wire::client_service_client(channel);
        let response = client
            .list_hosts(wire::ListHostsRequest {})
            .await
            .unwrap()
            .into_inner();
        assert!(
            response
                .hosts
                .iter()
                .any(|host| host.host_id == identity_b.host_id.as_bytes().to_vec())
        );

        connector_task.abort();
        acceptor_task.abort();
        drop(host_b);
    }

    #[tokio::test]
    async fn cloud_routing_service_serves_tcp_listener() {
        let user_id = Uuid::from_u128(100);
        let service = test_cloud_carrier_server(Uuid::from_u128(1), "token-a", user_id).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = service.serve_on_tcp_listener(listener);

        let connector = test_started_services_with_host_id(Uuid::from_u128(2)).await;
        let connector_task = crate::routing::spawn_connector_with_bearer_token(
            connector.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );

        // See above: live links, not host entries, are the observable.
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let cloud_links = service
                    .inner
                    .users
                    .read()
                    .await
                    .get(&user_id)
                    .map(|services| services.channels.link_registry());
                let cloud_sees_connector = match cloud_links {
                    Some(links) => links.link_to_peer(Uuid::from_u128(2)).await.is_some(),
                    None => false,
                };
                let connector_sees_cloud = connector
                    .channels
                    .link_registry()
                    .has_cloud_relay_link_to(Uuid::from_u128(1))
                    .await;
                if cloud_sees_connector && connector_sees_cloud {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for the removed cloud TCP RPC link");

        connector_task.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn cloud_routing_service_drives_remote_agent_inventory() {
        let user_id = Uuid::from_u128(100);
        let service = test_cloud_carrier_server(Uuid::from_u128(1), "token-a", user_id).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = service.serve_on_tcp_listener(listener);

        let identity_a = DeviceIdentity::for_test(Uuid::from_u128(2));
        let identity_b = DeviceIdentity::for_test(Uuid::from_u128(3));
        let host_a = test_started_services_with_identity_and_trust(
            identity_a.clone(),
            trust_store_for(&[&identity_b]),
        )
        .await;
        let host_b = test_started_services_with_identity_and_trust(
            identity_b.clone(),
            trust_store_for(&[&identity_a]),
        )
        .await;
        let agent_id = Uuid::from_u128(44);
        create_test_agent(&host_a, agent_id).await;

        let task_a = crate::routing::spawn_connector_with_bearer_token(
            host_a.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );
        let task_b = crate::routing::spawn_connector_with_bearer_token(
            host_b.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let agents = host_b.client.list_agents().await;
                if agents.iter().any(|agent| agent.id == agent_id) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for authenticated remote agent inventory");

        task_a.abort();
        task_b.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn cloud_pin_pairing_updates_both_trust_stores() {
        let user_id = Uuid::from_u128(100);
        let service = test_cloud_carrier_server(Uuid::from_u128(1), "token-a", user_id).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = service.serve_on_tcp_listener(listener);

        let data_dir_a = tempfile::tempdir().unwrap();
        let data_dir_b = tempfile::tempdir().unwrap();
        let identity_a = DeviceIdentity::for_test(Uuid::from_u128(2));
        let identity_b = DeviceIdentity::for_test(Uuid::from_u128(3));
        let security_a = DeviceRuntimeSecurity::new(
            identity_a.clone(),
            TrustStore::default(),
            data_dir_a.path().to_path_buf(),
        );
        let trust_a = security_a.trust_store.clone();
        let security_b = DeviceRuntimeSecurity::new(
            identity_b.clone(),
            TrustStore::default(),
            data_dir_b.path().to_path_buf(),
        );
        let trust_b = security_b.trust_store.clone();
        let host_a = start_user_services(
            test_state(identity_a.host_id),
            Some(crate::services::PtyAgentHost::new(identity_a.host_id)),
            security_a,
        )
        .await
        .unwrap();
        let host_b = start_user_services(
            test_state(identity_b.host_id),
            Some(crate::services::PtyAgentHost::new(identity_b.host_id)),
            security_b,
        )
        .await
        .unwrap();

        let task_a = crate::routing::spawn_connector_with_bearer_token(
            host_a.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );
        let task_b = crate::routing::spawn_connector_with_bearer_token(
            host_b.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if host_a
                    .client
                    .list_hosts()
                    .await
                    .iter()
                    .any(|host| host.id == identity_b.host_id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for cloud route to pairing target");

        host_b
            .pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let admin = crate::installation::ProfileAdmin::for_test(host_a.client.clone());
        let pending = admin
            .begin_pair_pin(identity_b.host_id, "123456")
            .await
            .unwrap();
        let paired_peer = admin.confirm_pair(pending).await.unwrap();

        assert_eq!(paired_peer.host_id, identity_b.host_id);
        assert_eq!(paired_peer.pubkey, identity_b.public_key());
        let trust_a_live = trust_a.read().unwrap();
        assert_eq!(
            trust_a_live
                .entry(identity_b.host_id)
                .unwrap()
                .reachabilities,
            vec![Reachability::Cloud]
        );
        drop(trust_a_live);
        let trust_b_live = trust_b.read().unwrap();
        let trust_b_entry = trust_b_live.entry(identity_a.host_id).unwrap();
        assert_eq!(trust_b_entry.pubkey, identity_a.public_key());
        assert_eq!(trust_b_entry.name, "local");
        assert_eq!(trust_b_entry.reachabilities, vec![Reachability::Cloud]);

        task_a.abort();
        task_b.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn cloud_qr_pairing_updates_both_trust_stores() {
        let user_id = Uuid::from_u128(101);
        let service = test_cloud_carrier_server(Uuid::from_u128(1), "token-a", user_id).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = service.serve_on_tcp_listener(listener);

        let data_dir_a = tempfile::tempdir().unwrap();
        let data_dir_b = tempfile::tempdir().unwrap();
        let identity_a = DeviceIdentity::for_test(Uuid::from_u128(2));
        let identity_b = DeviceIdentity::for_test(Uuid::from_u128(3));
        let security_a = DeviceRuntimeSecurity::new(
            identity_a.clone(),
            TrustStore::default(),
            data_dir_a.path().to_path_buf(),
        );
        let trust_a = security_a.trust_store.clone();
        let security_b = DeviceRuntimeSecurity::new(
            identity_b.clone(),
            TrustStore::default(),
            data_dir_b.path().to_path_buf(),
        );
        let trust_b = security_b.trust_store.clone();
        let host_a = start_user_services(
            test_state(identity_a.host_id),
            Some(crate::services::PtyAgentHost::new(identity_a.host_id)),
            security_a,
        )
        .await
        .unwrap();
        let host_b = start_user_services(
            test_state(identity_b.host_id),
            Some(crate::services::PtyAgentHost::new(identity_b.host_id)),
            security_b,
        )
        .await
        .unwrap();

        let task_a = crate::routing::spawn_connector_with_bearer_token(
            host_a.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );
        let task_b = crate::routing::spawn_connector_with_bearer_token(
            host_b.link_connector_ctx(),
            cloud_test_carrier(addr).await,
            "token-a".to_string(),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if host_a
                    .client
                    .list_hosts()
                    .await
                    .iter()
                    .any(|host| host.id == identity_b.host_id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for cloud route to pairing target");

        let secret = [9_u8; 32];
        host_b
            .pair_mode
            .start_qr_secret_for_duration(secret, Duration::from_secs(60))
            .unwrap();
        let admin = crate::installation::ProfileAdmin::for_test(host_a.client.clone());
        let wrong = crate::QrPairingPayload {
            host_id: identity_b.host_id,
            secret: vec![42; 32],
            addrs: Vec::new(),
            cloud_url: None,
        };
        admin
            .begin_pair_qr(&wrong)
            .await
            .expect_err("a wrong QR secret must fail without consuming the real one");
        let payload = crate::QrPairingPayload {
            host_id: identity_b.host_id,
            secret: secret.to_vec(),
            addrs: Vec::new(),
            cloud_url: None,
        };
        let pending = admin.begin_pair_qr(&payload).await.unwrap();
        let paired_peer = admin.confirm_pair(pending).await.unwrap();

        assert_eq!(paired_peer.host_id, identity_b.host_id);
        assert_eq!(paired_peer.pubkey, identity_b.public_key());
        let trust_a_live = trust_a.read().unwrap();
        assert_eq!(
            trust_a_live
                .entry(identity_b.host_id)
                .unwrap()
                .reachabilities,
            vec![Reachability::Cloud]
        );
        drop(trust_a_live);
        let trust_b_live = trust_b.read().unwrap();
        let trust_b_entry = trust_b_live.entry(identity_a.host_id).unwrap();
        assert_eq!(trust_b_entry.pubkey, identity_a.public_key());
        assert_eq!(trust_b_entry.name, "local");
        assert_eq!(trust_b_entry.reachabilities, vec![Reachability::Cloud]);

        task_a.abort();
        task_b.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn started_services_opens_in_process_client_service_channel() {
        let services = test_started_services().await;
        let (channel, server_task) = services.open_in_process_client_channel();
        let mut client = wire::client_service_client(channel);
        let agent_id = Uuid::from_u128(42);

        let created = client
            .create_agent(client_create_request(agent_id, "via-client-service"))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            created.agent.as_ref().unwrap().agent_id,
            agent_id.as_bytes()
        );

        let agents = client
            .list_agents(wire::ListAgentsRequest {})
            .await
            .unwrap()
            .into_inner();
        assert_eq!(agents.agents.len(), 1);
        assert_eq!(agents.agents[0].agent_id, agent_id.as_bytes());

        server_task.abort();
    }

    #[tokio::test]
    async fn started_services_public_client_wrapper_uses_in_process_channel() {
        let services = test_started_services().await;
        let (channel, server_task) = services.open_in_process_client_channel();
        let client = crate::Client::from_client_service_channel(channel);
        let agent_id = Uuid::from_u128(43);

        let mut host_events = client.subscribe_hosts().await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), host_events.recv())
            .await
            .expect("timed out waiting for routing snapshot")
            .unwrap();
        assert!(matches!(
            event,
            crate::routing::HostEvent::HostUpdated { host }
                if host.id == Uuid::from_u128(1) && host.online
        ));
        let event = tokio::time::timeout(Duration::from_secs(1), host_events.recv())
            .await
            .expect("timed out waiting for routing snapshot complete")
            .unwrap();
        assert!(matches!(event, crate::routing::HostEvent::SnapshotComplete));

        let mut agent_events = client.subscribe_agents().await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), agent_events.recv())
            .await
            .expect("timed out waiting for agent snapshot complete")
            .unwrap();
        assert!(matches!(event, crate::AgentEvent::SnapshotComplete));

        let created = client
            .create_agent(crate::CreateAgentRequest {
                agent_id,
                host_id: None,
                name: Some("public-client".to_string()),
                agent_type: crate::AgentType::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                },
                working_dir: std::env::temp_dir(),
                terminal_size: None,
                args: Vec::new(),
                parent: None,
                initial_prompt: None,
            })
            .await
            .unwrap();
        assert_eq!(created.id, agent_id);
        assert_eq!(created.host_id, Uuid::from_u128(1));

        let artifact = client
            .put_artifact(
                crate::AgentIdentifier::Id(agent_id),
                amux_artifacts::ArtifactKind::File,
                "notes.txt",
                "text/plain",
                b"public client bytes".to_vec(),
            )
            .await
            .unwrap();
        let (fetched, bytes) = client
            .get_artifact(crate::AgentIdentifier::Id(agent_id), &artifact.id)
            .await
            .unwrap();
        assert_eq!(fetched, artifact);
        assert_eq!(bytes, b"public client bytes");
        let diff_error = client
            .diff(
                crate::AgentIdentifier::Id(agent_id),
                crate::DiffBase::WorkingTree,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            diff_error,
            crate::ClientError::Protocol(ProtocolError::DiffUnavailable { .. })
        ));

        let other_agent_id = Uuid::from_u128(45);
        let injected = services
            .client
            .apply_agent_event(crate::AgentEvent::AgentUp {
                agent: crate::Agent {
                    id: other_agent_id,
                    host_id: Uuid::from_u128(99),
                    name: Some("public-client".to_string()),
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    kind: crate::AgentKind::TestAgent,
                    readonly: false,
                    args: Vec::new(),
                    created_at: chrono::Utc::now(),
                    parent: None,
                    working_on: None,
                },
            })
            .await;
        assert_eq!(
            injected,
            crate::services::client::AgentEventOutcome::Upserted
        );
        let ambiguous = client
            .rename_agent(
                crate::AgentIdentifier::Name("public-client".to_string()),
                "ambiguous".to_string(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            ambiguous,
            crate::ClientError::Protocol(ProtocolError::AmbiguousAgentName { name, agent_ids })
                if name == "public-client" && agent_ids == vec![agent_id, other_agent_id]
        ));
        let removed = services
            .client
            .apply_agent_event(crate::AgentEvent::AgentDown {
                agent_id: other_agent_id,
            })
            .await;
        assert_eq!(removed, crate::services::client::AgentEventOutcome::Removed);

        let event = wait_for_agent_event(&mut agent_events, |event| {
            matches!(
                event,
                crate::AgentEvent::AgentUp {
                    agent
                } if agent.id == agent_id && agent.host_id == Uuid::from_u128(1)
            )
        })
        .await;
        assert!(matches!(
            event,
            crate::AgentEvent::AgentUp {
                agent
            } if agent.id == agent_id && agent.host_id == Uuid::from_u128(1)
        ));

        let mut session = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: crate::AgentIdentifier::Name("public-client".to_string()),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap();
        client
            .send_input(crate::SendInputRequest {
                agent: crate::AgentIdentifier::Name("public-client".to_string()),
                input_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                io_protocol: TEST_ECHO_V1.to_string(),
                payload: bytes::Bytes::from_static(b"hello"),
                pin: Vec::new(),
            })
            .await
            .unwrap();
        let output = wait_for_session_output(&mut session, b"hello").await;
        assert_eq!(output, b"hello");

        let renamed = client
            .rename_agent(
                crate::AgentIdentifier::Name("public-client".to_string()),
                "public-client-renamed".to_string(),
            )
            .await
            .unwrap();
        assert_eq!(renamed.name.as_deref(), Some("public-client-renamed"));

        let event = wait_for_agent_event(&mut agent_events, |event| {
            matches!(
                event,
                crate::AgentEvent::AgentUpdated {
                    agent
                } if agent.id == agent_id && agent.name.as_deref() == Some("public-client-renamed")
            )
        })
        .await;
        assert!(matches!(
            event,
            crate::AgentEvent::AgentUpdated {
                agent
            } if agent.id == agent_id && agent.name.as_deref() == Some("public-client-renamed")
        ));

        let agents = client.list_agents().await.unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, agent_id);

        client
            .delete_agent(crate::AgentIdentifier::Name(
                "public-client-renamed".to_string(),
            ))
            .await
            .unwrap();
        let event = wait_for_agent_event(&mut agent_events, |event| {
            matches!(
                event,
                crate::AgentEvent::AgentDown {
                    agent_id: event_agent_id,
                } if *event_agent_id == agent_id
            )
        })
        .await;
        assert!(matches!(
            event,
            crate::AgentEvent::AgentDown {
                agent_id: event_agent_id,
            } if event_agent_id == agent_id
        ));

        let event = tokio::time::timeout(Duration::from_secs(1), session.recv())
            .await
            .expect("timed out waiting for session close")
            .unwrap();
        assert!(matches!(
            event,
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::AgentDeleted
            }
        ));
        assert!(client.list_agents().await.unwrap().is_empty());

        server_task.abort();
    }

    #[tokio::test]
    async fn public_client_preserves_first_session_closed_event() {
        let services = test_started_services().await;
        let remote = remote_host(2);
        let agent_id = Uuid::from_u128(44);
        services
            .routing
            .apply_claim_up(HostId::from_u128(9), remote.clone())
            .await;
        services
            .client
            .apply_agent_event(crate::AgentEvent::AgentUp {
                agent: crate::Agent {
                    id: agent_id,
                    host_id: remote.id,
                    name: Some("remote-unreachable".to_string()),
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    kind: crate::AgentKind::TestAgent,
                    readonly: false,
                    args: Vec::new(),
                    created_at: chrono::Utc::now(),
                    parent: None,
                    working_on: None,
                },
            })
            .await;

        let (channel, server_task) = services.open_in_process_client_channel();
        let client = crate::Client::from_client_service_channel(channel);
        let mut session = client
            .subscribe_session(crate::SubscribeSessionRequest {
                agent: agent_id.into(),
                io_protocol: TEST_ECHO_V1.to_string(),
                args: None,
            })
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), session.recv())
            .await
            .expect("timed out waiting for first session event")
            .unwrap();
        assert!(matches!(
            event,
            SubscribeSessionEvent::Closed {
                reason: SessionCloseReason::HostUnreachable
            }
        ));

        server_task.abort();
    }

    async fn wait_for_agent_event(
        stream: &mut crate::AgentEventStream,
        mut matches_event: impl FnMut(&crate::AgentEvent) -> bool,
    ) -> crate::AgentEvent {
        for _ in 0..8 {
            let event = tokio::time::timeout(Duration::from_secs(1), stream.recv())
                .await
                .expect("timed out waiting for agent event")
                .unwrap();
            if matches_event(&event) {
                return event;
            }
        }
        panic!("agent stream did not emit expected event");
    }

    async fn wait_for_session_output(
        session: &mut crate::SessionStream,
        expected: &[u8],
    ) -> Vec<u8> {
        for _ in 0..4 {
            let event = tokio::time::timeout(Duration::from_secs(1), session.recv())
                .await
                .expect("timed out waiting for session output")
                .unwrap();
            if let SubscribeSessionEvent::Output { payload } = event
                && payload == expected
            {
                return payload;
            }
        }
        panic!("session output did not match expected payload");
    }

    /// Activation now materializes a real tunnel (with the inner device-TLS
    /// handshake) instead of looking up a pre-registered channel, so tests
    /// poll for the active route rather than asserting it synchronously.
    async fn wait_for_active_direct_route(connections: &ConnectionManager, host_id: Uuid) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    connections.active_route(host_id).await,
                    Some(Route::Direct(_))
                ) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for an active direct route")
    }

    async fn wait_for_host_entry(routing: &RoutingCore, host_id: Uuid) -> Host {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(host) = routing.host_entry(host_id).await {
                    return host;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for host entry")
    }

    async fn wait_for_host_entry_removed(routing: &RoutingCore, host_id: Uuid) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if routing.host_entry(host_id).await.is_none() {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for host entry removal");
    }

    fn channel_from_transport(transport: tokio::io::DuplexStream) -> Channel {
        let transport = Arc::new(Mutex::new(Some(transport)));
        Endpoint::from_static("http://tunnel").connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let transport = Arc::clone(&transport);
                async move {
                    transport
                        .lock()
                        .expect("tunnel transport mutex poisoned")
                        .take()
                        .map(TokioIo::new)
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::AlreadyExists,
                                "test transport already consumed",
                            )
                        })
                }
            },
        ))
    }
}
