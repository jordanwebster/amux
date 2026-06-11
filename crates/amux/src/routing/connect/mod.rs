//! Runtime for `LinkService.Connect` host links.
//!
//! The bidi stream IS the link. The handshake exchanges identity, protocol
//! version, and the sender's current neighbor snapshot; everything after it
//! is a delta (`NeighborUp`/`NeighborDown`) or a `TunnelFrame`.

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
    ConnectHandshake, ConnectHandshakeEvent, Host, LinkCloseRequest, LinkId, LinkRegistry,
    LinkRole, RouteUpdateOutcome, RoutingCore, host_from_wire, host_to_wire,
    inbound_host_from_wire, neighbor_down_from_wire, neighbor_up_from_wire,
    protocol_error_hello_ack, protocol_error_link_close, validate_remote_host,
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

const LINK_AUTH_REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(300);
const LINK_AUTH_REAUTH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const LINK_CONNECT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedLinkUser {
    pub(crate) user_id: Uuid,
    pub(crate) client_id: String,
    pub(crate) expires_at: SystemTime,
}

#[tonic::async_trait]
pub(crate) trait LinkTokenAuthenticator: Send + Sync + 'static {
    async fn authenticate_token(&self, token: &str)
    -> Result<AuthenticatedLinkUser, tonic::Status>;
}

#[tonic::async_trait]
impl<T> LinkTokenAuthenticator for Arc<T>
where
    T: LinkTokenAuthenticator + ?Sized,
{
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        (**self).authenticate_token(token).await
    }
}

#[derive(Clone)]
pub(crate) struct LinkAuthSession {
    user: AuthenticatedLinkUser,
    authenticator: Arc<dyn LinkTokenAuthenticator>,
    minimum_client_version: Option<String>,
}

impl LinkAuthSession {
    pub(crate) fn new<T>(
        user: AuthenticatedLinkUser,
        authenticator: T,
        minimum_client_version: Option<String>,
    ) -> Self
    where
        T: LinkTokenAuthenticator,
    {
        Self {
            user,
            authenticator: Arc::new(authenticator),
            minimum_client_version,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LinkConnectorToken {
    pub(crate) token: String,
    pub(crate) expires_at: SystemTime,
}

#[tonic::async_trait]
pub(crate) trait LinkConnectorTokenRefresher: Send + Sync + 'static {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status>;
}

#[derive(Clone)]
pub(crate) struct LinkConnectorAuth {
    initial: LinkConnectorToken,
    refresher: Arc<dyn LinkConnectorTokenRefresher>,
}

impl LinkConnectorAuth {
    pub(crate) fn new(
        initial: LinkConnectorToken,
        refresher: Arc<dyn LinkConnectorTokenRefresher>,
    ) -> Self {
        Self { initial, refresher }
    }
}

/// Acceptor-side context: serves `LinkService.Connect`.
#[derive(Clone)]
pub(crate) struct LinkServiceCtx {
    local_host: Host,
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
    links: Arc<LinkRegistry>,
    auth_session: Option<LinkAuthSession>,
    tls_peer: Option<Uuid>,
    route_runtime: RouteRuntimeState,
}

impl LinkServiceCtx {
    pub(crate) fn new(
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
            auth_session: None,
            tls_peer: None,
            route_runtime,
        }
    }

    fn established(&self, link: LinkId) -> EstablishedConnectCtx {
        EstablishedConnectCtx {
            local_host_id: self.local_host.id,
            routing: self.routing.clone(),
            tunnels: self.tunnels.clone(),
            links: self.links.clone(),
            link,
            auth_session: self.auth_session.clone(),
            route_runtime: self.route_runtime.clone(),
            link_role: LinkRole::Peer,
        }
    }

    pub(crate) fn with_auth_session(mut self, auth_session: LinkAuthSession) -> Self {
        self.auth_session = Some(auth_session);
        self
    }

    fn with_tls_peer(mut self, tls_peer: Uuid) -> Self {
        self.tls_peer = Some(tls_peer);
        self
    }
}

/// Connector-side context: dials a peer's `LinkService.Connect`.
#[derive(Clone)]
pub(crate) struct LinkConnectorCtx {
    local_host: Host,
    routing: Arc<RoutingCore>,
    tunnels: Arc<TunnelPool>,
    links: Arc<LinkRegistry>,
    expected_peer: Option<HostId>,
    route_runtime: RouteRuntimeState,
    link_role: LinkRole,
}

impl LinkConnectorCtx {
    pub(crate) fn new(
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
            expected_peer: None,
            route_runtime,
            link_role: LinkRole::Peer,
        }
    }

    pub(crate) fn with_expected_peer(mut self, expected_peer: HostId) -> Self {
        self.expected_peer = Some(expected_peer);
        self
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) fn with_link_role(mut self, link_role: LinkRole) -> Self {
        self.link_role = link_role;
        self
    }

    fn established(&self, link: LinkId) -> EstablishedConnectCtx {
        EstablishedConnectCtx {
            local_host_id: self.local_host.id,
            routing: self.routing.clone(),
            tunnels: self.tunnels.clone(),
            links: self.links.clone(),
            link,
            auth_session: None,
            route_runtime: self.route_runtime.clone(),
            link_role: self.link_role,
        }
    }
}

#[cfg(test)]
pub(crate) fn spawn_connector_to_channel(ctx: LinkConnectorCtx, channel: Channel) -> ConnectorTask {
    spawn_connector_to_channel_with_authorization(ctx, channel, None, None)
}

