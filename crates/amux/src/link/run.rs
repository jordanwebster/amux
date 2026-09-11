//! Carrier-independent link control runtime.
//!
//! The connector opens the control stream and sends `Hello`; the acceptor
//! answers with `HelloAck`. The handshake carries the current adjacency
//! snapshot, and registry deltas, reauthentication, and orderly shutdown all
//! continue on that same length-prefixed protobuf stream.

use std::future;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

use super::{LinkCarrier as Carrier, read_message, write_message};
use crate::protocol::{
    PROTOCOL_VERSION, ProtocolError, protocol_error_from_status_details, protocol_status, wire,
};
use crate::routing::{
    ConnectHandshake, ConnectHandshakeEvent, Host, LinkAdmission, LinkCarrier as RoutingCarrier,
    LinkCloseRequest, LinkId, LinkProperties, LinkRegistry, LinkRole, LiveLocalHost,
    RouteUpdateOutcome, RoutingCore, host_from_wire, host_to_wire, inbound_host_from_wire,
    neighbor_down_from_wire, neighbor_up_from_wire, protocol_error_hello_ack,
    protocol_error_link_close, validate_remote_host,
};
use crate::{HostId, audit};

const LINK_AUTH_REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(300);
const LINK_CONNECT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum LinkError {
    #[error("link control I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("link rejected: {0}")]
    Status(#[from] tonic::Status),
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticatedLinkUser {
    pub(crate) user_id: Uuid,
    pub(crate) client_id: String,
    pub(crate) expires_at: SystemTime,
    pub(crate) tier: crate::Tier,
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
    user: Arc<StdRwLock<AuthenticatedLinkUser>>,
    authenticator: Arc<dyn LinkTokenAuthenticator>,
    minimum_client_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReauthError {
    pub(crate) original_user_id: Uuid,
    pub(crate) reauth_user_id: Uuid,
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
            user: Arc::new(StdRwLock::new(user)),
            authenticator: Arc::new(authenticator),
            minimum_client_version,
        }
    }

    pub(crate) fn tier(&self) -> crate::Tier {
        self.user().tier
    }

    pub(crate) fn apply_reauth(&self, user: AuthenticatedLinkUser) -> Result<(), ReauthError> {
        let mut current = self
            .user
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.user_id != user.user_id {
            return Err(ReauthError {
                original_user_id: current.user_id,
                reauth_user_id: user.user_id,
            });
        }
        *current = user;
        Ok(())
    }

    fn user(&self) -> AuthenticatedLinkUser {
        self.user
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LinkConnectorToken {
    pub(crate) token: String,
    pub(crate) expires_at: SystemTime,
    pub(crate) tier: crate::Tier,
}

pub(crate) struct LinkConnectorRefreshRequest {
    pub(crate) response: oneshot::Sender<Result<crate::Tier, tonic::Status>>,
}

pub(crate) type LinkConnectorRefreshReceiver =
    Arc<tokio::sync::Mutex<mpsc::Receiver<LinkConnectorRefreshRequest>>>;

#[tonic::async_trait]
pub(crate) trait LinkConnectorTokenRefresher: Send + Sync + 'static {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status>;
}

#[derive(Clone)]
pub(crate) struct LinkConnectorAuth {
    token: LinkConnectorToken,
    refresher: Arc<dyn LinkConnectorTokenRefresher>,
    free_refresh_interval: Option<Duration>,
    next_refresh_at: tokio::time::Instant,
    on_refreshed: Option<Arc<dyn Fn(crate::Tier) + Send + Sync>>,
}

impl LinkConnectorAuth {
    pub(crate) fn tier(&self) -> crate::Tier {
        self.token.tier
    }

    pub(crate) fn new(
        token: LinkConnectorToken,
        refresher: Arc<dyn LinkConnectorTokenRefresher>,
    ) -> Self {
        Self::with_free_refresh_interval(token, refresher, None)
    }

    pub(crate) fn with_free_refresh_interval(
        token: LinkConnectorToken,
        refresher: Arc<dyn LinkConnectorTokenRefresher>,
        free_refresh_interval: Option<Duration>,
    ) -> Self {
        let next_refresh_at = refresh_deadline(&token, free_refresh_interval);
        Self {
            token,
            refresher,
            free_refresh_interval,
            next_refresh_at,
            on_refreshed: None,
        }
    }

    pub(crate) fn with_refresh_observer(
        mut self,
        observer: impl Fn(crate::Tier) + Send + Sync + 'static,
    ) -> Self {
        self.on_refreshed = Some(Arc::new(observer));
        self
    }

    fn refresh_deadline(&self) -> tokio::time::Instant {
        self.next_refresh_at
    }

    async fn refresh(&mut self) -> Result<(wire::pb::Message, crate::Tier), tonic::Status> {
        let token = self.refresher.refresh_routing_token().await?;
        let message = wire::pb::Message {
            body: Some(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                auth_token: token.token.clone(),
            })),
        };
        let tier = token.tier;
        self.token = token;
        self.next_refresh_at = refresh_deadline(&self.token, self.free_refresh_interval);
        if let Some(observer) = &self.on_refreshed {
            observer(tier);
        }
        Ok((message, tier))
    }
}

