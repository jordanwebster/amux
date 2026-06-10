use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rustls::pki_types::ServerName;
use tokio::sync::{RwLock, mpsc};
use tokio_rustls::TlsConnector;
use tonic::transport::{Channel, Endpoint};

use crate::HostId;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::protocol::wire as pb;
use crate::resource_limits::{
    CLOUD_INBOUND_TUNNEL_ID_CACHE_CAP, CLOUD_INBOUND_TUNNEL_RATE_LIMIT,
    CLOUD_INBOUND_TUNNEL_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::routing::{Link, LinkRegistry, LinkRegistryError, Route, RoutingCore, route_to_wire};
use crate::transport::{
    channel_from_single_io, configure_tonic_endpoint_keepalive, pin_pairing_channel_from_io,
    qr_pairing_channel_from_io,
};
use crate::trust::SharedTrustStore;
use crate::tunnel::transport::TunnelTransport;
use crate::tunnel::types::{TunnelId, TunnelTypeError};
use crate::tunnel::{TUNNEL_FRAME_PAYLOAD_MAX, Tunnel, create_tunnel};

const TUNNEL_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PIN_PAIRING_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const RETIRED_TUNNEL_CAP: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TunnelPoolError {
    #[error("host {host_id} is not reachable")]
    NotFound { host_id: HostId },
    #[error("host {host_id} has no route")]
    EmptyRoute { host_id: HostId },
    #[error("route first hop {link} has no outgoing writer")]
    LinkUnavailable { link: Link },
    #[error("route first hop {link} is draining")]
    LinkDraining { link: Link },
    #[error("TunnelFrame missing tunnel_id")]
    MissingTunnelId,
    #[error("TunnelFrame missing dst")]
    MissingDestination,
    #[error("TunnelFrame has invalid route: {message}")]
    InvalidRoute { message: String },
    #[error("TunnelFrame payload exceeds {max} bytes: {actual} bytes")]
    PayloadTooLarge { actual: usize, max: usize },
    #[error(transparent)]
    InvalidTunnelId(#[from] TunnelTypeError),
    #[error("incoming tunnel receiver is closed")]
    IncomingTunnelsClosed,
    #[error("target-side tunnel closed before payload delivery")]
    InboundClosed,
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("tunnel TLS error: {0}")]
    Tls(String),
    #[allow(dead_code)]
    #[error("tunnel endpoint channels require device TLS")]
    DeviceTlsRequired,
}

struct ActiveTunnel {
    peer: HostId,
    route: Route,
    tunnel: Tunnel,
}

struct PoolState {
    tunnels: HashMap<TunnelId, ActiveTunnel>,
    retired_tunnels: BoundedTunnelIdSet,
    cloud_inbound_tunnel_limiter: SlidingWindowRateLimiter<()>,
    cloud_limited_tunnels: BoundedTunnelIdSet,
}

#[derive(Default)]
struct BoundedTunnelIdSet {
    ids: HashSet<TunnelId>,
    order: VecDeque<TunnelId>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            tunnels: HashMap::new(),
            retired_tunnels: BoundedTunnelIdSet::default(),
            cloud_inbound_tunnel_limiter: SlidingWindowRateLimiter::new(
                CLOUD_INBOUND_TUNNEL_RATE_LIMIT,
                CLOUD_INBOUND_TUNNEL_RATE_WINDOW,
            ),
            cloud_limited_tunnels: BoundedTunnelIdSet::default(),
        }
    }
}

pub(crate) struct TunnelPool {
    my_host_id: HostId,
    routing: Arc<RoutingCore>,
    links: Arc<LinkRegistry>,
    incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    channel_security: TunnelChannelSecurity,
    pin_pairing_handshake_timeout: Duration,
    state: Arc<RwLock<PoolState>>,
}

#[derive(Clone)]
enum TunnelChannelSecurity {
    Plain,
    DeviceTls {
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
        handshake_timeout: Duration,
    },
}

impl TunnelPool {
    pub(crate) fn new(
        my_host_id: HostId,
        routing: Arc<RoutingCore>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    ) -> Self {
        Self::with_link_registry(
            my_host_id,
            routing,
            Arc::new(LinkRegistry::default()),
            incoming_tunnels_tx,
        )
    }

    pub(crate) fn with_link_registry(
        my_host_id: HostId,
        routing: Arc<RoutingCore>,
        links: Arc<LinkRegistry>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    ) -> Self {
        Self::with_link_registry_and_security(
            my_host_id,
            routing,
            links,
            incoming_tunnels_tx,
            TunnelChannelSecurity::Plain,
        )
    }

    pub(crate) fn with_device_tls(
        my_host_id: HostId,
        routing: Arc<RoutingCore>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
        identity: DeviceIdentity,
        trust_store: SharedTrustStore,
    ) -> Self {
        Self::with_link_registry_and_security(
            my_host_id,
            routing,
            Arc::new(LinkRegistry::default()),
            incoming_tunnels_tx,
            TunnelChannelSecurity::DeviceTls {
                identity,
                trust_store,
                handshake_timeout: TUNNEL_TLS_HANDSHAKE_TIMEOUT,
            },
        )
    }

