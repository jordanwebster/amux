//! Runtime for `RoutingService.Connect` host links.

use std::future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use futures_util::{Stream, StreamExt, stream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::connection::RouteRuntimeState;
use crate::protocol::{PROTOCOL_VERSION, ProtocolError, protocol_status, wire};
use crate::routing::{
    ConnectHandshake, ConnectHandshakeEvent, Host, HostUpOutcome, InboundRoutingEvent, Link,
    LinkCloseReason, LinkRegistry, LinkRole, Route, RoutingCore, RoutingEvent as CoreRoutingEvent,
    host_from_wire, host_to_wire, outbound_routing_message, protocol_error_goaway,
    protocol_error_hello_ack, should_send_routing_event_to_link, validate_remote_host,
    wire_routing_event_to_inbound,
};
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo};
use crate::tunnel::TunnelPool;
use crate::{HostId, audit};

type ResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, tonic::Status>> + Send + 'static>>;
type ConnectInputStream =
    Pin<Box<dyn Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static>>;
type ConnectorTask = JoinHandle<Result<(), tonic::Status>>;
type EstablishmentSender = oneshot::Sender<Result<Host, tonic::Status>>;
type EstablishmentReceiver = oneshot::Receiver<Result<Host, tonic::Status>>;

const ROUTING_AUTH_REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(300);
const ROUTING_AUTH_REAUTH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const ROUTING_AUTH_EXPIRED_DRAIN_TIMEOUT_MS: u32 = 0;
const ROUTING_CONNECT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedRoutingUser {
    pub(crate) user_id: Uuid,
    pub(crate) client_id: String,
    pub(crate) expires_at: SystemTime,
}

#[tonic::async_trait]
pub(crate) trait RoutingTokenAuthenticator: Send + Sync + 'static {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedRoutingUser, tonic::Status>;
}

#[tonic::async_trait]
impl<T> RoutingTokenAuthenticator for Arc<T>
where
    T: RoutingTokenAuthenticator + ?Sized,
{
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedRoutingUser, tonic::Status> {
        (**self).authenticate_token(token).await
    }
}

#[derive(Clone)]
pub(crate) struct RoutingAuthSession {
    user: AuthenticatedRoutingUser,
    authenticator: Arc<dyn RoutingTokenAuthenticator>,
    minimum_client_version: Option<String>,
}

impl RoutingAuthSession {
    pub(crate) fn new<T>(
        user: AuthenticatedRoutingUser,
        authenticator: T,
        minimum_client_version: Option<String>,
    ) -> Self
    where
        T: RoutingTokenAuthenticator,
    {
        Self {
            user,
            authenticator: Arc::new(authenticator),
            minimum_client_version,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RoutingConnectorToken {
    pub(crate) token: String,
    pub(crate) expires_at: SystemTime,
}

#[tonic::async_trait]
pub(crate) trait RoutingConnectorTokenRefresher: Send + Sync + 'static {
    async fn refresh_routing_token(&self) -> Result<RoutingConnectorToken, tonic::Status>;
}

#[derive(Clone)]
pub(crate) struct RoutingConnectorAuth {
    initial: RoutingConnectorToken,
    refresher: Arc<dyn RoutingConnectorTokenRefresher>,
}

impl RoutingConnectorAuth {
    pub(crate) fn new(
        initial: RoutingConnectorToken,
        refresher: Arc<dyn RoutingConnectorTokenRefresher>,
    ) -> Self {
        Self { initial, refresher }
    }
}

#[derive(Clone)]
pub(crate) struct RoutingConnectCtx {
    local_host: Host,
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
    links: Arc<LinkRegistry>,
    assigned_link: Option<Link>,
    auth_session: Option<RoutingAuthSession>,
    tls_peer: Option<Uuid>,
    route_runtime: RouteRuntimeState,
    direct_route_mode: DirectRouteMode,
}

impl RoutingConnectCtx {
    #[cfg(test)]
    pub(crate) fn new(
        local_host: Host,
        routing: Arc<RoutingCore>,
        tunnels: Arc<TunnelPool>,
        assigned_link: Link,
        route_runtime: RouteRuntimeState,
    ) -> Self {
        Self {
            local_host,
            routing,
            links: tunnels.link_registry(),
            tunnels,
            assigned_link: Some(assigned_link),
            auth_session: None,
            tls_peer: None,
            route_runtime,
            direct_route_mode: DirectRouteMode::RequireChannel,
        }
    }

    pub(crate) fn dynamic(
        local_host: Host,
        routing: Arc<RoutingCore>,
        tunnels: Arc<TunnelPool>,
        route_runtime: RouteRuntimeState,
    ) -> Self {
        Self {
            local_host,
            routing,
            links: tunnels.link_registry(),
            tunnels,
            assigned_link: None,
            auth_session: None,
            tls_peer: None,
            route_runtime,
            direct_route_mode: DirectRouteMode::RequireChannel,
        }
    }

    fn established(&self, link: Link) -> EstablishedConnectCtx {
        EstablishedConnectCtx {
            local_host_id: self.local_host.id,
            routing: self.routing.clone(),
            tunnels: self.tunnels.clone(),
            links: self.links.clone(),
            link,
            auth_session: self.auth_session.clone(),
            route_runtime: self.route_runtime.clone(),
            direct_route_mode: self.direct_route_mode,
            link_role: LinkRole::Peer,
        }
    }

    pub(crate) fn with_auth_session(mut self, auth_session: RoutingAuthSession) -> Self {
        self.auth_session = Some(auth_session);
        self
    }

    pub(crate) fn with_routing_only_direct_routes(mut self) -> Self {
        self.direct_route_mode = DirectRouteMode::RoutingOnly;
        self
    }

    fn with_tls_peer(mut self, tls_peer: Uuid) -> Self {
        self.tls_peer = Some(tls_peer);
        self
    }
}

#[derive(Clone, Copy)]
enum DirectRouteMode {
    RequireChannel,
    RoutingOnly,
}

#[derive(Clone)]
pub(crate) struct RoutingConnectorCtx {
    local_host: Host,
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
    links: Arc<LinkRegistry>,
    proposed_link: Link,
    expected_peer: Option<HostId>,
    route_runtime: RouteRuntimeState,
    link_role: LinkRole,
}

impl RoutingConnectorCtx {
    pub(crate) fn new(
        local_host: Host,
        routing: Arc<RoutingCore>,
        tunnels: Arc<TunnelPool>,
        proposed_link: Link,
        route_runtime: RouteRuntimeState,
    ) -> Self {
        Self {
            local_host,
            routing,
            links: tunnels.link_registry(),
            tunnels,
            proposed_link,
            expected_peer: None,
            route_runtime,
            link_role: LinkRole::Peer,
        }
    }

    pub(crate) fn with_expected_peer(mut self, expected_peer: HostId) -> Self {
        self.expected_peer = Some(expected_peer);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_link_role(mut self, link_role: LinkRole) -> Self {
        self.link_role = link_role;
        self
    }

    fn established(&self, link: Link) -> EstablishedConnectCtx {
        EstablishedConnectCtx {
            local_host_id: self.local_host.id,
            routing: self.routing.clone(),
            tunnels: self.tunnels.clone(),
            links: self.links.clone(),
            link,
            auth_session: None,
            route_runtime: self.route_runtime.clone(),
            direct_route_mode: DirectRouteMode::RequireChannel,
            link_role: self.link_role,
        }
    }
}

#[cfg(test)]
pub(crate) fn spawn_connector_to_channel(
    ctx: RoutingConnectorCtx,
    channel: Channel,
) -> ConnectorTask {
    spawn_connector_to_channel_with_authorization(ctx, channel, None, None)
}

pub(crate) fn spawn_connector_to_channel_with_establishment(
    ctx: RoutingConnectorCtx,
    channel: Channel,
) -> (ConnectorTask, EstablishmentReceiver) {
    let (established_tx, established_rx) = oneshot::channel();
    let task = spawn_connector_to_channel_with_authorization_and_signal(
        ctx,
        channel,
        None,
        None,
        Some(established_tx),
    );
    (task, established_rx)
}

#[cfg(test)]
pub(crate) fn spawn_connector_to_channel_with_bearer_token(
    ctx: RoutingConnectorCtx,
    channel: Channel,
    token: String,
) -> JoinHandle<Result<(), tonic::Status>> {
    spawn_connector_to_channel_with_authorization(
        ctx.with_link_role(LinkRole::CloudRelay),
        channel,
        Some(format!("Bearer {token}")),
        None,
    )
}

pub(crate) fn spawn_connector_to_channel_with_auth_and_establishment(
    ctx: RoutingConnectorCtx,
    channel: Channel,
    auth: RoutingConnectorAuth,
) -> (ConnectorTask, EstablishmentReceiver) {
    let (established_tx, established_rx) = oneshot::channel();
    let task = spawn_connector_to_channel_with_authorization_and_signal(
        ctx,
        channel,
        None,
        Some(auth),
        Some(established_tx),
    );
    (task, established_rx)
}

#[cfg(test)]
fn spawn_connector_to_channel_with_authorization(
    ctx: RoutingConnectorCtx,
    channel: Channel,
    authorization: Option<String>,
    connector_auth: Option<RoutingConnectorAuth>,
) -> ConnectorTask {
    spawn_connector_to_channel_with_authorization_and_signal(
        ctx,
        channel,
        authorization,
        connector_auth,
        None,
    )
}

fn spawn_connector_to_channel_with_authorization_and_signal(
    ctx: RoutingConnectorCtx,
    channel: Channel,
    authorization: Option<String>,
    connector_auth: Option<RoutingConnectorAuth>,
    established_tx: Option<EstablishmentSender>,
) -> ConnectorTask {
    tokio::spawn(async move {
        let direct_channel = channel.clone();
        let mut client = wire::routing_service_client::RoutingServiceClient::new(channel);
        let (out_tx, out_rx) = mpsc::channel(256);
        let request_stream = stream::unfold(out_rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });
        let mut request = tonic::Request::new(request_stream);
        let authorization = connector_auth
            .as_ref()
            .map(|auth| format!("Bearer {}", auth.initial.token))
            .or(authorization);
        if let Some(authorization) = authorization {
            let authorization = tonic::metadata::MetadataValue::try_from(authorization)
                .map_err(|_| tonic::Status::invalid_argument("invalid authorization metadata"))?;
            request
                .metadata_mut()
                .insert("authorization", authorization);
        }
        let response = match client.connect(request).await {
            Ok(response) => response,
            Err(status) => return connector_establishment_failed(established_tx, status),
        };
        run_connector_connect(
            ctx,
            response.into_inner(),
            out_tx,
            connector_auth,
            established_tx,
            Some(direct_channel),
        )
        .await
    })
}

#[derive(Clone)]
struct EstablishedConnectCtx {
    local_host_id: uuid::Uuid,
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
    links: Arc<LinkRegistry>,
    link: Link,
    auth_session: Option<RoutingAuthSession>,
    route_runtime: RouteRuntimeState,
    direct_route_mode: DirectRouteMode,
    link_role: LinkRole,
}

#[tonic::async_trait]
impl wire::routing_service_server::RoutingService for RoutingConnectCtx {
    type ConnectStream = ResponseStream<wire::pb::Message>;

    async fn connect(
        &self,
        request: tonic::Request<tonic::Streaming<wire::pb::Message>>,
    ) -> Result<tonic::Response<Self::ConnectStream>, tonic::Status> {
        let tls_peer = request
            .extensions()
            .get::<BoxedGrpcConnectInfo>()
            .and_then(|info| match &info.auth {
                BoxedGrpcAuth::TlsTrusted { peer } => Some(*peer),
                BoxedGrpcAuth::LocalTrusted | BoxedGrpcAuth::PreTrustPairing { .. } => None,
            });
        let ctx = match tls_peer {
            Some(peer) => self.clone().with_tls_peer(peer),
            None => self.clone(),
        };
        let rx = spawn_acceptor_connect(ctx, request.into_inner());
        Ok(tonic::Response::new(Box::pin(receiver_stream(rx))))
    }
}

fn spawn_acceptor_connect<S>(ctx: RoutingConnectCtx, input: S) -> mpsc::Receiver<wire::pb::Message>
where
    S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
{
    let (out_tx, out_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        run_acceptor_connect(ctx, input, out_tx, ROUTING_CONNECT_HELLO_TIMEOUT).await;
    });
    out_rx
}

#[cfg(test)]
fn spawn_connector_connect<S>(
    ctx: RoutingConnectorCtx,
    input: S,
) -> mpsc::Receiver<wire::pb::Message>
where
    S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
{
    let (out_tx, out_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let direct_channel =
            tonic::transport::Endpoint::from_static("http://unit-test").connect_lazy();
        let _ = run_connector_connect(ctx, input, out_tx, None, None, Some(direct_channel)).await;
    });
    out_rx
}