fn refresh_deadline(
    token: &LinkConnectorToken,
    free_refresh_interval: Option<Duration>,
) -> tokio::time::Instant {
    if token.tier == crate::Tier::Free
        && let Some(interval) = free_refresh_interval
    {
        tokio::time::Instant::now() + interval
    } else {
        instant_for_system_time(token.expires_at, LINK_AUTH_REFRESH_BEFORE_EXPIRY)
    }
}

/// Everything the control runtime needs from its owner.
#[derive(Clone)]
pub(crate) struct LinkCtx {
    local_host: LiveLocalHost,
    routing: Arc<RoutingCore>,
    links: Arc<LinkRegistry>,
    incoming_streams_tx: Option<mpsc::Sender<(HostId, super::ByteStream)>>,
    piper: super::Piper,
    expected_peer: Option<HostId>,
    authenticated_peer: Option<HostId>,
    link_role: LinkRole,
    routing_carrier: RoutingCarrier,
    acceptor_session: Option<LinkAuthSession>,
    authenticator: Option<Arc<dyn LinkTokenAuthenticator>>,
    minimum_client_version: Option<String>,
    connector_auth: Option<LinkConnectorAuth>,
    established_tx: Option<Arc<StdRwLock<Option<EstablishmentSender>>>>,
    shutdown_rx: Option<watch::Receiver<bool>>,
    refresh_rx: Option<LinkConnectorRefreshReceiver>,
}

pub(crate) type LinkConnectorCtx = LinkCtx;
type ConnectorTask = tokio::task::JoinHandle<Result<(), tonic::Status>>;
type EstablishmentSender = oneshot::Sender<Result<Host, tonic::Status>>;
pub(crate) type EstablishmentReceiver = oneshot::Receiver<Result<Host, tonic::Status>>;

impl LinkCtx {
    pub(crate) fn new(
        local_host: Host,
        routing: Arc<RoutingCore>,
        links: Arc<LinkRegistry>,
    ) -> Self {
        Self::new_live(LiveLocalHost::new(local_host), routing, links)
    }

    pub(crate) fn new_live(
        local_host: LiveLocalHost,
        routing: Arc<RoutingCore>,
        links: Arc<LinkRegistry>,
    ) -> Self {
        let piper = super::Piper::new(local_host.id(), links.clone());
        Self {
            local_host,
            routing,
            links,
            incoming_streams_tx: None,
            piper,
            expected_peer: None,
            authenticated_peer: None,
            link_role: LinkRole::Peer,
            routing_carrier: RoutingCarrier::Direct,
            acceptor_session: None,
            authenticator: None,
            minimum_client_version: None,
            connector_auth: None,
            established_tx: None,
            shutdown_rx: None,
            refresh_rx: None,
        }
    }

