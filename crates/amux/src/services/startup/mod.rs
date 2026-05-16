//! Startup wiring for one user's runtime services.

mod cloud;

use std::collections::HashMap;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, UNIX_EPOCH};

pub(crate) use cloud::establish_cloud_connection;
use futures_util::{Stream, stream};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tonic::codegen::http;
use tonic::transport::Channel;
use tonic::transport::server::Connected;
use tower::Service;
use uuid::Uuid;

use crate::protocol::wire;
use crate::routing::{
    AuthenticatedRoutingUser, Host, HostReachabilityEvent, Link, RoutingAuthSession,
    RoutingConnectCtx, RoutingConnectorCtx, RoutingCore, RoutingTokenAuthenticator, local_host,
    spawn_routing_event_fanout,
};
use crate::services::client::ClientService;
use crate::services::{AgentServiceCtx, SharedAgentServiceState};
use crate::transport::{
    TcpServerTransport, in_process_channel, in_process_incoming, in_process_transport_pair,
    tcp_incoming,
};
#[cfg(unix)]
use crate::transport::{bind_unix_listener, unix_incoming};
use crate::tunnel::{TunnelPool, TunnelTransport};
use crate::user_state::ServerState;

#[derive(Clone)]
pub(crate) struct JwtCloudRoutingAuthenticator {
    state: Arc<RwLock<ServerState>>,
}

impl JwtCloudRoutingAuthenticator {
    pub(crate) fn new(state: Arc<RwLock<ServerState>>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl RoutingTokenAuthenticator for JwtCloudRoutingAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedRoutingUser, tonic::Status> {
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
        Ok(AuthenticatedRoutingUser {
            user_id,
            client_id: claims.client_id,
            expires_at,
        })
    }
}

fn bearer_token_from_metadata(
    metadata: &tonic::metadata::MetadataMap,
) -> Result<&str, tonic::Status> {
    let authorization = metadata
        .get("authorization")
        .ok_or_else(|| tonic::Status::unauthenticated("missing authorization metadata"))?
        .to_str()
        .map_err(|_| tonic::Status::unauthenticated("invalid authorization metadata"))?;
    authorization
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
        .ok_or_else(|| tonic::Status::unauthenticated("invalid authorization metadata"))
}

#[derive(Clone)]
pub(crate) struct CloudRoutingService {
    inner: Arc<CloudRoutingServiceInner>,
}

struct CloudRoutingServiceInner {
    state: Arc<RwLock<ServerState>>,
    authenticator: Arc<dyn RoutingTokenAuthenticator>,
    users: RwLock<HashMap<Uuid, StartedRoutingServices>>,
}

impl CloudRoutingService {
    pub(crate) fn new(state: Arc<RwLock<ServerState>>) -> Self {
        Self::with_authenticator(
            state.clone(),
            Arc::new(JwtCloudRoutingAuthenticator::new(state)),
        )
    }

