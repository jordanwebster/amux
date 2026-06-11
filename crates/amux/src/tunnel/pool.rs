//! The tunnel pool: endpoint state for tunnels this daemon initiates or
//! hosts, plus the relay forwarding rule.
//!
//! The lifecycle grammar is the link's, one layer up: `TunnelOpen` is the
//! only frame that allocates endpoint state (rate limiters key on Opens),
//! `TunnelData` for an unknown id is a confused or stale peer's violation —
//! a principled drop, zero allocation, link stays up — and `TunnelClose`
//! (or link death) ends a tunnel, sent proactively on normal teardown.
//!
//! Forwarding is rule 2 of the routing model: a frame addressed to `dst` is
//! forwarded iff this daemon holds a direct link to `dst`; otherwise it is
//! dropped. Relays keep no per-tunnel state and forward all three frame
//! types identically — forwarding consults only the link registry. Replies
//! travel back out the link the tunnel's frames arrive on, addressed to the
//! `src` carried once in the Open; no reverse-route lookup exists.

use std::collections::HashMap;
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
    CLOUD_INBOUND_TUNNEL_RATE_LIMIT, CLOUD_INBOUND_TUNNEL_RATE_WINDOW, SlidingWindowRateLimiter,
};
use crate::routing::{LinkId, LinkRegistry, LinkUnavailable, RoutingCore};
use crate::transport::{
    channel_from_single_io, configure_tonic_endpoint_keepalive, pairing_channel_from_io,
};
use crate::trust::SharedTrustStore;
use crate::tunnel::transport::TunnelTransport;
use crate::tunnel::types::{TunnelId, TunnelTypeError};
use crate::tunnel::{TUNNEL_DATA_PAYLOAD_MAX, Tunnel, create_tunnel, tunnel_close_message};