    pub(crate) fn with_expected_peer(mut self, peer: HostId) -> Self {
        self.expected_peer = Some(peer);
        self
    }

    pub(crate) fn with_authenticated_peer(mut self, peer: HostId) -> Self {
        self.authenticated_peer = Some(peer);
        self
    }

    pub(crate) fn with_link_role(mut self, role: LinkRole) -> Self {
        self.link_role = role;
        self
    }

    pub(crate) fn with_carrier(mut self, carrier: RoutingCarrier) -> Self {
        self.routing_carrier = carrier;
        self
    }

    pub(crate) fn with_incoming_streams(
        mut self,
        sender: mpsc::Sender<(HostId, super::ByteStream)>,
    ) -> Self {
        self.incoming_streams_tx = Some(sender);
        self
    }

    pub(crate) fn with_token_authenticator(
        mut self,
        authenticator: Arc<dyn LinkTokenAuthenticator>,
        minimum_client_version: Option<String>,
    ) -> Self {
        self.authenticator = Some(authenticator);
        self.minimum_client_version = minimum_client_version;
        self
    }

    fn take_established_tx(&self) -> Option<EstablishmentSender> {
        self.established_tx
            .as_ref()
            .and_then(|slot| slot.write().ok()?.take())
    }
}

/// Runs one complete link for either side of any `LinkCarrier`.
pub(crate) async fn run_link(
    mut ctx: LinkCtx,
    carrier: Arc<dyn Carrier>,
    role: crate::routing::ConnectRole,
) -> Result<(), LinkError> {
    let (mut sink, mut source) = carrier.control();
    let mut handshake = ConnectHandshake::new(role);
    let snapshot = ctx.links.neighbor_snapshot().await;

    let (peer_host, peer_neighbors) = match role {
        crate::routing::ConnectRole::Connector => {
            write_message(&mut sink, &connector_hello(&ctx, &snapshot)).await?;
            let first = read_first(&mut source, "HelloAck").await?;
            match handshake.receive(first) {
                Ok(ConnectHandshakeEvent::Accepted(accepted)) => {
                    accept_peer_hello_ack(&ctx, accepted)
                        .map_err(|error| protocol_status(wire::decode_protocol_error(error)))?
                }
                Ok(ConnectHandshakeEvent::Rejected(error)) => {
                    return Err(protocol_status(wire::decode_protocol_error(error)).into());
                }
                Ok(_) => return Err(tonic::Status::invalid_argument("expected HelloAck").into()),
                Err(error) => return Err(tonic::Status::invalid_argument(error.to_string()).into()),
            }
        }
        crate::routing::ConnectRole::Acceptor => {
            let first =
                match tokio::time::timeout(LINK_CONNECT_HELLO_TIMEOUT, read_message(&mut source))
                    .await
                {
                    Ok(Ok(Some(first))) => first,
                    Ok(Ok(None)) => return Ok(()),
                    Ok(Err(error)) => {
                        write_message(&mut sink, &protocol_error_link_close(error.to_string()))
                            .await?;
                        carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                        return Ok(());
                    }
                    Err(_) => {
                        carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                        return Ok(());
                    }
                };
            let hello = match handshake.receive(first) {
                Ok(ConnectHandshakeEvent::Hello(hello)) => hello,
                Ok(_) => {
                    write_message(
                        &mut sink,
                        &protocol_error_hello_ack("unexpected message while awaiting Hello"),
                    )
                    .await?;
                    carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                    return Ok(());
                }
                Err(error) => {
                    write_message(&mut sink, &protocol_error_hello_ack(error.to_string())).await?;
                    carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                    return Ok(());
                }
            };
            let session = match authenticate_hello(&ctx, &hello).await {
                Ok(session) => session,
                Err(status) => {
                    audit::auth_jwt_failure(&status);
                    write_message(&mut sink, &auth_expired_link_close()).await?;
                    carrier.close(wire::pb::LinkCloseReason::AuthExpired);
                    return Ok(());
                }
            };
            let peer = match accept_peer_hello(&ctx, hello, session.as_ref()) {
                Ok(peer) => peer,
                Err(error) => {
                    write_message(&mut sink, &error_hello_ack(error)).await?;
                    carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                    return Ok(());
                }
            };
            write_message(&mut sink, &accepted_hello_ack(&ctx, &snapshot, peer.0.id)).await?;
            handshake
                .acceptor_ack_sent()
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            ctx.acceptor_session = session;
            peer
        }
    };

    run_established(
        ctx,
        carrier,
        sink,
        source,
        handshake,
        (peer_host, peer_neighbors),
        snapshot.into_iter().map(|host| host.id).collect(),
    )
    .await
}