    fn with_link_registry_and_security(
        my_host_id: HostId,
        routing: Arc<RoutingCore>,
        links: Arc<LinkRegistry>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
        channel_security: TunnelChannelSecurity,
    ) -> Self {
        Self {
            my_host_id,
            routing,
            links,
            incoming_tunnels_tx,
            channel_security,
            pin_pairing_handshake_timeout: PIN_PAIRING_TLS_HANDSHAKE_TIMEOUT,
            state: Arc::new(RwLock::new(PoolState::default())),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_pin_pairing_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.pin_pairing_handshake_timeout = timeout;
        self
    }

    pub(crate) fn link_registry(&self) -> Arc<LinkRegistry> {
        self.links.clone()
    }

    #[cfg(test)]
    pub(crate) async fn channel_to(&self, peer: HostId) -> Result<Channel, TunnelPoolError> {
        let route = self
            .routing
            .best_route(peer)
            .await
            .ok_or(TunnelPoolError::NotFound { host_id: peer })?;
        self.channel_to_route(peer, route).await
    }

    pub(crate) async fn channel_to_route(
        &self,
        peer: HostId,
        route: Route,
    ) -> Result<Channel, TunnelPoolError> {
        let (first_hop, dst) = outgoing_route_parts(peer, &route)?;
        let outgoing_tx = self.links.outgoing_tx(&first_hop).await?;

        let id = TunnelId::new(self.my_host_id);
        let (tunnel, transport) = create_tunnel(id, dst, peer, outgoing_tx);
        let transport = self.transport_with_cleanup(id, transport);
        {
            let mut state = self.state.write().await;
            state.tunnels.insert(
                id,
                ActiveTunnel {
                    peer,
                    route: route.clone(),
                    tunnel,
                },
            );
        }

        let channel = match self.channel_from_transport(peer, transport).await {
            Ok(channel) => channel,
            Err(error) => {
                self.state.write().await.tunnels.remove(&id);
                return Err(error);
            }
        };

        Ok(channel)
    }

    pub(crate) async fn pin_pairing_channel_to_route(
        &self,
        peer: HostId,
        route: Route,
    ) -> Result<Channel, TunnelPoolError> {
        let (id, transport) = self.pairing_transport_to_route(peer, route).await?;
        match tokio::time::timeout(
            self.pin_pairing_handshake_timeout,
            pin_pairing_channel_from_io(transport),
        )
        .await
        {
            Err(_) => {
                self.state.write().await.tunnels.remove(&id);
                Err(TunnelPoolError::Tls(
                    "PIN pairing TLS handshake timed out".to_string(),
                ))
            }
            Ok(Ok(channel)) => Ok(channel),
            Ok(Err(error)) => {
                self.state.write().await.tunnels.remove(&id);
                Err(TunnelPoolError::Tls(error.to_string()))
            }
        }
    }

    pub(crate) async fn qr_pairing_channel_to_route(
        &self,
        peer: HostId,
        route: Route,
        expected_pubkey: Vec<u8>,
    ) -> Result<Channel, TunnelPoolError> {
        let (id, transport) = self.pairing_transport_to_route(peer, route).await?;
        match tokio::time::timeout(
            self.pin_pairing_handshake_timeout,
            qr_pairing_channel_from_io(transport, expected_pubkey),
        )
        .await
        {
            Err(_) => {
                self.state.write().await.tunnels.remove(&id);
                Err(TunnelPoolError::Tls(
                    "QR pairing TLS handshake timed out".to_string(),
                ))
            }
            Ok(Ok(channel)) => Ok(channel),
            Ok(Err(error)) => {
                self.state.write().await.tunnels.remove(&id);
                Err(TunnelPoolError::Tls(error.to_string()))
            }
        }
    }

    async fn pairing_transport_to_route(
        &self,
        peer: HostId,
        route: Route,
    ) -> Result<(TunnelId, TunnelTransport), TunnelPoolError> {
        let (first_hop, dst) = outgoing_route_parts(peer, &route)?;
        let outgoing_tx = self.links.outgoing_tx(&first_hop).await?;

        let id = TunnelId::new(self.my_host_id);
        let (tunnel, transport) = create_tunnel(id, dst, peer, outgoing_tx);
        let transport = self.transport_with_cleanup(id, transport);
        {
            let mut state = self.state.write().await;
            state.tunnels.insert(
                id,
                ActiveTunnel {
                    peer,
                    route: route.clone(),
                    tunnel,
                },
            );
        }
        Ok((id, transport))
    }

    async fn channel_from_transport(
        &self,
        peer: HostId,
        transport: TunnelTransport,
    ) -> Result<Channel, TunnelPoolError> {
        match &self.channel_security {
            #[cfg(test)]
            TunnelChannelSecurity::Plain => Ok(channel_from_transport(transport)),
            #[cfg(not(test))]
            TunnelChannelSecurity::Plain => Err(TunnelPoolError::DeviceTlsRequired),
            TunnelChannelSecurity::DeviceTls {
                identity,
                trust_store,
                handshake_timeout,
            } => {
                let config = identity
                    .client_tls_config_for_peer(trust_store.clone(), peer)
                    .map_err(TunnelPoolError::Identity)?;
                let connector = TlsConnector::from(Arc::new(config));
                let server_name = ServerName::try_from("amux-device".to_string())
                    .map_err(|error| TunnelPoolError::Tls(error.to_string()))?;
                let tls = tokio::time::timeout(
                    *handshake_timeout,
                    connector.connect(server_name, transport),
                )
                .await
                .map_err(|_| TunnelPoolError::Tls("TLS handshake timed out".to_string()))?
                .map_err(|error| TunnelPoolError::Tls(error.to_string()))?;
                Ok(channel_from_tls_transport(tls))
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn handle_inbound_frame(
        &self,
        frame: pb::TunnelFrame,
    ) -> Result<(), TunnelPoolError> {
        self.handle_inbound_frame_from_link(frame, None).await
    }

    pub(crate) async fn handle_inbound_frame_from_link(
        &self,
        mut frame: pb::TunnelFrame,
        origin_link: Option<&Link>,
    ) -> Result<(), TunnelPoolError> {
        if frame.payload.len() > TUNNEL_FRAME_PAYLOAD_MAX {
            return Err(TunnelPoolError::PayloadTooLarge {
                actual: frame.payload.len(),
                max: TUNNEL_FRAME_PAYLOAD_MAX,
            });
        }
        let id = frame
            .tunnel_id
            .clone()
            .ok_or(TunnelPoolError::MissingTunnelId)
            .map(TunnelId::try_from)??;
        let mut dst = route_from_wire(frame.dst.take())?;
        let cloud_origin = match origin_link {
            Some(link) => self.links.is_cloud_relay(link).await,
            None => false,
        };
        if cloud_origin && !self.allow_cloud_inbound_tunnel_id(id).await {
            tracing::warn!(
                tunnel_id = %id.nonce,
                initiator = %id.initiator,
                "cloud inbound tunnel rate limit exceeded"
            );
            return Ok(());
        }
        if let Some(next_hop) = dst.pop() {
            frame.dst = Some(route_to_wire(&dst));
            if let Some(outgoing_tx) = self.links.existing_tx(&next_hop).await {
                let _ = outgoing_tx
                    .send(pb::Message {
                        body: Some(pb::message::Body::TunnelFrame(frame)),
                    })
                    .await;
            }
            return Ok(());
        }

        if let Some(inbound_tx) = self
            .state
            .read()
            .await
            .tunnels
            .get(&id)
            .map(|active| active.tunnel.inbound_sender())
        {
            return inbound_tx
                .send(Bytes::from(frame.payload))
                .await
                .map_err(|_| TunnelPoolError::InboundClosed);
        }
        if self.state.read().await.retired_tunnels.contains(&id) {
            return Ok(());
        }

        let Some(route) = self.routing.best_route(id.initiator).await else {
            return Ok(());
        };
        let Ok((first_hop, dst)) = outgoing_route_parts(id.initiator, &route) else {
            return Ok(());
        };

        let Some(outgoing_tx) = self.links.existing_tx(&first_hop).await else {
            return Ok(());
        };
        let (tunnel, transport) = create_tunnel(id, dst, id.initiator, outgoing_tx);
        let transport = self
            .transport_with_cleanup(id, transport)
            .with_cloud_pairing_reachability(cloud_origin);
        let inbound_tx = tunnel.inbound_sender();
        let mut transport = Some(transport);

        let inbound_tx = {
            let mut state = self.state.write().await;
            if let Some(existing) = state.tunnels.get(&id) {
                transport = None;
                Some(existing.tunnel.inbound_sender())
            } else if state.retired_tunnels.contains(&id) {
                transport = None;
                None
            } else {
                state.tunnels.insert(
                    id,
                    ActiveTunnel {
                        peer: id.initiator,
                        route: route.clone(),
                        tunnel,
                    },
                );
                Some(inbound_tx)
            }
        };
        let Some(inbound_tx) = inbound_tx else {
            return Ok(());
        };

        if let Some(transport) = transport
            && self.incoming_tunnels_tx.send(transport).await.is_err()
        {
            self.state.write().await.tunnels.remove(&id);
            return Err(TunnelPoolError::IncomingTunnelsClosed);
        }

        inbound_tx
            .send(Bytes::from(frame.payload))
            .await
            .map_err(|_| TunnelPoolError::InboundClosed)
    }

    async fn allow_cloud_inbound_tunnel_id(&self, id: TunnelId) -> bool {
        let mut state = self.state.write().await;
        allow_cloud_inbound_tunnel_id(&mut state, id)
    }

    fn transport_with_cleanup(&self, id: TunnelId, transport: TunnelTransport) -> TunnelTransport {
        let state = self.state.clone();
        transport.with_drop_hook(move || {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return;
            };
            handle.spawn(async move {
                let mut state = state.write().await;
                if state.tunnels.remove(&id).is_some() {
                    retire_tunnel_id(&mut state, id);
                }
            });
        })
    }

    #[cfg(test)]
    pub(crate) async fn remove_host(&self, host_id: HostId) {
        self.remove_host_preserving_tunnel(host_id, None).await;
    }

    pub(crate) async fn remove_host_preserving_tunnel(
        &self,
        host_id: HostId,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        let mut state = self.state.write().await;
        let retired = state
            .tunnels
            .iter()
            .filter_map(|(id, active)| {
                (active.peer == host_id && Some(*id) != preserve_tunnel_id).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in retired {
            state.tunnels.remove(&id);
            retire_tunnel_id(&mut state, id);
        }
    }

    pub(crate) async fn remove_route(&self, route: &Route) {
        self.remove_route_preserving_tunnel(route, None).await;
    }

    /// Retires the tunnels *this daemon initiated* over `route`. Hosted
    /// inbound tunnels are deliberately left alone: `route` is only their
    /// locally-resolved reply path, and a local route removal (e.g. a
    /// make-then-break swap to a better route) says nothing about the
    /// remote initiator's tunnel — retiring it here would silently brick
    /// the initiator's cached channel until its HTTP/2 keepalive reaps it
    /// (see NETWORKING_REVIEW.md §6.9). Inbound tunnels die with their
    /// peer ([`Self::remove_host_preserving_tunnel`]), with their
    /// transport (drop hook), or at server keepalive.
    pub(crate) async fn remove_route_preserving_tunnel(
        &self,
        route: &Route,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        let mut state = self.state.write().await;
        let retired = state
            .tunnels
            .iter()
            .filter_map(|(id, active)| {
                (id.initiator == self.my_host_id
                    && active.route == *route
                    && Some(*id) != preserve_tunnel_id)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in retired {
            state.tunnels.remove(&id);
            retire_tunnel_id(&mut state, id);
        }
    }

    /// Testnet observation seam: every active tunnel as
    /// `(initiator, peer, reply/forward route)`.
    #[cfg(feature = "testnet")]
    pub(crate) async fn active_tunnels(&self) -> Vec<(TunnelId, HostId, Route)> {
        self.state
            .read()
            .await
            .tunnels
            .iter()
            .map(|(id, active)| (*id, active.peer, active.route.clone()))
            .collect()
    }

    #[cfg(test)]
    pub(crate) async fn counts(&self) -> (usize, usize) {
        let state = self.state.read().await;
        (state.tunnels.len(), state.retired_tunnels.len())
    }
}

impl BoundedTunnelIdSet {
    fn contains(&self, id: &TunnelId) -> bool {
        self.ids.contains(id)
    }

    fn insert(&mut self, id: TunnelId, cap: usize) -> bool {
        if !self.ids.insert(id) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > cap {
            if let Some(expired) = self.order.pop_front() {
                self.ids.remove(&expired);
            }
        }
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.ids.len()
    }
}

fn retire_tunnel_id(state: &mut PoolState, id: TunnelId) {
    state.retired_tunnels.insert(id, RETIRED_TUNNEL_CAP);
}

fn allow_cloud_inbound_tunnel_id(state: &mut PoolState, id: TunnelId) -> bool {
    if state.tunnels.contains_key(&id)
        || state.retired_tunnels.contains(&id)
        || state.cloud_limited_tunnels.contains(&id)
    {
        return true;
    }
    if !state.cloud_inbound_tunnel_limiter.allow(()) {
        return false;
    }
    remember_cloud_limited_tunnel_id(state, id);
    true
}

fn remember_cloud_limited_tunnel_id(state: &mut PoolState, id: TunnelId) {
    state
        .cloud_limited_tunnels
        .insert(id, CLOUD_INBOUND_TUNNEL_ID_CACHE_CAP);
}

impl From<LinkRegistryError> for TunnelPoolError {
    fn from(error: LinkRegistryError) -> Self {
        match error {
            LinkRegistryError::Unavailable { link } => Self::LinkUnavailable { link },
            LinkRegistryError::Draining { link } => Self::LinkDraining { link },
        }
    }
}

fn route_from_wire(route: Option<pb::Route>) -> Result<Route, TunnelPoolError> {
    let route = route.ok_or(TunnelPoolError::MissingDestination)?;
    Route::from_links(route.links).map_err(|error| TunnelPoolError::InvalidRoute {
        message: error.to_string(),
    })
}

#[cfg(test)]
fn channel_from_transport(transport: TunnelTransport) -> Channel {
    channel_from_single_io(
        configure_tonic_endpoint_keepalive(Endpoint::from_static("http://tunnel")),
        "TunnelTransport",
        transport,
    )
}

fn channel_from_tls_transport(
    transport: tokio_rustls::client::TlsStream<TunnelTransport>,
) -> Channel {
    channel_from_single_io(
        configure_tonic_endpoint_keepalive(Endpoint::from_static("https://tunnel")),
        "TLS TunnelTransport",
        transport,
    )
}

fn outgoing_route_parts(host_id: HostId, route: &Route) -> Result<(Link, Route), TunnelPoolError> {
    let mut dst = route.clone();
    let first_hop = dst.pop().ok_or(TunnelPoolError::EmptyRoute { host_id })?;
    Ok((first_hop, dst))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::routing::{
        Capabilities, FEATURE_CLOUD_RELAY, Host, LinkRole, Route, RoutingEvent, SupportedAgentType,
    };
    use crate::trust::{Reachability, TrustEntry, TrustStore};

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

    fn cloud_host(id: u128, name: &str) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: name.to_string(),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: vec![FEATURE_CLOUD_RELAY.to_string()],
                supported_agent_types: Vec::new(),
            },
        }
    }

    fn link(name: &str) -> Link {
        Link::new(name).unwrap()
    }

    fn route(name: &str) -> Route {
        Route::from_link(link(name))
    }

    fn tunnel_id(initiator: HostId, nonce: u128) -> TunnelId {
        TunnelId::from_parts(initiator, uuid::Uuid::from_u128(nonce))
    }

    async fn register_test_link(pool: &TunnelPool, name: &str, tx: mpsc::Sender<pb::Message>) {
        register_test_link_with_role(pool, name, tx, LinkRole::Peer).await;
    }

    async fn register_test_link_with_role(
        pool: &TunnelPool,
        name: &str,
        tx: mpsc::Sender<pb::Message>,
        role: LinkRole,
    ) {
        pool.link_registry()
            .register_with_role(link(name), HostId::from_u128(99), tx, role)
            .await;
    }

    async fn wait_for_counts(pool: &TunnelPool, expected: (usize, usize)) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if pool.counts().await == expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for tunnel pool counts");
    }

    fn trust_store_for(peer: &DeviceIdentity) -> TrustStore {
        let mut trust_store = TrustStore::default();
        trust_store.insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: peer.public_key().to_vec(),
                name: format!("peer-{}", peer.host_id),
                paired_at: DateTime::<Utc>::from_timestamp(200, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        trust_store
    }

    fn device_tls_pool_with_timeout(
        my_identity: DeviceIdentity,
        routing: Arc<RoutingCore>,
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
        peer: &DeviceIdentity,
        timeout: Duration,
    ) -> TunnelPool {
        TunnelPool::with_link_registry_and_security(
            my_identity.host_id,
            routing,
            Arc::new(LinkRegistry::default()),
            incoming_tunnels_tx,
            TunnelChannelSecurity::DeviceTls {
                identity: my_identity,
                trust_store: Arc::new(std::sync::RwLock::new(trust_store_for(peer))),
                handshake_timeout: timeout,
            },
        )
    }

    async fn recv_routing_event(rx: &mut mpsc::Receiver<pb::Message>) -> pb::RoutingEvent {
        let message = rx.recv().await.expect("expected routing message");
        let Some(pb::message::Body::RoutingEvent(event)) = message.body else {
            panic!("expected routing event message");
        };
        event
    }

    fn frame(id: TunnelId, payload: &[u8]) -> pb::TunnelFrame {
        pb::TunnelFrame {
            dst: Some(pb::Route { links: Vec::new() }),
            tunnel_id: Some(id.into()),
            payload: payload.to_vec(),
        }
    }

    fn routed_frame(dst: &[&str], id: TunnelId, payload: &[u8]) -> pb::TunnelFrame {
        pb::TunnelFrame {
            dst: Some(pb::Route {
                links: dst.iter().map(|link| link.to_string()).collect(),
            }),
            tunnel_id: Some(id.into()),
            payload: payload.to_vec(),
        }
    }

    fn routed_frame_with_id(dst: &[&str], id: TunnelId, payload: &[u8]) -> pb::TunnelFrame {
        pb::TunnelFrame {
            dst: Some(pb::Route {
                links: dst.iter().map(|link| link.to_string()).collect(),
            }),
            tunnel_id: Some(id.into()),
            payload: payload.to_vec(),
        }
    }

    #[tokio::test]
    async fn channel_to_returns_not_found_when_peer_has_no_route() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);

        assert!(matches!(
            pool.channel_to(HostId::from_u128(2)).await,
            Err(TunnelPoolError::NotFound { host_id }) if host_id == HostId::from_u128(2)
        ));
    }

    #[tokio::test]
    async fn inactive_link_buffers_deltas_until_snapshot_activation() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(8);
        let link_name = link("relay");
        let host = host(2, "peer");
        pool.link_registry()
            .register(link_name.clone(), HostId::from_u128(99), link_tx)
            .await;

        pool.link_registry()
            .broadcast_routing_event(&RoutingEvent::HostUp {
                host: host.clone(),
                route: route("relay"),
                origin_link: None,
            })
            .await;
        pool.link_registry()
            .broadcast_routing_event(&RoutingEvent::HostDown {
                host_id: host.id,
                route: route("relay"),
                origin_link: None,
            })
            .await;

        assert!(link_rx.try_recv().is_err());
        assert!(
            pool.link_registry()
                .activate(&link_name, [(host.id, route("relay"))])
                .await
        );

        let event = recv_routing_event(&mut link_rx).await;
        assert!(matches!(
            event.event,
            Some(pb::routing_event::Event::HostDown(down))
                if down.host_id == host.id.as_bytes()
        ));
        assert!(link_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn send_goaway_to_all_uses_typed_reason() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        pool.link_registry()
            .send_goaway_to_all(pb::GoAwayReason::Suspending, 200)
            .await;

        let message = link_rx.recv().await.expect("expected goaway message");
        let Some(pb::message::Body::Goaway(goaway)) = message.body else {
            panic!("expected GoAway");
        };
        assert_eq!(goaway.reason, pb::GoAwayReason::Suspending as i32);
        assert_eq!(goaway.drain_timeout_ms, 200);
    }

    #[tokio::test]
    async fn channel_to_creates_initiator_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let my_host_id = HostId::from_u128(1);
        let peer = HostId::from_u128(2);
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(my_host_id, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        let _first = pool.channel_to(peer).await.unwrap();
        let _second = pool.channel_to(peer).await.unwrap();

        assert_eq!(pool.counts().await, (2, 0));
    }

    #[tokio::test]
    async fn channel_to_rechecks_route_before_opening_channel() {
        let routing = Arc::new(RoutingCore::new());
        let my_host_id = HostId::from_u128(1);
        let peer = HostId::from_u128(2);
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(my_host_id, routing.clone(), incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        let _channel = pool.channel_to(peer).await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        routing.apply_host_down(peer, &route("relay"), None).await;

        assert!(matches!(
            pool.channel_to(peer).await,
            Err(TunnelPoolError::NotFound { host_id }) if host_id == peer
        ));
    }

    #[tokio::test]
    async fn channel_to_rejects_new_channels_on_draining_link() {
        let routing = Arc::new(RoutingCore::new());
        let my_host_id = HostId::from_u128(1);
        let peer = HostId::from_u128(2);
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(my_host_id, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let relay = link("relay");
        pool.link_registry()
            .register(relay.clone(), HostId::from_u128(99), link_tx)
            .await;
        assert!(pool.link_registry().mark_draining(&relay).await);

        assert!(matches!(
            pool.channel_to(peer).await,
            Err(TunnelPoolError::LinkDraining { link }) if link == relay
        ));
        assert_eq!(pool.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_reports_missing_first_hop_writer() {
        let routing = Arc::new(RoutingCore::new());
        let peer = HostId::from_u128(2);
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);

        assert!(matches!(
            pool.channel_to(peer).await,
            Err(TunnelPoolError::LinkUnavailable { link }) if link == Link::new("relay").unwrap()
        ));
    }

    #[tokio::test]
    async fn inbound_frame_creates_target_tunnel_and_reuses_it() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(host(1, "initiator"), route("relay"), None)
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(2);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        let id = tunnel_id(initiator, 42);
        pool.handle_inbound_frame(frame(id, b"hello"))
            .await
            .unwrap();
        pool.handle_inbound_frame(frame(id, b"!")).await.unwrap();

        let mut transport = incoming_rx.recv().await.unwrap();
        assert_eq!(transport.peer(), initiator);
        assert!(incoming_rx.try_recv().is_err());

        let mut buf = [0_u8; 6];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello!");
        assert_eq!(pool.counts().await, (1, 0));
    }

    #[tokio::test]
    async fn inbound_endpoint_frame_without_return_route_is_dropped() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (incoming_tx, mut incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(target, routing, incoming_tx);

        pool.handle_inbound_frame(frame(tunnel_id(initiator, 42), b"hello"))
            .await
            .unwrap();

        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn dropping_inbound_endpoint_transport_removes_target_tunnel() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(host(1, "initiator"), route("relay"), None)
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(2);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        let id = tunnel_id(initiator, 42);
        pool.handle_inbound_frame(frame(id, b"hello"))
            .await
            .unwrap();
        let transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_host_preserving_tunnel(initiator, Some(id))
            .await;
        assert_eq!(pool.counts().await, (1, 0));

        drop(transport);
        wait_for_counts(&pool, (0, 1)).await;

        pool.handle_inbound_frame(frame(id, b"late")).await.unwrap();

        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.counts().await, (0, 1));
    }

    #[tokio::test]
    async fn inbound_endpoint_transport_marks_cloud_pairing_reachability_from_origin_link() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(cloud_host(3, "cloud"), route("cloud"), None)
            .await;
        routing
            .apply_host_up(
                host(1, "initiator"),
                Route::from_links(["cloud".to_string(), "initiator".to_string()]).unwrap(),
                None,
            )
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(2);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link_with_role(&pool, "cloud", link_tx, LinkRole::CloudRelay).await;

        pool.handle_inbound_frame_from_link(
            frame(tunnel_id(initiator, 42), b"hello"),
            Some(&link("cloud")),
        )
        .await
        .unwrap();

        let transport = incoming_rx.recv().await.unwrap();
        assert!(transport.has_cloud_pairing_reachability());
    }

    #[tokio::test]
    async fn cloud_origin_new_inbound_tunnels_are_rate_limited() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(cloud_host(3, "cloud"), route("cloud"), None)
            .await;
        routing
            .apply_host_up(
                host(1, "initiator"),
                Route::from_links(["cloud".to_string(), "initiator".to_string()]).unwrap(),
                None,
            )
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(CLOUD_INBOUND_TUNNEL_RATE_LIMIT + 1);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(64);
        register_test_link_with_role(&pool, "cloud", link_tx, LinkRole::CloudRelay).await;
        pool.routing
            .apply_host_down(HostId::from_u128(3), &route("cloud"), None)
            .await;

        for nonce in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_frame_from_link(
                frame(tunnel_id(initiator, nonce + 1), b"hello"),
                Some(&link("cloud")),
            )
            .await
            .unwrap();
        }
        pool.handle_inbound_frame_from_link(
            frame(tunnel_id(initiator, 10_000), b"excess"),
            Some(&link("cloud")),
        )
        .await
        .unwrap();

        assert_eq!(pool.counts().await, (CLOUD_INBOUND_TUNNEL_RATE_LIMIT, 0));
        let mut received = 0;
        while incoming_rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
    }

