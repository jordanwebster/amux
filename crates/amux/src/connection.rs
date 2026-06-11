//! Outbound channel selection over the two route shapes.
//!
//! A peer is reachable either `Direct` (a link of our own, whose tonic
//! channel was registered at link establishment) or `Via` an adjacent relay
//! (a tunnel materialized on demand). The dual-path discipline — direct
//! channels eager, tunnels lazy — survives until every call is a tunnel.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use crate::HostId;
use crate::routing::{FEATURE_CLOUD_RELAY, Host, LinkId, Route, RoutingCore, RoutingEvent};
use crate::transport::TrustedPeerConnections;
use crate::tunnel::{TunnelPool, TunnelPoolError};

/// Key for one pooled channel: the link itself for direct channels, the
/// `(target, relay)` pair for tunnel-backed ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ChannelKey {
    Direct(LinkId),
    Via { target: HostId, relay: HostId },
}

impl ChannelKey {
    fn for_route(target: HostId, route: Route) -> Self {
        match route {
            Route::Direct(link) => Self::Direct(link),
            Route::Via(relay) => Self::Via { target, relay },
        }
    }
}

#[derive(Default)]
pub(crate) struct ConnectionPool {
    by_key: RwLock<HashMap<ChannelKey, Channel>>,
}

impl ConnectionPool {
    pub(crate) async fn register(&self, key: ChannelKey, channel: Channel) {
        self.by_key.write().await.insert(key, channel);
    }

    pub(crate) async fn get(&self, key: &ChannelKey) -> Option<Channel> {
        self.by_key.read().await.get(key).cloned()
    }

    #[cfg(test)]
    pub(crate) async fn get_direct(&self, link: LinkId) -> Option<Channel> {
        self.get(&ChannelKey::Direct(link)).await
    }

    pub(crate) async fn unregister(&self, key: &ChannelKey) {
        self.by_key.write().await.remove(key);
    }

    async fn unregister_for_host(&self, host_id: HostId) {
        self.by_key.write().await.retain(|key, _| match key {
            ChannelKey::Direct(link) => link.peer() != host_id,
            ChannelKey::Via { target, relay } => *target != host_id && *relay != host_id,
        });
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.by_key.read().await.len()
    }
}

pub(crate) struct ConnectionManager {
    routing: Arc<RoutingCore>,
    runtime: RouteRuntimeState,
    trusted_connections: TrustedPeerConnections,
    state: RwLock<ConnectionState>,
}

#[derive(Clone)]
pub(crate) struct RouteRuntimeState {
    pool: Arc<ConnectionPool>,
    tunnels: Arc<TunnelPool>,
}

impl RouteRuntimeState {
    pub(crate) fn new(tunnels: Arc<TunnelPool>) -> Self {
        Self {
            pool: Arc::new(ConnectionPool::default()),
            tunnels,
        }
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> Arc<ConnectionPool> {
        self.pool.clone()
    }

    /// Registers the link's own tonic channel (connector side, at link
    /// establishment, before the Direct route is recorded).
    pub(crate) async fn register_direct(&self, link: LinkId, channel: Channel) {
        self.pool.register(ChannelKey::Direct(link), channel).await;
    }

    /// Drops the link's pooled channel when the link dies.
    pub(crate) async fn remove_direct(&self, link: LinkId) {
        self.pool.unregister(&ChannelKey::Direct(link)).await;
    }
}

#[derive(Default)]
struct ConnectionState {
    active: HashMap<HostId, Route>,
    reachability_errors: HashMap<HostId, String>,
}

impl ConnectionManager {
    pub(crate) fn new(routing: Arc<RoutingCore>, tunnels: Arc<TunnelPool>) -> Self {
        Self {
            routing,
            runtime: RouteRuntimeState::new(tunnels),
            trusted_connections: TrustedPeerConnections::default(),
            state: RwLock::new(ConnectionState::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> Arc<ConnectionPool> {
        self.runtime.pool()
    }

    pub(crate) fn route_runtime(&self) -> RouteRuntimeState {
        self.runtime.clone()
    }

    pub(crate) fn trusted_connections(&self) -> TrustedPeerConnections {
        self.trusted_connections.clone()
    }

    pub(crate) async fn attach_routing_events(self: Arc<Self>) -> JoinHandle<()> {
        for event in self.routing.routing_events_snapshot().await {
            self.handle_event(event).await;
        }
        let mut rx = self.routing.subscribe_routing_events().await;
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                self.handle_event(event).await;
            }
        })
    }