async fn read_first(
    source: &mut super::ControlSource,
    expected: &'static str,
) -> Result<wire::pb::Message, LinkError> {
    match read_message(source).await? {
        Some(message) => Ok(message),
        None => Err(tonic::Status::unavailable(format!("link closed before {expected}")).into()),
    }
}

async fn authenticate_hello(
    ctx: &LinkCtx,
    hello: &wire::pb::Hello,
) -> Result<Option<LinkAuthSession>, tonic::Status> {
    let Some(authenticator) = &ctx.authenticator else {
        return Ok(ctx.acceptor_session.clone());
    };
    let token = hello
        .auth_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| tonic::Status::unauthenticated("missing link authorization"))?;
    let user = authenticator.authenticate_token(token).await?;
    Ok(Some(LinkAuthSession::new(
        user,
        authenticator.clone(),
        ctx.minimum_client_version.clone(),
    )))
}

async fn run_established(
    ctx: LinkCtx,
    carrier: Arc<dyn Carrier>,
    mut sink: super::ControlSink,
    mut source: super::ControlSource,
    mut handshake: ConnectHandshake,
    peer: (Host, Vec<Host>),
    sent_snapshot: Vec<HostId>,
) -> Result<(), LinkError> {
    debug_assert!(handshake.is_established());
    let (peer_host, peer_neighbors) = peer;
    let link = LinkId::new(peer_host.id);
    let link_role = if ctx.connector_auth.is_some() {
        LinkRole::CloudRelay
    } else {
        ctx.link_role
    };
    let admission = ctx
        .acceptor_session
        .as_ref()
        .map(|session| LinkAdmission::CloudToken {
            tier: session.tier(),
        })
        .unwrap_or(LinkAdmission::PinnedKey);
    let carrier_tag = match carrier.kind() {
        super::CarrierKind::Ssh => RoutingCarrier::Ssh,
        _ => ctx.routing_carrier,
    };
    let (out_tx, mut out_rx) = mpsc::channel(256);
    let mut link_close_rx = ctx
        .links
        .register_with_details(
            link,
            peer_host.clone(),
            out_tx.clone(),
            LinkProperties {
                role: link_role,
                admission,
                carrier: carrier_tag,
            },
            &sent_snapshot,
            Some(carrier.clone()),
        )
        .await;

    for neighbor in peer_neighbors {
        if neighbor.id != ctx.local_host.id() && neighbor.id != peer_host.id {
            ctx.routing.apply_claim_up(peer_host.id, neighbor).await;
        }
    }
    if ctx.acceptor_session.is_none() {
        match ctx.routing.apply_direct_up(peer_host.clone(), link).await {
            RouteUpdateOutcome::Inserted | RouteUpdateOutcome::AlreadyKnown => {}
            RouteUpdateOutcome::Replacing => {
                tracing::debug!(peer = %peer_host.id, %link, "link established during trust replacement");
            }
            RouteUpdateOutcome::RejectedByCap => {
                let status = tonic::Status::invalid_argument("routing host cap reached");
                signal_establishment(ctx.take_established_tx(), Err(clone_status(&status)));
                let _ = out_tx
                    .send(protocol_error_link_close(status.to_string()))
                    .await;
                cleanup_link(&ctx, link).await;
                return Err(status.into());
            }
        }
    }
    signal_establishment(ctx.take_established_tx(), Ok(peer_host.clone()));

    let mut acceptor_auth = ctx.acceptor_session.clone();
    let mut connector_auth = ctx.connector_auth.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let mut close_reason = wire::pb::LinkCloseReason::Unspecified;
    let mut close_status = None;

    loop {
        let auth_expiry = acceptor_auth
            .as_ref()
            .map(|auth| instant_for_system_time(auth.user().expires_at, Duration::ZERO));
        let refresh_deadline = connector_auth
            .as_ref()
            .map(LinkConnectorAuth::refresh_deadline);
        tokio::select! {
            inbound = read_message(&mut source) => {
                let message = match inbound {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = out_tx.send(protocol_error_link_close(error.to_string())).await;
                        close_reason = wire::pb::LinkCloseReason::ProtocolError;
                        break;
                    }
                };
                match handshake.receive(message) {
                    Ok(ConnectHandshakeEvent::PostHandshake(body)) => {
                        match handle_control_body(&ctx, link, body, acceptor_auth.as_mut()).await {
                            ControlAction::Continue => {}
                            ControlAction::Close(reason, status) => {
                                close_reason = reason;
                                close_status = status;
                                break;
                            }
                            ControlAction::ReplyAndClose(message, reason) => {
                                let _ = out_tx.send(message).await;
                                close_reason = reason;
                                break;
                            }
                        }
                    }
                    Ok(_) => unreachable!("handshake cannot repeat after establishment"),
                    Err(error) => {
                        let _ = out_tx.send(protocol_error_link_close(error.to_string())).await;
                        close_reason = wire::pb::LinkCloseReason::ProtocolError;
                        break;
                    }
                }
            }
            outbound = out_rx.recv() => {
                let Some(message) = outbound else { break };
                if let Err(error) = write_message(&mut sink, &message).await {
                    close_status = Some(tonic::Status::unavailable(error.to_string()));
                    break;
                }
            }
            stream = carrier.accept_stream() => {
                let Some((preface, mut stream)) = stream else { break };
                let destination = HostId::from_slice(&preface.dst).ok();
                if destination == Some(ctx.local_host.id()) {
                    if let Some(sender) = &ctx.incoming_streams_tx {
                        if let Err(error) = sender.send((peer_host.id, stream)).await {
                            let (_, mut stream) = error.0;
                            let _ = stream.reset(wire::pb::StreamRefusal::ShuttingDown).await;
                        }
                    } else {
                        let _ = stream.reset(wire::pb::StreamRefusal::ShuttingDown).await;
                    }
                } else {
                    let _ = ctx.piper.pipe(link, preface, stream).await;
                }
            }
            _ = maybe_sleep_until(auth_expiry), if auth_expiry.is_some() => {
                audit::auth_jwt_failure("link authorization expired");
                let _ = write_message(&mut sink, &auth_expired_link_close()).await;
                close_reason = wire::pb::LinkCloseReason::AuthExpired;
                break;
            }
            _ = maybe_sleep_until(refresh_deadline), if refresh_deadline.is_some() => {
                let Some(auth) = connector_auth.as_mut() else { continue };
                match auth.refresh().await {
                    Ok((message, _)) => {
                        if let Err(error) = write_message(&mut sink, &message).await {
                            close_status = Some(tonic::Status::unavailable(error.to_string()));
                            break;
                        }
                    }
                    Err(status) => {
                        if should_audit_auth_refresh_failure(&status) {
                            audit::auth_jwt_failure(&status);
                        }
                        close_status = Some(status);
                        break;
                    }
                }
            }
            request = receive_refresh_request(ctx.refresh_rx.as_ref()), if connector_auth.is_some() => {
                let Some(request) = request else { continue };
                let Some(auth) = connector_auth.as_mut() else { continue };
                match auth.refresh().await {
                    Ok((message, tier)) => {
                        let failed = match write_message(&mut sink, &message).await {
                            Ok(()) => {
                                let _ = request.response.send(Ok(tier));
                                false
                            }
                            Err(error) => {
                                let _ = request.response.send(Err(tonic::Status::unavailable(error.to_string())));
                                true
                            }
                        };
                        if failed { break; }
                    }
                    Err(status) => {
                        let _ = request.response.send(Err(clone_status(&status)));
                        close_status = Some(status);
                        break;
                    }
                }
            }
            request = link_close_rx.recv() => {
                close_status = Some(match request {
                    Some(LinkCloseRequest::OutgoingQueueFull) => tonic::Status::resource_exhausted("link outgoing queue full"),
                    Some(LinkCloseRequest::TrustReplaced) => tonic::Status::permission_denied("peer trust was replaced"),
                    None => tonic::Status::unavailable("link closed"),
                });
                break;
            }
            _ = wait_for_connector_shutdown(&mut shutdown_rx) => {
                let message = link_close(wire::pb::LinkCloseReason::UserShutdown);
                let _ = write_message(&mut sink, &message).await;
                close_reason = wire::pb::LinkCloseReason::UserShutdown;
                break;
            }
            reason = carrier.closed() => {
                close_reason = reason;
                break;
            }
        }
    }

    // Drain control messages already queued before closing, most importantly
    // the protocol close emitted for an invalid inbound frame.
    while let Ok(message) = out_rx.try_recv() {
        let _ = write_message(&mut sink, &message).await;
    }
    cleanup_link(&ctx, link).await;
    carrier.close(close_reason);
    match close_status {
        Some(status) => Err(status.into()),
        None => Ok(()),
    }
}