    pub(crate) fn with_authenticator(
        state: Arc<RwLock<ServerState>>,
        authenticator: Arc<dyn RoutingTokenAuthenticator>,
    ) -> Self {
        Self {
            inner: Arc::new(CloudRoutingServiceInner {
                state,
                authenticator,
                users: RwLock::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn serve_on_tcp_listener(&self, listener: TcpListener) -> JoinHandle<()> {
        spawn_cloud_routing_service_server(self.clone(), tcp_incoming(listener))
    }

    pub(crate) fn serve_on_tls_tcp_listener(
        &self,
        listener: TcpListener,
        acceptor: TlsAcceptor,
        handshake_timeout: Duration,
    ) -> JoinHandle<()> {
        let incoming = stream::unfold(
            (listener, acceptor, handshake_timeout),
            |(listener, acceptor, handshake_timeout)| async move {
                loop {
                    let (stream, addr) = match listener.accept().await {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            return Some((Err(error), (listener, acceptor, handshake_timeout)));
                        }
                    };
                    if let Err(error) = stream.set_nodelay(true) {
                        tracing::warn!(error = %error, "failed to set TCP_NODELAY");
                    }
                    crate::transport::configure_tcp_keepalive(&stream);
                    match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                        Ok(Ok(tls_stream)) => {
                            return Some((
                                Ok(TcpServerTransport::new(tls_stream)),
                                (listener, acceptor, handshake_timeout),
                            ));
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(peer = %addr, error = %error, "TLS handshake failed");
                        }
                        Err(_) => {
                            tracing::warn!(peer = %addr, "TLS handshake timed out");
                        }
                    }
                }
            },
        );
        spawn_cloud_routing_service_server(self.clone(), incoming)
    }

    async fn routing_connect_ctx_for_user(&self, user_id: Uuid) -> RoutingConnectCtx {
        if let Some(ctx) = self
            .inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(StartedRoutingServices::routing_connect_ctx)
        {
            return ctx;
        }

        let started = start_routing_services(self.inner.state.clone()).await;

        let mut users = self.inner.users.write().await;
        users
            .entry(user_id)
            .or_insert(started)
            .routing_connect_ctx()
    }

    pub(crate) async fn send_goaway_to_all(
        &self,
        reason: wire::pb::GoAwayReason,
        drain_timeout_ms: u32,
    ) {
        let tunnels = {
            let users = self.inner.users.read().await;
            users
                .values()
                .map(|services| services.tunnels.clone())
                .collect::<Vec<_>>()
        };
        for tunnels in tunnels {
            tunnels
                .link_registry()
                .send_goaway_to_all(reason, drain_timeout_ms)
                .await;
        }
    }
}

#[tonic::async_trait]
impl wire::routing_service_server::RoutingService for CloudRoutingService {
    type ConnectStream =
        <RoutingConnectCtx as wire::routing_service_server::RoutingService>::ConnectStream;

    async fn connect(
        &self,
        request: tonic::Request<tonic::Streaming<wire::pb::Message>>,
    ) -> Result<tonic::Response<Self::ConnectStream>, tonic::Status> {
        let user = request
            .extensions()
            .get::<AuthenticatedRoutingUser>()
            .cloned()
            .ok_or_else(|| tonic::Status::unauthenticated("missing routing auth claims"))?;
        let minimum_client_version = {
            let state = self.inner.state.read().await;
            state.minimum_client_version(&user.client_id)
        };
        let ctx = self
            .routing_connect_ctx_for_user(user.user_id)
            .await
            .with_auth_session(RoutingAuthSession::new(
                user,
                self.inner.authenticator.clone(),
                minimum_client_version,
            ));
        <RoutingConnectCtx as wire::routing_service_server::RoutingService>::connect(&ctx, request)
            .await
    }
}

#[derive(Clone)]
struct RoutingAuthInterceptor<S> {
    inner: S,
    authenticator: Arc<dyn RoutingTokenAuthenticator>,
}

impl<S> RoutingAuthInterceptor<S> {
    fn new(inner: S, authenticator: Arc<dyn RoutingTokenAuthenticator>) -> Self {
        Self {
            inner,
            authenticator,
        }
    }
}

impl<S> tonic::server::NamedService for RoutingAuthInterceptor<S>
where
    S: tonic::server::NamedService,
{
    const NAME: &'static str = S::NAME;
}

impl<S, B> Service<http::Request<B>> for RoutingAuthInterceptor<S>
where
    S: Service<http::Request<B>, Response = http::Response<tonic::body::BoxBody>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    B: Send + 'static,
{
    type Response = http::Response<tonic::body::BoxBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: http::Request<B>) -> Self::Future {
        let mut inner = self.inner.clone();
        std::mem::swap(&mut inner, &mut self.inner);
        let authenticator = self.authenticator.clone();
        Box::pin(async move {
            let metadata = tonic::metadata::MetadataMap::from_headers(request.headers().clone());
            let auth_result = match bearer_token_from_metadata(&metadata) {
                Ok(token) => authenticator.authenticate_token(token).await,
                Err(status) => Err(status),
            };
            match auth_result {
                Ok(user) => {
                    request.extensions_mut().insert(user);
                    inner.call(request).await
                }
                Err(status) => Ok(status.into_http()),
            }
        })
    }
}

fn cloud_routing_server(
    service: CloudRoutingService,
) -> RoutingAuthInterceptor<wire::routing_service_server::RoutingServiceServer<CloudRoutingService>>
{
    let authenticator = service.inner.authenticator.clone();
    RoutingAuthInterceptor::new(
        wire::routing_service_server::RoutingServiceServer::new(service),
        authenticator,
    )
}

pub(crate) struct StartedRoutingServices {
    pub(crate) routing: Arc<RoutingCore>,
    pub(crate) tunnels: Arc<TunnelPool>,
    local_host: Host,
    _incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    tasks: Vec<JoinHandle<()>>,
}

struct StartedRoutingParts {
    runtime: StartedRoutingServices,
    incoming_tunnels_rx: mpsc::Receiver<TunnelTransport>,
}

async fn start_routing_services_parts(state: Arc<RwLock<ServerState>>) -> StartedRoutingParts {
    let (host_id, host_name, is_cloud_server) = {
        let state = state.read().await;
        (
            state.host_id(),
            state.host_name().to_string(),
            state.is_cloud_server(),
        )
    };
    let host = local_host(host_id, &host_name, is_cloud_server);

    let routing = Arc::new(RoutingCore::new());
    let (incoming_tunnels_tx, incoming_tunnels_rx) = mpsc::channel(64);
    let tunnels = Arc::new(TunnelPool::new(
        host_id,
        routing.clone(),
        incoming_tunnels_tx.clone(),
    ));

    let mut tasks = Vec::with_capacity(2);
    tasks.push(spawn_routing_event_fanout(routing.clone(), tunnels.link_registry()).await);
    tasks.push(spawn_tunnel_cleanup_task(routing.clone(), tunnels.clone()).await);

    StartedRoutingParts {
        runtime: StartedRoutingServices {
            routing,
            tunnels,
            local_host: host,
            _incoming_tunnels_tx: incoming_tunnels_tx,
            tasks,
        },
        incoming_tunnels_rx,
    }
}

pub(crate) async fn start_routing_services(
    state: Arc<RwLock<ServerState>>,
) -> StartedRoutingServices {
    let mut parts = start_routing_services_parts(state).await;
    parts
        .runtime
        .tasks
        .push(spawn_discard_incoming_tunnels_task(
            parts.incoming_tunnels_rx,
        ));
    parts.runtime
}

pub(crate) struct StartedUserServices {
    runtime: StartedRoutingServices,
    #[cfg(test)]
    pub(crate) agent: AgentServiceCtx,
    pub(crate) client: ClientService,
}

pub(crate) async fn start_user_services(
    state: Arc<RwLock<ServerState>>,
    agent_state: SharedAgentServiceState,
) -> StartedUserServices {
    let mut parts = start_routing_services_parts(state.clone()).await;
    let host_id = parts.runtime.local_host.id;
    let is_cloud_server = {
        let state = state.read().await;
        state.is_cloud_server()
    };

    let agent = AgentServiceCtx::new(agent_state.clone(), host_id, is_cloud_server);
    let client = ClientService::new(
        agent.clone(),
        state,
        parts.runtime.routing.clone(),
        parts.runtime.tunnels.clone(),
    );

    client
        .apply_host_event(HostReachabilityEvent::HostAdded {
            host: parts.runtime.local_host.clone(),
        })
        .await;

    parts.runtime.tasks.push(spawn_host_service_server(
        agent.clone(),
        parts.incoming_tunnels_rx,
    ));
    parts.runtime.tasks.push(
        client
            .attach_routing_events(parts.runtime.routing.clone())
            .await,
    );
    if !parts
        .runtime
        .local_host
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
    }