pub(crate) fn spawn_connector_to_channel_with_establishment(
    ctx: LinkConnectorCtx,
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

#[cfg(any(test, feature = "testnet"))]
pub(crate) fn spawn_connector_to_channel_with_bearer_token(
    ctx: LinkConnectorCtx,
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
    ctx: LinkConnectorCtx,
    channel: Channel,
    auth: LinkConnectorAuth,
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

#[cfg(any(test, feature = "testnet"))]
fn spawn_connector_to_channel_with_authorization(
    ctx: LinkConnectorCtx,
    channel: Channel,
    authorization: Option<String>,
    connector_auth: Option<LinkConnectorAuth>,
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
    ctx: LinkConnectorCtx,
    channel: Channel,
    authorization: Option<String>,
    connector_auth: Option<LinkConnectorAuth>,
    established_tx: Option<EstablishmentSender>,
) -> ConnectorTask {
    tokio::spawn(async move {
        let direct_channel = channel.clone();
        let mut client = wire::link_service_client::LinkServiceClient::new(channel);
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
    link: LinkId,
    auth_session: Option<LinkAuthSession>,
    route_runtime: RouteRuntimeState,
    link_role: LinkRole,
}

#[tonic::async_trait]
impl wire::link_service_server::LinkService for LinkServiceCtx {
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

fn spawn_acceptor_connect<S>(ctx: LinkServiceCtx, input: S) -> mpsc::Receiver<wire::pb::Message>
where
    S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
{
    let (out_tx, out_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        run_acceptor_connect(ctx, input, out_tx, LINK_CONNECT_HELLO_TIMEOUT).await;
    });
    out_rx
}

#[cfg(test)]
fn spawn_connector_connect<S>(ctx: LinkConnectorCtx, input: S) -> mpsc::Receiver<wire::pb::Message>
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
    ctx: LinkServiceCtx,
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
            tracing::warn!("link Connect stream timed out waiting for Hello");
            return;
        }
    };

    let mut handshake = ConnectHandshake::acceptor();
    let (peer_host, peer_neighbors) = match handshake.receive(first) {
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

    // The snapshot is a field of the handshake: HelloAccepted carries the
    // neighbor set as of this moment; registration reconciles any change
    // that lands in between.
    let snapshot = ctx.links.neighbor_snapshot().await;
    if out_tx
        .send(accepted_hello_ack(&ctx, &snapshot, peer_host.id))
        .await
        .is_err()
    {
        return;
    }
    if handshake.acceptor_ack_sent().is_err() {
        return;
    }

    let link = LinkId::new(peer_host.id);
    let _ = run_established_connect(
        ctx.established(link),
        EstablishedConnectArgs {
            handshake,
            input,
            out_tx,
            peer_host,
            peer_neighbors,
            sent_snapshot: snapshot.into_iter().map(|host| host.id).collect(),
            connector_auth: None,
            established_tx: None,
            direct_channel: None,
        },
    )
    .await;
}

async fn run_connector_connect<S>(
    ctx: LinkConnectorCtx,
    input: S,
    out_tx: mpsc::Sender<wire::pb::Message>,
    connector_auth: Option<LinkConnectorAuth>,
    established_tx: Option<EstablishmentSender>,
    direct_channel: Option<Channel>,
) -> Result<(), tonic::Status>
where
    S: Stream<Item = Result<wire::pb::Message, tonic::Status>> + Send + 'static,
{
    let mut input: ConnectInputStream = Box::pin(input);
    let mut handshake = ConnectHandshake::connector();
    let snapshot = ctx.links.neighbor_snapshot().await;
    if out_tx.send(connector_hello(&ctx, &snapshot)).await.is_err() {
        return connector_establishment_failed(
            established_tx,
            tonic::Status::unavailable("link connect request stream closed before Hello"),
        );
    }

    let Some(first) = input.next().await else {
        return connector_establishment_failed(
            established_tx,
            tonic::Status::unavailable("link connect response stream closed before HelloAck"),
        );
    };
    let first = match first {
        Ok(first) => first,
        Err(status) => return connector_establishment_failed(established_tx, status),
    };

    let (peer_host, peer_neighbors) = match handshake.receive(first) {
        Ok(ConnectHandshakeEvent::Accepted(accepted)) => {
            match accept_peer_hello_ack(&ctx, accepted).await {
                Ok(peer) => peer,
                Err(error) => {
                    let message = error.message.clone();
                    let _ = out_tx
                        .send(protocol_error_link_close_from_error(error))
                        .await;
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
            let _ = out_tx.send(protocol_error_link_close(message)).await;
            return connector_establishment_failed(
                established_tx,
                tonic::Status::invalid_argument(message),
            );
        }
        Err(error) => {
            let message = error.to_string();
            let _ = out_tx
                .send(protocol_error_link_close(message.clone()))
                .await;
            return connector_establishment_failed(
                established_tx,
                tonic::Status::invalid_argument(message),
            );
        }
    };

    let link = LinkId::new(peer_host.id);
    run_established_connect(
        ctx.established(link),
        EstablishedConnectArgs {
            handshake,
            input,
            out_tx,
            peer_host,
            peer_neighbors,
            sent_snapshot: snapshot.into_iter().map(|host| host.id).collect(),
            connector_auth,
            established_tx,
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
    /// The peer's handshake snapshot: its direct neighbors at handshake time.
    peer_neighbors: Vec<Host>,
    /// The neighbor set our own handshake message advertised; registration
    /// reconciles it against the registry.
    sent_snapshot: Vec<HostId>,
    connector_auth: Option<LinkConnectorAuth>,
    established_tx: Option<EstablishmentSender>,
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
        peer_neighbors,
        sent_snapshot,
        connector_auth,
        established_tx,
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
        .register(
            ctx.link,
            peer_host.clone(),
            out_tx.clone(),
            link_role,
            &sent_snapshot,
        )
        .await;

    // Apply the peer's handshake snapshot as its adjacency claims.
    for neighbor in peer_neighbors {
        if neighbor.id == ctx.local_host_id || neighbor.id == peer_host.id {
            continue;
        }
        ctx.routing.apply_claim_up(peer_host.id, neighbor).await;
    }

    // Only the dialer holds an outbound channel on this link (the dual-path
    // discipline that dies with the every-call-is-a-tunnel chunk), so only
    // the dialer records a Direct route.
    let peer_host_id = peer_host.id;
    if let Some(channel) = direct_channel {
        ctx.route_runtime.register_direct(ctx.link, channel).await;
        match ctx
            .routing
            .apply_direct_up(peer_host.clone(), ctx.link)
            .await
        {
            RouteUpdateOutcome::Inserted | RouteUpdateOutcome::AlreadyKnown => {}
            RouteUpdateOutcome::Replacing => {
                tracing::debug!(
                    peer_host_id = %peer_host_id,
                    link = %ctx.link,
                    "direct link established during trust replacement; not recording a route"
                );
            }
            RouteUpdateOutcome::RejectedByCap => {
                let error =
                    format!("routing host cap reached while storing direct peer {peer_host_id}");
                if let Some(established_tx) = established_tx {
                    let _ =
                        established_tx.send(Err(tonic::Status::invalid_argument(error.clone())));
                }
                let _ = try_send_outbound(&out_tx, protocol_error_link_close(error));
                cleanup_link(&ctx).await;
                return Ok(());
            }
        }
    }
    if let Some(established_tx) = established_tx {
        let _ = established_tx.send(Ok(peer_host.clone()));
    }
    let mut acceptor_auth = ctx.auth_session.clone().map(EstablishedLinkAuth::new);
    let mut connector_reauth = connector_auth.map(ConnectorReauthState::new);
    let mut close_status = None;

    loop {
        let auth_expiry_deadline = acceptor_auth
            .as_ref()
            .map(EstablishedLinkAuth::expiry_deadline);
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
                            protocol_error_link_close(status.to_string()),
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
                            acceptor_auth.as_mut(),
                            connector_reauth.as_mut(),
                        ).await {
                            PostHandshakeAction::Continue => {}
                            PostHandshakeAction::Close => break,
                            PostHandshakeAction::LinkClosed { status } => {
                                close_status = close_status.or(status);
                                break;
                            }
                        }
                    }
                    Ok(_) => {
                        if !try_send_outbound(
                            &out_tx,
                            protocol_error_link_close("unexpected handshake event after establishment"),
                        ) {
                            break;
                        }
                        break;
                    }
                    Err(error) => {
                        let _ = try_send_outbound(&out_tx, protocol_error_link_close(error.to_string()));
                        break;
                    }
                }
            }
            _ = maybe_sleep_until(auth_expiry_deadline), if auth_expiry_deadline.is_some() => {
                audit::auth_jwt_failure("link authorization expired");
                let _ = try_send_outbound(&out_tx, auth_expired_link_close());
                break;
            }
            _ = maybe_sleep_until(connector_refresh_deadline), if connector_refresh_deadline.is_some() => {
                let Some(connector_reauth) = connector_reauth.as_mut() else {
                    continue;
                };
                if let Err(status) = connector_reauth.send_refresh(&out_tx).await {
                    audit::auth_jwt_failure(&status);
                    let _ = try_send_outbound(&out_tx, protocol_error_link_close(status.to_string()));
                    close_status = Some(status);
                    break;
                }
            }
            _ = maybe_sleep_until(connector_response_timeout), if connector_response_timeout.is_some() => {
                let _ = try_send_outbound(
                    &out_tx,
                    protocol_error_link_close("link reauth response timed out"),
                );
                audit::auth_jwt_failure("link reauth response timed out");
                break;
            }
            request = link_close_rx.recv() => {
                match request {
                    Some(LinkCloseRequest::OutgoingQueueFull) => {
                        close_status = Some(tonic::Status::resource_exhausted(
                            "link outgoing queue full",
                        ));
                    }
                    Some(LinkCloseRequest::TrustReplaced) => {
                        close_status = Some(tonic::Status::permission_denied(
                            "peer trust was replaced",
                        ));
                    }
                    None => {
                        close_status = Some(tonic::Status::unavailable(
                            "link closed",
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
    ctx: &LinkServiceCtx,
    hello: wire::pb::Hello,
) -> Result<(Host, Vec<Host>), wire::pb::Error> {
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
    let neighbors = neighbors_from_wire(hello.neighbors)?;
    Ok((host, neighbors))
}

fn validate_minimum_client_version(
    host: &Host,
    auth_session: &LinkAuthSession,
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
            "link client version below minimum"
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
    ctx: &LinkConnectorCtx,
    accepted: wire::pb::HelloAccepted,
) -> Result<(Host, Vec<Host>), wire::pb::Error> {
    if accepted.protocol_version != PROTOCOL_VERSION {
        return Err(wire::encode_protocol_error(
            &ProtocolError::ProtocolMismatch {
                supported_versions: vec![PROTOCOL_VERSION],
                peer_supported_versions: vec![accepted.protocol_version],
            },
        ));
    }
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
    let neighbors = neighbors_from_wire(accepted.neighbors)?;
    Ok((host, neighbors))
}

fn neighbors_from_wire(neighbors: Vec<wire::pb::Host>) -> Result<Vec<Host>, wire::pb::Error> {
    neighbors
        .into_iter()
        .map(|host| {
            inbound_host_from_wire(host, "handshake neighbor")
                .map_err(|error| invalid_argument_error(error.to_string()))
        })
        .collect()
}

fn connector_hello(ctx: &LinkConnectorCtx, snapshot: &[Host]) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            host: Some(host_to_wire(&ctx.local_host)),
            neighbors: snapshot.iter().map(host_to_wire).collect(),
        })),
    }
}

fn accepted_hello_ack(
    ctx: &LinkServiceCtx,
    snapshot: &[Host],
    peer_host_id: HostId,
) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Accepted(
                wire::pb::HelloAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    host: Some(host_to_wire(&ctx.local_host)),
                    neighbors: snapshot
                        .iter()
                        .filter(|host| host.id != peer_host_id)
                        .map(host_to_wire)
                        .collect(),
                },
            )),
        })),
    }
}