    #[tokio::test]
    async fn cloud_origin_forwarded_new_tunnels_are_rate_limited() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(cloud_host(3, "cloud"), route("cloud"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(target, routing.clone(), incoming_tx);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        register_test_link_with_role(&pool, "cloud", cloud_tx, LinkRole::CloudRelay).await;
        let (link_tx, mut link_rx) = mpsc::channel(CLOUD_INBOUND_TUNNEL_RATE_LIMIT + 1);
        register_test_link(&pool, "next", link_tx).await;
        routing
            .apply_host_down(HostId::from_u128(3), &route("cloud"), None)
            .await;

        for nonce in 0..=CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_frame_from_link(
                routed_frame(&["next"], tunnel_id(initiator, nonce + 1), b"hello"),
                Some(&link("cloud")),
            )
            .await
            .unwrap();
        }

        let mut forwarded = 0;
        while link_rx.try_recv().is_ok() {
            forwarded += 1;
        }
        assert_eq!(forwarded, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
    }

    #[tokio::test]
    async fn repeated_cloud_frames_for_one_tunnel_id_consume_one_rate_slot() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(cloud_host(3, "cloud"), route("cloud"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        register_test_link_with_role(&pool, "cloud", cloud_tx, LinkRole::CloudRelay).await;
        let (link_tx, mut link_rx) = mpsc::channel(128);
        register_test_link(&pool, "next", link_tx).await;
        let repeated = tunnel_id(initiator, 7);

        for _ in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT {
            pool.handle_inbound_frame_from_link(
                routed_frame(&["next"], repeated, b"same"),
                Some(&link("cloud")),
            )
            .await
            .unwrap();
        }
        for nonce in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_frame_from_link(
                routed_frame(&["next"], tunnel_id(initiator, 1_000 + nonce), b"unique"),
                Some(&link("cloud")),
            )
            .await
            .unwrap();
        }

        let mut forwarded = 0;
        while link_rx.try_recv().is_ok() {
            forwarded += 1;
        }
        assert_eq!(forwarded, (CLOUD_INBOUND_TUNNEL_RATE_LIMIT * 2) - 1);
    }

    #[tokio::test]
    async fn inbound_endpoint_transport_marks_non_cloud_origin_without_reusable_reachability() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(host(1, "initiator"), route("relay"), None)
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(2);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        pool.handle_inbound_frame_from_link(
            frame(tunnel_id(initiator, 42), b"hello"),
            Some(&link("relay")),
        )
        .await
        .unwrap();

        let transport = incoming_rx.recv().await.unwrap();
        assert!(!transport.has_cloud_pairing_reachability());
    }