async fn run_acceptor_connect<S>(
    ctx: RoutingConnectCtx,
    input: S,
    out_tx: mpsc::Sender<wire::pb::Message>,
    hello_timeout: Duration,
) where
    S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
{
    let mut input: ConnectInputStream = Box::pin(input);
    let first = match tokio::time::timeout(hello_timeout, input.next()).await {
        Ok(Some(Ok(first))) => first,
        Ok(Some(Err(_))) | Ok(None) => return,
        Err(_) => {
            tracing::warn!("routing Connect stream timed out waiting for Hello");
            return;
        }
    };

    let mut handshake = ConnectHandshake::acceptor();
    let (peer_host, assigned_link) = match handshake.receive(first) {
        Ok(ConnectHandshakeEvent::Hello(hello)) => match accept_peer_hello(&ctx, hello).await {
            Ok(peer) => peer,
            Err(error) => {
                let _ = out_tx.send(error_hello_ack(error)).await;
                return;
            }
        },
        Ok(_) => {
            let _ = out_tx
                .send(protocol_error_hello_ack(
                    "unexpected handshake event while awaiting hello",
                ))
                .await;
            return;
        }
        Err(error) => {
            let _ = out_tx
                .send(protocol_error_hello_ack(error.to_string()))
                .await;
            return;
        }
    };

    if out_tx
        .send(accepted_hello_ack(&ctx, &assigned_link))
        .await
        .is_err()
    {
        cleanup_link(&ctx.established(assigned_link)).await;
        return;
    }
    if handshake.acceptor_ack_sent().is_err() {
        cleanup_link(&ctx.established(assigned_link)).await;
        return;
    }

    let _ = run_established_connect(
        ctx.established(assigned_link),
        EstablishedConnectArgs {
            handshake,
            input,
            out_tx,
            peer_host,
            connector_auth: None,
            established_tx: None,
            peer_route_stored: false,
            direct_channel: None,
        },
    )
    .await;
}

async fn run_connector_connect<S>(
    ctx: RoutingConnectorCtx,
    input: S,
    out_tx: mpsc::Sender<wire::pb::Message>,
    connector_auth: Option<RoutingConnectorAuth>,
    established_tx: Option<EstablishmentSender>,
    direct_channel: Option<Channel>,
) -> Result<(), tonic::Status>
where
    S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
{
    let mut input: ConnectInputStream = Box::pin(input);
    let mut handshake = ConnectHandshake::connector();
    if out_tx.send(connector_hello(&ctx)).await.is_err() {
        return connector_establishment_failed(
            established_tx,
            tonic::Status::unavailable("routing connect request stream closed before Hello"),
        );
    }

    let Some(first) = input.next().await else {
        return connector_establishment_failed(
            established_tx,
            tonic::Status::unavailable("routing connect response stream closed before HelloAck"),
        );
    };
    let first = match first {
        Ok(first) => first,
        Err(status) => return connector_establishment_failed(established_tx, status),
    };

    let (peer_host, assigned_link) = match handshake.receive(first) {
        Ok(ConnectHandshakeEvent::Accepted(accepted)) => {
            match accept_peer_hello_ack(&ctx, accepted).await {
                Ok(peer) => peer,
                Err(error) => {
                    let message = error.message.clone();
                    let _ = out_tx.send(protocol_error_goaway_from_error(error)).await;
                    return connector_establishment_failed(
                        established_tx,
                        tonic::Status::invalid_argument(message),
                    );
                }
            }
        }
        Ok(ConnectHandshakeEvent::Rejected(error)) => {
            return connector_establishment_failed(
                established_tx,
                protocol_status(wire::decode_protocol_error(error)),
            );
        }
        Ok(_) => {
            let message = "unexpected handshake event while awaiting hello_ack";
            let _ = out_tx.send(protocol_error_goaway(message)).await;
            return connector_establishment_failed(
                established_tx,
                tonic::Status::invalid_argument(message),
            );
        }
        Err(error) => {
            let message = error.to_string();
            let _ = out_tx.send(protocol_error_goaway(message.clone())).await;
            return connector_establishment_failed(
                established_tx,
                tonic::Status::invalid_argument(message),
            );
        }
    };

    run_established_connect(
        ctx.established(assigned_link),
        EstablishedConnectArgs {
            handshake,
            input,
            out_tx,
            peer_host,
            connector_auth,
            established_tx,
            peer_route_stored: false,
            direct_channel,
        },
    )
    .await
}

fn connector_establishment_failed<T>(
    established_tx: Option<EstablishmentSender>,
    status: tonic::Status,
) -> Result<T, tonic::Status> {
    let return_status = clone_status(&status);
    if let Some(established_tx) = established_tx {
        let _ = established_tx.send(Err(status));
    }
    Err(return_status)
}

fn clone_status(status: &tonic::Status) -> tonic::Status {
    let mut cloned = tonic::Status::with_details(
        status.code(),
        status.message().to_string(),
        Bytes::copy_from_slice(status.details()),
    );
    *cloned.metadata_mut() = status.metadata().clone();
    cloned
}

fn try_send_outbound(out_tx: &mpsc::Sender<wire::pb::Message>, message: wire::pb::Message) -> bool {
    out_tx.try_send(message).is_ok()
}

struct EstablishedConnectArgs {
    handshake: ConnectHandshake,
    input: ConnectInputStream,
    out_tx: mpsc::Sender<wire::pb::Message>,
    peer_host: Host,
    connector_auth: Option<RoutingConnectorAuth>,
    established_tx: Option<EstablishmentSender>,
    peer_route_stored: bool,
    direct_channel: Option<Channel>,
}

async fn run_established_connect(
    ctx: EstablishedConnectCtx,
    args: EstablishedConnectArgs,
) -> Result<(), tonic::Status> {
    let EstablishedConnectArgs {
        mut handshake,
        mut input,
        out_tx,
        peer_host,
        connector_auth,
        established_tx,
        peer_route_stored,
        direct_channel,
    } = args;

    debug_assert!(handshake.is_established());
    let link_role = if connector_auth.is_some() {
        LinkRole::CloudRelay
    } else {
        ctx.link_role
    };
    let mut link_close_rx = ctx
        .links
        .register_with_role(ctx.link.clone(), peer_host.id, out_tx.clone(), link_role)
        .await;
    let direct_channel_registered = if let Some(channel) = direct_channel {
        ctx.route_runtime
            .register(Route::from_link(ctx.link.clone()), channel)
            .await;
        true
    } else {
        false
    };
    let can_store_direct_route =
        direct_channel_registered || matches!(ctx.direct_route_mode, DirectRouteMode::RoutingOnly);
    if !peer_route_stored && can_store_direct_route {
        match store_direct_peer(
            &ctx.routing,
            ctx.local_host_id,
            &ctx.link,
            peer_host.clone(),
        )
        .await
        {
            Ok(DirectPeerStoreOutcome::Inserted) => {}
            Ok(DirectPeerStoreOutcome::AlreadyKnown) => {
                tracing::debug!(
                    peer_host_id = %peer_host.id,
                    link = %ctx.link,
                    "direct peer already reachable; keeping duplicate link established"
                );
            }
            Err(error) => {
                if let Some(established_tx) = established_tx {
                    let _ =
                        established_tx.send(Err(tonic::Status::invalid_argument(error.clone())));
                }
                let _ = try_send_outbound(&out_tx, protocol_error_goaway(error));
                cleanup_link(&ctx).await;
                return Ok(());
            }
        }
    } else if !peer_route_stored {
        tracing::debug!(
            link = %ctx.link,
            "not storing acceptor-only direct route without outbound channel"
        );
    }
    if let Some(established_tx) = established_tx {
        let _ = established_tx.send(Ok(peer_host.clone()));
    }
    let peer_host_id = peer_host.id;
    if !send_initial_routing_snapshot(&ctx, &out_tx, peer_host_id).await {
        cleanup_link(&ctx).await;
        return Ok(());
    }
    let mut acceptor_auth = ctx.auth_session.clone().map(EstablishedRoutingAuth::new);
    let mut connector_reauth = connector_auth.map(ConnectorReauthState::new);
    let mut drain_deadline = None;
    let mut draining = false;
    let mut close_status = None;

    loop {
        let auth_expiry_deadline = acceptor_auth
            .as_ref()
            .map(EstablishedRoutingAuth::expiry_deadline);
        let connector_refresh_deadline = connector_reauth
            .as_ref()
            .and_then(ConnectorReauthState::refresh_deadline);
        let connector_response_timeout = connector_reauth
            .as_ref()
            .and_then(ConnectorReauthState::response_timeout);
        tokio::select! {
            inbound = input.next() => {
                let Some(inbound) = inbound else {
                    break;
                };
                let message = match inbound {
                    Ok(message) => message,
                    Err(status) => {
                    let _ = try_send_outbound(
                        &out_tx,
                            protocol_error_goaway(status.to_string()),
                    );
                    break;
                    }
                };
                match handshake.receive(message) {
                    Ok(ConnectHandshakeEvent::PostHandshake(body)) => {
                        match handle_post_handshake_body(
                            &ctx,
                            &out_tx,
                            body,
                            draining,
                            acceptor_auth.as_mut(),
                            connector_reauth.as_mut(),
                        ).await {
                            PostHandshakeAction::Continue => {}
                            PostHandshakeAction::Close => break,
                            PostHandshakeAction::Drain { duration, status } => {
                                ctx.links.mark_draining(&ctx.link).await;
                                draining = true;
                                acceptor_auth = None;
                                connector_reauth = None;
                                close_status = close_status.or(status);
                                if duration.is_zero() {
                                    break;
                                }
                                let deadline = tokio::time::Instant::now() + duration;
                                drain_deadline = Some(
                                    drain_deadline
                                        .map(|current: tokio::time::Instant| current.min(deadline))
                                        .unwrap_or(deadline),
                                );
                            }
                        }
                    }
                    Ok(_) => {
                        if !try_send_outbound(
                            &out_tx,
                            protocol_error_goaway("unexpected handshake event after establishment"),
                        ) {
                            break;
                        }
                        break;
                    }
                    Err(error) => {
                        let _ = try_send_outbound(&out_tx, protocol_error_goaway(error.to_string()));
                        break;
                    }
                }
            }
            _ = maybe_sleep_until(drain_deadline), if drain_deadline.is_some() => {
                break;
            }
            _ = maybe_sleep_until(auth_expiry_deadline), if auth_expiry_deadline.is_some() => {
                audit::auth_jwt_failure("routing authorization expired");
                let _ = try_send_outbound(&out_tx, auth_expired_goaway());
                break;
            }
            _ = maybe_sleep_until(connector_refresh_deadline), if connector_refresh_deadline.is_some() => {
                let Some(connector_reauth) = connector_reauth.as_mut() else {
                    continue;
                };
                if let Err(status) = connector_reauth.send_refresh(&out_tx).await {
                    audit::auth_jwt_failure(&status);
                    let _ = try_send_outbound(&out_tx, protocol_error_goaway(status.to_string()));
                    close_status = Some(status);
                    break;
                }
            }
            _ = maybe_sleep_until(connector_response_timeout), if connector_response_timeout.is_some() => {
                let _ = try_send_outbound(
                    &out_tx,
                    protocol_error_goaway("routing reauth response timed out"),
                );
                audit::auth_jwt_failure("routing reauth response timed out");
                break;
            }
            reason = link_close_rx.recv() => {
                match reason {
                    Some(LinkCloseReason::OutgoingQueueFull) => {
                        close_status = Some(tonic::Status::resource_exhausted(
                            "routing link outgoing queue full",
                        ));
                    }
                    Some(LinkCloseReason::TrustReplaced) => {
                        close_status = Some(tonic::Status::permission_denied(
                            "peer trust was replaced",
                        ));
                    }
                    None => {
                        close_status = Some(tonic::Status::unavailable(
                            "routing link closed",
                        ));
                    }
                }
                break;
            }
        }
    }

    cleanup_link(&ctx).await;
    match close_status {
        Some(status) => Err(status),
        None => Ok(()),
    }
}