enum PostHandshakeAction {
    Continue,
    Close,
    /// The peer declared the link closed (`LinkClose`): stop immediately.
    LinkClosed {
        status: Option<tonic::Status>,
    },
}

async fn handle_post_handshake_body(
    ctx: &EstablishedConnectCtx,
    out_tx: &mpsc::Sender<wire::pb::Message>,
    body: wire::pb::message::Body,
    acceptor_auth: Option<&mut EstablishedLinkAuth>,
    connector_reauth: Option<&mut ConnectorReauthState>,
) -> PostHandshakeAction {
    match body {
        wire::pb::message::Body::NeighborUp(event) => match neighbor_up_from_wire(event) {
            Ok(host) => {
                // A claim about ourselves or about the claimant says nothing
                // we don't already know from the link itself.
                if host.id != ctx.local_host_id && host.id != ctx.link.peer() {
                    ctx.routing.apply_claim_up(ctx.link.peer(), host).await;
                }
                PostHandshakeAction::Continue
            }
            Err(error) => {
                let _ = try_send_outbound(out_tx, protocol_error_link_close(error.to_string()));
                PostHandshakeAction::Close
            }
        },
        wire::pb::message::Body::NeighborDown(event) => match neighbor_down_from_wire(event) {
            Ok(host_id) => {
                ctx.routing.apply_claim_down(ctx.link.peer(), host_id).await;
                PostHandshakeAction::Continue
            }
            Err(error) => {
                let _ = try_send_outbound(out_tx, protocol_error_link_close(error.to_string()));
                PostHandshakeAction::Close
            }
        },
        wire::pb::message::Body::TunnelFrame(frame) => {
            match ctx
                .tunnels
                .handle_inbound_frame_from_link(frame, &ctx.link)
                .await
            {
                Ok(()) => PostHandshakeAction::Continue,
                Err(error) => {
                    let _ = try_send_outbound(out_tx, protocol_error_link_close(error.to_string()));
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
        wire::pb::message::Body::LinkClose(close) => PostHandshakeAction::LinkClosed {
            status: link_close_status(&close),
        },
        wire::pb::message::Body::Hello(_) | wire::pb::message::Body::HelloAck(_) => {
            unreachable!("handshake body should be rejected by ConnectHandshake")
        }
    }
}

fn link_close_status(close: &wire::pb::LinkClose) -> Option<tonic::Status> {
    let reason = wire::pb::LinkCloseReason::try_from(close.reason)
        .unwrap_or(wire::pb::LinkCloseReason::Unspecified);
    if reason != wire::pb::LinkCloseReason::UpdateRequired {
        return None;
    }
    Some(
        close
            .error
            .clone()
            .map(wire::decode_protocol_error)
            .map(protocol_status)
            .unwrap_or_else(|| tonic::Status::failed_precondition("amux update required")),
    )
}

struct EstablishedLinkAuth {
    user: AuthenticatedLinkUser,
    authenticator: Arc<dyn LinkTokenAuthenticator>,
}

impl EstablishedLinkAuth {
    fn new(session: LinkAuthSession) -> Self {
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
    refresher: Arc<dyn LinkConnectorTokenRefresher>,
}

impl ConnectorReauthState {
    fn new(auth: LinkConnectorAuth) -> Self {
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
            .then(|| instant_for_system_time(self.expires_at, LINK_AUTH_REFRESH_BEFORE_EXPIRY))
    }

    fn response_timeout(&self) -> Option<tokio::time::Instant> {
        self.awaiting_since
            .map(|since| since + LINK_AUTH_REAUTH_RESPONSE_TIMEOUT)
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
        .ok_or_else(|| tonic::Status::unavailable("link closed during reauth"))?;
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
                audit::auth_jwt_failure(format!("link reauth rejected: {}", error.message));
                tracing::warn!(code = error.code, message = %error.message, "link reauth rejected");
                false
            }
        }
    }
}

async fn handle_reauth(
    acceptor_auth: Option<&mut EstablishedLinkAuth>,
    out_tx: &mpsc::Sender<wire::pb::Message>,
    reauth: wire::pb::Reauth,
) -> bool {
    let Some(auth) = acceptor_auth else {
        let _ = try_send_outbound(
            out_tx,
            protocol_error_link_close("reauth received on unauthenticated link"),
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
            audit::auth_jwt_failure("link reauth user mismatch");
            tracing::warn!(
                original_user_id = %auth.user.user_id,
                reauth_user_id = %user.user_id,
                "link reauth user mismatch"
            );
            try_send_outbound(out_tx, unauthenticated_reauth_ack())
        }
        Err(status) => {
            audit::auth_jwt_failure(&status);
            tracing::warn!(error = %status, "link reauth token validation failed");
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
                protocol_error_link_close("reauth_ack received without connector auth"),
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
                message: "invalid link authorization".to_string(),
                details: Vec::new(),
            })),
        })),
    }
}