    #[tokio::test]
    async fn outbound_tls_timeout_removes_provisional_tunnel() {
        let local_identity = DeviceIdentity::for_test(HostId::from_u128(1));
        let peer_identity = DeviceIdentity::for_test(HostId::from_u128(2));
        let routing = Arc::new(RoutingCore::new());
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = device_tls_pool_with_timeout(
            local_identity,
            routing,
            incoming_tx,
            &peer_identity,
            Duration::from_millis(10),
        );
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        let result = pool.channel_to(peer_identity.host_id).await;
        assert!(
            matches!(result, Err(TunnelPoolError::Tls(message)) if message.contains("timed out"))
        );
        wait_for_counts(&pool, (0, 0)).await;
    }

    #[tokio::test]
    async fn pin_pairing_tls_timeout_removes_provisional_tunnel() {
        let routing = Arc::new(RoutingCore::new());
        let peer = HostId::from_u128(2);
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx)
            .with_pin_pairing_handshake_timeout(Duration::from_millis(10));
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        let result = pool
            .pin_pairing_channel_to_route(peer, route("relay"))
            .await;
        assert!(
            matches!(result, Err(TunnelPoolError::Tls(message)) if message.contains("timed out"))
        );
        wait_for_counts(&pool, (0, 0)).await;
    }

    #[tokio::test]
    async fn inbound_frame_delivers_to_existing_initiator_tunnel() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(initiator, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);

        let id = tunnel_id(initiator, 42);
        let (tunnel, mut transport) = create_tunnel(id, route("relay"), target, link_tx);
        pool.state.write().await.tunnels.insert(
            id,
            ActiveTunnel {
                #[cfg(test)]
                peer: target,
                route: route("relay"),
                tunnel,
            },
        );

        pool.handle_inbound_frame(frame(id, b"pong")).await.unwrap();

        let mut buf = [0_u8; 4];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn inbound_frame_delivers_existing_tunnel_on_draining_link() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(initiator, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let relay = link("relay");
        pool.link_registry()
            .register(relay.clone(), target, link_tx)
            .await;

        let id = tunnel_id(initiator, 42);
        let (tunnel, mut transport) = create_tunnel(id, route("relay"), target, mpsc::channel(8).0);
        pool.state.write().await.tunnels.insert(
            id,
            ActiveTunnel {
                #[cfg(test)]
                peer: target,
                route: route("relay"),
                tunnel,
            },
        );
        assert!(pool.link_registry().mark_draining(&relay).await);

        pool.handle_inbound_frame(frame(id, b"pong")).await.unwrap();

        let mut buf = [0_u8; 4];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    #[tokio::test]
    async fn inbound_frame_can_create_target_tunnel_on_draining_link() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        routing
            .apply_host_up(host(1, "initiator"), route("relay"), None)
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let relay = link("relay");
        pool.link_registry()
            .register(relay.clone(), initiator, link_tx)
            .await;
        assert!(pool.link_registry().mark_draining(&relay).await);

        let id = tunnel_id(initiator, 42);
        pool.handle_inbound_frame(frame(id, b"hello"))
            .await
            .unwrap();

        let mut transport = incoming_rx.recv().await.unwrap();
        let mut buf = [0_u8; 5];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn inbound_frame_with_destination_is_forwarded_to_next_hop() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(1);
        register_test_link(&pool, "relay", link_tx).await;

        let id = tunnel_id(HostId::from_u128(10), 20);
        pool.handle_inbound_frame(routed_frame(&["relay", "target"], id, b"payload"))
            .await
            .unwrap();

        let forwarded = link_rx.recv().await.unwrap();
        let Some(pb::message::Body::TunnelFrame(frame)) = forwarded.body else {
            panic!("expected forwarded TunnelFrame");
        };
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst.unwrap().links, ["target"]);
        assert_eq!(TunnelId::try_from(frame.tunnel_id.unwrap()).unwrap(), id);
    }

    #[tokio::test]
    async fn forwarded_tunnel_frame_does_not_synthesize_routing_events() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(10);
        routing
            .apply_host_up(host(10, "initiator"), route("from-initiator"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(30), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(4);
        register_test_link(&pool, "next", link_tx).await;

        let id = tunnel_id(initiator, 20);
        pool.handle_inbound_frame(routed_frame_with_id(&["next"], id, b"first"))
            .await
            .unwrap();
        pool.handle_inbound_frame(routed_frame_with_id(&["next"], id, b"second"))
            .await
            .unwrap();

        for expected_payload in [b"first".as_slice(), b"second".as_slice()] {
            let forwarded = link_rx.recv().await.unwrap();
            let Some(pb::message::Body::TunnelFrame(frame)) = forwarded.body else {
                panic!("expected TunnelFrame without tunnel-side routing events");
            };
            assert_eq!(frame.payload, expected_payload);
            assert_eq!(frame.dst.unwrap().links, Vec::<String>::new());
            assert_eq!(TunnelId::try_from(frame.tunnel_id.unwrap()).unwrap(), id);
        }
        assert!(link_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn forwarded_tunnel_frame_does_not_announce_initiator_back_to_origin_route() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(10);
        routing
            .apply_host_up(host(10, "initiator"), route("origin"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(30), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(2);
        register_test_link(&pool, "origin", link_tx).await;

        let id = tunnel_id(initiator, 20);
        pool.handle_inbound_frame(routed_frame_with_id(&["origin"], id, b"reply"))
            .await
            .unwrap();

        let forwarded = link_rx.recv().await.unwrap();
        let Some(pb::message::Body::TunnelFrame(frame)) = forwarded.body else {
            panic!("expected TunnelFrame without echoing HostUp to origin route");
        };
        assert_eq!(frame.payload, b"reply");
        assert!(link_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn inbound_frame_for_missing_next_hop_is_dropped() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);

        let id = tunnel_id(HostId::from_u128(10), 20);
        pool.handle_inbound_frame(routed_frame(&["missing"], id, b"payload"))
            .await
            .unwrap();
        assert_eq!(pool.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn inbound_frame_rejects_oversized_payload() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);
        let id = tunnel_id(HostId::from_u128(10), 20);
        let payload = vec![0_u8; TUNNEL_FRAME_PAYLOAD_MAX + 1];

        let error = pool
            .handle_inbound_frame(routed_frame(&["next"], id, &payload))
            .await
            .expect_err("oversized frame should be rejected");

        assert!(matches!(
            error,
            TunnelPoolError::PayloadTooLarge {
                actual,
                max,
            } if actual == TUNNEL_FRAME_PAYLOAD_MAX + 1 && max == TUNNEL_FRAME_PAYLOAD_MAX
        ));
    }

    #[tokio::test]
    async fn inbound_frame_requires_destination_and_tunnel_id() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);
        let id = tunnel_id(HostId::from_u128(10), 20);

        let missing_dst = pb::TunnelFrame {
            dst: None,
            tunnel_id: Some(id.into()),
            payload: Vec::new(),
        };
        assert!(matches!(
            pool.handle_inbound_frame(missing_dst).await,
            Err(TunnelPoolError::MissingDestination)
        ));

        let missing_tunnel_id = pb::TunnelFrame {
            dst: Some(pb::Route {
                links: vec!["relay".to_string()],
            }),
            tunnel_id: None,
            payload: Vec::new(),
        };
        assert!(matches!(
            pool.handle_inbound_frame(missing_tunnel_id).await,
            Err(TunnelPoolError::MissingTunnelId)
        ));
    }

    #[tokio::test]
    async fn endpoint_frame_uses_empty_dst_as_the_target_signal() {
        let routing = Arc::new(RoutingCore::new());
        let local = HostId::from_u128(1);
        let initiator = HostId::from_u128(3);
        routing
            .apply_host_up(host(3, "initiator"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(local, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;

        pool.handle_inbound_frame(frame(tunnel_id(initiator, 99), b"payload"))
            .await
            .unwrap();

        assert_eq!(pool.counts().await, (1, 0));
    }

    #[tokio::test]
    async fn remove_host_drops_related_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let my_host_id = HostId::from_u128(1);
        let peer = HostId::from_u128(2);
        routing
            .apply_host_up(host(2, "peer"), route("relay"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(my_host_id, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;
        let _channel = pool.channel_to(peer).await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_host(peer).await;
        assert_eq!(pool.counts().await, (0, 1));
    }

    #[tokio::test]
    async fn remove_route_drops_related_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let my_host_id = HostId::from_u128(1);
        let peer = HostId::from_u128(2);
        let peer_route = route("relay");
        routing
            .apply_host_up(host(2, "peer"), peer_route.clone(), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(my_host_id, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, "relay", link_tx).await;
        let _channel = pool.channel_to(peer).await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_route(&route("other")).await;
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_route(&peer_route).await;
        assert_eq!(pool.counts().await, (0, 1));
    }

    /// A local route removal (e.g. a make-then-break swap) must not retire
    /// tunnels a *remote* initiator is hosting here — the removed route was
    /// only this side's reply-path hint, and sweeping the tunnel would
    /// silently brick the initiator's cached channel (NETWORKING_REVIEW.md
    /// §6.9). Hosted inbound tunnels die with their peer instead.
    #[tokio::test]
    async fn removed_reply_route_keeps_hosted_inbound_tunnels_alive() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let old_route = route("old");
        let new_route = route("new");
        routing
            .apply_host_up(host(1, "initiator"), old_route.clone(), None)
            .await;
        routing
            .apply_host_up(host(1, "initiator"), new_route.clone(), None)
            .await;

        let (incoming_tx, mut incoming_rx) = mpsc::channel(2);
        let pool = TunnelPool::new(target, routing.clone(), incoming_tx);
        let (old_tx, _old_rx) = mpsc::channel(8);
        register_test_link(&pool, "old", old_tx).await;
        let (new_tx, _new_rx) = mpsc::channel(8);
        register_test_link(&pool, "new", new_tx).await;
        let id = tunnel_id(initiator, 42);

        pool.handle_inbound_frame(frame(id, b"first"))
            .await
            .unwrap();
        let _transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        routing.apply_host_down(initiator, &old_route, None).await;
        pool.remove_route(&old_route).await;
        assert_eq!(
            pool.counts().await,
            (1, 0),
            "the initiator's tunnel outlives this side's reply-route removal"
        );
        pool.handle_inbound_frame(frame(id, b"late")).await.unwrap();

        // The peer-scoped teardown (revocation, key replacement, peer gone)
        // is what retires hosted inbound tunnels.
        pool.remove_host(initiator).await;
        assert_eq!(pool.counts().await, (0, 1));

        pool.handle_inbound_frame(frame(id, b"dead")).await.unwrap();
        assert_eq!(pool.counts().await, (0, 1));
    }

    #[test]
    fn retired_tunnel_tombstones_are_bounded() {
        let mut state = PoolState::default();
        for nonce in 0..(RETIRED_TUNNEL_CAP as u128 + 2) {
            retire_tunnel_id(&mut state, tunnel_id(HostId::from_u128(1), nonce));
        }

        assert_eq!(state.retired_tunnels.len(), RETIRED_TUNNEL_CAP);
        assert_eq!(state.retired_tunnels.order.len(), RETIRED_TUNNEL_CAP);
        assert!(
            !state
                .retired_tunnels
                .contains(&tunnel_id(HostId::from_u128(1), 0))
        );
        assert!(state.retired_tunnels.contains(&tunnel_id(
            HostId::from_u128(1),
            RETIRED_TUNNEL_CAP as u128 + 1,
        )));
    }
}