    pub(crate) async fn channel_to(&self, peer: HostId) -> Result<Channel, TunnelPoolError> {
        let route = self
            .routing
            .route_to(peer)
            .await
            .ok_or(TunnelPoolError::NotFound { host_id: peer })?;
        self.activate_route(peer, route).await
    }

    pub(crate) async fn cloud_pin_pairing_channel_to(
        &self,
        peer: HostId,
    ) -> Result<Channel, TunnelPoolError> {
        let relay = self.cloud_relay_for(peer).await?;
        self.runtime
            .tunnels
            .pin_pairing_channel_via(peer, relay)
            .await
    }

    pub(crate) async fn cloud_qr_pairing_channel_to(
        &self,
        peer: HostId,
        expected_pubkey: Vec<u8>,
    ) -> Result<Channel, TunnelPoolError> {
        let relay = self.cloud_relay_for(peer).await?;
        self.runtime
            .tunnels
            .qr_pairing_channel_via(peer, relay, expected_pubkey)
            .await
    }

    pub(crate) async fn has_cloud_route(&self, peer: HostId) -> bool {
        self.cloud_relay_for(peer).await.is_ok()
    }

    pub(crate) async fn mark_client_visible_hosts(&self, host_ids: &[HostId]) {
        self.routing.mark_client_visible_hosts(host_ids).await;
    }

    pub(crate) async fn stored_reachability_error(&self, peer: HostId) -> Option<String> {
        self.state
            .read()
            .await
            .reachability_errors
            .get(&peer)
            .cloned()
    }

    pub(crate) async fn record_reachability_error(&self, peer: HostId, error: impl Into<String>) {
        {
            self.state
                .write()
                .await
                .reachability_errors
                .insert(peer, error.into());
        }
        self.routing.notify_host_status_changed(peer).await;
    }

    pub(crate) async fn clear_reachability_error(&self, peer: HostId) {
        let removed = self
            .state
            .write()
            .await
            .reachability_errors
            .remove(&peer)
            .is_some();
        if removed {
            self.routing.notify_host_status_changed(peer).await;
        }
    }

    pub(crate) async fn send_link_close_to_host(
        &self,
        peer: HostId,
        reason: crate::protocol::wire::pb::LinkCloseReason,
    ) {
        self.runtime
            .tunnels
            .link_registry()
            .send_link_close_to_host(peer, reason)
            .await;
    }

    pub(crate) async fn teardown_host(&self, peer: HostId) {
        self.routing.begin_replacement(peer).await;
        self.routing.remove_host(peer).await;
        self.remove_host_runtime_state(peer).await;
        self.trusted_connections.close_host(peer).await;
        self.runtime.tunnels.link_registry().close_host(peer).await;
        self.remove_host_runtime_state(peer).await;
    }

    pub(crate) async fn finish_host_replacement(&self, peer: HostId) {
        self.routing.finish_replacement(peer).await;
        self.trusted_connections.finish_host_replacement(peer);
    }

    async fn handle_event(&self, event: RoutingEvent) {
        match event {
            RoutingEvent::NeighborUp { host, link } => {
                self.clear_reachability_error(host.id).await;
                // The link's own channel beats any relay path; activate it
                // unless an equally-direct route is already active.
                let already_direct = matches!(
                    self.state.read().await.active.get(&host.id),
                    Some(Route::Direct(_))
                );
                if !already_direct
                    && let Err(error) = self.activate_route(host.id, Route::Direct(link)).await
                {
                    tracing::warn!(peer = %host.id, error = %error, "failed to activate direct route");
                }
            }
            RoutingEvent::NeighborDown { host_id, link, .. } => {
                self.runtime.remove_direct(link).await;
                self.runtime.tunnels.remove_link(&link).await;
                let mut state = self.state.write().await;
                if state.active.get(&host_id) == Some(&Route::Direct(link)) {
                    state.active.remove(&host_id);
                }
            }
            RoutingEvent::ClaimUp { relay, host } => {
                self.clear_reachability_error(host.id).await;
                // Never eagerly tunnel into a cloud relay: relays discard
                // inbound tunnels by design, so materialization can only
                // stall for the whole handshake timeout
                // (NETWORKING_REVIEW.md §6.7).
                if host_is_cloud_relay(&host) {
                    return;
                }
                let has_active = self.state.read().await.active.contains_key(&host.id);
                if !has_active
                    && let Err(error) = self.activate_route(host.id, Route::Via(relay)).await
                {
                    tracing::warn!(peer = %host.id, relay = %relay, error = %error, "failed to activate relay route");
                }
            }
            RoutingEvent::ClaimDown { relay, host_id } => {
                let key = ChannelKey::Via {
                    target: host_id,
                    relay,
                };
                self.runtime.pool.unregister(&key).await;
                self.runtime
                    .tunnels
                    .remove_initiated_via(host_id, relay)
                    .await;
                let mut state = self.state.write().await;
                if state.active.get(&host_id) == Some(&Route::Via(relay)) {
                    state.active.remove(&host_id);
                }
            }
        }
    }

