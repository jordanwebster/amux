//! The tunnel pool: endpoint state for tunnels this daemon initiates or
//! hosts, plus the relay forwarding rule.
//!
//! Forwarding is rule 2 of the routing model: a frame addressed to `dst` is
//! forwarded iff this daemon holds a direct link to `dst`; otherwise it is
//! dropped. Relays keep no per-tunnel state — forwarding consults only the
//! link registry. Replies travel back out the link the tunnel's frames
//! arrive on, addressed to the initiator; no reverse-route lookup exists.

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
use crate::routing::{LinkId, LinkRegistry, LinkUnavailable, RoutingCore};
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
    #[error("no live link to host {host_id}")]
    LinkUnavailable { host_id: HostId },
    #[error("TunnelFrame missing tunnel_id")]
    MissingTunnelId,
    #[error("TunnelFrame dst must be a 16-byte host_id, got {actual} bytes")]
    InvalidDestination { actual: usize },
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
    /// The remote endpoint (initiator for hosted tunnels, target for
    /// initiated ones).
    peer: HostId,
    /// The link the tunnel is pinned to; the tunnel dies with it.
    link: LinkId,
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
    #[allow(dead_code)]
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
        Self::with_link_registry_and_security(
            my_host_id,
            routing,
            Arc::new(LinkRegistry::default()),
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

    /// Opens a tunnel-backed channel to `peer` through the adjacent
    /// `relay`. The tunnel is pinned to the relay link chosen here.
    pub(crate) async fn channel_via(
        &self,
        peer: HostId,
        relay: HostId,
    ) -> Result<Channel, TunnelPoolError> {
        let (link, outgoing_tx) = self
            .links
            .link_to_peer(relay)
            .await
            .ok_or(TunnelPoolError::LinkUnavailable { host_id: relay })?;

        let id = TunnelId::new(self.my_host_id);
        let (tunnel, transport) = create_tunnel(id, peer, outgoing_tx);
        let transport = self.transport_with_cleanup(id, transport);
        {
            let mut state = self.state.write().await;
            state
                .tunnels
                .insert(id, ActiveTunnel { peer, link, tunnel });
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

    pub(crate) async fn pin_pairing_channel_via(
        &self,
        peer: HostId,
        relay: HostId,
    ) -> Result<Channel, TunnelPoolError> {
        let (id, transport) = self.pairing_transport_via(peer, relay).await?;
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

    pub(crate) async fn qr_pairing_channel_via(
        &self,
        peer: HostId,
        relay: HostId,
        expected_pubkey: Vec<u8>,
    ) -> Result<Channel, TunnelPoolError> {
        let (id, transport) = self.pairing_transport_via(peer, relay).await?;
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

    async fn pairing_transport_via(
        &self,
        peer: HostId,
        relay: HostId,
    ) -> Result<(TunnelId, TunnelTransport), TunnelPoolError> {
        let (link, outgoing_tx) = self
            .links
            .link_to_peer(relay)
            .await
            .ok_or(TunnelPoolError::LinkUnavailable { host_id: relay })?;

        let id = TunnelId::new(self.my_host_id);
        let (tunnel, transport) = create_tunnel(id, peer, outgoing_tx);
        let transport = self.transport_with_cleanup(id, transport);
        {
            let mut state = self.state.write().await;
            state
                .tunnels
                .insert(id, ActiveTunnel { peer, link, tunnel });
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

    /// Handles one inbound frame from `origin_link`. `dst != self` is the
    /// relay path: forward iff a direct link to `dst` exists, else drop.
    /// `dst == self` delivers to the addressed tunnel, creating a hosted
    /// endpoint — pinned to the arrival link, replying to the initiator —
    /// for a fresh id.
    pub(crate) async fn handle_inbound_frame_from_link(
        &self,
        frame: pb::TunnelFrame,
        origin_link: &LinkId,
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
        let dst = host_id_from_wire(&frame.dst)?;
        let cloud_origin = self.links.is_cloud_relay(origin_link).await;
        if cloud_origin && !self.allow_cloud_inbound_tunnel_id(id).await {
            tracing::warn!(
                tunnel_id = %id.nonce,
                initiator = %id.initiator,
                "cloud inbound tunnel rate limit exceeded"
            );
            return Ok(());
        }

        if dst != self.my_host_id {
            // Rule 2: forward only to adjacency.
            let message = pb::Message {
                body: Some(pb::message::Body::TunnelFrame(frame)),
            };
            if !self.links.forward_to_peer(dst, message).await {
                tracing::debug!(
                    dst = %dst,
                    tunnel_id = %id.nonce,
                    "dropping tunnel frame for a host with no direct link"
                );
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

        // A fresh inbound tunnel: pinned to its arrival link, replying to
        // the frame's initiator out that same link.
        let Ok(outgoing_tx) = self.links.outgoing_tx(origin_link).await else {
            return Ok(());
        };
        let (tunnel, transport) = create_tunnel(id, id.initiator, outgoing_tx);
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
                        link: *origin_link,
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

    pub(crate) async fn remove_host(&self, host_id: HostId) {
        let mut state = self.state.write().await;
        let retired = state
            .tunnels
            .iter()
            .filter_map(|(id, active)| (active.peer == host_id).then_some(*id))
            .collect::<Vec<_>>();
        for id in retired {
            state.tunnels.remove(&id);
            retire_tunnel_id(&mut state, id);
        }
    }

    /// A tunnel is pinned to the link its first frame used and dies with
    /// that link — initiated and hosted alike.
    pub(crate) async fn remove_link(&self, link: &LinkId) {
        let mut state = self.state.write().await;
        let retired = state
            .tunnels
            .iter()
            .filter_map(|(id, active)| (active.link == *link).then_some(*id))
            .collect::<Vec<_>>();
        for id in retired {
            state.tunnels.remove(&id);
            retire_tunnel_id(&mut state, id);
        }
    }

    /// Retires the tunnels *this daemon initiated* to `peer` over a link to
    /// `relay` (a make-then-break swap or a withdrawn claim). Hosted inbound
    /// tunnels are deliberately left alone: a local route change says
    /// nothing about the remote initiator's tunnel, and sweeping it would
    /// silently brick the initiator's cached channel
    /// (NETWORKING_REVIEW.md §6.9).
    pub(crate) async fn remove_initiated_via(&self, peer: HostId, relay: HostId) {
        let mut state = self.state.write().await;
        let retired = state
            .tunnels
            .iter()
            .filter_map(|(id, active)| {
                (id.initiator == self.my_host_id
                    && active.peer == peer
                    && active.link.peer() == relay)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in retired {
            state.tunnels.remove(&id);
            retire_tunnel_id(&mut state, id);
        }
    }

    /// Testnet observation seam: every active tunnel as
    /// `(id, remote peer, pinned link)`.
    #[cfg(feature = "testnet")]
    pub(crate) async fn active_tunnels(&self) -> Vec<(TunnelId, HostId, LinkId)> {
        self.state
            .read()
            .await
            .tunnels
            .iter()
            .map(|(id, active)| (*id, active.peer, active.link))
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

impl From<LinkUnavailable> for TunnelPoolError {
    fn from(error: LinkUnavailable) -> Self {
        Self::LinkUnavailable {
            host_id: error.host_id,
        }
    }
}

fn host_id_from_wire(bytes: &[u8]) -> Result<HostId, TunnelPoolError> {
    HostId::from_slice(bytes).map_err(|_| TunnelPoolError::InvalidDestination {
        actual: bytes.len(),
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::routing::{Capabilities, Host, LinkRole, SupportedAgentType};
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

    fn tunnel_id(initiator: HostId, nonce: u128) -> TunnelId {
        TunnelId::from_parts(initiator, uuid::Uuid::from_u128(nonce))
    }

    fn test_pool(my_host_id: HostId) -> (TunnelPool, mpsc::Receiver<TunnelTransport>) {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, incoming_rx) = mpsc::channel(64);
        (
            TunnelPool::new(my_host_id, routing, incoming_tx),
            incoming_rx,
        )
    }

    async fn register_test_link(
        pool: &TunnelPool,
        peer: u128,
        tx: mpsc::Sender<pb::Message>,
    ) -> LinkId {
        register_test_link_with_role(pool, peer, tx, LinkRole::Peer).await
    }

    async fn register_test_link_with_role(
        pool: &TunnelPool,
        peer: u128,
        tx: mpsc::Sender<pb::Message>,
        role: LinkRole,
    ) -> LinkId {
        let link = LinkId::new(HostId::from_u128(peer));
        pool.link_registry()
            .register(link, host(peer, &format!("peer-{peer}")), tx, role, &[])
            .await;
        link
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
        incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
        peer: &DeviceIdentity,
        timeout: Duration,
    ) -> TunnelPool {
        TunnelPool::with_link_registry_and_security(
            my_identity.host_id,
            Arc::new(RoutingCore::new()),
            Arc::new(LinkRegistry::default()),
            incoming_tunnels_tx,
            TunnelChannelSecurity::DeviceTls {
                identity: my_identity,
                trust_store: Arc::new(std::sync::RwLock::new(trust_store_for(peer))),
                handshake_timeout: timeout,
            },
        )
    }

    fn frame_to(dst: HostId, id: TunnelId, payload: &[u8]) -> pb::TunnelFrame {
        pb::TunnelFrame {
            dst: dst.as_bytes().to_vec(),
            tunnel_id: Some(id.into()),
            payload: payload.to_vec(),
        }
    }

    #[tokio::test]
    async fn channel_via_reports_missing_relay_link() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));

        assert!(matches!(
            pool.channel_via(HostId::from_u128(2), HostId::from_u128(99)).await,
            Err(TunnelPoolError::LinkUnavailable { host_id }) if host_id == HostId::from_u128(99)
        ));
    }

    #[tokio::test]
    async fn channel_via_creates_initiator_tunnels_pinned_to_the_relay_link() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let relay_link = register_test_link(&pool, 99, link_tx).await;

        let _first = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();
        let _second = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();

        assert_eq!(pool.counts().await, (2, 0));
        for (_, tunnel_peer, link) in pool_active(&pool).await {
            assert_eq!(tunnel_peer, peer);
            assert_eq!(link, relay_link);
        }
    }

    async fn pool_active(pool: &TunnelPool) -> Vec<(TunnelId, HostId, LinkId)> {
        pool.state
            .read()
            .await
            .tunnels
            .iter()
            .map(|(id, active)| (*id, active.peer, active.link))
            .collect()
    }

    /// The reply rule: a frame for an unknown tunnel addressed to us creates
    /// a hosted endpoint that replies out the arrival link to the initiator.
    /// No reverse-route lookup exists to fail.
    #[tokio::test]
    async fn inbound_frame_creates_target_tunnel_replying_out_the_arrival_link() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        let id = tunnel_id(initiator, 42);
        pool.handle_inbound_frame_from_link(frame_to(target, id, b"hello"), &arrival)
            .await
            .unwrap();
        pool.handle_inbound_frame_from_link(frame_to(target, id, b"!"), &arrival)
            .await
            .unwrap();

        let mut transport = incoming_rx.recv().await.unwrap();
        assert_eq!(transport.peer(), initiator);
        assert!(incoming_rx.try_recv().is_err());

        let mut buf = [0_u8; 6];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello!");
        assert_eq!(pool.counts().await, (1, 0));
        let active = pool_active(&pool).await;
        assert_eq!(active[0].1, initiator, "replies address the initiator");
        assert_eq!(active[0].2, arrival, "pinned to the arrival link");
    }

    /// D10: a host teardown retires *every* tunnel for that host — there is
    /// no preserved-tunnel exception any more — and a late frame for the
    /// retired id is dropped without resurrecting the tunnel. The transport's
    /// drop hook must not double-count the already-retired tunnel.
    #[tokio::test]
    async fn dropping_inbound_endpoint_transport_removes_target_tunnel() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        let id = tunnel_id(initiator, 42);
        pool.handle_inbound_frame_from_link(frame_to(target, id, b"hello"), &arrival)
            .await
            .unwrap();
        let transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_host(initiator).await;
        assert_eq!(pool.counts().await, (0, 1));

        drop(transport);
        tokio::task::yield_now().await;
        assert_eq!(pool.counts().await, (0, 1));

        pool.handle_inbound_frame_from_link(frame_to(target, id, b"late"), &arrival)
            .await
            .unwrap();

        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.counts().await, (0, 1));
    }

    #[tokio::test]
    async fn inbound_endpoint_transport_marks_cloud_pairing_reachability_from_origin_link() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let cloud = register_test_link_with_role(&pool, 3, link_tx, LinkRole::CloudRelay).await;

        pool.handle_inbound_frame_from_link(
            frame_to(target, tunnel_id(initiator, 42), b"hello"),
            &cloud,
        )
        .await
        .unwrap();

        let transport = incoming_rx.recv().await.unwrap();
        assert!(transport.has_cloud_pairing_reachability());
    }

    #[tokio::test]
    async fn cloud_origin_new_inbound_tunnels_are_rate_limited() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, mut incoming_rx) = mpsc::channel(CLOUD_INBOUND_TUNNEL_RATE_LIMIT + 1);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(64);
        let cloud = register_test_link_with_role(&pool, 3, link_tx, LinkRole::CloudRelay).await;

        for nonce in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_frame_from_link(
                frame_to(target, tunnel_id(initiator, nonce + 1), b"hello"),
                &cloud,
            )
            .await
            .unwrap();
        }
        pool.handle_inbound_frame_from_link(
            frame_to(target, tunnel_id(initiator, 10_000), b"excess"),
            &cloud,
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
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(9);
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(2));
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let cloud = register_test_link_with_role(&pool, 3, cloud_tx, LinkRole::CloudRelay).await;
        let (link_tx, mut link_rx) = mpsc::channel(CLOUD_INBOUND_TUNNEL_RATE_LIMIT + 1);
        register_test_link(&pool, 9, link_tx).await;

        for nonce in 0..=CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_frame_from_link(
                frame_to(target, tunnel_id(initiator, nonce + 1), b"hello"),
                &cloud,
            )
            .await
            .unwrap();
        }

        // The link also carries adjacency events (registration reconciles
        // neighbor views); only tunnel frames count as forwarded traffic.
        let mut forwarded = 0;
        while let Ok(message) = link_rx.try_recv() {
            if matches!(message.body, Some(pb::message::Body::TunnelFrame(_))) {
                forwarded += 1;
            }
        }
        assert_eq!(forwarded, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
    }

    #[tokio::test]
    async fn repeated_cloud_frames_for_one_tunnel_id_consume_one_rate_slot() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(9);
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(2));
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let cloud = register_test_link_with_role(&pool, 3, cloud_tx, LinkRole::CloudRelay).await;
        let (link_tx, mut link_rx) = mpsc::channel(128);
        register_test_link(&pool, 9, link_tx).await;
        let repeated = tunnel_id(initiator, 7);

        for _ in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT {
            pool.handle_inbound_frame_from_link(frame_to(target, repeated, b"same"), &cloud)
                .await
                .unwrap();
        }
        for nonce in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_frame_from_link(
                frame_to(target, tunnel_id(initiator, 1_000 + nonce), b"unique"),
                &cloud,
            )
            .await
            .unwrap();
        }

        // Count tunnel frames only; the link also carries adjacency events.
        let mut forwarded = 0;
        while let Ok(message) = link_rx.try_recv() {
            if matches!(message.body, Some(pb::message::Body::TunnelFrame(_))) {
                forwarded += 1;
            }
        }
        assert_eq!(forwarded, (CLOUD_INBOUND_TUNNEL_RATE_LIMIT * 2) - 1);
    }

    #[tokio::test]
    async fn inbound_endpoint_transport_marks_non_cloud_origin_without_reusable_reachability() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        pool.handle_inbound_frame_from_link(
            frame_to(target, tunnel_id(initiator, 42), b"hello"),
            &arrival,
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

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = device_tls_pool_with_timeout(
            local_identity,
            incoming_tx,
            &peer_identity,
            Duration::from_millis(10),
        );
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, link_tx).await;

        let result = pool
            .channel_via(peer_identity.host_id, HostId::from_u128(99))
            .await;
        assert!(
            matches!(result, Err(TunnelPoolError::Tls(message)) if message.contains("timed out"))
        );
        wait_for_counts(&pool, (0, 0)).await;
    }

    #[tokio::test]
    async fn pin_pairing_tls_timeout_removes_provisional_tunnel() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let pool = pool.with_pin_pairing_handshake_timeout(Duration::from_millis(10));
        let peer = HostId::from_u128(2);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, link_tx).await;

        let result = pool
            .pin_pairing_channel_via(peer, HostId::from_u128(99))
            .await;
        assert!(
            matches!(result, Err(TunnelPoolError::Tls(message)) if message.contains("timed out"))
        );
        wait_for_counts(&pool, (0, 0)).await;
    }

    #[tokio::test]
    async fn inbound_frame_delivers_to_existing_initiator_tunnel() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, _incoming_rx) = test_pool(initiator);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let link = register_test_link(&pool, 2, link_tx.clone()).await;

        let id = tunnel_id(initiator, 42);
        let (tunnel, mut transport) = create_tunnel(id, target, link_tx);
        pool.state.write().await.tunnels.insert(
            id,
            ActiveTunnel {
                peer: target,
                link,
                tunnel,
            },
        );

        pool.handle_inbound_frame_from_link(frame_to(initiator, id, b"pong"), &link)
            .await
            .unwrap();

        let mut buf = [0_u8; 4];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    /// Rule 2: a frame addressed to an adjacent host is forwarded out a
    /// direct link to it, unchanged.
    #[tokio::test]
    async fn inbound_frame_for_an_adjacent_destination_is_forwarded() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let (target_tx, mut target_rx) = mpsc::channel(8);
        register_test_link(&pool, 9, target_tx).await;

        let id = tunnel_id(HostId::from_u128(10), 20);
        pool.handle_inbound_frame_from_link(
            frame_to(HostId::from_u128(9), id, b"payload"),
            &origin,
        )
        .await
        .unwrap();

        // The link also carries adjacency events; the first tunnel frame is
        // the forwarded one.
        let frame = loop {
            match target_rx.recv().await.unwrap().body {
                Some(pb::message::Body::TunnelFrame(frame)) => break frame,
                _ => continue,
            }
        };
        assert_eq!(frame.payload, b"payload");
        assert_eq!(frame.dst, HostId::from_u128(9).as_bytes().to_vec());
        assert_eq!(TunnelId::try_from(frame.tunnel_id.unwrap()).unwrap(), id);
        assert_eq!(pool.counts().await, (0, 0), "relays keep no tunnel state");
    }

    /// Rule 2's other half: no direct link to dst → the frame is dropped.
    /// Forwarding is non-recursive by construction; nothing is looked up
    /// beyond the link registry.
    #[tokio::test]
    async fn inbound_frame_for_a_non_adjacent_destination_is_dropped() {
        let (pool, mut incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;

        let id = tunnel_id(HostId::from_u128(10), 20);
        pool.handle_inbound_frame_from_link(
            frame_to(HostId::from_u128(77), id, b"payload"),
            &origin,
        )
        .await
        .unwrap();

        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn inbound_frame_rejects_oversized_payload() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let id = tunnel_id(HostId::from_u128(10), 20);
        let payload = vec![0_u8; TUNNEL_FRAME_PAYLOAD_MAX + 1];

        let error = pool
            .handle_inbound_frame_from_link(frame_to(HostId::from_u128(9), id, &payload), &origin)
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
    async fn inbound_frame_requires_well_formed_destination_and_tunnel_id() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let id = tunnel_id(HostId::from_u128(10), 20);

        let bad_dst = pb::TunnelFrame {
            dst: vec![1, 2, 3],
            tunnel_id: Some(id.into()),
            payload: Vec::new(),
        };
        assert!(matches!(
            pool.handle_inbound_frame_from_link(bad_dst, &origin).await,
            Err(TunnelPoolError::InvalidDestination { actual: 3 })
        ));

        let missing_tunnel_id = pb::TunnelFrame {
            dst: HostId::from_u128(9).as_bytes().to_vec(),
            tunnel_id: None,
            payload: Vec::new(),
        };
        assert!(matches!(
            pool.handle_inbound_frame_from_link(missing_tunnel_id, &origin)
                .await,
            Err(TunnelPoolError::MissingTunnelId)
        ));
    }

    #[tokio::test]
    async fn remove_host_drops_related_tunnels() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, link_tx).await;
        let _channel = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_host(peer).await;
        assert_eq!(pool.counts().await, (0, 1));
    }

    /// D2: a tunnel is pinned to the link its first frame used and dies
    /// with that link — nothing cleverer.
    #[tokio::test]
    async fn remove_link_drops_tunnels_pinned_to_it() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (first_tx, _first_rx) = mpsc::channel(8);
        let first_link = register_test_link(&pool, 99, first_tx).await;
        let (other_tx, _other_rx) = mpsc::channel(8);
        let other_link = register_test_link(&pool, 98, other_tx).await;

        let _via_first = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();
        let _via_other = pool.channel_via(peer, HostId::from_u128(98)).await.unwrap();
        assert_eq!(pool.counts().await, (2, 0));

        pool.remove_link(&first_link).await;
        assert_eq!(pool.counts().await, (1, 1));
        let remaining = pool_active(&pool).await;
        assert_eq!(remaining[0].2, other_link);
    }

    #[tokio::test]
    async fn remove_initiated_via_retires_only_matching_initiator_tunnels() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (relay_tx, _relay_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, relay_tx).await;
        let _channel = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_initiated_via(peer, HostId::from_u128(98)).await;
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_initiated_via(peer, HostId::from_u128(99)).await;
        assert_eq!(pool.counts().await, (0, 1));
    }

    /// A local route change must not retire tunnels a *remote* initiator is
    /// hosting here — sweeping the tunnel would silently brick the
    /// initiator's cached channel (NETWORKING_REVIEW.md §6.9). Hosted
    /// inbound tunnels die with their peer, their transport, or their link.
    #[tokio::test]
    async fn removed_claim_keeps_hosted_inbound_tunnels_alive() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = tunnel_id(initiator, 42);

        pool.handle_inbound_frame_from_link(frame_to(target, id, b"first"), &arrival)
            .await
            .unwrap();
        let _transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.counts().await, (1, 0));

        pool.remove_initiated_via(initiator, HostId::from_u128(1))
            .await;
        assert_eq!(
            pool.counts().await,
            (1, 0),
            "the initiator's tunnel outlives this side's route bookkeeping"
        );
        pool.handle_inbound_frame_from_link(frame_to(target, id, b"late"), &arrival)
            .await
            .unwrap();

        // The peer-scoped teardown (revocation, key replacement, peer gone)
        // is what retires hosted inbound tunnels.
        pool.remove_host(initiator).await;
        assert_eq!(pool.counts().await, (0, 1));

        pool.handle_inbound_frame_from_link(frame_to(target, id, b"dead"), &arrival)
            .await
            .unwrap();
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