fn auth_expired_link_close() -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::LinkClose(wire::pb::LinkClose {
            reason: wire::pb::LinkCloseReason::AuthExpired as i32,
            error: Some(wire::pb::Error {
                code: wire::pb::ErrorCode::Unauthenticated as i32,
                message: "link authorization expired".to_string(),
                details: Vec::new(),
            }),
        })),
    }
}

fn protocol_error_link_close_from_error(error: wire::pb::Error) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::LinkClose(wire::pb::LinkClose {
            reason: wire::pb::LinkCloseReason::ProtocolError as i32,
            error: Some(error),
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

/// Tears down everything the link carried: the registry entry (which
/// advertises `NeighborDown` if it was the peer's last link), every tunnel
/// pinned to the link, the Direct route, and — when no link to the peer
/// remains — every adjacency claim the peer ever made.
async fn cleanup_link(ctx: &EstablishedConnectCtx) {
    let peer = ctx.link.peer();
    ctx.links.remove(&ctx.link).await;
    ctx.tunnels.remove_link(&ctx.link).await;
    ctx.routing.apply_direct_down(ctx.link).await;
    ctx.route_runtime.remove_direct(ctx.link).await;
    if ctx.links.link_to_peer(peer).await.is_none() {
        ctx.routing.remove_relay_claims(peer).await;
    }
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
    use crate::routing::{Capabilities, Route, SupportedAgentType};

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

    fn message(body: wire::pb::message::Body) -> wire::pb::Message {
        wire::pb::Message { body: Some(body) }
    }

    fn hello(peer: &Host) -> wire::pb::Message {
        hello_with_neighbors(peer, &[])
    }

    fn hello_with_neighbors(peer: &Host, neighbors: &[Host]) -> wire::pb::Message {
        message(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            host: Some(host_to_wire(peer)),
            neighbors: neighbors.iter().map(host_to_wire).collect(),
        }))
    }

    fn hello_with_versions(
        peer: &Host,
        supported_protocol_versions: Vec<u32>,
    ) -> wire::pb::Message {
        message(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions,
            host: Some(host_to_wire(peer)),
            neighbors: Vec::new(),
        }))
    }

    fn neighbor_up(host: &Host) -> wire::pb::Message {
        message(wire::pb::message::Body::NeighborUp(wire::pb::NeighborUp {
            host: Some(host_to_wire(host)),
        }))
    }

    fn neighbor_down(host_id: HostId) -> wire::pb::Message {
        message(wire::pb::message::Body::NeighborDown(
            wire::pb::NeighborDown {
                host_id: host_id.as_bytes().to_vec(),
                reason: None,
            },
        ))
    }

    fn tunnel_frame(dst: HostId, payload: &[u8]) -> wire::pb::Message {
        message(wire::pb::message::Body::TunnelFrame(
            wire::pb::TunnelFrame {
                dst: dst.as_bytes().to_vec(),
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

    fn link_close(reason: wire::pb::LinkCloseReason) -> wire::pb::Message {
        message(wire::pb::message::Body::LinkClose(wire::pb::LinkClose {
            reason: reason as i32,
            error: None,
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

    async fn test_ctx() -> (LinkServiceCtx, Arc<RoutingCore>, Arc<TunnelPool>) {
        let local = host(1, "local");
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let tunnels = Arc::new(TunnelPool::new(local.id, routing.clone(), incoming_tx));
        let ctx = LinkServiceCtx::new(
            local,
            routing.clone(),
            tunnels.clone(),
            route_runtime(&tunnels),
        );
        (ctx, routing, tunnels)
    }

    async fn connector_test_ctx() -> (LinkConnectorCtx, Arc<RoutingCore>, Arc<TunnelPool>, Host) {
        let local = host(1, "local");
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(4);
        let tunnels = Arc::new(TunnelPool::new(local.id, routing.clone(), incoming_tx));
        let ctx = LinkConnectorCtx::new(
            local.clone(),
            routing.clone(),
            tunnels.clone(),
            route_runtime(&tunnels),
        );
        (ctx, routing, tunnels, local)
    }

    fn auth_user(user_id: u128, client_id: &str, expires_in: Duration) -> AuthenticatedLinkUser {
        AuthenticatedLinkUser {
            user_id: uuid::Uuid::from_u128(user_id),
            client_id: client_id.to_string(),
            expires_at: SystemTime::now() + expires_in,
        }
    }

    #[derive(Clone)]
    struct TestTokenAuthenticator {
        tokens: Arc<Vec<(String, AuthenticatedLinkUser)>>,
    }

    impl TestTokenAuthenticator {
        fn new(tokens: Vec<(&str, AuthenticatedLinkUser)>) -> Self {
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
    impl LinkTokenAuthenticator for TestTokenAuthenticator {
        async fn authenticate_token(
            &self,
            token: &str,
        ) -> Result<AuthenticatedLinkUser, tonic::Status> {
            self.tokens
                .iter()
                .find_map(|(candidate, user)| (candidate == token).then(|| user.clone()))
                .ok_or_else(|| tonic::Status::unauthenticated("unknown token"))
        }
    }

    #[derive(Clone)]
    struct TestTokenRefresher {
        token: LinkConnectorToken,
        calls: Arc<Mutex<usize>>,
    }

    #[tonic::async_trait]
    impl LinkConnectorTokenRefresher for TestTokenRefresher {
        async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.token.clone())
        }
    }

    fn accepted_ack(peer: &Host) -> wire::pb::Message {
        accepted_ack_with(peer, PROTOCOL_VERSION, &[])
    }

    fn accepted_ack_with(
        peer: &Host,
        protocol_version: u32,
        neighbors: &[Host],
    ) -> wire::pb::Message {
        message(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Accepted(
                wire::pb::HelloAccepted {
                    protocol_version,
                    host: Some(host_to_wire(peer)),
                    neighbors: neighbors.iter().map(host_to_wire).collect(),
                },
            )),
        }))
    }

    async fn recv_message(rx: &mut mpsc::Receiver<wire::pb::Message>) -> wire::pb::Message {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for LinkService.Connect output")
            .expect("LinkService.Connect output closed")
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
        for _ in 0..4 {
            let message = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out waiting for forwarded TunnelFrame")
                .expect("forwarded TunnelFrame channel closed");
            match message.body {
                Some(wire::pb::message::Body::TunnelFrame(frame)) => return frame,
                Some(wire::pb::message::Body::NeighborUp(_))
                | Some(wire::pb::message::Body::NeighborDown(_)) => continue,
                _ => panic!("expected forwarded TunnelFrame"),
            }
        }
        panic!("expected forwarded TunnelFrame");
    }

    async fn recv_neighbor_up(rx: &mut mpsc::Receiver<wire::pb::Message>) -> wire::pb::NeighborUp {
        let message = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for NeighborUp")
            .expect("NeighborUp channel closed");
        let Some(wire::pb::message::Body::NeighborUp(up)) = message.body else {
            panic!("expected NeighborUp, got {message:?}");
        };
        up
    }

    async fn establish(
        ctx: LinkServiceCtx,
        peer: &Host,
    ) -> (
        mpsc::Sender<wire::pb::Message>,
        mpsc::Receiver<wire::pb::Message>,
    ) {
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(peer)).await.unwrap();
        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        (input_tx, output_rx)
    }

    async fn establish_connector(
        ctx: LinkConnectorCtx,
        peer: &Host,
    ) -> (
        mpsc::Sender<wire::pb::Message>,
        mpsc::Receiver<wire::pb::Message>,
    ) {
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);
        input_tx.send(accepted_ack(peer)).await.unwrap();
        (input_tx, output_rx)
    }

    fn spawn_connector_connect_with_auth<S>(
        ctx: LinkConnectorCtx,
        input: S,
        auth: LinkConnectorAuth,
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
        assert!(hello.host.is_some());
    }

    fn assert_accepted_hello_ack(message: &wire::pb::Message) {
        let accepted = accepted_hello_ack_message(message);
        assert_eq!(accepted.protocol_version, PROTOCOL_VERSION);
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

    fn assert_protocol_link_close(message: &wire::pb::Message) {
        let Some(wire::pb::message::Body::LinkClose(close)) = &message.body else {
            panic!("expected LinkClose");
        };
        assert_eq!(
            close.reason,
            wire::pb::LinkCloseReason::ProtocolError as i32
        );
    }

    fn protocol_link_close_error(message: &wire::pb::Message) -> &wire::pb::Error {
        let Some(wire::pb::message::Body::LinkClose(close)) = &message.body else {
            panic!("expected LinkClose");
        };
        assert_eq!(
            close.reason,
            wire::pb::LinkCloseReason::ProtocolError as i32
        );
        close.error.as_ref().expect("expected LinkClose error")
    }

    fn assert_auth_expired_link_close(message: &wire::pb::Message) {
        let Some(wire::pb::message::Body::LinkClose(close)) = &message.body else {
            panic!("expected LinkClose");
        };
        assert_eq!(close.reason, wire::pb::LinkCloseReason::AuthExpired as i32);
        assert_eq!(
            close.error.as_ref().map(|error| error.code),
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

    /// The acceptor accepts a Hello and applies its neighbor snapshot as the
    /// peer's adjacency claims — but, holding no outbound channel on the
    /// link, records no Direct route to the dialer itself.
    #[tokio::test]
    async fn acceptor_applies_handshake_snapshot_but_stores_no_direct_route() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx
            .send(hello_with_neighbors(&peer, std::slice::from_ref(&remote)))
            .await
            .unwrap();

        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await == Some(Route::Via(peer.id)) }
        })
        .await;
        assert_eq!(routing.route_to(peer.id).await, None);

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
        let ctx = ctx.with_auth_session(LinkAuthSession::new(initial_user, authenticator, None));

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
        let ctx = ctx.with_auth_session(LinkAuthSession::new(initial_user, authenticator, None));

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
        let error = protocol_link_close_error(&response);
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
        let error = protocol_link_close_error(&response);
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
        let ctx = ctx.with_auth_session(LinkAuthSession::new(initial_user, authenticator, None));

        let (_input_tx, mut output_rx) = establish(ctx, &peer).await;

        assert_auth_expired_link_close(&recv_message(&mut output_rx).await);
    }

    #[tokio::test]
    async fn authenticated_acceptor_rejects_client_below_minimum_version() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let mut peer = host(2, "peer-host");
        peer.version = "1.0.0".to_string();
        let initial_user = auth_user(100, "client-a", Duration::from_secs(3600));
        let authenticator = Arc::new(TestTokenAuthenticator::new(Vec::new()));
        let ctx = ctx.with_auth_session(LinkAuthSession::new(
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
            .send(hello_with_versions(&peer, vec![PROTOCOL_VERSION + 1]))
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
    async fn connector_sends_reauth_before_token_expiry() {
        let (ctx, _routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let calls = Arc::new(Mutex::new(0));
        let refresher = Arc::new(TestTokenRefresher {
            token: LinkConnectorToken {
                token: "token-b".to_string(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
            },
            calls: calls.clone(),
        });
        let auth = LinkConnectorAuth::new(
            LinkConnectorToken {
                token: "token-a".to_string(),
                expires_at: SystemTime::now(),
            },
            refresher,
        );

        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect_with_auth(ctx, stream_from_rx(input_rx), auth);
        assert_connector_hello(&recv_message(&mut output_rx).await);
        input_tx.send(accepted_ack(&peer)).await.unwrap();

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

    /// D14: the snapshot is a field of the handshake. A second link
    /// established after the first learns the existing neighbor in its
    /// HelloAccepted, not from any separate snapshot phase.
    #[tokio::test]
    async fn hello_ack_carries_the_current_neighbor_snapshot() {
        let (ctx, _routing, _tunnels) = test_ctx().await;
        let first_peer = host(2, "first-peer");
        let (_first_tx, _first_rx) = establish(ctx.clone(), &first_peer).await;

        let second_peer = host(3, "second-peer");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_rx(input_rx));
        input_tx.send(hello(&second_peer)).await.unwrap();

        let ack = recv_message(&mut output_rx).await;
        let accepted = accepted_hello_ack_message(&ack);
        assert_eq!(
            accepted
                .neighbors
                .iter()
                .map(|host| host.host_id.clone())
                .collect::<Vec<_>>(),
            vec![first_peer.id.as_bytes().to_vec()]
        );
        drop(input_tx);
    }

    #[tokio::test]
    async fn inbound_neighbor_up_records_an_adjacency_claim() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");
        let (input_tx, _output_rx) = establish(ctx, &peer).await;

        input_tx.send(neighbor_up(&remote)).await.unwrap();

        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await == Some(Route::Via(peer.id)) }
        })
        .await;
        drop(input_tx);
    }

    #[tokio::test]
    async fn inbound_neighbor_down_withdraws_the_claim() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");
        let (input_tx, _output_rx) = establish(ctx, &peer).await;

        input_tx.send(neighbor_up(&remote)).await.unwrap();
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_some() }
        })
        .await;

        input_tx.send(neighbor_down(remote.id)).await.unwrap();
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_none() }
        })
        .await;
        drop(input_tx);
    }

    /// Rule 1 on the outbound side: a claim learned from one neighbor is
    /// never re-advertised to another. The other link hears nothing.
    #[tokio::test]
    async fn claims_are_not_re_advertised_to_other_links() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let first_peer = host(2, "first-peer");
        let second_peer = host(3, "second-peer");
        let (_first_tx, mut first_rx) = establish(ctx.clone(), &first_peer).await;
        let (second_tx, _second_rx) = establish(ctx, &second_peer).await;

        // The first link learns about the second link coming up (adjacency).
        let up = recv_neighbor_up(&mut first_rx).await;
        assert_eq!(up.host.unwrap().host_id, second_peer.id.as_bytes().to_vec());

        // The second peer claims adjacency to a remote host…
        let remote = host(4, "remote-host");
        second_tx.send(neighbor_up(&remote)).await.unwrap();
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_some() }
        })
        .await;

        // …and the first link must hear nothing about it.
        assert!(
            first_rx.try_recv().is_err(),
            "learned claims must never be re-advertised"
        );
    }

    /// A peer's claim about *us* says nothing we don't already know from
    /// the link itself; it is ignored, not a protocol violation.
    #[tokio::test]
    async fn claims_about_the_local_host_are_ignored() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let local_self = host(1, "local");
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;

        input_tx.send(neighbor_up(&local_self)).await.unwrap();
        input_tx
            .send(neighbor_up(&host(3, "real-claim")))
            .await
            .unwrap();

        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(HostId::from_u128(3)).await.is_some() }
        })
        .await;
        assert!(routing.route_to(local_self.id).await.is_none());
        assert!(output_rx.try_recv().is_err(), "the link stays up");
        drop(input_tx);
    }

    #[tokio::test]
    async fn inbound_neighbor_up_requires_semantically_valid_host() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let mut invalid_host = host(3, "invalid-host");
        invalid_host.name.clear();
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;

        input_tx.send(neighbor_up(&invalid_host)).await.unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_link_close_error(&message);
        assert!(error.message.contains("host name must be non-empty"));
        assert!(routing.host_entry(invalid_host.id).await.is_none());
    }

    /// Rule 2: a frame addressed to a host we hold a direct link to is
    /// forwarded out that link.
    #[tokio::test]
    async fn frames_for_a_direct_neighbor_are_forwarded() {
        let (ctx, _routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let target = host(9, "target-host");

        let (target_tx, mut target_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(
                crate::routing::LinkId::new(target.id),
                target.clone(),
                target_tx,
                LinkRole::Peer,
                &[],
            )
            .await;

        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        input_tx
            .send(tunnel_frame(target.id, b"payload"))
            .await
            .unwrap();

        let frame = recv_forwarded_tunnel_frame(&mut target_rx).await;
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst, target.id.as_bytes().to_vec());
        assert!(frame.tunnel_id.is_some());
    }

    /// Rule 2's other half: no direct link to dst, no forwarding — the
    /// frame is dropped without disturbing the link.
    #[tokio::test]
    async fn frames_for_an_unknown_destination_are_dropped() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        assert!(routing.host_entry(peer.id).await.is_none());

        input_tx
            .send(tunnel_frame(HostId::from_u128(77), b"to-nowhere"))
            .await
            .unwrap();
        input_tx
            .send(neighbor_up(&host(3, "still-alive")))
            .await
            .unwrap();

        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(HostId::from_u128(3)).await.is_some() }
        })
        .await;
        assert!(output_rx.try_recv().is_err(), "the link stays up");
        drop(input_tx);
    }

    #[tokio::test]
    async fn oversized_tunnel_frame_sends_protocol_link_close_and_cleans_link() {
        let (ctx, _routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, mut output_rx) = establish(ctx, &peer).await;
        wait_until(|| {
            let tunnels = tunnels.clone();
            async move {
                tunnels
                    .link_registry()
                    .link_to_peer(peer.id)
                    .await
                    .is_some()
            }
        })
        .await;

        let oversized = vec![0_u8; crate::tunnel::TUNNEL_FRAME_PAYLOAD_MAX + 1];
        input_tx
            .send(tunnel_frame(HostId::from_u128(9), &oversized))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_link_close_error(&message);
        assert_eq!(error.code, wire::pb::ErrorCode::InvalidArgument as i32);
        assert!(error.message.contains("payload exceeds"));
        wait_until(|| {
            let tunnels = tunnels.clone();
            async move {
                tunnels
                    .link_registry()
                    .link_to_peer(peer.id)
                    .await
                    .is_none()
            }
        })
        .await;
    }

    #[tokio::test]
    async fn post_handshake_stream_error_sends_protocol_link_close_and_cleans_link() {
        let (ctx, _routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_acceptor_connect(ctx, stream_from_result_rx(input_rx));

        input_tx.send(Ok(hello(&peer))).await.unwrap();
        assert_accepted_hello_ack(&recv_message(&mut output_rx).await);
        wait_until(|| {
            let tunnels = tunnels.clone();
            async move {
                tunnels
                    .link_registry()
                    .link_to_peer(peer.id)
                    .await
                    .is_some()
            }
        })
        .await;
        input_tx
            .send(Err(tonic::Status::resource_exhausted("message too large")))
            .await
            .unwrap();
        let message = recv_message(&mut output_rx).await;
        let error = protocol_link_close_error(&message);
        assert!(error.message.contains("message too large"));
        wait_until(|| {
            let tunnels = tunnels.clone();
            async move {
                tunnels
                    .link_registry()
                    .link_to_peer(peer.id)
                    .await
                    .is_none()
            }
        })
        .await;
    }

    /// D11: an inbound `LinkClose` means "the link is closed now" — the
    /// handler tears the link down immediately, including the claims the
    /// peer had made.
    #[tokio::test]
    async fn inbound_link_close_tears_the_link_down_immediately() {
        let (ctx, routing, tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");

        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        input_tx.send(neighbor_up(&remote)).await.unwrap();
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_some() }
        })
        .await;

        input_tx
            .send(link_close(wire::pb::LinkCloseReason::UserShutdown))
            .await
            .unwrap();

        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_none() }
        })
        .await;
        wait_until(|| {
            let tunnels = tunnels.clone();
            async move {
                tunnels
                    .link_registry()
                    .link_to_peer(peer.id)
                    .await
                    .is_none()
            }
        })
        .await;
    }

    #[tokio::test]
    async fn connect_cleans_claims_when_input_stream_closes() {
        let (ctx, routing, _tunnels) = test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");
        let (input_tx, _output_rx) = establish(ctx, &peer).await;
        input_tx.send(neighbor_up(&remote)).await.unwrap();
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_some() }
        })
        .await;

        drop(input_tx);
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await.is_none() }
        })
        .await;
    }

    #[tokio::test]
    async fn connector_sends_hello_accepts_ack_and_stores_direct_route() {
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

        input_tx.send(accepted_ack(&peer)).await.unwrap();

        wait_until(|| {
            let routing = routing.clone();
            async move {
                matches!(routing.route_to(peer.id).await, Some(Route::Direct(link)) if link.peer() == peer.id)
            }
        })
        .await;
    }

    #[tokio::test]
    async fn connector_applies_hello_ack_snapshot_as_claims() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let remote = host(3, "remote-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx
            .send(accepted_ack_with(
                &peer,
                PROTOCOL_VERSION,
                std::slice::from_ref(&remote),
            ))
            .await
            .unwrap();

        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(remote.id).await == Some(Route::Via(peer.id)) }
        })
        .await;
        drop(input_tx);
    }

    #[tokio::test]
    async fn connector_rejects_bad_first_acceptor_message_with_link_close() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx
            .send(neighbor_down(HostId::from_u128(9)))
            .await
            .unwrap();

        assert_protocol_link_close(&recv_message(&mut output_rx).await);
        assert!(routing.host_entry(HostId::from_u128(9)).await.is_none());
    }

    #[tokio::test]
    async fn connector_rejects_protocol_version_mismatch_with_structured_link_close() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx
            .send(accepted_ack_with(&peer, PROTOCOL_VERSION + 1, &[]))
            .await
            .unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_link_close_error(&message);
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
    async fn connector_rejects_hello_accepted_host_id_that_does_not_match_expected_peer() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let ctx = ctx.with_expected_peer(HostId::from_u128(2));
        let spoofed = host(3, "spoofed-peer");
        let (input_tx, input_rx) = mpsc::channel(8);
        let mut output_rx = spawn_connector_connect(ctx, stream_from_rx(input_rx));
        assert_connector_hello(&recv_message(&mut output_rx).await);

        input_tx.send(accepted_ack(&spoofed)).await.unwrap();

        let message = recv_message(&mut output_rx).await;
        let error = protocol_link_close_error(&message);
        assert!(matches!(
            wire::decode_protocol_error(error.clone()),
            ProtocolError::InvalidArgument { message }
                if message.contains("does not match expected peer")
        ));
        assert!(routing.host_entry(spoofed.id).await.is_none());
    }

    #[tokio::test]
    async fn connector_dispatches_tunnel_frames_to_pool() {
        let (ctx, _routing, tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let target = host(9, "target-host");
        let (target_tx, mut target_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(
                crate::routing::LinkId::new(target.id),
                target.clone(),
                target_tx,
                LinkRole::Peer,
                &[],
            )
            .await;

        let (input_tx, _output_rx) = establish_connector(ctx, &peer).await;
        input_tx
            .send(tunnel_frame(target.id, b"payload"))
            .await
            .unwrap();

        let frame = recv_forwarded_tunnel_frame(&mut target_rx).await;
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst, target.id.as_bytes().to_vec());
        assert!(frame.tunnel_id.is_some());
    }

    #[tokio::test]
    async fn connector_cleans_direct_route_when_input_stream_closes() {
        let (ctx, routing, _tunnels, _local) = connector_test_ctx().await;
        let peer = host(2, "peer-host");
        let (input_tx, _output_rx) = establish_connector(ctx, &peer).await;
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(peer.id).await.is_some() }
        })
        .await;

        drop(input_tx);
        wait_until(|| {
            let routing = routing.clone();
            async move { routing.route_to(peer.id).await.is_none() }
        })
        .await;
    }

    #[tokio::test]
    async fn connector_to_channel_establishes_link_service_over_tonic() {
        let acceptor_host = host(1, "acceptor");
        let acceptor_routing = Arc::new(RoutingCore::new());
        let (acceptor_incoming_tx, _acceptor_incoming_rx) = mpsc::channel(4);
        let acceptor_tunnels = Arc::new(TunnelPool::new(
            acceptor_host.id,
            acceptor_routing.clone(),
            acceptor_incoming_tx,
        ));
        let acceptor_ctx = LinkServiceCtx::new(
            acceptor_host.clone(),
            acceptor_routing.clone(),
            acceptor_tunnels.clone(),
            route_runtime(&acceptor_tunnels),
        );

        let connector_host = host(2, "connector");
        let connector_routing = Arc::new(RoutingCore::new());
        let (connector_incoming_tx, _connector_incoming_rx) = mpsc::channel(4);
        let connector_tunnels = Arc::new(TunnelPool::new(
            connector_host.id,
            connector_routing.clone(),
            connector_incoming_tx,
        ));
        let connector_route_runtime = route_runtime(&connector_tunnels);
        let connector_ctx = LinkConnectorCtx::new(
            connector_host.clone(),
            connector_routing.clone(),
            connector_tunnels,
            connector_route_runtime.clone(),
        );

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let incoming =
            stream::once(async move { Ok::<_, std::io::Error>(TestTransport::new(server_io)) });
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(wire::link_service_server::LinkServiceServer::new(
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
                let acceptor_has_link = acceptor_tunnels
                    .link_registry()
                    .link_to_peer(connector_host.id)
                    .await
                    .is_some();
                let connector_routes_direct = matches!(
                    connector_routing.route_to(acceptor_host.id).await,
                    Some(Route::Direct(_))
                );
                if acceptor_has_link && connector_routes_direct {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for tonic LinkService.Connect establishment");

        let Some(Route::Direct(link)) = connector_routing.route_to(acceptor_host.id).await else {
            panic!("expected a direct route");
        };
        assert!(
            connector_route_runtime
                .pool()
                .get_direct(link)
                .await
                .is_some()
        );
        // The acceptor advertises the dialer as wire adjacency but records
        // no local route over a link it cannot call on (pre-tunnel chunk).
        assert_eq!(acceptor_routing.route_to(connector_host.id).await, None);

        connector_task.abort();
        server_task.abort();
    }

    /// D11: an inbound `LinkClose` on the connector side cleans the link up
    /// at once — the cached link-keyed channel is dropped and later calls
    /// fail with no route instead of lingering through a drain.
    #[tokio::test]
    async fn connector_manager_rejects_cached_channel_after_inbound_link_close() {
        let acceptor_host = host(1, "acceptor");
        let acceptor_routing = Arc::new(RoutingCore::new());
        let (acceptor_incoming_tx, _acceptor_incoming_rx) = mpsc::channel(4);
        let acceptor_tunnels = Arc::new(TunnelPool::new(
            acceptor_host.id,
            acceptor_routing.clone(),
            acceptor_incoming_tx,
        ));
        let acceptor_ctx = LinkServiceCtx::new(
            acceptor_host.clone(),
            acceptor_routing.clone(),
            acceptor_tunnels.clone(),
            route_runtime(&acceptor_tunnels),
        );

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
        let connector_ctx = LinkConnectorCtx::new(
            connector_host.clone(),
            connector_routing.clone(),
            connector_tunnels.clone(),
            connector_manager.route_runtime(),
        );

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let incoming =
            stream::once(async move { Ok::<_, std::io::Error>(TestTransport::new(server_io)) });
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(wire::link_service_server::LinkServiceServer::new(
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
                if matches!(
                    connector_routing.route_to(acceptor_host.id).await,
                    Some(Route::Direct(_))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for connector direct route");
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
            panic!("expected acceptor link writer");
        };

        acceptor_tx
            .send(link_close(wire::pb::LinkCloseReason::UserShutdown))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if connector_routing.route_to(acceptor_host.id).await.is_none()
                    && connector_tunnels
                        .link_registry()
                        .link_to_peer(acceptor_host.id)
                        .await
                        .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for connector link cleanup");

        let error = connector_manager
            .channel_to(acceptor_host.id)
            .await
            .unwrap_err();

        assert!(
            matches!(error, crate::tunnel::TunnelPoolError::NotFound { host_id } if host_id == acceptor_host.id)
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
        let acceptor_ctx = LinkServiceCtx::new(
            acceptor_host.clone(),
            acceptor_routing.clone(),
            acceptor_tunnels.clone(),
            route_runtime(&acceptor_tunnels),
        );

        let connector_host = host(2, "connector");
        let connector_routing = Arc::new(RoutingCore::new());
        let (connector_incoming_tx, _connector_incoming_rx) = mpsc::channel(4);
        let connector_tunnels = Arc::new(TunnelPool::new(
            connector_host.id,
            connector_routing.clone(),
            connector_incoming_tx,
        ));
        let connector_ctx = LinkConnectorCtx::new(
            connector_host.clone(),
            connector_routing.clone(),
            connector_tunnels.clone(),
            route_runtime(&connector_tunnels),
        );

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let incoming =
            stream::once(async move { Ok::<_, std::io::Error>(TestTransport::new(server_io)) });
        let server_task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(wire::link_service_server::LinkServiceServer::new(
                    MetadataCheckingLinkService {
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
                let acceptor_has_link = acceptor_tunnels
                    .link_registry()
                    .link_to_peer(connector_host.id)
                    .await
                    .is_some();
                let connector_sees_acceptor = connector_routing
                    .host_entry(acceptor_host.id)
                    .await
                    .is_some();
                if acceptor_has_link && connector_sees_acceptor {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for metadata-authenticated LinkService.Connect");

        assert!(!connector_task.is_finished());
        connector_task.abort();
        server_task.abort();
    }

    #[derive(Clone)]
    struct MetadataCheckingLinkService {
        inner: LinkServiceCtx,
        expected_authorization: &'static str,
    }

    #[tonic::async_trait]
    impl wire::link_service_server::LinkService for MetadataCheckingLinkService {
        type ConnectStream =
            <LinkServiceCtx as wire::link_service_server::LinkService>::ConnectStream;

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
            <LinkServiceCtx as wire::link_service_server::LinkService>::connect(
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
        Endpoint::from_static("http://link-test").connect_with_connector_lazy(service_fn(
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