enum ControlAction {
    Continue,
    Close(wire::pb::LinkCloseReason, Option<tonic::Status>),
    ReplyAndClose(wire::pb::Message, wire::pb::LinkCloseReason),
}

async fn handle_control_body(
    ctx: &LinkCtx,
    link: LinkId,
    body: wire::pb::message::Body,
    acceptor_auth: Option<&mut LinkAuthSession>,
) -> ControlAction {
    match body {
        wire::pb::message::Body::NeighborUp(event) => match neighbor_up_from_wire(event) {
            Ok(host) => {
                if host.id != ctx.local_host.id() && host.id != link.peer() {
                    ctx.routing.apply_claim_up(link.peer(), host).await;
                }
                ControlAction::Continue
            }
            Err(error) => ControlAction::ReplyAndClose(
                protocol_error_link_close(error.to_string()),
                wire::pb::LinkCloseReason::ProtocolError,
            ),
        },
        wire::pb::message::Body::NeighborDown(event) => match neighbor_down_from_wire(event) {
            Ok(host_id) => {
                ctx.routing.apply_claim_down(link.peer(), host_id).await;
                ControlAction::Continue
            }
            Err(error) => ControlAction::ReplyAndClose(
                protocol_error_link_close(error.to_string()),
                wire::pb::LinkCloseReason::ProtocolError,
            ),
        },
        wire::pb::message::Body::Reauth(reauth) => {
            let Some(auth) = acceptor_auth else {
                return ControlAction::ReplyAndClose(
                    protocol_error_link_close("reauth received on unauthenticated link"),
                    wire::pb::LinkCloseReason::ProtocolError,
                );
            };
            match auth
                .authenticator
                .authenticate_token(&reauth.auth_token)
                .await
            {
                Ok(user) => match auth.apply_reauth(user) {
                    Ok(()) => {
                        ctx.links.update_cloud_tier(&link, auth.tier()).await;
                        ControlAction::Continue
                    }
                    Err(error) => {
                        audit::auth_jwt_failure("link reauth user mismatch");
                        tracing::warn!(
                            original_user_id = %error.original_user_id,
                            reauth_user_id = %error.reauth_user_id,
                            "link reauth user mismatch"
                        );
                        ControlAction::ReplyAndClose(
                            auth_expired_link_close(),
                            wire::pb::LinkCloseReason::AuthExpired,
                        )
                    }
                },
                Err(status) => {
                    audit::auth_jwt_failure(&status);
                    ControlAction::ReplyAndClose(
                        auth_expired_link_close(),
                        wire::pb::LinkCloseReason::AuthExpired,
                    )
                }
            }
        }
        wire::pb::message::Body::LinkClose(close) => {
            let reason = wire::pb::LinkCloseReason::try_from(close.reason)
                .unwrap_or(wire::pb::LinkCloseReason::Unspecified);
            ControlAction::Close(reason, link_close_status(&close))
        }
        wire::pb::message::Body::Hello(_) | wire::pb::message::Body::HelloAck(_) => {
            unreachable!("repeated handshake message is rejected by ConnectHandshake")
        }
    }
}