const TUNNEL_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PAIRING_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum TunnelPoolError {
    #[error("host {host_id} is not reachable")]
    NotFound { host_id: HostId },
    #[error("no live link to host {host_id}")]
    LinkUnavailable { host_id: HostId },
    #[error("tunnel frame dst must be a 16-byte host_id, got {actual} bytes")]
    InvalidDestination { actual: usize },
    #[error("tunnel frame src must be a 16-byte host_id, got {actual} bytes")]
    InvalidSource { actual: usize },
    #[error("TunnelData payload exceeds {max} bytes: {actual} bytes")]
    PayloadTooLarge { actual: usize, max: usize },
    #[error(transparent)]
    InvalidTunnelId(#[from] TunnelTypeError),
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
    /// initiated ones) — where this tunnel's outbound frames are addressed.
    peer: HostId,
    /// The link the tunnel is pinned to; the tunnel dies with it.
    link: LinkId,
    /// Whether this daemon opened the tunnel (vs hosting a peer's Open).
    initiated: bool,
    tunnel: Tunnel,
}

struct PoolState {
    tunnels: HashMap<TunnelId, ActiveTunnel>,
    cloud_inbound_open_limiter: SlidingWindowRateLimiter<()>,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            tunnels: HashMap::new(),
            cloud_inbound_open_limiter: SlidingWindowRateLimiter::new(
                CLOUD_INBOUND_TUNNEL_RATE_LIMIT,
                CLOUD_INBOUND_TUNNEL_RATE_WINDOW,
            ),
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
    pairing_handshake_timeout: Duration,
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
            pairing_handshake_timeout: PAIRING_TLS_HANDSHAKE_TIMEOUT,
            state: Arc::new(RwLock::new(PoolState::default())),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_pairing_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.pairing_handshake_timeout = timeout;
        self
    }

    pub(crate) fn link_registry(&self) -> Arc<LinkRegistry> {
        self.links.clone()
    }

    /// Opens a tunnel-backed channel to `peer` over the specific `link` —
    /// the materialization path for `Route::Direct`. `dst == peer`, zero
    /// relays; the tunnel is pinned to `link`.
    pub(crate) async fn channel_on_link(
        &self,
        peer: HostId,
        link: LinkId,
    ) -> Result<Channel, TunnelPoolError> {
        let outgoing_tx = self.links.outgoing_tx(&link).await?;
        let (id, transport) = self.open_tunnel(peer, link, outgoing_tx).await;
        self.secured_channel(id, peer, transport).await
    }

    /// Opens a tunnel-backed channel to `peer` through the adjacent
    /// `relay` — the materialization path for `Route::Via`. The tunnel is
    /// pinned to the relay link chosen here.
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
        let (id, transport) = self.open_tunnel(peer, link, outgoing_tx).await;
        self.secured_channel(id, peer, transport).await
    }

    pub(crate) async fn pairing_channel_via(
        &self,
        peer: HostId,
        relay: HostId,
    ) -> Result<Channel, TunnelPoolError> {
        let (id, transport) = self.pairing_transport_via(peer, relay).await?;
        match tokio::time::timeout(
            self.pairing_handshake_timeout,
            pairing_channel_from_io(transport),
        )
        .await
        {
            Err(_) => {
                self.retire_and_notify(id).await;
                Err(TunnelPoolError::Tls(
                    "pairing TLS handshake timed out".to_string(),
                ))
            }
            Ok(Ok(channel)) => Ok(channel),
            Ok(Err(error)) => {
                self.retire_and_notify(id).await;
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
        Ok(self.open_tunnel(peer, link, outgoing_tx).await)
    }

    /// Creates an initiator endpoint to `peer` pinned to `link`. The wire
    /// `TunnelOpen` leaves lazily, just ahead of the first data frame.
    async fn open_tunnel(
        &self,
        peer: HostId,
        link: LinkId,
        outgoing_tx: crate::routing::LinkOutputTx,
    ) -> (TunnelId, TunnelTransport) {
        let id = TunnelId::new();
        let (tunnel, transport) = create_tunnel(id, peer, Some(self.my_host_id), outgoing_tx);
        let transport = self.transport_with_cleanup(id, transport);
        self.state.write().await.tunnels.insert(
            id,
            ActiveTunnel {
                peer,
                link,
                initiated: true,
                tunnel,
            },
        );
        (id, transport)
    }

    async fn secured_channel(
        &self,
        id: TunnelId,
        peer: HostId,
        transport: TunnelTransport,
    ) -> Result<Channel, TunnelPoolError> {
        match self.channel_from_transport(peer, transport).await {
            Ok(channel) => Ok(channel),
            Err(error) => {
                self.retire_and_notify(id).await;
                Err(error)
            }
        }
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

    /// Handles an inbound `TunnelOpen` from `origin_link`: the only frame
    /// that allocates endpoint state. `dst != self` forwards by rule 2;
    /// `dst == self` creates a hosted endpoint pinned to the arrival link,
    /// replying to the Open's `src`. A duplicate Open for a live id is
    /// dropped; rejection (no tunnel consumer) is answered with
    /// `TunnelClose` — there is no open-ack.
    pub(crate) async fn handle_inbound_open(
        &self,
        open: pb::TunnelOpen,
        origin_link: &LinkId,
    ) -> Result<(), TunnelPoolError> {
        let id = TunnelId::from_wire(&open.tunnel_id)?;
        let dst = host_id_from_wire(&open.dst)?;
        if self.links.is_cloud_relay(origin_link).await
            && !self
                .state
                .write()
                .await
                .cloud_inbound_open_limiter
                .allow(())
        {
            tracing::warn!(tunnel_id = %id, "cloud inbound TunnelOpen rate limit exceeded");
            return Ok(());
        }

        if dst != self.my_host_id {
            self.forward(dst, message_from_open(open), &id).await;
            return Ok(());
        }

        let src = host_id_from_wire(&open.src).map_err(|_| TunnelPoolError::InvalidSource {
            actual: open.src.len(),
        })?;
        let Ok(outgoing_tx) = self.links.outgoing_tx(origin_link).await else {
            return Ok(());
        };
        let cloud_origin = self.links.is_cloud_relay(origin_link).await;

        // Allocate only after winning the insert: a speculatively-created
        // endpoint's drop hook would tear down the live tunnel under a
        // duplicate Open.
        let transport = {
            let mut state = self.state.write().await;
            if state.tunnels.contains_key(&id) {
                tracing::debug!(tunnel_id = %id, "dropping duplicate TunnelOpen for a live tunnel");
                return Ok(());
            }
            // Hosted endpoints never send an Open of their own; their frames
            // reply to the initiator out the arrival link.
            let (tunnel, transport) = create_tunnel(id, src, None, outgoing_tx);
            state.tunnels.insert(
                id,
                ActiveTunnel {
                    peer: src,
                    link: *origin_link,
                    initiated: false,
                    tunnel,
                },
            );
            self.transport_with_cleanup(id, transport)
                .with_cloud_pairing_reachability(cloud_origin)
        };

        // No consumer for inbound tunnels (a pure relay, or shutdown):
        // dropping the rejected endpoint retires it and sends the rejection
        // TunnelClose through its drop hook — there is no open-ack.
        let _ = self.incoming_tunnels_tx.send(transport).await;
        Ok(())
    }

    /// Handles an inbound `TunnelData` from `origin_link`. `dst != self`
    /// forwards by rule 2. `dst == self` delivers to the addressed tunnel;
    /// data for an unknown id — never opened, just closed, or retired — is
    /// dropped without allocating anything, and the link stays up.
    pub(crate) async fn handle_inbound_data(
        &self,
        data: pb::TunnelData,
        _origin_link: &LinkId,
    ) -> Result<(), TunnelPoolError> {
        if data.payload.len() > TUNNEL_DATA_PAYLOAD_MAX {
            return Err(TunnelPoolError::PayloadTooLarge {
                actual: data.payload.len(),
                max: TUNNEL_DATA_PAYLOAD_MAX,
            });
        }
        let id = TunnelId::from_wire(&data.tunnel_id)?;
        let dst = host_id_from_wire(&data.dst)?;
        if dst != self.my_host_id {
            self.forward(dst, message_from_data(data), &id).await;
            return Ok(());
        }

        let Some(inbound_tx) = self
            .state
            .read()
            .await
            .tunnels
            .get(&id)
            .map(|active| active.tunnel.inbound_sender())
        else {
            tracing::debug!(tunnel_id = %id, "dropping TunnelData for an unknown tunnel");
            return Ok(());
        };
        if inbound_tx.send(Bytes::from(data.payload)).await.is_err() {
            // The endpoint died under the frame (the just-closed window):
            // retire it and tell the peer, instead of escalating to the link.
            self.retire_and_notify(id).await;
        }
        Ok(())
    }

    /// Handles an inbound `TunnelClose` from `origin_link`. `dst != self`
    /// forwards by rule 2; `dst == self` ends the addressed tunnel. A Close
    /// for an unknown id is dropped.
    pub(crate) async fn handle_inbound_close(
        &self,
        close: pb::TunnelClose,
        _origin_link: &LinkId,
    ) -> Result<(), TunnelPoolError> {
        let id = TunnelId::from_wire(&close.tunnel_id)?;
        let dst = host_id_from_wire(&close.dst)?;
        if dst != self.my_host_id {
            self.forward(dst, message_from_close(close), &id).await;
            return Ok(());
        }
        // Dropping the endpoint closes its byte stream; the peer asked for
        // the close, so no TunnelClose goes back.
        self.state.write().await.tunnels.remove(&id);
        Ok(())
    }

    /// Rule 2: forward iff a direct link to `dst` exists, else drop.
    async fn forward(&self, dst: HostId, message: pb::Message, id: &TunnelId) {
        if !self.links.forward_to_peer(dst, message).await {
            tracing::debug!(
                dst = %dst,
                tunnel_id = %id,
                "dropping tunnel frame for a host with no direct link"
            );
        }
    }

    /// Removes `id` and, when it was still live, sends a proactive
    /// `TunnelClose` to its peer on its pinned link (best effort — the peer
    /// also learns through link death or the inner stream stalling).
    async fn retire_and_notify(&self, id: TunnelId) {
        let removed = self.state.write().await.tunnels.remove(&id);
        if let Some(active) = removed {
            self.links
                .send_best_effort(&active.link, tunnel_close_message(id, active.peer))
                .await;
        }
    }

    fn transport_with_cleanup(&self, id: TunnelId, transport: TunnelTransport) -> TunnelTransport {
        let state = self.state.clone();
        let links = self.links.clone();
        transport.with_drop_hook(move || {
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return;
            };
            handle.spawn(async move {
                let removed = state.write().await.tunnels.remove(&id);
                if let Some(active) = removed {
                    // Normal endpoint teardown: tell the peer proactively.
                    links
                        .send_best_effort(&active.link, tunnel_close_message(id, active.peer))
                        .await;
                }
            });
        })
    }

    /// Ends every tunnel whose remote endpoint is `host_id` (revocation,
    /// trust replacement, peer teardown), notifying over still-live links.
    pub(crate) async fn remove_host(&self, host_id: HostId) {
        let retired = {
            let state = self.state.read().await;
            state
                .tunnels
                .iter()
                .filter_map(|(id, active)| (active.peer == host_id).then_some(*id))
                .collect::<Vec<_>>()
        };
        for id in retired {
            self.retire_and_notify(id).await;
        }
    }

    /// A tunnel is pinned to the link its first frame used and dies with
    /// that link — initiated and hosted alike. The link is gone, so there
    /// is nowhere to send a TunnelClose; the peer's half dies the same way.
    pub(crate) async fn remove_link(&self, link: &LinkId) {
        let mut state = self.state.write().await;
        state.tunnels.retain(|_, active| active.link != *link);
    }

    /// Retires the tunnels *this daemon initiated* to `peer` pinned to a
    /// link whose neighbor is `link_peer` (a make-then-break swap or a
    /// withdrawn claim), closing them toward the peer. Hosted inbound
    /// tunnels are deliberately left alone: a local route change says
    /// nothing about the remote initiator's tunnel, and sweeping it would
    /// silently brick the initiator's cached channel.
    pub(crate) async fn remove_initiated_over(&self, peer: HostId, link_peer: HostId) {
        let retired = {
            let state = self.state.read().await;
            state
                .tunnels
                .iter()
                .filter_map(|(id, active)| {
                    (active.initiated && active.peer == peer && active.link.peer() == link_peer)
                        .then_some(*id)
                })
                .collect::<Vec<_>>()
        };
        for id in retired {
            self.retire_and_notify(id).await;
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
    pub(crate) async fn active_count(&self) -> usize {
        self.state.read().await.tunnels.len()
    }

    /// Test observation seam: each active tunnel as `(remote peer, pinned
    /// link)`.
    #[cfg(test)]
    pub(crate) async fn active_tunnels_for_test(&self) -> Vec<(HostId, LinkId)> {
        self.state
            .read()
            .await
            .tunnels
            .values()
            .map(|active| (active.peer, active.link))
            .collect()
    }
}

fn message_from_open(open: pb::TunnelOpen) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelOpen(open)),
    }
}