    StartedUserServices {
        runtime: parts.runtime,
        #[cfg(test)]
        agent,
        client,
    }
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

impl StartedUserServices {
    pub(crate) fn open_in_process_client_channel(&self) -> (Channel, JoinHandle<()>) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = in_process_incoming(server_transport);
        let server = spawn_client_service_server(self.client.clone(), incoming);
        (in_process_channel(client_transport), server)
    }

    #[cfg(unix)]
    pub(crate) fn serve_client_service_on_unix_socket(
        &mut self,
        socket_path: &Path,
    ) -> std::io::Result<()> {
        let listener = bind_unix_listener(socket_path)?;
        let incoming = unix_incoming(listener);
        let client = self.client.clone();
        self.tasks
            .push(spawn_client_service_server(client, incoming));
        Ok(())
    }
}

impl StartedRoutingServices {
    pub(crate) fn routing_connector_ctx(&self, proposed_link: Link) -> RoutingConnectorCtx {
        RoutingConnectorCtx::new(
            self.local_host.clone(),
            self.routing.clone(),
            self.tunnels.clone(),
            proposed_link,
        )
    }

    pub(crate) fn serve_routing_service_on_tcp_listener(&mut self, listener: TcpListener) {
        self.tasks.push(spawn_routing_service_server(
            self.routing_connect_ctx(),
            tcp_incoming(listener),
        ));
    }

