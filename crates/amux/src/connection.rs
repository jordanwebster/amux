//! Route-keyed outbound connection selection.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use crate::HostId;
use crate::routing::{Route, RoutingCore, RoutingEvent, select_best_route};
use crate::transport::TrustedPeerConnections;
use crate::tunnel::{TunnelId, TunnelPool, TunnelPoolError};

#[derive(Default)]
pub(crate) struct ConnectionPool {
    by_route: RwLock<HashMap<Route, Channel>>,
}

impl ConnectionPool {
    pub(crate) async fn register(&self, route: Route, channel: Channel) {
        self.by_route.write().await.insert(route, channel);
    }

    pub(crate) async fn get(&self, route: &Route) -> Option<Channel> {
        self.by_route.read().await.get(route).cloned()
    }

    pub(crate) async fn unregister(&self, route: &Route) {
        self.by_route.write().await.remove(route);
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.by_route.read().await.len()
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

    pub(crate) async fn register(&self, route: Route, channel: Channel) {
        self.pool.register(route, channel).await;
    }

    pub(crate) async fn remove_route(&self, route: &Route) {
        self.pool.unregister(route).await;
        self.tunnels.remove_route(route).await;
    }

    pub(crate) async fn remove_host_preserving_tunnel(
        &self,
        host_id: HostId,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        self.tunnels
            .remove_host_preserving_tunnel(host_id, preserve_tunnel_id)
            .await;
    }

    pub(crate) async fn remove_route_preserving_tunnel(
        &self,
        route: &Route,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        self.pool.unregister(route).await;
        self.tunnels
            .remove_route_preserving_tunnel(route, preserve_tunnel_id)
            .await;
    }
}

#[derive(Default)]
struct ConnectionState {
    routes: HashMap<HostId, Vec<Route>>,
    active: HashMap<HostId, Route>,
    replacing: HashSet<HostId>,
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

    pub(crate) async fn seed(&self, events: Vec<RoutingEvent>) {
        for event in events {
            self.record_event(event).await;
        }
    }