fn message_from_data(data: pb::TunnelData) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelData(data)),
    }
}

fn message_from_close(close: pb::TunnelClose) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::TunnelClose(close)),
    }
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

    async fn wait_for_active_count(pool: &TunnelPool, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if pool.active_count().await == expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for tunnel pool count");
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

    fn open_to(dst: HostId, src: HostId, id: TunnelId) -> pb::TunnelOpen {
        pb::TunnelOpen {
            tunnel_id: id.to_wire(),
            src: src.as_bytes().to_vec(),
            dst: dst.as_bytes().to_vec(),
        }
    }

    fn data_to(dst: HostId, id: TunnelId, payload: &[u8]) -> pb::TunnelData {
        pb::TunnelData {
            tunnel_id: id.to_wire(),
            dst: dst.as_bytes().to_vec(),
            payload: payload.to_vec(),
        }
    }

    fn close_to(dst: HostId, id: TunnelId) -> pb::TunnelClose {
        pb::TunnelClose {
            tunnel_id: id.to_wire(),
            dst: dst.as_bytes().to_vec(),
        }
    }

    async fn recv_tunnel_close(rx: &mut mpsc::Receiver<pb::Message>) -> pb::TunnelClose {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            let message = tokio::time::timeout_at(deadline, rx.recv())
                .await
                .expect("timed out waiting for TunnelClose")
                .expect("link writer closed");
            if let Some(pb::message::Body::TunnelClose(close)) = message.body {
                return close;
            }
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

        assert_eq!(pool.active_count().await, 2);
        for (_, tunnel_peer, link) in pool_active(&pool).await {
            assert_eq!(tunnel_peer, peer);
            assert_eq!(link, relay_link);
        }
    }

    #[tokio::test]
    async fn channel_on_link_creates_an_initiator_tunnel_pinned_to_that_link() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let link = register_test_link(&pool, 2, link_tx).await;

        let _channel = pool.channel_on_link(peer, link).await.unwrap();

        let active = pool_active(&pool).await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].1, peer);
        assert_eq!(active[0].2, link);
    }

    #[tokio::test]
    async fn channel_on_link_reports_a_dead_link() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let dead = LinkId::new(HostId::from_u128(2));

        assert!(matches!(
            pool.channel_on_link(HostId::from_u128(2), dead).await,
            Err(TunnelPoolError::LinkUnavailable { host_id }) if host_id == HostId::from_u128(2)
        ));
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

    /// The reply rule: a TunnelOpen addressed to us creates a hosted
    /// endpoint that replies out the arrival link to the Open's `src`. No
    /// reverse-route lookup exists to fail.
    #[tokio::test]
    async fn inbound_open_creates_a_hosted_endpoint_replying_out_the_arrival_link() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        let id = TunnelId::from_u128(42);
        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();
        pool.handle_inbound_data(data_to(target, id, b"hello"), &arrival)
            .await
            .unwrap();
        pool.handle_inbound_data(data_to(target, id, b"!"), &arrival)
            .await
            .unwrap();

        let mut transport = incoming_rx.recv().await.unwrap();
        assert_eq!(transport.peer(), initiator);
        assert!(incoming_rx.try_recv().is_err());

        let mut buf = [0_u8; 6];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello!");
        assert_eq!(pool.active_count().await, 1);
        let active = pool_active(&pool).await;
        assert_eq!(active[0].1, initiator, "replies address the Open's src");
        assert_eq!(active[0].2, arrival, "pinned to the arrival link");
    }

    /// Only an Open allocates: TunnelData for an id that was never opened
    /// is a violation by a confused peer — dropped without allocating
    /// anything, and without disturbing the link.
    #[tokio::test]
    async fn inbound_data_for_an_unknown_tunnel_is_dropped_without_allocation() {
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        pool.handle_inbound_data(data_to(target, TunnelId::from_u128(42), b"ghost"), &arrival)
            .await
            .unwrap();

        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.active_count().await, 0);
    }

    /// A duplicate Open for a live tunnel allocates nothing and does not
    /// disturb the existing endpoint.
    #[tokio::test]
    async fn a_duplicate_open_for_a_live_tunnel_is_dropped() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = TunnelId::from_u128(42);

        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();
        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();

        let _transport = incoming_rx.recv().await.unwrap();
        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.active_count().await, 1);
    }

    /// An inbound TunnelClose ends the addressed tunnel; data after it is
    /// the just-closed window and is dropped without resurrecting anything.
    #[tokio::test]
    async fn inbound_close_ends_the_tunnel_and_later_data_is_dropped() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = TunnelId::from_u128(42);

        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();
        let _transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.active_count().await, 1);

        pool.handle_inbound_close(close_to(target, id), &arrival)
            .await
            .unwrap();
        assert_eq!(pool.active_count().await, 0);

        pool.handle_inbound_data(data_to(target, id, b"late"), &arrival)
            .await
            .unwrap();
        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.active_count().await, 0);
    }

    /// There is no open-ack, and rejection is a TunnelClose: an Open with
    /// nobody to terminate the tunnel (a pure relay, or shutdown) is
    /// answered by closing it back to the initiator.
    #[tokio::test]
    async fn an_open_with_no_tunnel_consumer_is_rejected_with_tunnel_close() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, incoming_rx) = mpsc::channel(1);
        drop(incoming_rx);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = TunnelId::from_u128(7);

        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();

        let close = recv_tunnel_close(&mut link_rx).await;
        assert_eq!(close.tunnel_id, id.to_wire());
        assert_eq!(close.dst, initiator.as_bytes().to_vec());
        wait_for_active_count(&pool, 0).await;
    }

    /// A TunnelClose for an unknown id is dropped — closes are idempotent.
    #[tokio::test]
    async fn inbound_close_for_an_unknown_tunnel_is_dropped() {
        let target = HostId::from_u128(2);
        let (pool, _incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        pool.handle_inbound_close(close_to(target, TunnelId::from_u128(7)), &arrival)
            .await
            .unwrap();

        assert_eq!(pool.active_count().await, 0);
    }

    /// Dropping a hosted endpoint's transport removes the tunnel and sends
    /// a proactive TunnelClose to the initiator on the arrival link.
    #[tokio::test]
    async fn dropping_a_hosted_endpoint_sends_tunnel_close_to_the_initiator() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, mut link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = TunnelId::from_u128(42);

        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();
        let transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.active_count().await, 1);

        drop(transport);
        wait_for_active_count(&pool, 0).await;

        let close = recv_tunnel_close(&mut link_rx).await;
        assert_eq!(close.tunnel_id, id.to_wire());
        assert_eq!(close.dst, initiator.as_bytes().to_vec());
    }

    /// D10: a host teardown retires *every* tunnel for that host — there is
    /// no preserved-tunnel exception — closing toward the peer, and a late
    /// frame for the retired id is dropped without resurrecting the tunnel.
    #[tokio::test]
    async fn remove_host_retires_hosted_tunnels_and_late_data_is_dropped() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, mut link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = TunnelId::from_u128(42);

        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();
        let transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.active_count().await, 1);

        pool.remove_host(initiator).await;
        assert_eq!(pool.active_count().await, 0);
        let close = recv_tunnel_close(&mut link_rx).await;
        assert_eq!(close.tunnel_id, id.to_wire());

        // The transport's later drop must not double-close.
        drop(transport);
        tokio::task::yield_now().await;
        assert!(link_rx.try_recv().is_err());

        pool.handle_inbound_data(data_to(target, id, b"late"), &arrival)
            .await
            .unwrap();
        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.active_count().await, 0);
    }

    #[tokio::test]
    async fn inbound_endpoint_transport_marks_cloud_pairing_reachability_from_origin_link() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let cloud = register_test_link_with_role(&pool, 3, link_tx, LinkRole::CloudRelay).await;

        pool.handle_inbound_open(open_to(target, initiator, TunnelId::from_u128(42)), &cloud)
            .await
            .unwrap();

        let transport = incoming_rx.recv().await.unwrap();
        assert!(transport.has_cloud_pairing_reachability());
    }

    /// Rate limiters key on Opens: cloud-origin TunnelOpens beyond the
    /// budget are dropped before any allocation.
    #[tokio::test]
    async fn cloud_origin_opens_are_rate_limited() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, mut incoming_rx) = mpsc::channel(CLOUD_INBOUND_TUNNEL_RATE_LIMIT + 1);
        let pool = TunnelPool::new(target, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(64);
        let cloud = register_test_link_with_role(&pool, 3, link_tx, LinkRole::CloudRelay).await;

        for nonce in 0..=CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_open(
                open_to(target, initiator, TunnelId::from_u128(nonce + 1)),
                &cloud,
            )
            .await
            .unwrap();
        }

        assert_eq!(pool.active_count().await, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
        let mut received = 0;
        while incoming_rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
    }

    #[tokio::test]
    async fn cloud_origin_forwarded_opens_are_rate_limited() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(9);
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(2));
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let cloud = register_test_link_with_role(&pool, 3, cloud_tx, LinkRole::CloudRelay).await;
        let (link_tx, mut link_rx) = mpsc::channel(CLOUD_INBOUND_TUNNEL_RATE_LIMIT + 1);
        register_test_link(&pool, 9, link_tx).await;

        for nonce in 0..=CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_open(
                open_to(target, initiator, TunnelId::from_u128(nonce + 1)),
                &cloud,
            )
            .await
            .unwrap();
        }

        // The link also carries adjacency events (registration reconciles
        // neighbor views); only tunnel frames count as forwarded traffic.
        let mut forwarded = 0;
        while let Ok(message) = link_rx.try_recv() {
            if matches!(message.body, Some(pb::message::Body::TunnelOpen(_))) {
                forwarded += 1;
            }
        }
        assert_eq!(forwarded, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
    }

    /// Data consumes no rate slots: a cloud-origin data flood neither
    /// allocates nor starves later Opens.
    #[tokio::test]
    async fn cloud_origin_data_consumes_no_rate_slots() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(9);
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(2));
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let cloud = register_test_link_with_role(&pool, 3, cloud_tx, LinkRole::CloudRelay).await;
        let (link_tx, mut link_rx) = mpsc::channel(256);
        register_test_link(&pool, 9, link_tx).await;

        for nonce in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_data(
                data_to(target, TunnelId::from_u128(1_000 + nonce), b"flood"),
                &cloud,
            )
            .await
            .unwrap();
        }
        for nonce in 0..CLOUD_INBOUND_TUNNEL_RATE_LIMIT as u128 {
            pool.handle_inbound_open(
                open_to(target, initiator, TunnelId::from_u128(nonce + 1)),
                &cloud,
            )
            .await
            .unwrap();
        }

        let mut forwarded_opens = 0;
        while let Ok(message) = link_rx.try_recv() {
            if matches!(message.body, Some(pb::message::Body::TunnelOpen(_))) {
                forwarded_opens += 1;
            }
        }
        assert_eq!(forwarded_opens, CLOUD_INBOUND_TUNNEL_RATE_LIMIT);
    }

    #[tokio::test]
    async fn inbound_endpoint_transport_marks_non_cloud_origin_without_reusable_reachability() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;

        pool.handle_inbound_open(
            open_to(target, initiator, TunnelId::from_u128(42)),
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
        wait_for_active_count(&pool, 0).await;
    }

    #[tokio::test]
    async fn pairing_tls_timeout_removes_provisional_tunnel() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let pool = pool.with_pairing_handshake_timeout(Duration::from_millis(10));
        let peer = HostId::from_u128(2);
        let (link_tx, _link_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, link_tx).await;

        let result = pool.pairing_channel_via(peer, HostId::from_u128(99)).await;
        assert!(
            matches!(result, Err(TunnelPoolError::Tls(message)) if message.contains("timed out"))
        );
        wait_for_active_count(&pool, 0).await;
    }

    #[tokio::test]
    async fn inbound_data_delivers_to_existing_initiator_tunnel() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, _incoming_rx) = test_pool(initiator);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let link = register_test_link(&pool, 2, link_tx.clone()).await;

        let id = TunnelId::from_u128(42);
        let (tunnel, mut transport) = create_tunnel(id, target, Some(initiator), link_tx);
        pool.state.write().await.tunnels.insert(
            id,
            ActiveTunnel {
                peer: target,
                link,
                initiated: true,
                tunnel,
            },
        );

        pool.handle_inbound_data(data_to(initiator, id, b"pong"), &link)
            .await
            .unwrap();

        let mut buf = [0_u8; 4];
        transport.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
    }

    /// Rule 2: all three frame types addressed to an adjacent host are
    /// forwarded out a direct link to it, unchanged. Relays keep no
    /// per-tunnel state.
    #[tokio::test]
    async fn frames_for_an_adjacent_destination_are_forwarded_statelessly() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let (target_tx, mut target_rx) = mpsc::channel(8);
        register_test_link(&pool, 9, target_tx).await;
        let initiator = HostId::from_u128(10);
        let dst = HostId::from_u128(9);
        let id = TunnelId::from_u128(20);

        pool.handle_inbound_open(open_to(dst, initiator, id), &origin)
            .await
            .unwrap();
        pool.handle_inbound_data(data_to(dst, id, b"payload"), &origin)
            .await
            .unwrap();
        pool.handle_inbound_close(close_to(dst, id), &origin)
            .await
            .unwrap();

        // The link also carries adjacency events; collect the tunnel frames.
        let mut bodies = Vec::new();
        while let Ok(message) = target_rx.try_recv() {
            match message.body {
                Some(pb::message::Body::TunnelOpen(open)) => {
                    assert_eq!(open.dst, dst.as_bytes().to_vec());
                    assert_eq!(open.src, initiator.as_bytes().to_vec());
                    assert_eq!(open.tunnel_id, id.to_wire());
                    bodies.push("open");
                }
                Some(pb::message::Body::TunnelData(data)) => {
                    assert_eq!(data.payload, b"payload");
                    assert_eq!(data.dst, dst.as_bytes().to_vec());
                    assert_eq!(data.tunnel_id, id.to_wire());
                    bodies.push("data");
                }
                Some(pb::message::Body::TunnelClose(close)) => {
                    assert_eq!(close.dst, dst.as_bytes().to_vec());
                    assert_eq!(close.tunnel_id, id.to_wire());
                    bodies.push("close");
                }
                _ => {}
            }
        }
        assert_eq!(bodies, ["open", "data", "close"]);
        assert_eq!(pool.active_count().await, 0, "relays keep no tunnel state");
    }

    /// Rule 2's other half: no direct link to dst → the frame is dropped.
    /// Forwarding is non-recursive by construction; nothing is looked up
    /// beyond the link registry.
    #[tokio::test]
    async fn frames_for_a_non_adjacent_destination_are_dropped() {
        let (pool, mut incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let nowhere = HostId::from_u128(77);
        let id = TunnelId::from_u128(20);

        pool.handle_inbound_open(open_to(nowhere, HostId::from_u128(10), id), &origin)
            .await
            .unwrap();
        pool.handle_inbound_data(data_to(nowhere, id, b"payload"), &origin)
            .await
            .unwrap();

        assert!(incoming_rx.try_recv().is_err());
        assert_eq!(pool.active_count().await, 0);
    }

    #[tokio::test]
    async fn inbound_data_rejects_oversized_payload() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let payload = vec![0_u8; TUNNEL_DATA_PAYLOAD_MAX + 1];

        let error = pool
            .handle_inbound_data(
                data_to(HostId::from_u128(9), TunnelId::from_u128(20), &payload),
                &origin,
            )
            .await
            .expect_err("oversized frame should be rejected");

        assert!(matches!(
            error,
            TunnelPoolError::PayloadTooLarge {
                actual,
                max,
            } if actual == TUNNEL_DATA_PAYLOAD_MAX + 1 && max == TUNNEL_DATA_PAYLOAD_MAX
        ));
    }

    #[tokio::test]
    async fn inbound_frames_require_well_formed_destination_and_tunnel_id() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let (origin_tx, _origin_rx) = mpsc::channel(8);
        let origin = register_test_link(&pool, 10, origin_tx).await;
        let id = TunnelId::from_u128(20);

        let bad_dst = pb::TunnelData {
            tunnel_id: id.to_wire(),
            dst: vec![1, 2, 3],
            payload: Vec::new(),
        };
        assert!(matches!(
            pool.handle_inbound_data(bad_dst, &origin).await,
            Err(TunnelPoolError::InvalidDestination { actual: 3 })
        ));

        let bad_id = pb::TunnelData {
            tunnel_id: vec![1, 2, 3],
            dst: HostId::from_u128(9).as_bytes().to_vec(),
            payload: Vec::new(),
        };
        assert!(matches!(
            pool.handle_inbound_data(bad_id, &origin).await,
            Err(TunnelPoolError::InvalidTunnelId(_))
        ));

        let bad_src = pb::TunnelOpen {
            tunnel_id: id.to_wire(),
            src: vec![1, 2, 3],
            dst: HostId::from_u128(1).as_bytes().to_vec(),
        };
        assert!(matches!(
            pool.handle_inbound_open(bad_src, &origin).await,
            Err(TunnelPoolError::InvalidSource { actual: 3 })
        ));
    }

    #[tokio::test]
    async fn remove_host_drops_initiated_tunnels_and_closes_them() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (link_tx, mut link_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, link_tx).await;
        let _channel = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();
        assert_eq!(pool.active_count().await, 1);

        pool.remove_host(peer).await;
        assert_eq!(pool.active_count().await, 0);
        let close = recv_tunnel_close(&mut link_rx).await;
        assert_eq!(close.dst, peer.as_bytes().to_vec());
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
        assert_eq!(pool.active_count().await, 2);

        pool.remove_link(&first_link).await;
        assert_eq!(pool.active_count().await, 1);
        let remaining = pool_active(&pool).await;
        assert_eq!(remaining[0].2, other_link);
        let _ = first_link;
    }

    #[tokio::test]
    async fn remove_initiated_over_retires_only_matching_tunnels() {
        let (pool, _incoming_rx) = test_pool(HostId::from_u128(1));
        let peer = HostId::from_u128(2);
        let (relay_tx, _relay_rx) = mpsc::channel(8);
        register_test_link(&pool, 99, relay_tx).await;
        let _channel = pool.channel_via(peer, HostId::from_u128(99)).await.unwrap();
        assert_eq!(pool.active_count().await, 1);

        pool.remove_initiated_over(peer, HostId::from_u128(98))
            .await;
        assert_eq!(pool.active_count().await, 1);

        pool.remove_initiated_over(peer, HostId::from_u128(99))
            .await;
        assert_eq!(pool.active_count().await, 0);
    }

    /// A local route change must not retire tunnels a *remote* initiator is
    /// hosting here — sweeping the tunnel would silently brick the
    /// initiator's cached channel. Hosted inbound tunnels die with their
    /// peer, their transport, their link, or the peer's own TunnelClose.
    #[tokio::test]
    async fn removed_claim_keeps_hosted_inbound_tunnels_alive() {
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (pool, mut incoming_rx) = test_pool(target);
        let (link_tx, _link_rx) = mpsc::channel(8);
        let arrival = register_test_link(&pool, 1, link_tx).await;
        let id = TunnelId::from_u128(42);

        pool.handle_inbound_open(open_to(target, initiator, id), &arrival)
            .await
            .unwrap();
        let _transport = incoming_rx.recv().await.unwrap();
        assert_eq!(pool.active_count().await, 1);

        pool.remove_initiated_over(initiator, HostId::from_u128(1))
            .await;
        assert_eq!(
            pool.active_count().await,
            1,
            "the initiator's tunnel outlives this side's route bookkeeping"
        );
        pool.handle_inbound_data(data_to(target, id, b"late"), &arrival)
            .await
            .unwrap();

        // The peer-scoped teardown (revocation, key replacement, peer gone)
        // is what retires hosted inbound tunnels.
        pool.remove_host(initiator).await;
        assert_eq!(pool.active_count().await, 0);
    }
}