    fn routing_connect_ctx(&self) -> RoutingConnectCtx {
        RoutingConnectCtx::dynamic(
            self.local_host.clone(),
            self.routing.clone(),
            self.tunnels.clone(),
        )
    }
}

impl Drop for StartedRoutingServices {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn spawn_host_service_server(
    agent: AgentServiceCtx,
    incoming_rx: mpsc::Receiver<TunnelTransport>,
) -> JoinHandle<()> {
    let incoming = stream::unfold(
        incoming_rx,
        |mut rx: mpsc::Receiver<TunnelTransport>| async {
            rx.recv()
                .await
                .map(|transport| (Ok::<_, std::io::Error>(transport), rx))
        },
    );

    tokio::spawn(async move {
        if let Err(error) = crate::transport::tonic_server_builder()
            .add_service(wire::agent_service_server::AgentServiceServer::new(agent))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::warn!(error = %error, "host AgentService server exited with error");
        }
    })
}

fn spawn_discard_incoming_tunnels_task(
    mut incoming_rx: mpsc::Receiver<TunnelTransport>,
) -> JoinHandle<()> {
    tokio::spawn(async move { while incoming_rx.recv().await.is_some() {} })
}

async fn spawn_tunnel_cleanup_task(
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
) -> JoinHandle<()> {
    let mut rx = routing.subscribe_hosts().await;
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            tunnels.handle_host_event(&event).await;
        }
    })
}

fn spawn_client_service_server<I, IO>(client: ClientService, incoming: I) -> JoinHandle<()>
where
    I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    IO::ConnectInfo: Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        if let Err(error) = crate::transport::tonic_server_builder()
            .add_service(wire::client_service_server::ClientServiceServer::new(
                client,
            ))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::warn!(error = %error, "ClientService server exited with error");
        }
    })
}

fn spawn_cloud_routing_service_server<I, IO>(
    service: CloudRoutingService,
    incoming: I,
) -> JoinHandle<()>
where
    I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    IO::ConnectInfo: Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        if let Err(error) = crate::transport::tonic_server_builder()
            .add_service(cloud_routing_server(service))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::warn!(error = %error, "cloud RoutingService server exited with error");
        }
    })
}