async fn accept_peer_hello(
    ctx: &RoutingConnectCtx,
    hello: wire::pb::Hello,
) -> Result<(Host, Link), wire::pb::Error> {
    if !hello
        .supported_protocol_versions
        .contains(&PROTOCOL_VERSION)
    {
        return Err(wire::encode_protocol_error(
            &ProtocolError::ProtocolMismatch {
                supported_versions: vec![PROTOCOL_VERSION],
                peer_supported_versions: hello.supported_protocol_versions,
            },
        ));
    }

    let host = hello
        .host
        .ok_or_else(|| "Hello.host is required".to_string())
        .and_then(|host| host_from_wire(host).map_err(|error| error.to_string()))
        .map_err(invalid_argument_error)?;
    validate_remote_host(&host).map_err(invalid_argument_error)?;
    if let Some(tls_peer) = ctx.tls_peer
        && host.id != tls_peer
    {
        return Err(invalid_argument_error(format!(
            "Hello.host_id {} does not match TLS peer {}",
            host.id, tls_peer
        )));
    }
    if let Some(auth_session) = &ctx.auth_session {
        validate_minimum_client_version(&host, auth_session)?;
    }
    if host.id == ctx.local_host.id {
        return Err(host_id_collision_error(format!(
            "peer host_id {} matches local host_id",
            host.id
        )));
    }
    let proposed_link_name = hello.proposed_link_name;
    let proposed_link = Link::new(proposed_link_name.clone()).map_err(|error| {
        wire::encode_protocol_error(&ProtocolError::InvalidLinkName {
            name: proposed_link_name,
            reason: error.to_string(),
        })
    })?;
    let assigned_link = match &ctx.assigned_link {
        Some(link) if ctx.routing.reserve_exact_link(link).await => link.clone(),
        Some(_) | None => ctx.routing.reserve_link(&proposed_link).await,
    };
    Ok((host, assigned_link))
}

fn validate_minimum_client_version(
    host: &Host,
    auth_session: &RoutingAuthSession,
) -> Result<(), wire::pb::Error> {
    let Some(minimum_version) = &auth_session.minimum_client_version else {
        return Ok(());
    };
    let reject = match (
        semver::Version::parse(&host.version),
        semver::Version::parse(minimum_version),
    ) {
        (Ok(client), Ok(minimum)) => client < minimum,
        _ => true,
    };
    if reject {
        tracing::warn!(
            client_id = %auth_session.user.client_id,
            client_version = %host.version,
            minimum_version = %minimum_version,
            "routing client version below minimum"
        );
        return Err(wire::encode_protocol_error(
            &ProtocolError::UpdateRequired {
                minimum_version: minimum_version.clone(),
                client_version: host.version.clone(),
            },
        ));
    }
    Ok(())
}

fn error_hello_ack(error: wire::pb::Error) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Error(error)),
        })),
    }
}

fn invalid_argument_error(message: impl Into<String>) -> wire::pb::Error {
    wire::encode_protocol_error(&ProtocolError::InvalidArgument {
        message: message.into(),
    })
}

fn host_id_collision_error(message: impl Into<String>) -> wire::pb::Error {
    wire::encode_protocol_error(&ProtocolError::AlreadyExists {
        message: message.into(),
    })
}

async fn accept_peer_hello_ack(
    ctx: &RoutingConnectorCtx,
    accepted: wire::pb::HelloAccepted,
) -> Result<(Host, Link), wire::pb::Error> {
    if accepted.protocol_version != PROTOCOL_VERSION {
        return Err(wire::encode_protocol_error(
            &ProtocolError::ProtocolMismatch {
                supported_versions: vec![PROTOCOL_VERSION],
                peer_supported_versions: vec![accepted.protocol_version],
            },
        ));
    }
    let assigned_link_name = accepted.assigned_link_name;
    let assigned_link = Link::new(assigned_link_name.clone()).map_err(|error| {
        wire::encode_protocol_error(&ProtocolError::InvalidLinkName {
            name: assigned_link_name,
            reason: error.to_string(),
        })
    })?;
    let host = accepted
        .host
        .ok_or_else(|| "HelloAccepted.host is required".to_string())
        .and_then(|host| host_from_wire(host).map_err(|error| error.to_string()))
        .map_err(invalid_argument_error)?;
    validate_remote_host(&host).map_err(invalid_argument_error)?;
    if let Some(expected_peer) = ctx.expected_peer
        && host.id != expected_peer
    {
        return Err(invalid_argument_error(format!(
            "HelloAccepted.host_id {} does not match expected peer {}",
            host.id, expected_peer
        )));
    }
    if host.id == ctx.local_host.id {
        return Err(host_id_collision_error(format!(
            "peer host_id {} matches local host_id",
            host.id
        )));
    }
    if !ctx.routing.reserve_exact_link(&assigned_link).await {
        return Err(host_id_collision_error(format!(
            "HelloAccepted.assigned_link_name `{assigned_link}` is already in use"
        )));
    }
    Ok((host, assigned_link))
}

enum DirectPeerStoreOutcome {
    Inserted,
    AlreadyKnown,
}

async fn store_direct_peer(
    routing: &RoutingCore,
    local_host_id: uuid::Uuid,
    link: &Link,
    host: Host,
) -> Result<DirectPeerStoreOutcome, String> {
    if host.id == local_host_id {
        return Err("peer host_id must not match local host_id".to_string());
    }

    let host_id = host.id;
    let route = Route::from_link(link.clone());
    match routing.apply_host_up(host, route, None).await {
        HostUpOutcome::Inserted => Ok(DirectPeerStoreOutcome::Inserted),
        HostUpOutcome::AlreadyKnown => {
            tracing::debug!(%host_id, "direct peer already reachable");
            Ok(DirectPeerStoreOutcome::AlreadyKnown)
        }
        HostUpOutcome::RejectedByCap => Err(format!(
            "routing host cap reached while storing direct peer {host_id}"
        )),
    }
}

fn connector_hello(ctx: &RoutingConnectorCtx) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            proposed_link_name: ctx.proposed_link.as_str().to_string(),
            host: Some(host_to_wire(&ctx.local_host)),
        })),
    }
}

fn accepted_hello_ack(ctx: &RoutingConnectCtx, assigned_link: &Link) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Accepted(
                wire::pb::HelloAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    assigned_link_name: assigned_link.as_str().to_string(),
                    host: Some(host_to_wire(&ctx.local_host)),
                },
            )),
        })),
    }
}

async fn send_initial_routing_snapshot(
    ctx: &EstablishedConnectCtx,
    out_tx: &mpsc::Sender<wire::pb::Message>,
    peer_host_id: uuid::Uuid,
) -> bool {
    let snapshot = ctx.routing.routing_events_snapshot().await;
    let mut snapshot_routes = Vec::new();
    for event in snapshot {
        if should_send_routing_event_to_link(&event, &ctx.link, Some(peer_host_id)) {
            if let CoreRoutingEvent::HostUp { host, route, .. } = &event {
                snapshot_routes.push((host.id, route.clone()));
            }
            if out_tx.try_send(outbound_routing_message(&event)).is_err() {
                return false;
            }
        }
    }
    if out_tx
        .try_send(wire::pb::Message {
            body: Some(wire::pb::message::Body::RoutingEvent(
                wire::pb::RoutingEvent {
                    event: Some(wire::pb::routing_event::Event::SnapshotComplete(
                        wire::pb::SnapshotComplete {},
                    )),
                },
            )),
        })
        .is_err()
    {
        return false;
    }
    ctx.links.activate(&ctx.link, snapshot_routes).await
}

enum PostHandshakeAction {
    Continue,
    Close,
    Drain {
        duration: Duration,
        status: Option<tonic::Status>,
    },
}