    pub(crate) async fn attach_routing_events(self: Arc<Self>) -> JoinHandle<()> {
        self.seed(self.routing.routing_events_snapshot().await)
            .await;
        let mut rx = self.routing.subscribe_routing_events().await;
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                self.handle_event(event).await;
            }
        })
    }

    pub(crate) async fn channel_to(&self, peer: HostId) -> Result<Channel, TunnelPoolError> {
        let route = self
            .best_route(peer)
            .await
            .ok_or(TunnelPoolError::NotFound { host_id: peer })?;

        self.activate_route(peer, route).await
    }

    pub(crate) async fn cloud_pin_pairing_channel_to(
        &self,
        peer: HostId,
    ) -> Result<Channel, TunnelPoolError> {
        let route = self.cloud_route(peer).await?;
        self.runtime
            .tunnels
            .pin_pairing_channel_to_route(peer, route)
            .await
    }

    pub(crate) async fn cloud_qr_pairing_channel_to(
        &self,
        peer: HostId,
        expected_pubkey: Vec<u8>,
    ) -> Result<Channel, TunnelPoolError> {
        let route = self.cloud_route(peer).await?;
        self.runtime
            .tunnels
            .qr_pairing_channel_to_route(peer, route, expected_pubkey)
            .await
    }

    pub(crate) async fn has_cloud_route(&self, peer: HostId) -> bool {
        self.cloud_route(peer).await.is_ok()
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

    pub(crate) async fn send_goaway_to_host(
        &self,
        peer: HostId,
        reason: crate::protocol::wire::pb::GoAwayReason,
        drain_timeout_ms: u32,
    ) {
        self.runtime
            .tunnels
            .link_registry()
            .send_goaway_to_host(peer, reason, drain_timeout_ms)
            .await;
    }

    #[allow(dead_code)]
    pub(crate) async fn teardown_host(&self, peer: HostId) {
        self.teardown_host_preserving_tunnel(peer, None).await;
    }

    pub(crate) async fn teardown_host_preserving_tunnel(
        &self,
        peer: HostId,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        {
            self.state.write().await.replacing.insert(peer);
        }
        for event in self.routing.remove_host_routes(peer, None).await {
            self.record_event_preserving_tunnel(event, preserve_tunnel_id)
                .await;
        }
        self.remove_host_runtime_state_preserving_tunnel(peer, preserve_tunnel_id)
            .await;
        self.trusted_connections.close_host(peer).await;
        self.runtime.tunnels.link_registry().close_host(peer).await;
        self.remove_host_runtime_state_preserving_tunnel(peer, preserve_tunnel_id)
            .await;
    }

    #[allow(dead_code)]
    pub(crate) async fn finish_host_replacement(&self, peer: HostId) {
        self.finish_host_replacement_preserving_tunnel(peer, None)
            .await;
    }

    pub(crate) async fn finish_host_replacement_preserving_tunnel(
        &self,
        peer: HostId,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        for event in self.routing.remove_host_routes(peer, None).await {
            self.record_event_preserving_tunnel(event, preserve_tunnel_id)
                .await;
        }
        self.state.write().await.replacing.remove(&peer);
        self.trusted_connections.finish_host_replacement(peer);
    }

    async fn handle_event(&self, event: RoutingEvent) {
        let swap = self.record_event(event).await;
        if let Some((peer, route)) = swap
            && let Err(error) = self.activate_route(peer, route).await
        {
            tracing::warn!(%peer, error = %error, "failed to activate route");
        }
    }

    async fn record_event(&self, event: RoutingEvent) -> Option<(HostId, Route)> {
        match event {
            RoutingEvent::HostUp { host, route, .. } => {
                let peer = host.id;
                let mut state = self.state.write().await;
                if state.replacing.contains(&peer) {
                    return None;
                }
                state.reachability_errors.remove(&peer);
                let peer_routes = state.routes.entry(peer).or_default();
                if !peer_routes.contains(&route) {
                    peer_routes.push(route.clone());
                }
                if should_activate(&state, peer, &route) {
                    Some((peer, route))
                } else {
                    None
                }
            }
            RoutingEvent::HostDown { host_id, route, .. } => {
                let (replacing, known, active_removed) = {
                    let mut state = self.state.write().await;
                    let replacing = state.replacing.contains(&host_id);
                    let mut known = false;
                    let mut active_removed = false;
                    if let Some(peer_routes) = state.routes.get_mut(&host_id) {
                        known = peer_routes.contains(&route);
                        peer_routes.retain(|known| known != &route);
                        if peer_routes.is_empty() {
                            state.routes.remove(&host_id);
                        }
                    }
                    if state.active.get(&host_id) == Some(&route) {
                        known = true;
                        state.active.remove(&host_id);
                        active_removed = true;
                    }
                    (replacing, known, active_removed)
                };
                if active_removed {
                    self.routing.mark_active_route(host_id, None).await;
                }
                if replacing || !known {
                    return None;
                }
                self.remove_route_runtime_state(&route).await;
                None
            }
        }
    }

    async fn record_event_preserving_tunnel(
        &self,
        event: RoutingEvent,
        preserve_tunnel_id: Option<TunnelId>,
    ) -> Option<(HostId, Route)> {
        match event {
            RoutingEvent::HostDown { host_id, route, .. } => {
                let mut active_removed = false;
                {
                    let mut state = self.state.write().await;
                    if let Some(peer_routes) = state.routes.get_mut(&host_id) {
                        peer_routes.retain(|known| known != &route);
                        if peer_routes.is_empty() {
                            state.routes.remove(&host_id);
                        }
                    }
                    if state.active.get(&host_id) == Some(&route) {
                        state.active.remove(&host_id);
                        active_removed = true;
                    }
                }
                if active_removed {
                    self.routing.mark_active_route(host_id, None).await;
                }
                self.remove_route_runtime_state_preserving_tunnel(&route, preserve_tunnel_id)
                    .await;
                None
            }
            other => self.record_event(other).await,
        }
    }

    async fn remove_host_runtime_state_preserving_tunnel(
        &self,
        peer: HostId,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        let routes = {
            let mut state = self.state.write().await;
            let mut routes = state.routes.remove(&peer).unwrap_or_default();
            if let Some(active) = state.active.remove(&peer)
                && !routes.contains(&active)
            {
                routes.push(active);
            }
            routes
        };
        for route in &routes {
            self.remove_route_runtime_state_preserving_tunnel(route, preserve_tunnel_id)
                .await;
        }
        self.runtime
            .remove_host_preserving_tunnel(peer, preserve_tunnel_id)
            .await;
    }

    async fn best_route(&self, peer: HostId) -> Option<Route> {
        select_best_route(
            self.state
                .read()
                .await
                .routes
                .get(&peer)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
    }

    async fn cloud_route(&self, peer: HostId) -> Result<Route, TunnelPoolError> {
        let routes = self
            .state
            .read()
            .await
            .routes
            .get(&peer)
            .cloned()
            .unwrap_or_default();
        let mut cloud_routes = Vec::new();
        for route in routes {
            if self.route_uses_cloud_link(&route).await {
                cloud_routes.push(route);
            }
        }
        select_best_route(&cloud_routes).ok_or(TunnelPoolError::NotFound { host_id: peer })
    }

    async fn route_uses_cloud_link(&self, route: &Route) -> bool {
        let Some(link) = route.iter().next() else {
            return false;
        };
        self.runtime
            .tunnels
            .link_registry()
            .is_cloud_relay(link)
            .await
    }

    async fn activate_route(&self, peer: HostId, route: Route) -> Result<Channel, TunnelPoolError> {
        let channel = self.materialize(peer, &route).await?;
        let (known, old) = {
            let mut state = self.state.write().await;
            if !route_is_known(&state, peer, &route) {
                (false, None)
            } else {
                (true, state.active.insert(peer, route.clone()))
            }
        };
        if !known {
            self.remove_route_runtime_state(&route).await;
            return Err(TunnelPoolError::NotFound { host_id: peer });
        }
        if old.as_ref().is_some_and(|old| old != &route)
            && let Some(old) = old
        {
            self.remove_route_runtime_state(&old).await;
        }
        self.clear_reachability_error(peer).await;
        self.routing
            .mark_active_route(peer, Some(route.clone()))
            .await;
        Ok(channel)
    }

    async fn remove_route_runtime_state(&self, route: &Route) {
        self.runtime.remove_route(route).await;
    }

    async fn remove_route_runtime_state_preserving_tunnel(
        &self,
        route: &Route,
        preserve_tunnel_id: Option<TunnelId>,
    ) {
        self.runtime
            .remove_route_preserving_tunnel(route, preserve_tunnel_id)
            .await;
    }

    async fn materialize(&self, peer: HostId, route: &Route) -> Result<Channel, TunnelPoolError> {
        if let Some(channel) = self.runtime.pool.get(route).await {
            self.ensure_cached_route_first_hop_available(peer, route)
                .await?;
            return Ok(channel);
        }
        if route.len() == 1 {
            let link = route
                .iter()
                .next()
                .cloned()
                .ok_or(TunnelPoolError::EmptyRoute { host_id: peer })?;
            return Err(TunnelPoolError::LinkUnavailable { link });
        }
        let channel = self
            .runtime
            .tunnels
            .channel_to_route(peer, route.clone())
            .await?;
        self.runtime.register(route.clone(), channel.clone()).await;
        Ok(channel)
    }

    async fn ensure_cached_route_first_hop_available(
        &self,
        peer: HostId,
        route: &Route,
    ) -> Result<(), TunnelPoolError> {
        let first_hop = route
            .iter()
            .next()
            .cloned()
            .ok_or(TunnelPoolError::EmptyRoute { host_id: peer })?;
        let _ = self
            .runtime
            .tunnels
            .link_registry()
            .outgoing_tx(&first_hop)
            .await?;
        Ok(())
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) async fn active_route(&self, peer: HostId) -> Option<Route> {
        self.state.read().await.active.get(&peer).cloned()
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) async fn known_routes(&self, peer: HostId) -> Vec<Route> {
        self.state
            .read()
            .await
            .routes
            .get(&peer)
            .cloned()
            .unwrap_or_default()
    }
}

fn should_activate(state: &ConnectionState, peer: HostId, route: &Route) -> bool {
    match state.active.get(&peer) {
        Some(active) => route.len() < active.len(),
        None => true,
    }
}

fn route_is_known(state: &ConnectionState, peer: HostId, route: &Route) -> bool {
    state
        .routes
        .get(&peer)
        .is_some_and(|routes| routes.contains(route))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;
    use tonic::transport::Endpoint;

    use super::*;
    use crate::routing::{
        Capabilities, FEATURE_CLOUD_RELAY, Host, HostReachabilityEvent, Link, LinkCloseReason,
        LinkRole, SupportedAgentType,
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

    fn route(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    #[tokio::test]
    async fn manager_tracks_shortest_route_and_fifo_ties() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register_with_role(
                Link::new("cloud").unwrap(),
                HostId::from_u128(100),
                cloud_tx,
                LinkRole::CloudRelay,
            )
            .await;

        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route(&["cloud", "peer"]),
                origin_link: None,
            })
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route(&["other", "peer"]),
                origin_link: None,
            })
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: Route::from_link(Link::new("direct").unwrap()),
                origin_link: None,
            })
            .await;

        assert_eq!(manager.best_route(peer).await, Some(route(&["direct"])));
        assert_eq!(
            manager.known_routes(peer).await,
            vec![
                route(&["cloud", "peer"]),
                route(&["other", "peer"]),
                route(&["direct"]),
            ]
        );
    }

    #[tokio::test]
    async fn cloud_pin_pairing_uses_cloud_route_not_shortest_route() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register_with_role(
                Link::new("cloud").unwrap(),
                HostId::from_u128(100),
                cloud_tx,
                LinkRole::CloudRelay,
            )
            .await;
        tunnels
            .link_registry()
            .mark_draining(&Link::new("cloud").unwrap())
            .await;

        manager
            .record_event(RoutingEvent::HostUp {
                host: cloud_host(100),
                route: route(&["cloud"]),
                origin_link: None,
            })
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route(&["cloud", "peer"]),
                origin_link: Some(Link::new("cloud").unwrap()),
            })
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route(&["direct"]),
                origin_link: None,
            })
            .await;

        let error = manager
            .cloud_pin_pairing_channel_to(peer)
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            TunnelPoolError::LinkDraining { link } if link == Link::new("cloud").unwrap()
        ));
    }

    #[tokio::test]
    async fn cloud_pairing_rejects_spoofed_cloud_relay_capability_on_peer_link() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let (spoof_tx, _spoof_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register_with_role(
                Link::new("spoof").unwrap(),
                HostId::from_u128(100),
                spoof_tx,
                LinkRole::Peer,
            )
            .await;

        manager
            .record_event(RoutingEvent::HostUp {
                host: cloud_host(100),
                route: route(&["spoof"]),
                origin_link: None,
            })
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route(&["spoof", "peer"]),
                origin_link: Some(Link::new("spoof").unwrap()),
            })
            .await;

        let error = manager
            .cloud_pin_pairing_channel_to(peer)
            .await
            .unwrap_err();

        assert!(matches!(error, TunnelPoolError::NotFound { host_id } if host_id == peer));
    }

    #[tokio::test]
    async fn reachability_error_changes_emit_host_status_event() {
        let routing = Arc::new(RoutingCore::new());
        let mut rx = routing.subscribe_hosts().await;
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels);
        let peer = HostId::from_u128(2);

        manager.record_reachability_error(peer, "failed").await;

        assert!(matches!(
            rx.recv().await,
            Some(HostReachabilityEvent::StatusChanged { host_id }) if host_id == peer
        ));
    }

    #[tokio::test]
    async fn cloud_pin_pairing_rejects_non_cloud_routes() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels);
        let peer = HostId::from_u128(2);

        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route(&["direct"]),
                origin_link: None,
            })
            .await;

        let error = manager
            .cloud_pin_pairing_channel_to(peer)
            .await
            .unwrap_err();

        assert!(matches!(error, TunnelPoolError::NotFound { host_id } if host_id == peer));
    }

    #[tokio::test]
    async fn manager_removes_active_route_on_host_down() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels);
        let peer = HostId::from_u128(2);
        let route = route(&["cloud", "peer"]);

        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;
        manager
            .state
            .write()
            .await
            .active
            .insert(peer, route.clone());
        manager
            .pool()
            .register(
                route.clone(),
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;

        manager
            .record_event(RoutingEvent::HostDown {
                host_id: peer,
                route: route.clone(),
                origin_link: None,
            })
            .await;

        assert!(manager.active_route(peer).await.is_none());
        assert!(manager.known_routes(peer).await.is_empty());
        assert_eq!(manager.pool().len().await, 0);
    }

    #[tokio::test]
    async fn teardown_host_removes_routes_channels_tunnels_and_links() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let direct_route = route(&["direct"]);
        let tunnel_route = route(&["relay", "peer"]);
        let channel = Endpoint::from_static("http://example.invalid").connect_lazy();

        routing
            .apply_host_up(host(2), direct_route.clone(), None)
            .await;
        routing
            .apply_host_up(host(2), tunnel_route.clone(), None)
            .await;
        manager.seed(routing.routing_events_snapshot().await).await;
        manager.pool().register(direct_route, channel).await;
        let direct = Link::new("direct").unwrap();
        let (direct_tx, _direct_rx) = mpsc::channel(1);
        let mut close_rx = tunnels
            .link_registry()
            .register(direct.clone(), peer, direct_tx)
            .await;
        let link_registry = tunnels.link_registry();
        tokio::spawn(async move {
            assert_eq!(close_rx.recv().await, Some(LinkCloseReason::TrustReplaced));
            link_registry.remove(&direct).await;
        });
        let (relay_tx, _relay_rx) = mpsc::channel(1);
        tunnels
            .link_registry()
            .register(Link::new("relay").unwrap(), HostId::from_u128(99), relay_tx)
            .await;
        let _tunnel_channel = tunnels
            .channel_to_route(peer, tunnel_route.clone())
            .await
            .unwrap();
        assert_eq!(tunnels.counts().await, (1, 0));
        assert_eq!(manager.pool().len().await, 1);

        manager.teardown_host(peer).await;

        assert!(manager.known_routes(peer).await.is_empty());
        assert!(routing.host_entry(peer).await.is_none());
        assert_eq!(manager.pool().len().await, 0);
        assert_eq!(tunnels.counts().await, (0, 1));
        let late_old_route = route(&["late-old-key"]);
        assert_eq!(
            routing
                .apply_host_up(host(2), late_old_route.clone(), None)
                .await,
            crate::routing::HostUpOutcome::Inserted
        );
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: late_old_route,
                origin_link: None,
            })
            .await;
        assert!(manager.known_routes(peer).await.is_empty());
        assert!(routing.host_entry(peer).await.is_some());
        manager.finish_host_replacement(peer).await;
        assert!(routing.host_entry(peer).await.is_none());
        assert_eq!(
            routing
                .apply_host_up(host(2), tunnel_route.clone(), None)
                .await,
            crate::routing::HostUpOutcome::Inserted
        );
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: tunnel_route.clone(),
                origin_link: None,
            })
            .await;
        assert_eq!(manager.known_routes(peer).await, vec![tunnel_route]);
    }

    #[tokio::test]
    async fn channel_to_materializes_and_reuses_route_keyed_channel() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let route = route(&["relay", "peer"]);
        let (link_tx, _link_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("relay").unwrap(), peer, link_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;

        let _first = manager.channel_to(peer).await.unwrap();
        let _second = manager.channel_to(peer).await.unwrap();

        assert_eq!(manager.pool().len().await, 1);
        assert_eq!(manager.active_route(peer).await, Some(route));
        assert_eq!(tunnels.counts().await, (1, 0));
    }

    #[tokio::test]
    async fn channel_to_rejects_cached_route_on_draining_first_hop() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let relay = Link::new("relay").unwrap();
        let route = route(&["relay", "peer"]);
        let (link_tx, _link_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(relay.clone(), peer, link_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;
        let _channel = manager.channel_to(peer).await.unwrap();
        assert_eq!(manager.active_route(peer).await, Some(route.clone()));
        assert_eq!(manager.pool().len().await, 1);

        assert!(tunnels.link_registry().mark_draining(&relay).await);
        let error = manager.channel_to(peer).await.unwrap_err();

        assert!(matches!(error, TunnelPoolError::LinkDraining { link } if link == relay));
        assert_eq!(manager.active_route(peer).await, Some(route));
        assert_eq!(manager.pool().len().await, 1);
    }

    #[tokio::test]
    async fn channel_to_rejects_single_hop_route_without_registered_channel() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let route = Route::from_link(Link::new("direct").unwrap());
        let (link_tx, _link_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("direct").unwrap(), peer, link_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;

        let error = manager.channel_to(peer).await.unwrap_err();

        assert!(
            matches!(error, TunnelPoolError::LinkUnavailable { link } if link == Link::new("direct").unwrap())
        );
        assert_eq!(manager.pool().len().await, 0);
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_does_not_fallback_to_tunnel_when_shortest_one_hop_lacks_channel() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let direct = Route::from_link(Link::new("direct").unwrap());
        let multi_hop = route(&["relay", "peer"]);
        let (relay_tx, _relay_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("relay").unwrap(), peer, relay_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: multi_hop,
                origin_link: None,
            })
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: direct.clone(),
                origin_link: None,
            })
            .await;

        let error = manager.channel_to(peer).await.unwrap_err();

        assert!(
            matches!(error, TunnelPoolError::LinkUnavailable { link } if link == Link::new("direct").unwrap())
        );
        assert!(manager.active_route(peer).await.is_none());
        assert_eq!(manager.pool().len().await, 0);
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_uses_pre_registered_single_hop_channel() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let route = Route::from_link(Link::new("direct").unwrap());
        let (link_tx, _link_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("direct").unwrap(), peer, link_tx)
            .await;
        manager
            .pool()
            .register(
                route.clone(),
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;

        let _channel = manager.channel_to(peer).await.unwrap();

        assert_eq!(manager.pool().len().await, 1);
        assert_eq!(manager.active_route(peer).await, Some(route));
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_rejects_cached_single_hop_channel_on_draining_link() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let direct = Link::new("direct").unwrap();
        let route = Route::from_link(direct.clone());
        let (link_tx, _link_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(direct.clone(), peer, link_tx)
            .await;
        manager
            .pool()
            .register(
                route.clone(),
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;
        let _channel = manager.channel_to(peer).await.unwrap();

        assert!(tunnels.link_registry().mark_draining(&direct).await);
        let error = manager.channel_to(peer).await.unwrap_err();

        assert!(matches!(error, TunnelPoolError::LinkDraining { link } if link == direct));
        assert_eq!(manager.active_route(peer).await, Some(route));
        assert_eq!(manager.pool().len().await, 1);
        assert_eq!(tunnels.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn channel_to_rejects_pre_active_cached_channel_on_draining_link() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let direct = Link::new("direct").unwrap();
        let route = Route::from_link(direct.clone());
        let (link_tx, _link_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(direct.clone(), peer, link_tx)
            .await;
        manager
            .pool()
            .register(
                route.clone(),
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route,
                origin_link: None,
            })
            .await;
        assert!(manager.active_route(peer).await.is_none());

        assert!(tunnels.link_registry().mark_draining(&direct).await);
        let error = manager.channel_to(peer).await.unwrap_err();

        assert!(matches!(error, TunnelPoolError::LinkDraining { link } if link == direct));
        assert!(manager.active_route(peer).await.is_none());
    }

    #[tokio::test]
    async fn channel_to_swaps_to_shorter_route_make_then_break() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let longer = route(&["cloud", "peer"]);
        let shorter = Route::from_link(Link::new("direct").unwrap());
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("cloud").unwrap(), peer, cloud_tx)
            .await;
        let (direct_tx, _direct_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("direct").unwrap(), peer, direct_tx)
            .await;

        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: longer.clone(),
                origin_link: None,
            })
            .await;
        let first = manager.channel_to(peer).await.unwrap();
        assert_eq!(manager.active_route(peer).await, Some(longer));

        manager
            .pool()
            .register(
                shorter.clone(),
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: shorter.clone(),
                origin_link: None,
            })
            .await;
        drop(first);
        let _second = manager.channel_to(peer).await.unwrap();

        assert_eq!(manager.pool().len().await, 1);
        assert_eq!(manager.active_route(peer).await, Some(shorter));
    }

    #[tokio::test]
    async fn swapping_routes_drops_old_route_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let longer = route(&["cloud", "relay", "peer"]);
        let shorter = route(&["direct-relay", "peer"]);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let (direct_tx, _direct_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("cloud").unwrap(), peer, cloud_tx)
            .await;
        tunnels
            .link_registry()
            .register(Link::new("direct-relay").unwrap(), peer, direct_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: longer.clone(),
                origin_link: None,
            })
            .await;
        let old_channel = manager.channel_to(peer).await.unwrap();
        assert_eq!(tunnels.counts().await, (1, 0));

        manager
            .handle_event(RoutingEvent::HostUp {
                host: host(2),
                route: shorter.clone(),
                origin_link: None,
            })
            .await;

        assert_eq!(manager.active_route(peer).await, Some(shorter));
        assert!(manager.pool().get(&longer).await.is_none());
        assert_eq!(tunnels.counts().await, (1, 1));
        drop(old_channel);
    }

    #[tokio::test]
    async fn host_up_shorter_multihop_materializes_and_swaps_make_then_break() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let longer = route(&["cloud", "relay", "peer"]);
        let shorter = route(&["direct-relay", "peer"]);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let (direct_tx, _direct_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("cloud").unwrap(), peer, cloud_tx)
            .await;
        tunnels
            .link_registry()
            .register(Link::new("direct-relay").unwrap(), peer, direct_tx)
            .await;

        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: longer.clone(),
                origin_link: None,
            })
            .await;
        let first = manager.channel_to(peer).await.unwrap();
        assert_eq!(manager.active_route(peer).await, Some(longer.clone()));

        manager
            .handle_event(RoutingEvent::HostUp {
                host: host(2),
                route: shorter.clone(),
                origin_link: None,
            })
            .await;

        assert_eq!(manager.active_route(peer).await, Some(shorter.clone()));
        assert!(manager.pool().get(&longer).await.is_none());
        assert!(manager.pool().get(&shorter).await.is_some());
        drop(first);
    }

    #[tokio::test]
    async fn host_down_drops_route_tunnels() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let route = route(&["relay", "peer"]);
        let (relay_tx, _relay_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("relay").unwrap(), peer, relay_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;
        let channel = manager.channel_to(peer).await.unwrap();
        assert_eq!(tunnels.counts().await, (1, 0));

        manager
            .record_event(RoutingEvent::HostDown {
                host_id: peer,
                route,
                origin_link: None,
            })
            .await;

        assert_eq!(tunnels.counts().await, (0, 1));
        assert_eq!(manager.pool().len().await, 0);
        drop(channel);
    }

    #[tokio::test]
    async fn host_down_wins_race_after_materialization_before_active_insert() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = Arc::new(ConnectionManager::new(routing.clone(), tunnels.clone()));
        let peer = HostId::from_u128(2);
        let route = route(&["relay", "peer"]);
        let (relay_tx, _relay_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("relay").unwrap(), peer, relay_tx)
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: route.clone(),
                origin_link: None,
            })
            .await;
        let channel = manager.materialize(peer, &route).await.unwrap();
        let read_guard = manager.state.read().await;

        let remove_manager = manager.clone();
        let remove_route = route.clone();
        let remove = tokio::spawn(async move {
            remove_manager
                .record_event(RoutingEvent::HostDown {
                    host_id: peer,
                    route: remove_route,
                    origin_link: None,
                })
                .await;
        });
        tokio::task::yield_now().await;

        let activate_manager = manager.clone();
        let activate_route = route.clone();
        let activate =
            tokio::spawn(
                async move { activate_manager.activate_route(peer, activate_route).await },
            );
        tokio::task::yield_now().await;

        drop(read_guard);
        remove.await.unwrap();
        let error = activate.await.unwrap().unwrap_err();
        drop(channel);

        assert!(matches!(error, TunnelPoolError::NotFound { host_id } if host_id == peer));
        assert!(manager.active_route(peer).await.is_none());
        assert!(manager.known_routes(peer).await.is_empty());
        assert_eq!(manager.pool().len().await, 0);
    }

    #[tokio::test]
    async fn direct_route_down_falls_back_to_known_multi_hop_route() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let tunnels = Arc::new(TunnelPool::new(
            HostId::from_u128(1),
            routing.clone(),
            incoming_tx,
        ));
        let manager = ConnectionManager::new(routing.clone(), tunnels.clone());
        let peer = HostId::from_u128(2);
        let multi_hop = route(&["cloud", "peer"]);
        let direct = Route::from_link(Link::new("direct").unwrap());
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("cloud").unwrap(), peer, cloud_tx)
            .await;
        let (direct_tx, _direct_rx) = mpsc::channel(8);
        tunnels
            .link_registry()
            .register(Link::new("direct").unwrap(), peer, direct_tx)
            .await;

        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: multi_hop.clone(),
                origin_link: None,
            })
            .await;
        let first = manager.channel_to(peer).await.unwrap();
        assert_eq!(manager.active_route(peer).await, Some(multi_hop.clone()));

        manager
            .pool()
            .register(
                direct.clone(),
                Endpoint::from_static("http://example.com").connect_lazy(),
            )
            .await;
        manager
            .record_event(RoutingEvent::HostUp {
                host: host(2),
                route: direct.clone(),
                origin_link: None,
            })
            .await;
        drop(first);
        let direct_channel = manager.channel_to(peer).await.unwrap();
        assert_eq!(manager.active_route(peer).await, Some(direct.clone()));

        manager
            .record_event(RoutingEvent::HostDown {
                host_id: peer,
                route: direct,
                origin_link: None,
            })
            .await;
        drop(direct_channel);
        let _fallback = manager.channel_to(peer).await.unwrap();

        assert_eq!(manager.active_route(peer).await, Some(multi_hop));
        assert_eq!(manager.pool().len().await, 1);
    }
}