fn spawn_routing_service_server<I, IO>(service: RoutingConnectCtx, incoming: I) -> JoinHandle<()>
where
    I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Connected + Unpin + Send + 'static,
    IO::ConnectInfo: Clone + Send + Sync + 'static,
{
    tokio::spawn(async move {
        if let Err(error) = crate::transport::tonic_server_builder()
            .add_service(wire::routing_service_server::RoutingServiceServer::new(
                service,
            ))
            .serve_with_incoming(incoming)
            .await
        {
            tracing::warn!(error = %error, "direct RoutingService server exited with error");
        }
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures_util::StreamExt;
    use hyper_util::rt::TokioIo;
    use tonic::codegen::http::Uri;
    use tonic::transport::{Channel, Endpoint};
    use tower::service_fn;

    use super::*;
    use crate::agents::{
        CreateAgentConfig, CreateAgentRpcRequest, TEST_ECHO_COMMAND, TEST_ECHO_V1,
    };
    use crate::config::Config;
    use crate::protocol::ProtocolError;
    use crate::routing::{Capabilities, Host, Route, SupportedAgentType};
    use crate::user_state::ShutdownRequest;
    use crate::{SessionCloseReason, SubscribeSessionEvent};

    fn test_state(host_id: Uuid) -> Arc<RwLock<ServerState>> {
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<ShutdownRequest>(1);
        let config = Config {
            host_name: "local".to_string(),
            ..Config::default()
        };
        Arc::new(RwLock::new(ServerState::new(
            config,
            host_id,
            shutdown_tx,
            None,
            None,
        )))
    }

    async fn test_started_services() -> StartedUserServices {
        test_started_services_with_host_id(Uuid::from_u128(1)).await
    }

    async fn test_started_services_with_host_id(host_id: Uuid) -> StartedUserServices {
        let state = test_state(host_id);
        let agent_state = Arc::new(RwLock::new(crate::services::AgentServiceState::new()));
        start_user_services(state, agent_state).await
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
        }
    }

    #[derive(Clone)]
    struct StaticCloudRoutingAuthenticator {
        token_users: Arc<HashMap<String, AuthenticatedRoutingUser>>,
    }

    impl StaticCloudRoutingAuthenticator {
        fn new(token: &str, user_id: Uuid) -> Self {
            Self {
                token_users: Arc::new(HashMap::from([(
                    token.to_string(),
                    AuthenticatedRoutingUser {
                        user_id,
                        client_id: "test-client".to_string(),
                        expires_at: std::time::SystemTime::now() + Duration::from_secs(3600),
                    },
                )])),
            }
        }
    }

    #[tonic::async_trait]
    impl RoutingTokenAuthenticator for StaticCloudRoutingAuthenticator {
        async fn authenticate_token(
            &self,
            token: &str,
        ) -> Result<AuthenticatedRoutingUser, tonic::Status> {
            self.token_users
                .get(token)
                .cloned()
                .ok_or_else(|| tonic::Status::unauthenticated("unknown token"))
        }
    }

    async fn create_test_agent(services: &StartedUserServices, agent_id: Uuid) {
        services
            .agent
            .create(CreateAgentRpcRequest {
                agent_id,
                name: Some("echo".to_string()),
                agent: CreateAgentConfig::TestAgent {
                    command: TEST_ECHO_COMMAND.to_string(),
                    working_dir: PathBuf::from("/tmp"),
                    terminal_size: None,
                },
            })
            .await
            .unwrap();
    }

    fn client_create_request(agent_id: Uuid, name: &str) -> wire::ClientCreateAgentRequest {
        wire::ClientCreateAgentRequest {
            agent_id: agent_id.as_bytes().to_vec(),
            name: Some(name.to_string()),
            host_id: None,
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
            .apply_host_up(
                remote_host(2),
                Route::from_links(["to-remote".to_string()]).unwrap(),
                None,
            )
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
    async fn started_services_serves_agent_service_on_incoming_tunnels() {
        let services = test_started_services().await;
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        services
            ._incoming_tunnels_tx
            .send(TunnelTransport::new(server_io, Uuid::from_u128(20)))
            .await
            .unwrap();

        let channel = channel_from_transport(TunnelTransport::new(client_io, Uuid::from_u128(10)));
        let mut client = wire::agent_service_client::AgentServiceClient::new(channel);
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
    async fn cloud_routing_service_rejects_missing_authorization() {
        let state = test_state(Uuid::from_u128(1));
        let user_id = Uuid::from_u128(100);
        let service = CloudRoutingService::with_authenticator(
            state,
            Arc::new(StaticCloudRoutingAuthenticator::new("token-a", user_id)),
        );

        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = in_process_incoming(server_transport);
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(cloud_routing_server(service))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        let connector = test_started_services_with_host_id(Uuid::from_u128(2)).await;
        let connector_task = crate::routing::spawn_connector_to_channel(
            connector.routing_connector_ctx(Link::new("connector").unwrap()),
            in_process_channel(client_transport),
        );

        let result = tokio::time::timeout(Duration::from_secs(1), connector_task)
            .await
            .expect("timed out waiting for unauthenticated connector rejection")
            .expect("connector task panicked");
        let error = result.expect_err("connector unexpectedly authenticated");
        assert_eq!(error.code(), tonic::Code::Unauthenticated);

        server_task.abort();
    }

    #[tokio::test]
    async fn cloud_routing_service_selects_user_services_from_bearer_metadata() {
        let state = test_state(Uuid::from_u128(1));
        let user_id = Uuid::from_u128(100);
        let service = CloudRoutingService::with_authenticator(
            state,
            Arc::new(StaticCloudRoutingAuthenticator::new("token-a", user_id)),
        );

        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = in_process_incoming(server_transport);
        let server_service = service.clone();
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(cloud_routing_server(server_service))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        let connector = test_started_services_with_host_id(Uuid::from_u128(2)).await;
        let connector_task = crate::routing::spawn_connector_to_channel_with_bearer_token(
            connector.routing_connector_ctx(Link::new("connector").unwrap()),
            in_process_channel(client_transport),
            "token-a".to_string(),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let cloud_routing = service
                    .inner
                    .users
                    .read()
                    .await
                    .get(&user_id)
                    .map(|services| services.routing.clone());
                let cloud_sees_connector = match cloud_routing {
                    Some(routing) => routing.host_entry(Uuid::from_u128(2)).await.is_some(),
                    None => false,
                };
                let connector_sees_cloud = connector
                    .routing
                    .host_entry(Uuid::from_u128(1))
                    .await
                    .is_some();
                if cloud_sees_connector && connector_sees_cloud {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for cloud RoutingService.Connect");

        assert_eq!(service.inner.users.read().await.len(), 1);
        assert!(service.inner.users.read().await.contains_key(&user_id));

        connector_task.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn cloud_routing_service_serves_tcp_listener() {
        let state = test_state(Uuid::from_u128(1));
        let user_id = Uuid::from_u128(100);
        let service = CloudRoutingService::with_authenticator(
            state,
            Arc::new(StaticCloudRoutingAuthenticator::new("token-a", user_id)),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = service.serve_on_tcp_listener(listener);

        let connector = test_started_services_with_host_id(Uuid::from_u128(2)).await;
        let channel = Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let connector_task = crate::routing::spawn_connector_to_channel_with_bearer_token(
            connector.routing_connector_ctx(Link::new("connector").unwrap()),
            channel,
            "token-a".to_string(),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let cloud_routing = service
                    .inner
                    .users
                    .read()
                    .await
                    .get(&user_id)
                    .map(|services| services.routing.clone());
                let cloud_sees_connector = match cloud_routing {
                    Some(routing) => routing.host_entry(Uuid::from_u128(2)).await.is_some(),
                    None => false,
                };
                let connector_sees_cloud = connector
                    .routing
                    .host_entry(Uuid::from_u128(1))
                    .await
                    .is_some();
                if cloud_sees_connector && connector_sees_cloud {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for cloud TCP RoutingService.Connect");

        connector_task.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn cloud_routing_service_drives_remote_agent_inventory() {
        let state = test_state(Uuid::from_u128(1));
        let user_id = Uuid::from_u128(100);
        let service = CloudRoutingService::with_authenticator(
            state,
            Arc::new(StaticCloudRoutingAuthenticator::new("token-a", user_id)),
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = service.serve_on_tcp_listener(listener);

        let host_a = test_started_services_with_host_id(Uuid::from_u128(2)).await;
        let host_b = test_started_services_with_host_id(Uuid::from_u128(3)).await;
        let agent_id = Uuid::from_u128(44);
        create_test_agent(&host_a, agent_id).await;

        let channel_a = Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let task_a = crate::routing::spawn_connector_to_channel_with_bearer_token(
            host_a.routing_connector_ctx(Link::new("host-a").unwrap()),
            channel_a,
            "token-a".to_string(),
        );
        let channel_b = Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let task_b = crate::routing::spawn_connector_to_channel_with_bearer_token(
            host_b.routing_connector_ctx(Link::new("host-b").unwrap()),
            channel_b,
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
    async fn started_services_opens_in_process_client_service_channel() {
        let services = test_started_services().await;
        let (channel, server_task) = services.open_in_process_client_channel();
        let mut client = wire::client_service_client::ClientServiceClient::new(channel);
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
        let client = crate::Client::from_client_service_channel(channel, Some(Arc::new(())));
        let agent_id = Uuid::from_u128(43);

        let mut host_events = client.subscribe_hosts().await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(1), host_events.recv())
            .await
            .expect("timed out waiting for routing snapshot")
            .unwrap();
        assert!(matches!(
            event,
            crate::routing::HostEvent::HostAdded { host }
                if host.id == Uuid::from_u128(1)
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
            })
            .await
            .unwrap();
        assert_eq!(created.id, agent_id);
        assert_eq!(created.host_id, Uuid::from_u128(1));

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
                    agent_type: "test-agent".to_string(),
                    io_protocols: vec![TEST_ECHO_V1.to_string()],
                    readonly: false,
                    args: Vec::new(),
                    created_at: chrono::Utc::now(),
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
                io_protocol: TEST_ECHO_V1.to_string(),
                payload: bytes::Bytes::from_static(b"hello"),
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
        for task in &services.tasks {
            task.abort();
        }
        let remote = remote_host(2);
        let agent_id = Uuid::from_u128(44);
        services
            .routing
            .apply_host_up(
                remote.clone(),
                Route::from_link(Link::new("missing").unwrap()),
                None,
            )
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
                    agent_type: "test-agent".to_string(),
                    io_protocols: vec![TEST_ECHO_V1.to_string()],
                    readonly: false,
                    args: Vec::new(),
                    created_at: chrono::Utc::now(),
                },
            })
            .await;

        let (channel, server_task) = services.open_in_process_client_channel();
        let client = crate::Client::from_client_service_channel(channel, Some(Arc::new(())));
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

    fn channel_from_transport(transport: TunnelTransport) -> Channel {
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
                                "TunnelTransport already consumed",
                            )
                        })
                }
            },
        ))
    }
}