async fn handle_post_handshake_body(
    ctx: &EstablishedConnectCtx,
    out_tx: &mpsc::Sender<wire::pb::Message>,
    body: wire::pb::message::Body,
    draining: bool,
    acceptor_auth: Option<&mut EstablishedRoutingAuth>,
    connector_reauth: Option<&mut ConnectorReauthState>,
) -> PostHandshakeAction {
    if draining
        && !matches!(
            body,
            wire::pb::message::Body::TunnelFrame(_) | wire::pb::message::Body::Goaway(_)
        )
    {
        return PostHandshakeAction::Continue;
    }

    match body {
        wire::pb::message::Body::RoutingEvent(event) => {
            match wire_routing_event_to_inbound(event, &ctx.link) {
                Ok(InboundRoutingEvent::HostUp { host, route }) => {
                    if host.id == ctx.local_host_id {
                        let _ = try_send_outbound(
                            out_tx,
                            protocol_error_goaway(
                                "inbound HostUp host_id must not match local host_id",
                            ),
                        );
                        return PostHandshakeAction::Close;
                    }
                    ctx.routing
                        .apply_host_up(host, route, Some(ctx.link.clone()))
                        .await;
                    PostHandshakeAction::Continue
                }
                Ok(InboundRoutingEvent::HostDown { host_id, route }) => {
                    let removed = ctx
                        .routing
                        .apply_host_down(host_id, &route, Some(ctx.link.clone()))
                        .await;
                    if removed {
                        ctx.route_runtime.remove_route(&route).await;
                    }
                    PostHandshakeAction::Continue
                }
                Ok(InboundRoutingEvent::SnapshotComplete) => PostHandshakeAction::Continue,
                Ok(InboundRoutingEvent::RouteOverHopCap) => PostHandshakeAction::Continue,
                Err(error) => {
                    let _ = try_send_outbound(out_tx, protocol_error_goaway(error.to_string()));
                    PostHandshakeAction::Close
                }
            }
        }
        wire::pb::message::Body::TunnelFrame(frame) => {
            match ctx
                .tunnels
                .handle_inbound_frame_from_link(frame, Some(&ctx.link))
                .await
            {
                Ok(()) => PostHandshakeAction::Continue,
                Err(error) => {
                    let _ = try_send_outbound(out_tx, protocol_error_goaway(error.to_string()));
                    PostHandshakeAction::Close
                }
            }
        }
        wire::pb::message::Body::Reauth(reauth) => {
            if handle_reauth(acceptor_auth, out_tx, reauth).await {
                PostHandshakeAction::Continue
            } else {
                PostHandshakeAction::Close
            }
        }
        wire::pb::message::Body::ReauthAck(ack) => {
            if handle_reauth_ack(connector_reauth, out_tx, ack).await {
                PostHandshakeAction::Continue
            } else {
                PostHandshakeAction::Close
            }
        }
        wire::pb::message::Body::Goaway(goaway) => PostHandshakeAction::Drain {
            duration: goaway_drain_duration(&goaway),
            status: goaway_status(&goaway),
        },
        wire::pb::message::Body::Hello(_) | wire::pb::message::Body::HelloAck(_) => {
            unreachable!("handshake body should be rejected by ConnectHandshake")
        }
    }
}

fn goaway_status(goaway: &wire::pb::GoAway) -> Option<tonic::Status> {
    let reason = wire::pb::GoAwayReason::try_from(goaway.reason)
        .unwrap_or(wire::pb::GoAwayReason::Unspecified);
    if reason != wire::pb::GoAwayReason::UpdateRequired {
        return None;
    }
    Some(
        goaway
            .error
            .clone()
            .map(wire::decode_protocol_error)
            .map(protocol_status)
            .unwrap_or_else(|| tonic::Status::failed_precondition("amux update required")),
    )
}

fn goaway_drain_duration(goaway: &wire::pb::GoAway) -> Duration {
    let reason = wire::pb::GoAwayReason::try_from(goaway.reason)
        .unwrap_or(wire::pb::GoAwayReason::Unspecified);
    if reason == wire::pb::GoAwayReason::ProtocolError || goaway.drain_timeout_ms == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(u64::from(goaway.drain_timeout_ms))
    }
}

struct EstablishedRoutingAuth {
    user: AuthenticatedRoutingUser,
    authenticator: Arc<dyn RoutingTokenAuthenticator>,
}

impl EstablishedRoutingAuth {
    fn new(session: RoutingAuthSession) -> Self {
        Self {
            user: session.user,
            authenticator: session.authenticator,
        }
    }

    fn expiry_deadline(&self) -> tokio::time::Instant {
        instant_for_system_time(self.user.expires_at, Duration::ZERO)
    }
}

struct ConnectorReauthState {
    expires_at: SystemTime,
    pending_expires_at: Option<SystemTime>,
    awaiting_since: Option<tokio::time::Instant>,
    refresher: Arc<dyn RoutingConnectorTokenRefresher>,
}

impl ConnectorReauthState {
    fn new(auth: RoutingConnectorAuth) -> Self {
        Self {
            expires_at: auth.initial.expires_at,
            pending_expires_at: None,
            awaiting_since: None,
            refresher: auth.refresher,
        }
    }

    fn refresh_deadline(&self) -> Option<tokio::time::Instant> {
        self.awaiting_since
            .is_none()
            .then(|| instant_for_system_time(self.expires_at, ROUTING_AUTH_REFRESH_BEFORE_EXPIRY))
    }

    fn response_timeout(&self) -> Option<tokio::time::Instant> {
        self.awaiting_since
            .map(|since| since + ROUTING_AUTH_REAUTH_RESPONSE_TIMEOUT)
    }

    async fn send_refresh(
        &mut self,
        out_tx: &mpsc::Sender<wire::pb::Message>,
    ) -> Result<(), tonic::Status> {
        let token = self.refresher.refresh_routing_token().await?;
        self.pending_expires_at = Some(token.expires_at);
        try_send_outbound(
            out_tx,
            wire::pb::Message {
                body: Some(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                    auth_token: token.token,
                })),
            },
        )
        .then_some(())
        .ok_or_else(|| tonic::Status::unavailable("routing link closed during reauth"))?;
        self.awaiting_since = Some(tokio::time::Instant::now());
        Ok(())
    }

    fn handle_ack(&mut self, ack: wire::pb::ReauthAck) -> bool {
        let Some(outcome) = ack.outcome else {
            return false;
        };
        match outcome {
            wire::pb::reauth_ack::Outcome::Accepted(_) => {
                if let Some(expires_at) = self.pending_expires_at.take() {
                    self.expires_at = expires_at;
                }
                self.awaiting_since = None;
                true
            }
            wire::pb::reauth_ack::Outcome::Error(error) => {
                audit::auth_jwt_failure(format!("routing reauth rejected: {}", error.message));
                tracing::warn!(code = error.code, message = %error.message, "routing reauth rejected");
                false
            }
        }
    }
}

async fn handle_reauth(
    acceptor_auth: Option<&mut EstablishedRoutingAuth>,
    out_tx: &mpsc::Sender<wire::pb::Message>,
    reauth: wire::pb::Reauth,
) -> bool {
    let Some(auth) = acceptor_auth else {
        let _ = try_send_outbound(
            out_tx,
            protocol_error_goaway("routing reauth received on unauthenticated link"),
        );
        return false;
    };
    match auth
        .authenticator
        .authenticate_token(&reauth.auth_token)
        .await
    {
        Ok(user) if user.user_id == auth.user.user_id => {
            auth.user = user;
            try_send_outbound(out_tx, accepted_reauth_ack())
        }
        Ok(user) => {
            audit::auth_jwt_failure("routing reauth user mismatch");
            tracing::warn!(
                original_user_id = %auth.user.user_id,
                reauth_user_id = %user.user_id,
                "routing reauth user mismatch"
            );
            try_send_outbound(out_tx, unauthenticated_reauth_ack())
        }
        Err(status) => {
            audit::auth_jwt_failure(&status);
            tracing::warn!(error = %status, "routing reauth token validation failed");
            try_send_outbound(out_tx, unauthenticated_reauth_ack())
        }
    }
}

async fn handle_reauth_ack(
    connector_reauth: Option<&mut ConnectorReauthState>,
    out_tx: &mpsc::Sender<wire::pb::Message>,
    ack: wire::pb::ReauthAck,
) -> bool {
    match connector_reauth {
        Some(connector_reauth) => connector_reauth.handle_ack(ack),
        None => {
            let _ = try_send_outbound(
                out_tx,
                protocol_error_goaway("routing reauth_ack received without connector auth"),
            );
            false
        }
    }
}

fn accepted_reauth_ack() -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::ReauthAck(wire::pb::ReauthAck {
            outcome: Some(wire::pb::reauth_ack::Outcome::Accepted(wire::pb::Empty {})),
        })),
    }
}

fn unauthenticated_reauth_ack() -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::ReauthAck(wire::pb::ReauthAck {
            outcome: Some(wire::pb::reauth_ack::Outcome::Error(wire::pb::Error {
                code: wire::pb::ErrorCode::Unauthenticated as i32,
                message: "invalid routing authorization".to_string(),
                details: Vec::new(),
            })),
        })),
    }
}

fn auth_expired_goaway() -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Goaway(wire::pb::GoAway {
            reason: wire::pb::GoAwayReason::AuthExpired as i32,
            error: Some(wire::pb::Error {
                code: wire::pb::ErrorCode::Unauthenticated as i32,
                message: "routing authorization expired".to_string(),
                details: Vec::new(),
            }),
            drain_timeout_ms: ROUTING_AUTH_EXPIRED_DRAIN_TIMEOUT_MS,
        })),
    }
}

fn protocol_error_goaway_from_error(error: wire::pb::Error) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Goaway(wire::pb::GoAway {
            reason: wire::pb::GoAwayReason::ProtocolError as i32,
            error: Some(error),
            drain_timeout_ms: 0,
        })),
    }
}

async fn maybe_sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => future::pending().await,
    }
}

fn instant_for_system_time(time: SystemTime, early_by: Duration) -> tokio::time::Instant {
    let target = time.checked_sub(early_by).unwrap_or(time);
    let now = SystemTime::now();
    match target.duration_since(now) {
        Ok(delay) => tokio::time::Instant::now() + delay,
        Err(_) => tokio::time::Instant::now(),
    }
}

async fn cleanup_link(ctx: &EstablishedConnectCtx) {
    ctx.links.remove(&ctx.link).await;
    for event in ctx.routing.remove_link_routes(&ctx.link).await {
        if let CoreRoutingEvent::HostDown { route, .. } = event {
            ctx.route_runtime.remove_route(&route).await;
        }
    }
    ctx.routing.release_link(&ctx.link).await;
}