fn accept_peer_hello(
    ctx: &LinkCtx,
    hello: wire::pb::Hello,
    auth_session: Option<&LinkAuthSession>,
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
    if let Some(authenticated_peer) = ctx.authenticated_peer
        && host.id != authenticated_peer
    {
        return Err(invalid_argument_error(format!(
            "Hello.host_id {} does not match carrier peer {}",
            host.id, authenticated_peer
        )));
    }
    if let Some(auth_session) = auth_session {
        validate_minimum_client_version(&host, auth_session)?;
    }
    if host.id == ctx.local_host.id() {
        return Err(host_id_collision_error(format!(
            "peer host_id {} matches local host_id",
            host.id
        )));
    }
    Ok((host, neighbors_from_wire(hello.neighbors)?))
}

fn accept_peer_hello_ack(
    ctx: &LinkCtx,
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
    if host.id == ctx.local_host.id() {
        return Err(host_id_collision_error(
            "peer host_id matches local host_id",
        ));
    }
    Ok((host, neighbors_from_wire(accepted.neighbors)?))
}

fn validate_minimum_client_version(
    host: &Host,
    auth_session: &LinkAuthSession,
) -> Result<(), wire::pb::Error> {
    let Some(minimum) = &auth_session.minimum_client_version else {
        return Ok(());
    };
    let reject = match (
        semver::Version::parse(&host.version),
        semver::Version::parse(minimum),
    ) {
        (Ok(client), Ok(minimum)) => client < minimum,
        _ => true,
    };
    if reject {
        return Err(wire::encode_protocol_error(
            &ProtocolError::UpdateRequired {
                minimum_version: minimum.clone(),
                client_version: host.version.clone(),
            },
        ));
    }
    Ok(())
}

