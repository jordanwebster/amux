//! Outbound channel selection over native link streams.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use crate::HostId;
use crate::link::{ChannelClass, ChannelError, ChannelKey, ChannelPool};
use crate::routing::{
    FEATURE_CLOUD_RELAY, Host, HostVia, LinkCarrier, Route, RoutingCore, RoutingEvent,
};
use crate::transport::{TrustedPeerConnections, pairing_channel_from_io};

pub struct ConnectionManager {
    routing: Arc<RoutingCore>,
    channels: Arc<ChannelPool>,
    trusted_connections: TrustedPeerConnections,
    state: RwLock<ConnectionState>,
}

#[derive(Default)]
struct ConnectionState {
    active: HashMap<HostId, Route>,
    reachability_errors: HashMap<HostId, String>,
}

impl ConnectionManager {
    pub(crate) fn new(routing: Arc<RoutingCore>, channels: Arc<ChannelPool>) -> Self {
        Self {
            routing,
            channels,
            trusted_connections: TrustedPeerConnections::default(),
            state: RwLock::new(ConnectionState::default()),
        }
    }

    pub(crate) fn trusted_connections(&self) -> TrustedPeerConnections {
        self.trusted_connections.clone()
    }

    #[cfg(test)]
    pub(crate) fn pool(&self) -> Arc<ChannelPool> {
        self.channels.clone()
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

    pub async fn channel_to(&self, peer: HostId) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Calls).await
    }

    pub async fn session_channel_to(
        &self,
        peer: HostId,
        agent: crate::AgentId,
    ) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Session { agent })
            .await
    }

    pub async fn bulk_channel_to(&self, peer: HostId) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Bulk).await
    }

    async fn channel_to_class(
        &self,
        peer: HostId,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        let route = self
            .routing
            .route_to(peer)
            .await
            .ok_or(ChannelError::NoRoute { host_id: peer })?;
        self.activate_route(peer, route, class).await
    }

    pub(crate) async fn cloud_pairing_channel_to(
        &self,
        peer: HostId,
    ) -> Result<Channel, ChannelError> {
        let relay = self.cloud_relay_for(peer).await?;
        let stream = self
            .channels
            .pairing_stream(peer, Route::Via(relay))
            .await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            pairing_channel_from_io(stream),
        )
        .await
        .map_err(|_| ChannelError::Handshake("pairing TLS handshake timed out".into()))?
        .map_err(|error| ChannelError::Tls(error.to_string()))
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
        self.state
            .write()
            .await
            .reachability_errors
            .insert(peer, error.into());
    }

    pub(crate) async fn clear_reachability_error(&self, peer: HostId) {
        self.state.write().await.reachability_errors.remove(&peer);
    }

    pub(crate) async fn via_for(&self, peer: HostId) -> HostVia {
        let route = self.state.read().await.active.get(&peer).copied();
        let route = match route {
            Some(route) => Some(route),
            None => self.routing.route_to(peer).await,
        };
        match route {
            Some(Route::Via(_)) => HostVia::Relay,
            Some(Route::Direct(link)) => match self.channels.link_registry().carrier(&link).await {
                Some(LinkCarrier::Ssh) => HostVia::Ssh,
                Some(LinkCarrier::Direct) => HostVia::Direct,
                None => HostVia::Offline,
            },
            None => HostVia::Offline,
        }
    }

    pub(crate) async fn send_link_close_to_host(
        &self,
        peer: HostId,
        reason: wire::pb::LinkCloseReason,
    ) {
        self.channels
            .link_registry()
            .send_link_close_to_host(peer, reason)
            .await;
    }

    pub(crate) async fn teardown_host(&self, peer: HostId) {
        self.routing.begin_replacement(peer).await;
        self.routing.remove_host(peer).await;
        self.remove_host_runtime_state(peer).await;
        self.trusted_connections.close_host(peer).await;
        self.channels.link_registry().close_host(peer).await;
        self.remove_host_runtime_state(peer).await;
    }

    /// Ends live access to `peer` — trusted streams, channels, links and our
    /// own routes — without forgetting the relay's word that it is online.
    /// Used when a peer is unpaired: the trust is gone and nothing of the old
    /// connection survives, but the peer stays reachable for pairing, the way
    /// any other machine on the account is before it is ever paired.
    pub(crate) async fn close_host_access(&self, peer: HostId) {
        self.routing.remove_direct_links(peer).await;
        self.remove_host_runtime_state(peer).await;
        self.trusted_connections.close_host(peer).await;
        self.channels.link_registry().close_host(peer).await;
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
                if host_is_cloud_relay(&host) {
                    return;
                }
                let already_direct = matches!(
                    self.state.read().await.active.get(&host.id),
                    Some(Route::Direct(_))
                );
                if !already_direct
                    && let Err(error) = self
                        .activate_route(host.id, Route::Direct(link), ChannelClass::Calls)
                        .await
                {
                    tracing::warn!(peer = %host.id, error = %error, "failed to activate direct route");
                }
            }
            RoutingEvent::NeighborDown { host_id, link, .. } => {
                self.channels.drop_link(link);
                let mut state = self.state.write().await;
                if state.active.get(&host_id) == Some(&Route::Direct(link)) {
                    state.active.remove(&host_id);
                }
            }
            RoutingEvent::ClaimUp { relay, host } => {
                self.clear_reachability_error(host.id).await;
                if host_is_cloud_relay(&host)
                    || self
                        .channels
                        .link_registry()
                        .has_cloud_relay_link_to(relay)
                        .await
                {
                    return;
                }
                if !self.state.read().await.active.contains_key(&host.id)
                    && let Err(error) = self
                        .activate_route(host.id, Route::Via(relay), ChannelClass::Calls)
                        .await
                {
                    tracing::warn!(peer = %host.id, relay = %relay, error = %error, "failed to activate relay route");
                }
            }
            RoutingEvent::ClaimDown { relay, host_id } => {
                self.channels.drop_route(host_id, Route::Via(relay));
                let mut state = self.state.write().await;
                if state.active.get(&host_id) == Some(&Route::Via(relay)) {
                    state.active.remove(&host_id);
                }
            }
        }
    }

    async fn activate_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        let channel = self
            .channels
            .channel(ChannelKey { peer, route, class })
            .await?;
        let old = {
            let mut state = self.state.write().await;
            if !self.routing.routes_to(peer).await.contains(&route) {
                drop(state);
                self.channels.drop_host(peer);
                return Err(ChannelError::NoRoute { host_id: peer });
            }
            match state.active.get(&peer) {
                Some(active @ Route::Direct(_)) if !route.is_direct() && *active != route => {
                    return Ok(channel);
                }
                _ => state.active.insert(peer, route),
            }
        };
        if let Some(old_route) = old
            && old_route != route
        {
            self.channels.drop_route(peer, old_route);
        }
        self.clear_reachability_error(peer).await;
        Ok(channel)
    }

    async fn remove_host_runtime_state(&self, peer: HostId) {
        self.state.write().await.active.remove(&peer);
        self.channels.drop_host(peer);
    }

    async fn cloud_relay_for(&self, peer: HostId) -> Result<HostId, ChannelError> {
        let registry = self.channels.link_registry();
        for relay in self.routing.relays_to(peer).await {
            if registry.has_cloud_relay_link_to(relay).await {
                return Ok(relay);
            }
        }
        Err(ChannelError::CloudPairingUnavailable)
    }

    pub(crate) fn routing(&self) -> &Arc<RoutingCore> {
        &self.routing
    }
    pub(crate) fn channels(&self) -> &Arc<ChannelPool> {
        &self.channels
    }

    pub async fn active_route(&self, peer: HostId) -> Option<Route> {
        self.state.read().await.active.get(&peer).copied()
    }

    pub async fn known_routes(&self, peer: HostId) -> Vec<Route> {
        self.routing.routes_to(peer).await
    }
}

fn host_is_cloud_relay(host: &Host) -> bool {
    host.capabilities
        .features
        .iter()
        .any(|feature| feature == FEATURE_CLOUD_RELAY)
}