fn receiver_stream<T>(rx: mpsc::Receiver<T>) -> ResponseStream<T>
where
    T: Send + 'static,
{
    Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (Ok(item), rx))
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use std::time::{Duration, SystemTime};

    use hyper_util::rt::TokioIo;
    use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
    use tonic::codegen::http::Uri;
    use tonic::transport::Endpoint;
    use tower::service_fn;

    use super::*;
    use crate::HostId;
    use crate::connection::ConnectionManager;
    use crate::routing::{Capabilities, SupportedAgentType};

    fn link(name: &str) -> Link {
        Link::new(name).unwrap()
    }

    fn host(id: u128, name: &str) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: name.to_string(),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: vec![SupportedAgentType {
                    agent_type: "test-agent".to_string(),
                }],
            },
        }
    }

    fn route(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    fn route_to_wire(route: &[&str]) -> wire::pb::Route {
        wire::pb::Route {
            links: route.iter().map(|link| (*link).to_string()).collect(),
        }
    }

    fn message(body: wire::pb::message::Body) -> wire::pb::Message {
        wire::pb::Message { body: Some(body) }
    }

    fn hello(peer: &Host) -> wire::pb::Message {
        message(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            proposed_link_name: "peer".to_string(),
            host: Some(host_to_wire(peer)),
        }))
    }

    fn hello_with(
        peer: &Host,
        supported_protocol_versions: Vec<u32>,
        proposed_link_name: impl Into<String>,
    ) -> wire::pb::Message {
        message(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions,
            proposed_link_name: proposed_link_name.into(),
            host: Some(host_to_wire(peer)),
        }))
    }

    fn routing_host_up(host: &Host, route: &[&str]) -> wire::pb::Message {
        message(wire::pb::message::Body::RoutingEvent(
            wire::pb::RoutingEvent {
                event: Some(wire::pb::routing_event::Event::HostUp(wire::pb::HostUp {
                    host: Some(host_to_wire(host)),
                    route: Some(route_to_wire(route)),
                })),
            },
        ))
    }

    fn routing_snapshot_complete() -> wire::pb::Message {
        message(wire::pb::message::Body::RoutingEvent(
            wire::pb::RoutingEvent {
                event: Some(wire::pb::routing_event::Event::SnapshotComplete(
                    wire::pb::SnapshotComplete {},
                )),
            },
        ))
    }

    fn tunnel_frame(dst: &[&str], payload: &[u8]) -> wire::pb::Message {
        message(wire::pb::message::Body::TunnelFrame(
            wire::pb::TunnelFrame {
                dst: Some(route_to_wire(dst)),
                tunnel_id: Some(
                    crate::tunnel::TunnelId::from_parts(
                        HostId::from_u128(2),
                        uuid::Uuid::from_u128(3),
                    )
                    .into(),
                ),
                payload: payload.to_vec(),
            },
        ))
    }

    fn goaway(reason: wire::pb::GoAwayReason, drain_timeout_ms: u32) -> wire::pb::Message {
        message(wire::pb::message::Body::Goaway(wire::pb::GoAway {
            reason: reason as i32,
            error: None,
            drain_timeout_ms,
        }))
    }

    fn stream_from_rx(
        rx: mpsc::Receiver<wire::pb::Message>,
    ) -> Pin<Box<dyn Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send>> {
        Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (Ok(message), rx))
        }))
    }

    fn stream_from_result_rx(
        rx: mpsc::Receiver<Result<wire::pb::Message, tonic::Status>>,
    ) -> Pin<Box<dyn Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send>> {
        Box::pin(futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        }))
    }

    fn route_runtime(tunnels: &Arc<TunnelPool>) -> RouteRuntimeState {
        RouteRuntimeState::new(tunnels.clone())
    }

    async fn test_ctx() -> (RoutingConnectCtx, Arc<RoutingCore>, Arc<TunnelPool>) {
        let local = host(1, "local");
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let tunnels = Arc::new(TunnelPool::new(local.id, routing.clone(), incoming_tx));
        crate::routing::spawn_routing_event_fanout(routing.clone(), tunnels.link_registry()).await;
        let ctx = RoutingConnectCtx::new(
            local,
            routing.clone(),
            tunnels.clone(),
            link("peer"),
            route_runtime(&tunnels),
        )
        .with_routing_only_direct_routes();
        (ctx, routing, tunnels)
    }

    async fn connector_test_ctx() -> (RoutingConnectorCtx, Arc<RoutingCore>, Arc<TunnelPool>, Host)
    {
        let local = host(1, "local");
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let tunnels = Arc::new(TunnelPool::new(local.id, routing.clone(), incoming_tx));
        crate::routing::spawn_routing_event_fanout(routing.clone(), tunnels.link_registry()).await;
        let ctx = RoutingConnectorCtx::new(
            local.clone(),
            routing.clone(),
            tunnels.clone(),
            link("local"),
            route_runtime(&tunnels),
        );
        (ctx, routing, tunnels, local)
    }

    fn auth_user(user_id: u128, client_id: &str, expires_in: Duration) -> AuthenticatedRoutingUser {
        AuthenticatedRoutingUser {
            user_id: uuid::Uuid::from_u128(user_id),
            client_id: client_id.to_string(),
            expires_at: SystemTime::now() + expires_in,
        }
    }

    #[derive(Clone)]
    struct TestTokenAuthenticator {
        tokens: Arc<Vec<(String, AuthenticatedRoutingUser)>>,
    }

    impl TestTokenAuthenticator {
        fn new(tokens: Vec<(&str, AuthenticatedRoutingUser)>) -> Self {
            Self {
                tokens: Arc::new(
                    tokens
                        .into_iter()
                        .map(|(token, user)| (token.to_string(), user))
                        .collect(),
                ),
            }
        }
    }

    #[tonic::async_trait]
    impl RoutingTokenAuthenticator for TestTokenAuthenticator {
        async fn authenticate_token(
            &self,
            token: &str,
        ) -> Result<AuthenticatedRoutingUser, tonic::Status> {
            self.tokens
                .iter()
                .find_map(|(candidate, user)| (candidate == token).then(|| user.clone()))
                .ok_or_else(|| tonic::Status::unauthenticated("unknown token"))
        }
    }

    #[derive(Clone)]
    struct TestTokenRefresher {
        token: RoutingConnectorToken,
        calls: Arc<Mutex<usize>>,
    }

    #[tonic::async_trait]
    impl RoutingConnectorTokenRefresher for TestTokenRefresher {
        async fn refresh_routing_token(&self) -> Result<RoutingConnectorToken, tonic::Status> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.token.clone())
        }
    }

    fn accepted_ack(peer: &Host, assigned_link: &str) -> wire::pb::Message {
        accepted_ack_with(peer, assigned_link, PROTOCOL_VERSION)
    }

    fn accepted_ack_with(
        peer: &Host,
        assigned_link: &str,
        protocol_version: u32,
    ) -> wire::pb::Message {
        message(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Accepted(
                wire::pb::HelloAccepted {
                    protocol_version,
                    assigned_link_name: assigned_link.to_string(),
                    host: Some(host_to_wire(peer)),
                },
            )),
        }))
    }

    async fn recv_message(rx: &mut mpsc::Receiver<wire::pb::Message>) -> wire::pb::Message {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for RoutingService.Connect output")
            .expect("RoutingService.Connect output closed")
    }

    async fn wait_until<F, Fut>(mut condition: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if condition().await {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for condition");
    }

    async fn recv_forwarded_tunnel_frame(
        rx: &mut mpsc::Receiver<wire::pb::Message>,
    ) -> wire::pb::TunnelFrame {
        for _ in 0..2 {
            let message = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out waiting for forwarded TunnelFrame")
                .expect("forwarded TunnelFrame channel closed");
            match message.body {
                Some(wire::pb::message::Body::TunnelFrame(frame)) => return frame,
                Some(wire::pb::message::Body::RoutingEvent(_)) => continue,
                _ => panic!("expected forwarded TunnelFrame"),
            }
        }
        panic!("expected forwarded TunnelFrame");
    }

    async fn recv_forwarded_host_up(
        rx: &mut mpsc::Receiver<wire::pb::Message>,
    ) -> wire::pb::HostUp {
        let message = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for forwarded HostUp")
            .expect("forwarded HostUp channel closed");
        let Some(wire::pb::message::Body::RoutingEvent(wire::pb::RoutingEvent {
            event: Some(wire::pb::routing_event::Event::HostUp(host_up)),
        })) = message.body
        else {
            panic!("expected forwarded HostUp");
        };
        host_up
    }

    async fn establish(
        ctx: RoutingConnectCtx,
        peer: &Host,
    ) -> (
        mpsc::Sender<wire::pb::Message>,
        mpsc::Receiver<wire::pb::Message>,
    ) {
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(peer)).await.unwrap();
        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        assert_snapshot_complete(&recv_message(&mut output_rx).await);
        (input_tx, output_rx)
    }

    async fn establish_connector(
        ctx: RoutingConnectorCtx,
        peer: &Host,
    ) -> (
        mpsc::Sender<wire::pb::Message>,
        mpsc::Receiver<wire::pb::Message>,
    ) {
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);
        input_tx.send(accepted_ack(peer, "assigned")).await.unwrap();
        assert_snapshot_complete(&recv_message(&mut output_rx).await);
        (input_tx, output_rx)
    }

    fn spawn_connector_connect_with_auth<S>(
        ctx: RoutingConnectorCtx,
        input: S,
        auth: RoutingConnectorAuth,
    ) -> mpsc::Receiver<wire::pb::Message>
    where
        S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
    {
        let (out_tx, out_rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let direct_channel =
                tonic::transport::Endpoint::from_static("http://unit-test").connect_lazy();
            let _ =
                run_connector_connect(ctx, input, out_tx, Some(auth), None, Some(direct_channel))
                    .await;
        });
        out_rx
    }

    fn assert_connector_hello(message: &wire::pb::Message) {
        let Some(wire::pb::message::Body::Hello(hello)) = &message.body else {
            panic!("expected Hello");
        };
        assert_eq!(hello.supported_protocol_versions, vec![PROTOCOL_VERSION]);
        assert_eq!(hello.proposed_link_name, "local");
        assert!(hello.host.is_some());
    }

    fn assert_accepted_hello_ack(message: &wire::pb::Message) {
        let accepted = accepted_hello_ack_message(message);
        assert_eq!(accepted.protocol_version, PROTOCOL_VERSION);
        assert_eq!(accepted.assigned_link_name, "peer");
    }

    fn accepted_hello_ack_message(message: &wire::pb::Message) -> &wire::pb::HelloAccepted {
        let Some(wire::pb::message::Body::HelloAck(ack)) = &message.body else {
            panic!("expected HelloAck");
        };
        let Some(wire::pb::hello_ack::Outcome::Accepted(accepted)) = &ack.outcome else {
            panic!("expected accepted HelloAck");
        };
        accepted
    }

    fn assert_error_hello_ack(message: &wire::pb::Message) {
        assert!(matches!(
            &message.body,
            Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
                outcome: Some(wire::pb::hello_ack::Outcome::Error(_))
            }))
        ));
    }

    fn hello_ack_error(message: &wire::pb::Message) -> &wire::pb::Error {
        let Some(wire::pb::message::Body::HelloAck(ack)) = &message.body else {
            panic!("expected HelloAck");
        };
        let Some(wire::pb::hello_ack::Outcome::Error(error)) = &ack.outcome else {
            panic!("expected error HelloAck");
        };
        error
    }

    fn assert_snapshot_complete(message: &wire::pb::Message) {
        assert!(matches!(
            &message.body,
            Some(wire::pb::message::Body::RoutingEvent(
                wire::pb::RoutingEvent {
                    event: Some(wire::pb::routing_event::Event::SnapshotComplete(_))
                }
            ))
        ));
    }

    fn assert_protocol_goaway(message: &wire::pb::Message) {
        let Some(wire::pb::message::Body::Goaway(goaway)) = &message.body else {
            panic!("expected GoAway");
        };
        assert_eq!(goaway.reason, wire::pb::GoAwayReason::ProtocolError as i32);
        assert_eq!(goaway.drain_timeout_ms, 0);
    }

    fn protocol_goaway_error(message: &wire::pb::Message) -> &wire::pb::Error {
        let Some(wire::pb::message::Body::Goaway(goaway)) = &message.body else {
            panic!("expected GoAway");
        };
        assert_eq!(goaway.reason, wire::pb::GoAwayReason::ProtocolError as i32);
        assert_eq!(goaway.drain_timeout_ms, 0);
        goaway.error.as_ref().expect("expected GoAway error")
    }

    fn assert_auth_expired_goaway(message: &wire::pb::Message) {
        let Some(wire::pb::message::Body::Goaway(goaway)) = &message.body else {
            panic!("expected GoAway");
        };
        assert_eq!(goaway.reason, wire::pb::GoAwayReason::AuthExpired as i32);
        assert_eq!(goaway.drain_timeout_ms, 0);
        assert_eq!(
            goaway.error.as_ref().map(|error| error.code),
            Some(wire::pb::ErrorCode::Unauthenticated as i32)
        );
    }

    fn assert_accepted_reauth_ack(message: &wire::pb::Message) {
        assert!(matches!(
            &message.body,
            Some(wire::pb::message::Body::ReauthAck(wire::pb::ReauthAck {
                outcome: Some(wire::pb::reauth_ack::Outcome::Accepted(_))
            }))
        ));
    }

    fn assert_unauthenticated_reauth_ack(message: &wire::pb::Message) {
        let Some(wire::pb::message::Body::ReauthAck(ack)) = &message.body else {
            panic!("expected ReauthAck");
        };
        let Some(wire::pb::reauth_ack::Outcome::Error(error)) = &ack.outcome else {
            panic!("expected error ReauthAck");
        };
        assert_eq!(error.code, wire::pb::ErrorCode::Unauthenticated as i32);
    }

    fn assert_reauth_message(message: &wire::pb::Message, expected_token: &str) {
        let Some(wire::pb::message::Body::Reauth(reauth)) = &message.body else {
            panic!("expected Reauth");
        };
        assert_eq!(reauth.auth_token, expected_token);
    }

    #[tokio::test]
    async fn connect_accepts_hello_sends_ack_and_stores_direct_peer() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&peer)).await.unwrap();

        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        assert_snapshot_complete(&recv_message(&mut output_rx).await);

        let entry = routing.host_entry(peer.id).await.unwrap();
        assert_eq!(entry.host, peer);
        assert_eq!(entry.route, route(&["peer"]));

        drop(input_tx);
    }

    #[tokio::test]
    async fn acceptor_without_outbound_channel_does_not_store_direct_peer_by_default() {
        let local = host(1, "local");
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let tunnels = Arc::new(TunnelPool::new(local.id, routing.clone(), incoming_tx));
        crate::routing::spawn_routing_event_fanout(routing.clone(), tunnels.link_registry()).await;
        let ctx = RoutingConnectCtx::new(
            local,
            routing.clone(),
            tunnels.clone(),
            link("peer"),
            route_runtime(&tunnels),
        );
        let peer = host(2, "peer-host");

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&peer)).await.unwrap();

        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        assert_snapshot_complete(&recv_message(&mut output_rx).await);
        assert!(routing.host_entry(peer.id).await.is_none());

        drop(input_tx);
    }

    #[tokio::test]
    async fn acceptor_times_out_waiting_for_initial_hello() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let (out_tx, mut out_rx) = mpsc::channel(8);

        run_acceptor_connect(
            ctx,
            futures_util::stream::pending::<Result<wire::pb::Message, tonic::Status>>(),
            out_tx,
            Duration::from_millis(10),
        )
        .await;

        assert!(out_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn acceptor_rejects_hello_host_id_that_does_not_match_tls_peer() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let ctx = ctx.with_tls_peer(HostId::from_u128(2));
        let spoofed = host(3, "spoofed-peer");

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&spoofed)).await.unwrap();

        let ack = recv_message(&mut output_rx).await;
        assert_error_hello_ack(&ack);
        assert!(
            hello_ack_error(&ack)
                .message
                .contains("does not match TLS peer")
        );
        assert!(routing.host_entry(spoofed.id).await.is_none());
    }

    #[tokio::test]
    async fn authenticated_acceptor_accepts_reauth_for_same_user() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let initial_user = auth_user(100, "client-a", Duration::from_secs(3600));
        let refreshed_user = auth_user(100, "client-a", Duration::from_secs(7200));
        let authenticator = Arc::new(TestTokenAuthenticator::new(vec![(
            "token-b",
            refreshed_user,
        )]));
        let ctx = ctx.with_auth_session(RoutingAuthSession::new(initial_user, authenticator, None));

        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(message(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                auth_token: "token-b".to_string(),
            })))
            .await
            .unwrap();

        assert_accepted_reauth_ack(&recv_message(&mut output_rx).await);
        drop(input_tx);
    }

    #[tokio::test]
    async fn authenticated_acceptor_rejects_reauth_for_different_user() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let initial_user = auth_user(100, "client-a", Duration::from_secs(3600));
        let wrong_user = auth_user(200, "client-a", Duration::from_secs(7200));
        let authenticator = Arc::new(TestTokenAuthenticator::new(vec![("token-b", wrong_user)]));
        let ctx = ctx.with_auth_session(RoutingAuthSession::new(initial_user, authenticator, None));

        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(message(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                auth_token: "token-b".to_string(),
            })))
            .await
            .unwrap();

        assert_unauthenticated_reauth_ack(&recv_message(&mut output_rx).await);
        drop(input_tx);
    }

    #[tokio::test]
    async fn unauthenticated_acceptor_rejects_reauth() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");

        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(message(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                auth_token: "token-b".to_string(),
            })))
            .await
            .unwrap();

        let response = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&response);
        assert!(
            error
                .message
                .contains("reauth received on unauthenticated link")
        );
        drop(input_tx);
    }

    #[tokio::test]
    async fn unauthenticated_acceptor_rejects_reauth_ack() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");

        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(message(wire::pb::message::Body::ReauthAck(
                wire::pb::ReauthAck {
                    outcome: Some(wire::pb::reauth_ack::Outcome::Accepted(wire::pb::Empty {})),
                },
            )))
            .await
            .unwrap();

        let response = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&response);
        assert!(
            error
                .message
                .contains("reauth_ack received without connector auth")
        );
        drop(input_tx);
    }

    #[tokio::test]
    async fn authenticated_acceptor_closes_when_token_expires_without_reauth() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let initial_user = auth_user(100, "client-a", Duration::from_millis(25));
        let authenticator = Arc::new(TestTokenAuthenticator::new(Vec::new()));
        let ctx = ctx.with_auth_session(RoutingAuthSession::new(initial_user, authenticator, None));

        let (_input_tx, mut output_rx) = establish(ctx, &peer).await;

        assert_auth_expired_goaway(&recv_message(&mut output_rx).await);
    }

    #[tokio::test]
    async fn authenticated_acceptor_rejects_client_below_minimum_version() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let mut peer = host(2, "peer-host");
        peer.version = "1.0.0".to_string();
        let initial_user = auth_user(100, "client-a", Duration::from_secs(3600));
        let authenticator = Arc::new(TestTokenAuthenticator::new(Vec::new()));
        let ctx = ctx.with_auth_session(RoutingAuthSession::new(
            initial_user,
            authenticator,
            Some("2.0.0".to_string()),
        ));

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&peer)).await.unwrap();

        let response = recv_message(&mut output_rx).await;
        let error = hello_ack_error(&response);
        assert_eq!(error.code, wire::pb::ErrorCode::FailedPrecondition as i32);
        assert!(error.message.contains("minimum 2.0.0"));
        assert!(routing.host_entry(peer.id).await.is_none());
    }

    #[tokio::test]
    async fn acceptor_rejects_protocol_version_mismatch_with_structured_error() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx
            .send(hello_with(&peer, vec![PROTOCOL_VERSION + 1], "peer"))
            .await
            .unwrap();

        let response = recv_message(&mut output_rx).await;
        let error = hello_ack_error(&response);
        assert!(matches!(
            wire::decode_protocol_error(error.clone()),
            ProtocolError::ProtocolMismatch {
                supported_versions,
                peer_supported_versions
            } if supported_versions == vec![PROTOCOL_VERSION]
                && peer_supported_versions == vec![PROTOCOL_VERSION + 1]
        ));
        assert!(routing.host_entry(peer.id).await.is_none());
    }

    #[tokio::test]
    async fn acceptor_rejects_invalid_proposed_link_with_structured_error() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx
            .send(hello_with(&peer, vec![PROTOCOL_VERSION], "bad.link"))
            .await
            .unwrap();

        let response = recv_message(&mut output_rx).await;
        let error = hello_ack_error(&response);
        assert!(matches!(
            wire::decode_protocol_error(error.clone()),
            ProtocolError::InvalidLinkName { name, .. } if name == "bad.link"
        ));
        assert!(routing.host_entry(peer.id).await.is_none());
    }

    #[tokio::test]
    async fn connector_sends_reauth_before_token_expiry() {
        let (ctx, _routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let calls = Arc::new(Mutex::new(0));
        let refresher = Arc::new(TestTokenRefresher {
            token: RoutingConnectorToken {
                token: "token-b".to_string(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
            },
            calls: calls.clone(),
        });
        let auth = RoutingConnectorAuth::new(
            RoutingConnectorToken {
                token: "token-a".to_string(),
                expires_at: SystemTime::now(),
            },
            refresher,
        );

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect_with_auth(ctx, stream_from_rx(input_rx), auth);
        assert_connector_hello(&recv_message(&mut output_rx).await);
        input_tx
            .send(accepted_ack(&peer, "assigned"))
            .await
            .unwrap();
        assert_snapshot_complete(&recv_message(&mut output_rx).await);

        assert_reauth_message(&recv_message(&mut output_rx).await, "token-b");
        input_tx
            .send(message(wire::pb::message::Body::ReauthAck(
                wire::pb::ReauthAck {
                    outcome: Some(wire::pb::reauth_ack::Outcome::Accepted(wire::pb::Empty {})),
                },
            )))
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), 1);
        drop(input_tx);
    }

    #[tokio::test]
    async fn dynamic_acceptor_suffixes_busy_proposed_link_and_releases_on_close() {
        let local = host(1, "local");
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let tunnels = Arc::new(TunnelPool::new(local.id, routing.clone(), incoming_tx));
        let ctx = RoutingConnectCtx::dynamic(
            local,
            routing.clone(),
            tunnels.clone(),
            route_runtime(&tunnels),
        )
        .with_routing_only_direct_routes();
        let busy_link = link("peer");
        assert!(routing.reserve_exact_link(&busy_link).await);

        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&peer)).await.unwrap();

        let ack = recv_message(&mut output_rx).await;
        let accepted = accepted_hello_ack_message(&ack);
        assert_eq!(accepted.protocol_version, PROTOCOL_VERSION);
        assert_ne!(accepted.assigned_link_name, "peer");
        assert!(accepted.assigned_link_name.starts_with("peer-"));
        assert_snapshot_complete(&recv_message(&mut output_rx).await);

        let assigned_link = link(&accepted.assigned_link_name);
        let entry = routing.host_entry(peer.id).await.unwrap();
        assert_eq!(entry.route, Route::from_link(assigned_link.clone()));

        drop(input_tx);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if routing.host_entry(peer.id).await.is_none()
                    && routing.reserve_exact_link(&assigned_link).await
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for dynamic link cleanup");
    }

    #[tokio::test]
    async fn acceptor_allows_distinct_route_for_already_reachable_host() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let existing_route = route(&["existing"]);
        routing
            .apply_host_up(peer.clone(), existing_route.clone(), None)
            .await;

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&peer)).await.unwrap();

        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        assert_snapshot_complete(&recv_message(&mut output_rx).await);
        assert_eq!(
            routing.host_entry(peer.id).await.unwrap().route,
            existing_route
        );
        assert_eq!(
            routing
                .routing_events_snapshot()
                .await
                .into_iter()
                .filter_map(|event| match event {
                    CoreRoutingEvent::HostUp { host, route, .. } if host.id == peer.id => {
                        Some(route)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![existing_route, route(&["peer"])]
        );
        drop(input_tx);
    }

    #[tokio::test]
    async fn direct_peer_store_adds_distinct_route_for_already_reachable_host() {
        let (_ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let existing_route = route(&["existing"]);

        routing
            .apply_host_up(peer.clone(), existing_route.clone(), None)
            .await;

        let outcome =
            store_direct_peer(&routing, HostId::from_u128(1), &link("peer"), peer.clone())
                .await
                .unwrap();
        assert!(matches!(outcome, DirectPeerStoreOutcome::Inserted));
        assert_eq!(
            routing.host_entry(peer.id).await.unwrap().route,
            existing_route
        );
        assert_eq!(
            routing
                .routing_events_snapshot()
                .await
                .into_iter()
                .filter_map(|event| match event {
                    CoreRoutingEvent::HostUp { host, route, .. } if host.id == peer.id => {
                        Some(route)
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![existing_route, route(&["peer"])]
        );
    }

    #[tokio::test]
    async fn acceptor_rejects_semantically_invalid_remote_host() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let mut peer = host(2, "peer-host");
        peer.name.clear();
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&peer)).await.unwrap();

        let response = recv_message(&mut output_rx).await;
        let error = hello_ack_error(&response);
        assert_eq!(error.code, wire::pb::ErrorCode::InvalidArgument as i32);
        assert!(error.message.contains("host name must be non-empty"));
        assert!(routing.host_entry(peer.id).await.is_none());
    }

    #[tokio::test]
    async fn connect_applies_inbound_routing_events_with_incoming_link_origin() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");
        let (input_tx, _output_rx) = establish(ctx, &peer).await;

        input_tx
            .send(routing_host_up(&remote, &["remote-link"]))
            .await
            .unwrap();
        let entry = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(entry) = routing.host_entry(remote.id).await {
                    break entry;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for inbound HostUp to be stored");

        assert_eq!(entry.host, remote);
        assert_eq!(entry.route, route(&["peer", "remote-link"]));
    }

    #[tokio::test]
    async fn connect_forwards_live_routing_events_after_snapshot() {
        let (ctx, _routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let other = host(4, "other-host");
        let (input_tx, _output_rx) = establish(ctx, &peer).await;

        let (next_tx, mut next_rx) = mpsc::channel(4);
        let next_link = link("next");
        tunnels
            .link_registry()
            .register(next_link.clone(), HostId::from_u128(99), next_tx)
            .await;
        tunnels.link_registry().activate(&next_link, []).await;

        input_tx
            .send(routing_host_up(&other, &["other"]))
            .await
            .unwrap();

        let up = recv_forwarded_host_up(&mut next_rx).await;
        assert_eq!(up.host.unwrap().host_id, other.id.as_bytes());
        assert_eq!(up.route.unwrap().links, ["peer", "other"]);
    }

    #[tokio::test]
    async fn connect_rejects_bad_first_message_with_hello_ack_error() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));

        input_tx.send(routing_snapshot_complete()).await.unwrap();

        assert_error_hello_ack(&recv_message(&mut output_rx).await);
        assert!(routing.hosts_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn connect_dispatches_tunnel_frames_to_pool() {
        let (ctx, _routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (next_tx, mut next_rx) = mpsc::channel(4);
        let next_link = link("next");
        tunnels
            .link_registry()
            .register(next_link.clone(), HostId::from_u128(99), next_tx)
            .await;
        tunnels.link_registry().activate(&next_link, []).await;

        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(tunnel_frame(&["next", "target"], b"payload"))
            .await
            .unwrap();

        let frame = recv_forwarded_tunnel_frame(&mut next_rx).await;
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst.unwrap().links, ["target"]);
        assert!(frame.tunnel_id.is_some());
    }

    #[tokio::test]
    async fn oversized_tunnel_frame_sends_protocol_goaway_and_cleans_link() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        assert!(routing.host_entry(peer.id).await.is_some());

        let oversized = vec![0_u8; crate::tunnel::TUNNEL_FRAME_PAYLOAD_MAX + 1];
        input_tx
            .send(tunnel_frame(&["next"], &oversized))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert_eq!(error.code, wire::pb::ErrorCode::InvalidArgument as i32);
        assert!(error.message.contains("payload exceeds"));
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.host_entry(peer.id).await.is_none() }
        })
        .await;
    }

    #[tokio::test]
    async fn post_handshake_stream_error_sends_protocol_goaway_and_cleans_link() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_result_rx(input_rx));

        input_tx.send(Ok(hello(&peer))).await.unwrap();
        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        assert_snapshot_complete(&recv_message(&mut output_rx).await);
        assert!(routing.host_entry(peer.id).await.is_some());
        input_tx
            .send(Err(tonic::Status::resource_exhausted("message too large")))
            .await
            .unwrap();
        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert!(error.message.contains("message too large"));
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.host_entry(peer.id).await.is_none() }
        })
        .await;
    }

    #[tokio::test]
    async fn inbound_goaway_drains_before_cleanup_and_keeps_tunnel_frames_flowing() {
        let (ctx, routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let next_link = link("next");
        let (next_tx, mut next_rx) = mpsc::channel(4);
        tunnels
            .link_registry()
            .register(next_link.clone(), HostId::from_u128(99), next_tx)
            .await;
        tunnels.link_registry().activate(&next_link, []).await;

        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(goaway(wire::pb::GoAwayReason::UserShutdown, 75))
            .await
            .unwrap();

        wait_until(|| {
            let tunnels = tunnels.clone();
            async move { tunnels.link_registry().is_draining(&link("peer")).await }
        })
        .await;
        assert!(routing.host_entry(peer.id).await.is_some());
        let peer_link = link("peer");
        assert!(matches!(
            tunnels.channel_to(peer.id).await,
            Err(crate::tunnel::TunnelPoolError::LinkDraining { link }) if link == peer_link
        ));

        input_tx
            .send(tunnel_frame(&["next", "target"], b"payload"))
            .await
            .unwrap();
        let frame = recv_forwarded_tunnel_frame(&mut next_rx).await;
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst.unwrap().links, ["target"]);

        wait_until(|| {
            let routing = routing.clone();
            async move { routing.host_entry(peer.id).await.is_none() }
        })
        .await;
    }

    #[tokio::test]
    async fn connect_enqueues_host_up_before_later_peer_tunnel_frame() {
        let (ctx, _routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (next_tx, mut next_rx) = mpsc::channel(4);
        let next_link = link("next");
        tunnels
            .link_registry()
            .register(next_link.clone(), HostId::from_u128(99), next_tx)
            .await;
        tunnels.link_registry().activate(&next_link, []).await;

        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(tunnel_frame(&["next"], b"payload"))
            .await
            .unwrap();

        let host_up = recv_forwarded_host_up(&mut next_rx).await;
        assert_eq!(host_up.host.unwrap().host_id, peer.id.as_bytes());
        assert_eq!(host_up.route.unwrap().links, ["peer"]);

        let frame = recv_forwarded_tunnel_frame(&mut next_rx).await;
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst.unwrap().links, Vec::<String>::new());
        assert_eq!(
            crate::tunnel::TunnelId::try_from(frame.tunnel_id.unwrap())
                .unwrap()
                .initiator,
            peer.id
        );
    }

    #[tokio::test]
    async fn connect_cleans_link_routes_when_input_stream_closes() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        assert!(routing.host_entry(peer.id).await.is_some());

        drop(input_tx);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if routing.host_entry(peer.id).await.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for link route cleanup");
    }

    #[tokio::test]
    async fn connector_sends_hello_accepts_ack_and_stores_direct_peer() {
        let (ctx, routing, _tunnels, local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));

        let hello = recv_message(&mut output_rx).await;
        assert_connector_hello(&hello);
        let Some(wire::pb::message::Body::Hello(hello)) = hello.body else {
            panic!("expected Hello");
        };
        assert_eq!(hello.host.unwrap().host_id, local.id.as_bytes());

        input_tx
            .send(accepted_ack(&peer, "assigned"))
            .await
            .unwrap();
        assert_snapshot_complete(&recv_message(&mut output_rx).await);

        let entry = routing.host_entry(peer.id).await.unwrap();
        assert_eq!(entry.host, peer);
        assert_eq!(entry.route, route(&["assigned"]));
    }

    #[tokio::test]
    async fn connector_applies_inbound_routing_events_with_assigned_link_origin() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");
        let (input_tx, _output_rx) = establish_connector(ctx, &peer).await;

        input_tx
            .send(routing_host_up(&remote, &["remote-link"]))
            .await
            .unwrap();
        let entry = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(entry) = routing.host_entry(remote.id).await {
                    break entry;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for connector inbound HostUp to be stored");

        assert_eq!(entry.host, remote);
        assert_eq!(entry.route, route(&["assigned", "remote-link"]));
    }

    #[tokio::test]
    async fn connector_rejects_bad_first_acceptor_message_with_goaway() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx.send(routing_snapshot_complete()).await.unwrap();

        assert_protocol_goaway(&recv_message(&mut output_rx).await);
        assert!(routing.hosts_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn connector_rejects_protocol_version_mismatch_with_structured_goaway() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx
            .send(accepted_ack_with(&peer, "assigned", PROTOCOL_VERSION + 1))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert!(matches!(
            wire::decode_protocol_error(error.clone()),
            ProtocolError::ProtocolMismatch {
                supported_versions,
                peer_supported_versions
            } if supported_versions == vec![PROTOCOL_VERSION]
                && peer_supported_versions == vec![PROTOCOL_VERSION + 1]
        ));
        assert!(routing.hosts_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn connector_rejects_invalid_assigned_link_with_structured_goaway() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx
            .send(accepted_ack(&peer, "bad.link"))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert!(matches!(
            wire::decode_protocol_error(error.clone()),
            ProtocolError::InvalidLinkName { name, .. } if name == "bad.link"
        ));
        assert!(routing.hosts_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn connector_rejects_hello_accepted_host_id_that_does_not_match_expected_peer() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let ctx = ctx.with_expected_peer(HostId::from_u128(2));
        let spoofed = host(3, "spoofed-peer");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx
            .send(accepted_ack(&spoofed, "assigned"))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert!(matches!(
            wire::decode_protocol_error(error.clone()),
            ProtocolError::InvalidArgument { message }
                if message.contains("does not match expected peer")
        ));
        assert!(routing.hosts_snapshot().await.is_empty());
    }

    #[tokio::test]
    async fn connector_releases_assigned_link_when_accepted_host_is_invalid() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;

        let error = accept_peer_hello_ack(
            &ctx,
            wire::pb::HelloAccepted {
                protocol_version: PROTOCOL_VERSION,
                assigned_link_name: "assigned".to_string(),
                host: None,
            },
        )
        .await
        .unwrap_err();

        assert!(error.message.contains("HelloAccepted.host is required"));
        assert!(
            routing.reserve_exact_link(&link("assigned")).await,
            "assigned link leaked after invalid accepted host"
        );
    }

    #[tokio::test]
    async fn inbound_host_up_for_local_host_id_is_protocol_error() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let local_collision = host(1, "local-collision");
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;

        input_tx
            .send(routing_host_up(&local_collision, &["remote-link"]))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert!(error.message.contains("must not match local host_id"));
        assert!(routing.host_entry(local_collision.id).await.is_none());
    }

    #[tokio::test]
    async fn inbound_host_up_requires_semantically_valid_host() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let mut invalid_host = host(3, "invalid-host");
        invalid_host.name.clear();
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;

        input_tx
            .send(routing_host_up(&invalid_host, &["remote-link"]))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_goaway_error(&message);
        assert!(error.message.contains("host name must be non-empty"));
        assert!(routing.host_entry(invalid_host.id).await.is_none());
    }

    #[tokio::test]
    async fn connector_dispatches_tunnel_frames_to_pool() {
        let (ctx, _routing, tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (next_tx, mut next_rx) = mpsc::channel(4);
        tunnels
            .link_registry()
            .register(link("next"), HostId::from_u128(99), next_tx)
            .await;

        let (input_tx, _output_rx) = establish_connector(ctx, &peer).await;
        input_tx
            .send(tunnel_frame(&["next", "target"], b"payload"))
            .await
            .unwrap();

        let frame = recv_forwarded_tunnel_frame(&mut next_rx).await;
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst.unwrap().links, ["target"]);
        assert!(frame.tunnel_id.is_some());
    }

    #[tokio::test]
    async fn connector_cleans_link_routes_when_input_stream_closes() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, _output_rx) = establish_connector(ctx, &peer).await;
        assert!(routing.host_entry(peer.id).await.is_some());

        drop(input_tx);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if routing.host_entry(peer.id).await.is_none() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for connector link route cleanup");
    }

    #[tokio::test]
    async fn connector_to_channel_establishes_routing_service_over_tonic() {
        let acceptor_host = host(1, "acceptor");
        let acceptor_routing = Arc::new(RoutingCore::new());
        let (acceptor_incoming_tx, _acceptor_incoming_rx) = mpsc::channel(4);
        let acceptor_tunnels = Arc::new(TunnelPool::new(
            acceptor_host.id,
            acceptor_routing.clone(),
            acceptor_incoming_tx,
        ));
        let acceptor_ctx = RoutingConnectCtx::dynamic(
            acceptor_host.clone(),
            acceptor_routing.clone(),
            acceptor_tunnels.clone(),
            route_runtime(&acceptor_tunnels),
        )
        .with_routing_only_direct_routes();

        let connector_host = host(2, "connector");
        let connector_routing = Arc::new(RoutingCore::new());
        let (connector_incoming_tx, _connector_incoming_rx) = mpsc::channel(4);
        let connector_tunnels = Arc::new(TunnelPool::new(
            connector_host.id,
            connector_routing.clone(),
            connector_incoming_tx,
        ));
        let connector_route_runtime = route_runtime(&connector_tunnels);
        let connector_ctx = RoutingConnectorCtx::new(
            connector_host.clone(),
            connector_routing.clone(),
            connector_tunnels,
            link("connector"),
            connector_route_runtime.clone(),
        );

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let incoming =
            stream::once(async move { Ok::<_, std::io::Error>(TestTransport::new(server_io)) });
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(wire::routing_service_server::RoutingServiceServer::new(
                    acceptor_ctx,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        let connector_task =
            spawn_connector_to_channel(connector_ctx, channel_from_test_transport(client_io));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let acceptor_sees_connector = acceptor_routing
                    .host_entry(connector_host.id)
                    .await
                    .is_some();
                let connector_sees_acceptor = connector_routing
                    .host_entry(acceptor_host.id)
                    .await
                    .is_some();
                if acceptor_sees_connector && connector_sees_acceptor {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for tonic RoutingService.Connect establishment");

        let acceptor_entry = acceptor_routing
            .host_entry(connector_host.id)
            .await
            .unwrap();
        assert_eq!(acceptor_entry.route, route(&["connector"]));
        let connector_entry = connector_routing
            .host_entry(acceptor_host.id)
            .await
            .unwrap();
        assert_eq!(connector_entry.route, route(&["connector"]));
        assert!(
            connector_route_runtime
                .pool()
                .get(&route(&["connector"]))
                .await
                .is_some()
        );

        connector_task.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn connector_manager_rejects_cached_channel_after_inbound_goaway() {
        let acceptor_host = host(1, "acceptor");
        let acceptor_routing = Arc::new(RoutingCore::new());
        let (acceptor_incoming_tx, _acceptor_incoming_rx) = mpsc::channel(4);
        let acceptor_tunnels = Arc::new(TunnelPool::new(
            acceptor_host.id,
            acceptor_routing.clone(),
            acceptor_incoming_tx,
        ));
        let acceptor_ctx = RoutingConnectCtx::dynamic(
            acceptor_host.clone(),
            acceptor_routing.clone(),
            acceptor_tunnels.clone(),
            route_runtime(&acceptor_tunnels),
        )
        .with_routing_only_direct_routes();

        let connector_host = host(2, "connector");
        let connector_routing = Arc::new(RoutingCore::new());
        let (connector_incoming_tx, _connector_incoming_rx) = mpsc::channel(4);
        let connector_tunnels = Arc::new(TunnelPool::new(
            connector_host.id,
            connector_routing.clone(),
            connector_incoming_tx,
        ));
        let connector_manager = Arc::new(ConnectionManager::new(
            connector_routing.clone(),
            connector_tunnels.clone(),
        ));
        let connector_ctx = RoutingConnectorCtx::new(
            connector_host.clone(),
            connector_routing.clone(),
            connector_tunnels.clone(),
            link("connector"),
            connector_manager.route_runtime(),
        );

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let incoming =
            stream::once(async move { Ok::<_, std::io::Error>(TestTransport::new(server_io)) });
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(wire::routing_service_server::RoutingServiceServer::new(
                    acceptor_ctx,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        let connector_task =
            spawn_connector_to_channel(connector_ctx, channel_from_test_transport(client_io));
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if connector_routing
                    .host_entry(acceptor_host.id)
                    .await
                    .is_some()
                    && connector_manager
                        .pool()
                        .get(&route(&["connector"]))
                        .await
                        .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for connector route/channel");
        connector_manager
            .seed(connector_routing.routing_events_snapshot().await)
            .await;
        let _cached = connector_manager
            .channel_to(acceptor_host.id)
            .await
            .unwrap();
        let Some(acceptor_tx) = acceptor_tunnels
            .link_registry()
            .outgoing_writers()
            .await
            .into_iter()
            .next()
        else {
            panic!("expected acceptor routing writer");
        };

        acceptor_tx
            .send(goaway(wire::pb::GoAwayReason::UserShutdown, 200))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if connector_manager
                    .pool()
                    .get(&route(&["connector"]))
                    .await
                    .is_some()
                    && connector_tunnels
                        .link_registry()
                        .is_draining(&link("connector"))
                        .await
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for connector link drain");

        let error = connector_manager
            .channel_to(acceptor_host.id)
            .await
            .unwrap_err();

        assert!(
            matches!(error, crate::tunnel::TunnelPoolError::LinkDraining { link: observed } if observed == link("connector"))
        );
        connector_task.abort();
        server_task.abort();
    }

    #[tokio::test]
    async fn connector_to_channel_can_attach_bearer_metadata() {
        let acceptor_host = host(1, "acceptor");
        let acceptor_routing = Arc::new(RoutingCore::new());
        let (acceptor_incoming_tx, _acceptor_incoming_rx) = mpsc::channel(4);
        let acceptor_tunnels = Arc::new(TunnelPool::new(
            acceptor_host.id,
            acceptor_routing.clone(),
            acceptor_incoming_tx,
        ));
        let acceptor_ctx = RoutingConnectCtx::dynamic(
            acceptor_host.clone(),
            acceptor_routing.clone(),
            acceptor_tunnels.clone(),
            route_runtime(&acceptor_tunnels),
        )
        .with_routing_only_direct_routes();

        let connector_host = host(2, "connector");
        let connector_routing = Arc::new(RoutingCore::new());
        let (connector_incoming_tx, _connector_incoming_rx) = mpsc::channel(4);
        let connector_tunnels = Arc::new(TunnelPool::new(
            connector_host.id,
            connector_routing.clone(),
            connector_incoming_tx,
        ));
        let connector_ctx = RoutingConnectorCtx::new(
            connector_host.clone(),
            connector_routing.clone(),
            connector_tunnels.clone(),
            link("connector"),
            route_runtime(&connector_tunnels),
        );

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let incoming =
            stream::once(async move { Ok::<_, std::io::Error>(TestTransport::new(server_io)) });
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(wire::routing_service_server::RoutingServiceServer::new(
                    MetadataCheckingRoutingService {
                        inner: acceptor_ctx,
                        expected_authorization: "Bearer test-token",
                    },
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });

        let connector_task = spawn_connector_to_channel_with_bearer_token(
            connector_ctx,
            channel_from_test_transport(client_io),
            "test-token".to_string(),
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let acceptor_sees_connector = acceptor_routing
                    .host_entry(connector_host.id)
                    .await
                    .is_some();
                let connector_sees_acceptor = connector_routing
                    .host_entry(acceptor_host.id)
                    .await
                    .is_some();
                if acceptor_sees_connector && connector_sees_acceptor {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for metadata-authenticated RoutingService.Connect");

        assert!(!connector_task.is_finished());
        connector_task.abort();
        server_task.abort();
    }

    #[derive(Clone)]
    struct MetadataCheckingRoutingService {
        inner: RoutingConnectCtx,
        expected_authorization: &'static str,
    }

    #[tonic::async_trait]
    impl wire::routing_service_server::RoutingService for MetadataCheckingRoutingService {
        type ConnectStream =
            <RoutingConnectCtx as wire::routing_service_server::RoutingService>::ConnectStream;

        async fn connect(
            &self,
            request: tonic::Request<tonic::Streaming<wire::pb::Message>>,
        ) -> Result<tonic::Response<Self::ConnectStream>, tonic::Status> {
            let authorization = request
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok());
            if authorization != Some(self.expected_authorization) {
                return Err(tonic::Status::unauthenticated(
                    "missing expected authorization metadata",
                ));
            }
            <RoutingConnectCtx as wire::routing_service_server::RoutingService>::connect(
                &self.inner,
                request,
            )
            .await
        }
    }

    struct TestTransport {
        inner: DuplexStream,
    }

    impl TestTransport {
        fn new(inner: DuplexStream) -> Self {
            Self { inner }
        }
    }

    impl AsyncRead for TestTransport {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for TestTransport {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, std::io::Error>> {
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), std::io::Error>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    impl tonic::transport::server::Connected for TestTransport {
        type ConnectInfo = ();

        fn connect_info(&self) -> Self::ConnectInfo {}
    }

    fn channel_from_test_transport(transport: DuplexStream) -> Channel {
        let transport = Arc::new(Mutex::new(Some(TestTransport::new(transport))));
        Endpoint::from_static("http://routing-test").connect_with_connector_lazy(service_fn(
            move |_uri: Uri| {
                let transport = Arc::clone(&transport);
                async move {
                    transport
                        .lock()
                        .expect("test transport mutex poisoned")
                        .take()
                        .map(TokioIo::new)
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::AlreadyExists,
                                "test transport already consumed",
                            )
                        })
                }
            },
        ))
    }
}