fn connector_hello(ctx: &LinkCtx, snapshot: &[Host]) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            host: Some(host_to_wire(&ctx.local_host.snapshot())),
            neighbors: snapshot.iter().map(host_to_wire).collect(),
            auth_token: ctx
                .connector_auth
                .as_ref()
                .map(|auth| auth.token.token.clone()),
        })),
    }
}

fn accepted_hello_ack(ctx: &LinkCtx, snapshot: &[Host], peer: HostId) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Accepted(
                wire::pb::HelloAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    host: Some(host_to_wire(&ctx.local_host.snapshot())),
                    neighbors: snapshot
                        .iter()
                        .filter(|host| host.id != peer)
                        .map(host_to_wire)
                        .collect(),
                },
            )),
        })),
    }
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

fn link_close(reason: wire::pb::LinkCloseReason) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::LinkClose(wire::pb::LinkClose {
            reason: reason as i32,
            error: None,
        })),
    }
}

fn link_close_status(close: &wire::pb::LinkClose) -> Option<tonic::Status> {
    let reason = wire::pb::LinkCloseReason::try_from(close.reason)
        .unwrap_or(wire::pb::LinkCloseReason::Unspecified);
    (reason == wire::pb::LinkCloseReason::UpdateRequired).then(|| {
        close
            .error
            .clone()
            .map(wire::decode_protocol_error)
            .map(protocol_status)
            .unwrap_or_else(|| tonic::Status::failed_precondition("amux update required"))
    })
}

fn should_audit_auth_refresh_failure(status: &tonic::Status) -> bool {
    protocol_error_from_status_details(status) != Some(ProtocolError::PaymentRequired)
}