    async fn activate_route(&self, peer: HostId, route: Route) -> Result<Channel, TunnelPoolError> {
        let channel = self.materialize(peer, route).await?;
        let old = {
            let mut state = self.state.write().await;
            if !self.routing.routes_to(peer).await.contains(&route) {
                // The route raced away while we were materializing.
                drop(state);
                self.remove_route_runtime_state(peer, route).await;
                return Err(TunnelPoolError::NotFound { host_id: peer });
            }
            match state.active.get(&peer) {
                // A stale in-flight activation must not demote an active
                // direct route to a relay path: serve the call on the
                // channel we built; the active route stays as is
                // (NETWORKING_REVIEW.md §6.2).
                Some(active @ Route::Direct(_)) if !route.is_direct() && *active != route => {
                    return Ok(channel);
                }
                _ => state.active.insert(peer, route),
            }
        };
        if let Some(old) = old
            && old != route
        {
            self.remove_route_runtime_state(peer, old).await;
        }
        self.clear_reachability_error(peer).await;
        Ok(channel)
    }

    async fn materialize(&self, peer: HostId, route: Route) -> Result<Channel, TunnelPoolError> {
        let key = ChannelKey::for_route(peer, route);
        let registry = self.runtime.tunnels.link_registry();
        if let Some(channel) = self.runtime.pool.get(&key).await {
            // A cached channel is only as alive as the link under it.
            let link_alive = match route {
                Route::Direct(link) => registry.has_link(&link).await,
                Route::Via(relay) => registry.link_to_peer(relay).await.is_some(),
            };
            if !link_alive {
                return Err(TunnelPoolError::LinkUnavailable {
                    host_id: route_link_peer(route),
                });
            }
            return Ok(channel);
        }
        match route {
            // Direct channels are registered at link establishment and never
            // re-materialized; a missing one means the link cannot carry
            // calls from this side.
            Route::Direct(link) => Err(TunnelPoolError::LinkUnavailable {
                host_id: link.peer(),
            }),
            Route::Via(relay) => {
                let channel = self.runtime.tunnels.channel_via(peer, relay).await?;
                self.runtime.pool.register(key, channel.clone()).await;
                Ok(channel)
            }
        }
    }

    async fn remove_route_runtime_state(&self, peer: HostId, route: Route) {
        let key = ChannelKey::for_route(peer, route);
        self.runtime.pool.unregister(&key).await;
        if let Route::Via(relay) = route {
            self.runtime.tunnels.remove_initiated_via(peer, relay).await;
        }
    }

    async fn remove_host_runtime_state(&self, peer: HostId) {
        self.state.write().await.active.remove(&peer);
        self.runtime.pool.unregister_for_host(peer).await;
        self.runtime.tunnels.remove_host(peer).await;
    }

    /// A relay for `peer` whose link is the authenticated cloud link.
    /// Pairing route selection keys on the link role, never on a peer's
    /// self-asserted relay capability.
    async fn cloud_relay_for(&self, peer: HostId) -> Result<HostId, TunnelPoolError> {
        let registry = self.runtime.tunnels.link_registry();
        for relay in self.routing.relays_to(peer).await {
            if registry.has_cloud_relay_link_to(relay).await {
                return Ok(relay);
            }
        }
        Err(TunnelPoolError::NotFound { host_id: peer })
    }

