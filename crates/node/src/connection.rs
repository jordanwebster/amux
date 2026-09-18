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
    /// What each host was last described as, so a description is sent when
    /// the answer has moved and not merely when something happened.
    announced: HashMap<HostId, HostDescription>,
}

/// What a client would be told about a host: the route carrying its traffic,
/// how that reads, and the account-binding fact recorded for it.
///
/// The route's identity belongs here beside the way it reads, because one
/// direct link replacing another reads the same both times and still ends
/// every stream on the carrier that went away.
type HostDescription = (Option<Route>, HostVia, Option<bool>);

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
        match self.activate_route(peer, route, class).await {
            // When both hosts dial each other, the preferred link supersedes
            // the other and closes it. A call that chose the losing link just
            // before that fails while opening its stream, though routing by
            // then names the link that replaced it: try that one, once.
            Err(error @ ChannelError::LinkUnavailable { .. }) if route.is_direct() => {
                match self.routing.route_to(peer).await {
                    Some(next) if next != route => self.activate_route(peer, next, class).await,
                    _ => Err(error),
                }
            }
            result => result,
        }
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

    /// Applies one routing event, then describes the host again if what a
    /// client would be told about it has moved.
    ///
    /// The routing table knows where a host can be reached; this manager knows
    /// which of those routes is carrying traffic, and it is the second that a
    /// client is shown and that its inventory subscription rides on.
    ///
    /// The comparison is against what was last announced rather than against
    /// the moment before this event, because the active route is not only
    /// settled here: an outgoing call opens a channel on whatever the routing
    /// table prefers, so a direct link can already be carrying traffic by the
    /// time its own event arrives. Read as a difference across the handler,
    /// that would look like nothing happening; read against what a client was
    /// last told, it is exactly the change the client is waiting for.
    async fn handle_event(&self, event: RoutingEvent) {
        let host_id = event.host_id();
        // A host nobody has described yet was just announced as present by the
        // routing table, and that announcement carried whatever was true as
        // this event was queued. That is the client's starting point, so it is
        // this one's: a first link that changes nothing after it is applied
        // leaves the host correctly described and says nothing further.
        let known = self.state.read().await.announced.get(&host_id).copied();
        let baseline = match known {
            Some(description) => description,
            None => self.host_description(host_id).await,
        };
        self.apply_event(event).await;
        let description = self.host_description(host_id).await;
        self.state
            .write()
            .await
            .announced
            .insert(host_id, description);
        if description != baseline {
            self.routing.announce_route(host_id).await;
        }
    }

    async fn host_description(&self, host_id: HostId) -> HostDescription {
        let active = self.state.read().await.active.get(&host_id).copied();
        (
            active,
            self.via_for(host_id).await,
            self.routing.signed_in_for(host_id),
        )
    }

    async fn apply_event(&self, event: RoutingEvent) {
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
        let mut state = self.state.write().await;
        state.active.remove(&peer);
        // A host that is gone is described afresh if it ever comes back.
        state.announced.remove(&peer);
        drop(state);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures_util::future::{self, BoxFuture};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::link::{ByteStream, CarrierKind, ControlSink, ControlSource, OpenError};
    use crate::routing::{
        Capabilities, LinkAdmission, LinkCarrier as RoutingCarrier, LinkId, LinkProperties,
        LinkRegistry, LinkRole,
    };

    fn peer() -> Host {
        Host {
            platform: None,
            id: Uuid::from_u128(2),
            name: "peer".to_string(),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: Vec::new(),
            },
            signed_in: Some(true),
        }
    }

    /// A link that is superseded while a stream is being opened on it: the
    /// replacement joins routing, this link leaves it, and the open fails the
    /// way a closed QUIC connection fails it.
    struct SupersededDuringOpen {
        routing: Arc<RoutingCore>,
        superseded: LinkId,
        replacement: LinkId,
    }

    /// A link that answers every open with a refusal, recording that it was
    /// asked.
    #[derive(Default)]
    struct Refusing {
        asked: AtomicBool,
    }

    impl crate::link::LinkCarrier for SupersededDuringOpen {
        fn kind(&self) -> CarrierKind {
            CarrierKind::Quic
        }

        fn control(&self) -> (ControlSink, ControlSource) {
            panic!("the test drives no control loop")
        }

        fn open_stream(
            &self,
            _preface: wire::pb::StreamPreface,
        ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
            Box::pin(async move {
                self.routing.apply_direct_up(peer(), self.replacement).await;
                self.routing.apply_direct_down(self.superseded).await;
                Err(OpenError::LinkClosed)
            })
        }

        fn accept_stream(&self) -> BoxFuture<'_, Option<(wire::pb::StreamPreface, ByteStream)>> {
            Box::pin(future::pending())
        }

        fn close(&self, _reason: wire::pb::LinkCloseReason) {}

        fn closed(&self) -> BoxFuture<'_, wire::pb::LinkCloseReason> {
            Box::pin(future::pending())
        }
    }

    impl crate::link::LinkCarrier for Refusing {
        fn kind(&self) -> CarrierKind {
            CarrierKind::Quic
        }

        fn control(&self) -> (ControlSink, ControlSource) {
            panic!("the test drives no control loop")
        }

        fn open_stream(
            &self,
            _preface: wire::pb::StreamPreface,
        ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
            self.asked.store(true, Ordering::SeqCst);
            Box::pin(async { Err(OpenError::Refused(wire::pb::StreamRefusal::ShuttingDown)) })
        }

        fn accept_stream(&self) -> BoxFuture<'_, Option<(wire::pb::StreamPreface, ByteStream)>> {
            Box::pin(future::pending())
        }

        fn close(&self, _reason: wire::pb::LinkCloseReason) {}

        fn closed(&self) -> BoxFuture<'_, wire::pb::LinkCloseReason> {
            Box::pin(future::pending())
        }
    }

    async fn register(
        links: &LinkRegistry,
        link: LinkId,
        carrier: Arc<dyn crate::link::LinkCarrier>,
    ) -> mpsc::Receiver<crate::routing::LinkCloseRequest> {
        let (tx, _rx) = mpsc::channel(8);
        links
            .register_with_details(
                link,
                peer(),
                tx,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission: LinkAdmission::PinnedKey,
                    carrier: RoutingCarrier::Direct,
                    incarnation: crate::routing::Incarnation::random(),
                    direct_order: None,
                },
                &[],
                Some(carrier),
            )
            .await
            .expect("a lone link registers")
    }

    #[tokio::test]
    async fn a_call_on_a_link_superseded_while_opening_retries_on_its_replacement() {
        let links = Arc::new(LinkRegistry::default());
        let routing = Arc::new(RoutingCore::new());
        let manager =
            ConnectionManager::new(routing.clone(), Arc::new(ChannelPool::new(links.clone())));
        let superseded = LinkId::new(peer().id);
        let replacement = LinkId::new(peer().id);
        let refusing = Arc::new(Refusing::default());
        let _superseded_close = register(
            &links,
            superseded,
            Arc::new(SupersededDuringOpen {
                routing: routing.clone(),
                superseded,
                replacement,
            }),
        )
        .await;
        let _replacement_close = register(&links, replacement, refusing.clone()).await;
        routing.apply_direct_up(peer(), superseded).await;

        let error = manager.channel_to(peer().id).await.unwrap_err();

        assert!(
            refusing.asked.load(Ordering::SeqCst),
            "the replacement link was never tried"
        );
        assert!(
            matches!(
                error,
                ChannelError::Refused(wire::pb::StreamRefusal::ShuttingDown)
            ),
            "the call ended on the superseded link: {error}"
        );
    }
}