async fn cleanup_link(ctx: &LinkCtx, link: LinkId) {
    let peer = link.peer();
    ctx.links.remove(&link).await;
    ctx.routing.apply_direct_down(link).await;
    if ctx.links.link_to_peer(peer).await.is_none() {
        ctx.routing.remove_relay_claims(peer).await;
    }
}

async fn receive_refresh_request(
    refresh_rx: Option<&LinkConnectorRefreshReceiver>,
) -> Option<LinkConnectorRefreshRequest> {
    match refresh_rx {
        Some(rx) => match rx.lock().await.recv().await {
            Some(request) => Some(request),
            None => future::pending().await,
        },
        None => future::pending().await,
    }
}

async fn wait_for_connector_shutdown(shutdown_rx: &mut Option<watch::Receiver<bool>>) {
    let Some(shutdown_rx) = shutdown_rx else {
        future::pending::<()>().await;
        return;
    };
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        if shutdown_rx.changed().await.is_err() {
            future::pending::<()>().await;
        }
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
    match target.duration_since(SystemTime::now()) {
        Ok(delay) => tokio::time::Instant::now() + delay,
        Err(_) => tokio::time::Instant::now(),
    }
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

fn signal_establishment(sender: Option<EstablishmentSender>, result: Result<Host, tonic::Status>) {
    if let Some(sender) = sender {
        let _ = sender.send(result);
    }
}

// These compatibility entry points keep transport call sites compiling while
// native carrier attachment moves outward into the connection boundaries.
pub(crate) fn spawn_connector_to_channel_with_establishment(
    _ctx: LinkConnectorCtx,
    _channel: tonic::transport::Channel,
) -> (ConnectorTask, EstablishmentReceiver) {
    unavailable_connector()
}

pub(crate) fn spawn_connector_to_channel_with_auth_establishment_and_shutdown(
    _ctx: LinkConnectorCtx,
    _channel: tonic::transport::Channel,
    _auth: LinkConnectorAuth,
    _shutdown_rx: watch::Receiver<bool>,
    _refresh_rx: Option<LinkConnectorRefreshReceiver>,
) -> (ConnectorTask, EstablishmentReceiver) {
    unavailable_connector()
}

#[cfg(test)]
pub(crate) fn spawn_connector_to_channel_with_bearer_token(
    _ctx: LinkConnectorCtx,
    _channel: tonic::transport::Channel,
    _token: String,
) -> ConnectorTask {
    unavailable_connector().0
}

fn unavailable_connector() -> (ConnectorTask, EstablishmentReceiver) {
    let (established_tx, established_rx) = oneshot::channel();
    let status = tonic::Status::unavailable("native link carrier is not attached");
    let task_status = clone_status(&status);
    let _ = established_tx.send(Err(status));
    let task = tokio::spawn(async move { Err(task_status) });
    (task, established_rx)
}

#[cfg(testnet)]
pub(crate) async fn link_reauth_tier_probe() -> (crate::Tier, crate::Tier) {
    #[derive(Clone)]
    struct TierAuthenticator(AuthenticatedLinkUser);
    #[tonic::async_trait]
    impl LinkTokenAuthenticator for TierAuthenticator {
        async fn authenticate_token(
            &self,
            _token: &str,
        ) -> Result<AuthenticatedLinkUser, tonic::Status> {
            Ok(self.0.clone())
        }
    }

    let user_id = Uuid::new_v4();
    let initial = AuthenticatedLinkUser {
        user_id,
        client_id: "testnet".into(),
        expires_at: SystemTime::now() + Duration::from_secs(60),
        tier: crate::Tier::Free,
    };
    let refreshed = AuthenticatedLinkUser {
        user_id,
        client_id: "testnet".into(),
        expires_at: SystemTime::now() + Duration::from_secs(3600),
        tier: crate::Tier::Pro,
    };
    let session = LinkAuthSession::new(initial, TierAuthenticator(refreshed), None);
    let before = session.tier();
    let user = session
        .authenticator
        .authenticate_token("refreshed")
        .await
        .unwrap();
    session.apply_reauth(user).unwrap();
    (before, session.tier())
}