    #[cfg(test)]
    pub(crate) fn routing(&self) -> &Arc<RoutingCore> {
        &self.routing
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) async fn active_route(&self, peer: HostId) -> Option<Route> {
        self.state.read().await.active.get(&peer).copied()
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) async fn known_routes(&self, peer: HostId) -> Vec<Route> {
        self.routing.routes_to(peer).await
    }
}

fn route_link_peer(route: Route) -> HostId {
    match route {
        Route::Direct(link) => link.peer(),
        Route::Via(relay) => relay,
    }
}

fn host_is_cloud_relay(host: &Host) -> bool {
    host.capabilities
        .features
        .iter()
        .any(|feature| feature == FEATURE_CLOUD_RELAY)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;
    use tonic::transport::Endpoint;

    use super::*;
    use crate::routing::{
        Capabilities, FEATURE_CLOUD_RELAY, Host, HostReachabilityEvent, LinkRole,
        SupportedAgentType,
    };

    fn host(id: u128) -> Host {
        Host {
            id: HostId::from_u128(id),
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

    fn cloud_host(id: u128) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: format!("cloud-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: vec![FEATURE_CLOUD_RELAY.to_string()],
                supported_agent_types: Vec::new(),
            },
        }
    }

    fn test_pool(my_host_id: HostId, routing: &Arc<RoutingCore>) -> Arc<TunnelPool> {
        let (incoming_tx, _incoming_rx) = mpsc::channel(8);
        Arc::new(TunnelPool::new(my_host_id, routing.clone(), incoming_tx))
    }

    async fn register_link(
        tunnels: &TunnelPool,
        peer: &Host,
        role: LinkRole,
    ) -> (LinkId, mpsc::Receiver<crate::protocol::wire::pb::Message>) {
        let (tx, rx) = mpsc::channel(64);
        let link = LinkId::new(peer.id);
        tunnels
            .link_registry()
            .register(link, peer.clone(), tx, role, &[])
            .await;
        (link, rx)
    }

    fn lazy_channel() -> Channel {
        Endpoint::from_static("http://example.invalid").connect_lazy()
    }

    #[tokio::test]
    async fn direct_routes_beat_relay_routes() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let relay = host(100);
        let (relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        let _ = relay_link;
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;

        routing.apply_claim_up(relay.id, peer.clone()).await;
        routing.apply_direct_up(peer.clone(), direct_link).await;

        let _channel = manager.channel_to(peer.id).await.unwrap();
        assert_eq!(
            manager.active_route(peer.id).await,
            Some(Route::Direct(direct_link))
        );
    }

    /// Cloud pairing must select the cloud link even when a direct route
    /// exists: the pairing TLS ClientHello leaves on the cloud link.
    #[tokio::test]
    async fn cloud_pin_pairing_uses_the_cloud_link_not_the_direct_route() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone()));
        let peer = host(2);
        let cloud = cloud_host(100);
        let (_cloud_link, mut cloud_rx) =
            register_link(&tunnels, &cloud, LinkRole::CloudRelay).await;
        let (direct_link, mut direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;
        routing.apply_direct_up(peer.clone(), direct_link).await;
        routing.apply_claim_up(cloud.id, peer.clone()).await;

        let pairing_manager = manager.clone();
        let peer_id = peer.id;
        let pairing =
            tokio::spawn(
                async move { pairing_manager.cloud_pin_pairing_channel_to(peer_id).await },
            );

        // Links also carry adjacency events; the assertion is about where
        // the pairing *tunnel frames* go.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let message = tokio::time::timeout_at(deadline, cloud_rx.recv())
                .await
                .expect("timed out waiting for pairing traffic on the cloud link")
                .expect("cloud link closed");
            if matches!(
                message.body,
                Some(crate::protocol::wire::pb::message::Body::TunnelFrame(_))
            ) {
                break;
            }
        }
        while let Ok(message) = direct_rx.try_recv() {
            assert!(
                !matches!(
                    message.body,
                    Some(crate::protocol::wire::pb::message::Body::TunnelFrame(_))
                ),
                "no tunnel frame may leave on the direct link"
            );
        }
        pairing.abort();
    }

    /// Regression for NETWORKING_REVIEW.md §6.7: a claim whose target is a
    /// cloud relay (learned through a peer before our own cloud link is up)
    /// must be recorded but never eagerly tunneled into — relays discard
    /// inbound tunnels, so materializing one can only stall the event loop.
    #[tokio::test]
    async fn relay_targets_are_recorded_but_never_eagerly_activated() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone()));
        let _task = manager.clone().attach_routing_events().await;
        let peer = host(2);
        let cloud = cloud_host(100);
        let (_peer_link, _peer_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;

        routing.apply_claim_up(peer.id, cloud.clone()).await;

        tokio::task::yield_now().await;
        assert_eq!(
            manager.known_routes(cloud.id).await,
            vec![Route::Via(peer.id)],
            "the route itself is still recorded"
        );
        assert!(
            manager.active_route(cloud.id).await.is_none(),
            "relay targets must not auto-activate"
        );
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn cloud_pairing_rejects_spoofed_cloud_relay_capability_on_peer_link() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        // The spoofing relay advertises the cloud capability, but its link
        // carries the ordinary Peer role.
        let spoof = cloud_host(100);
        let (_spoof_link, _spoof_rx) = register_link(&tunnels, &spoof, LinkRole::Peer).await;

        routing.apply_claim_up(spoof.id, peer.clone()).await;

        let error = manager
            .cloud_pin_pairing_channel_to(peer.id)
            .await
            .unwrap_err();

        assert!(matches!(error, TunnelPoolError::NotFound { host_id } if host_id == peer.id));
    }

    #[tokio::test]
    async fn cloud_pin_pairing_rejects_peers_without_cloud_claims() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        routing.apply_direct_up(peer.clone(), direct_link).await;

        let error = manager
            .cloud_pin_pairing_channel_to(peer.id)
            .await
            .unwrap_err();

        assert!(matches!(error, TunnelPoolError::NotFound { host_id } if host_id == peer.id));
    }

    #[tokio::test]
    async fn reachability_error_changes_emit_host_status_event() {
        let routing = Arc::new(RoutingCore::new());
        let mut rx = routing.subscribe_hosts().await;
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels);
        let peer = HostId::from_u128(2);

        manager.record_reachability_error(peer, "failed").await;

        assert!(matches!(
            rx.recv().await,
            Some(HostReachabilityEvent::StatusChanged { host_id }) if host_id == peer
        ));
    }

    #[tokio::test]
    async fn channel_to_materializes_and_reuses_relay_channel() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let relay = host(100);
        let (_relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        routing.apply_claim_up(relay.id, peer.clone()).await;

        let _first = manager.channel_to(peer.id).await.unwrap();
        let _second = manager.channel_to(peer.id).await.unwrap();

        assert_eq!(manager.pool().len().await, 1);
        assert_eq!(
            manager.active_route(peer.id).await,
            Some(Route::Via(relay.id))
        );
        assert_eq!(tunnels.counts().await, (1, 0));
    }

    #[tokio::test]
    async fn channel_to_rejects_cached_relay_channel_when_relay_link_is_gone() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let relay = host(100);
        let (relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        routing.apply_claim_up(relay.id, peer.clone()).await;
        let _channel = manager.channel_to(peer.id).await.unwrap();

        tunnels.link_registry().remove(&relay_link).await;
        let error = manager.channel_to(peer.id).await.unwrap_err();

        assert!(
            matches!(error, TunnelPoolError::LinkUnavailable { host_id } if host_id == relay.id)
        );
    }

    #[tokio::test]
    async fn channel_to_rejects_direct_route_without_registered_channel() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        routing.apply_direct_up(peer.clone(), direct_link).await;

        let error = manager.channel_to(peer.id).await.unwrap_err();

        assert!(
            matches!(error, TunnelPoolError::LinkUnavailable { host_id } if host_id == peer.id)
        );
        assert_eq!(manager.pool().len().await, 0);
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_uses_pre_registered_direct_channel() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;
        routing.apply_direct_up(peer.clone(), direct_link).await;

        let _channel = manager.channel_to(peer.id).await.unwrap();

        assert_eq!(manager.pool().len().await, 1);
        assert_eq!(
            manager.active_route(peer.id).await,
            Some(Route::Direct(direct_link))
        );
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_rejects_cached_direct_channel_when_link_is_gone() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;
        routing.apply_direct_up(peer.clone(), direct_link).await;
        let _channel = manager.channel_to(peer.id).await.unwrap();

        tunnels.link_registry().remove(&direct_link).await;
        let error = manager.channel_to(peer.id).await.unwrap_err();

        assert!(
            matches!(error, TunnelPoolError::LinkUnavailable { host_id } if host_id == peer.id)
        );
    }

    #[tokio::test]
    async fn a_recovering_direct_link_swaps_back_and_drops_relay_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone()));
        let _task = manager.clone().attach_routing_events().await;
        let peer = host(2);
        let relay = host(100);
        let (_relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        routing.apply_claim_up(relay.id, peer.clone()).await;
        let old_channel = manager.channel_to(peer.id).await.unwrap();
        assert_eq!(tunnels.counts().await, (1, 0));

        // The direct link comes (back) up: make-then-break to the direct
        // channel, retiring the relay tunnel.
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;
        routing.apply_direct_up(peer.clone(), direct_link).await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if manager.active_route(peer.id).await == Some(Route::Direct(direct_link)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for the direct swap");
        assert_eq!(tunnels.counts().await, (0, 1), "the relay tunnel retired");
        drop(old_channel);
    }

    #[tokio::test]
    async fn claim_down_drops_relay_channel_and_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone()));
        let _task = manager.clone().attach_routing_events().await;
        let peer = host(2);
        let relay = host(100);
        let (_relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        routing.apply_claim_up(relay.id, peer.clone()).await;
        let channel = manager.channel_to(peer.id).await.unwrap();
        assert_eq!(tunnels.counts().await, (1, 0));

        routing.apply_claim_down(relay.id, peer.id).await;

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if manager.active_route(peer.id).await.is_none() && tunnels.counts().await == (0, 1)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for claim-down cleanup");
        assert_eq!(manager.pool().len().await, 0);
        drop(channel);
    }

    #[tokio::test]
    async fn direct_route_down_falls_back_to_relay_claim() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone()));
        let _task = manager.clone().attach_routing_events().await;
        let peer = host(2);
        let relay = host(100);
        let (_relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;
        routing.apply_claim_up(relay.id, peer.clone()).await;
        routing.apply_direct_up(peer.clone(), direct_link).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if manager.active_route(peer.id).await == Some(Route::Direct(direct_link)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for direct activation");

        routing.apply_direct_down(direct_link).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if manager.active_route(peer.id).await != Some(Route::Direct(direct_link)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for direct route removal");

        let _fallback = manager.channel_to(peer.id).await.unwrap();
        assert_eq!(
            manager.active_route(peer.id).await,
            Some(Route::Via(relay.id))
        );
    }

    #[tokio::test]
    async fn teardown_host_removes_routes_channels_tunnels_and_links() {
        let routing = Arc::new(RoutingCore::new());
        let tunnels = test_pool(HostId::from_u128(1), &routing);
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = host(2);
        let relay = host(100);
        let (_relay_link, _relay_rx) = register_link(&tunnels, &relay, LinkRole::Peer).await;
        let (direct_link, _direct_rx) = register_link(&tunnels, &peer, LinkRole::Peer).await;
        manager
            .route_runtime()
            .register_direct(direct_link, lazy_channel())
            .await;
        routing.apply_direct_up(peer.clone(), direct_link).await;
        routing.apply_claim_up(relay.id, peer.clone()).await;
        let registry = tunnels.link_registry();
        let registry_for_close = registry.clone();
        tokio::spawn(async move {
            // Stand in for the link's connect task: honor the close request.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            registry_for_close.remove(&direct_link).await;
        });
        let _tunnel_channel = manager.channel_to(peer.id).await.ok();
        let _relay_channel = tunnels.channel_via(peer.id, relay.id).await.unwrap();
        assert!(manager.pool().len().await >= 1);

        manager.teardown_host(peer.id).await;

        assert!(manager.known_routes(peer.id).await.is_empty());
        assert!(routing.host_entry(peer.id).await.is_none());
        assert_eq!(manager.pool().len().await, 0);
        let (active, _retired) = tunnels.counts().await;
        assert_eq!(active, 0);

        // Late updates during the replacement window are suppressed…
        assert_eq!(
            routing.apply_claim_up(relay.id, peer.clone()).await,
            crate::routing::RouteUpdateOutcome::Replacing
        );
        manager.finish_host_replacement(peer.id).await;
        // …and flow again once the replacement completes.
        assert_eq!(
            routing.apply_claim_up(relay.id, peer.clone()).await,
            crate::routing::RouteUpdateOutcome::Inserted
        );
        assert_eq!(
            manager.known_routes(peer.id).await,
            vec![Route::Via(relay.id)]
        );
    }
}
