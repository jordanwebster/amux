use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{RwLock, mpsc};
use tonic::transport::{Channel, Endpoint};

use crate::HostId;
use crate::protocol::wire as pb;
use crate::routing::{
    HostReachabilityEvent, Link, LinkRegistry, LinkRegistryError, Route, RoutingCore, route_to_wire,
};
use crate::transport::{channel_from_single_io, configure_tonic_endpoint_keepalive};
use crate::tunnel::transport::TunnelTransport;
use crate::tunnel::types::{TunnelId, TunnelTypeError};
use crate::tunnel::{Tunnel, create_tunnel};

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
    #[error(transparent)]
    InvalidTunnelId(#[from] TunnelTypeError),
    #[error("TunnelFrame target {target} does not match local host {local}")]
    TargetMismatch { target: HostId, local: HostId },
    #[error("incoming tunnel receiver is closed")]
    IncomingTunnelsClosed,
    #[error("target-side tunnel closed before payload delivery")]
    InboundClosed,
}

#[derive(Default)]
struct PoolState {
    tunnels: HashMap<TunnelId, Tunnel>,
    channels: HashMap<HostId, Channel>,
}

pub(crate) struct TunnelPool {
    my_host_id: HostId,
    routing: Arc<RoutingCore>,
    links: Arc<LinkRegistry>,
    incoming_tunnels_tx: mpsc::Sender<TunnelTransport>,
    state: RwLock<PoolState>,
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
        Self {
            my_host_id,
            routing,
            links,
            incoming_tunnels_tx,
            state: RwLock::new(PoolState::default()),
        }
    }

    pub(crate) fn link_registry(&self) -> Arc<LinkRegistry> {
        self.links.clone()
    }

    pub(crate) async fn channel_to(&self, peer: HostId) -> Result<Channel, TunnelPoolError> {
        let host_entry = self
            .routing
            .host_entry(peer)
            .await
            .ok_or(TunnelPoolError::NotFound { host_id: peer })?;
        let (first_hop, dst) = outgoing_route_parts(peer, &host_entry.route)?;

        let outgoing_tx = self.links.outgoing_tx(&first_hop).await?;

        let mut state = self.state.write().await;
        if let Some(channel) = state.channels.get(&peer).cloned() {
            return Ok(channel);
        }

        let id = TunnelId::new(self.my_host_id, peer);
        let (tunnel, transport) = create_tunnel(id, dst, peer, outgoing_tx);
        let channel = channel_from_transport(transport);

        state.tunnels.insert(id, tunnel);
        state.channels.insert(peer, channel.clone());
        Ok(channel)
    }

    pub(crate) async fn handle_inbound_frame(
        &self,
        mut frame: pb::TunnelFrame,
    ) -> Result<(), TunnelPoolError> {
        let id = frame
            .tunnel_id
            .clone()
            .ok_or(TunnelPoolError::MissingTunnelId)
            .map(TunnelId::try_from)??;
        let mut dst = route_from_wire(frame.dst.take())?;
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
            .map(Tunnel::inbound_sender)
        {
            return inbound_tx
                .send(Bytes::from(frame.payload))
                .await
                .map_err(|_| TunnelPoolError::InboundClosed);
        }

        if id.target != self.my_host_id {
            return Err(TunnelPoolError::TargetMismatch {
                target: id.target,
                local: self.my_host_id,
            });
        }

        let host_entry =
            self.routing
                .host_entry(id.initiator)
                .await
                .ok_or(TunnelPoolError::NotFound {
                    host_id: id.initiator,
                })?;
        let (first_hop, dst) = outgoing_route_parts(id.initiator, &host_entry.route)?;

        let outgoing_tx = self.links.existing_tx(&first_hop).await.ok_or_else(|| {
            TunnelPoolError::LinkUnavailable {
                link: first_hop.clone(),
            }
        })?;
        let (tunnel, transport) = create_tunnel(id, dst, id.initiator, outgoing_tx);
        let inbound_tx = tunnel.inbound_sender();
        let mut transport = Some(transport);

        let inbound_tx = {
            let mut state = self.state.write().await;
            if let Some(existing) = state.tunnels.get(&id) {
                transport = None;
                existing.inbound_sender()
            } else {
                state.tunnels.insert(id, tunnel);
                inbound_tx
            }
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

    pub(crate) async fn remove_host(&self, host_id: HostId) {
        let mut state = self.state.write().await;
        state.channels.remove(&host_id);
        state
            .tunnels
            .retain(|id, _| id.initiator != host_id && id.target != host_id);
    }

    pub(crate) async fn handle_host_event(&self, event: &HostReachabilityEvent) {
        if let HostReachabilityEvent::HostRemoved { host_id } = event {
            self.remove_host(*host_id).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn counts(&self) -> (usize, usize) {
        let state = self.state.read().await;
        (state.tunnels.len(), state.channels.len())
    }
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

fn channel_from_transport(transport: TunnelTransport) -> Channel {
    channel_from_single_io(
        configure_tonic_endpoint_keepalive(Endpoint::from_static("http://tunnel")),
        "TunnelTransport",
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
    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::routing::{Capabilities, Host, Route, RoutingEvent, SupportedAgentType};

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

    fn link(name: &str) -> Link {
        Link::new(name).unwrap()
    }

    fn route(name: &str) -> Route {
        Route::from_link(link(name))
    }

    async fn register_test_link(pool: &TunnelPool, name: &str, tx: mpsc::Sender<pb::Message>) {
        pool.link_registry()
            .register(link(name), HostId::from_u128(99), tx)
            .await;
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
        assert!(pool.link_registry().activate(&link_name, [host.id]).await);

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
    async fn channel_to_creates_and_caches_initiator_tunnel() {
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

        assert_eq!(pool.counts().await, (1, 1));
    }

    #[tokio::test]
    async fn channel_to_rechecks_route_before_returning_cached_channel() {
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
        assert_eq!(pool.counts().await, (1, 1));

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

        let id = TunnelId::new(initiator, target);
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
    async fn inbound_frame_delivers_to_existing_initiator_tunnel() {
        let routing = Arc::new(RoutingCore::new());
        let initiator = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(initiator, routing, incoming_tx);
        let (link_tx, _link_rx) = mpsc::channel(8);

        let id = TunnelId::new(initiator, target);
        let (tunnel, mut transport) = create_tunnel(id, route("relay"), target, link_tx);
        pool.state.write().await.tunnels.insert(id, tunnel);

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

        let id = TunnelId::new(initiator, target);
        let (tunnel, mut transport) = create_tunnel(id, route("relay"), target, mpsc::channel(8).0);
        pool.state.write().await.tunnels.insert(id, tunnel);
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

        let id = TunnelId::new(initiator, target);
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

        let id = TunnelId::new(HostId::from_u128(10), HostId::from_u128(20));
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
        let target = HostId::from_u128(20);
        routing
            .apply_host_up(host(10, "initiator"), route("from-initiator"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(30), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(4);
        register_test_link(&pool, "next", link_tx).await;

        let id = TunnelId::new(initiator, target);
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
        let target = HostId::from_u128(20);
        routing
            .apply_host_up(host(10, "initiator"), route("origin"), None)
            .await;

        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(30), routing, incoming_tx);
        let (link_tx, mut link_rx) = mpsc::channel(2);
        register_test_link(&pool, "origin", link_tx).await;

        let id = TunnelId::new(initiator, target);
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

        let id = TunnelId::new(HostId::from_u128(10), HostId::from_u128(20));
        pool.handle_inbound_frame(routed_frame(&["missing"], id, b"payload"))
            .await
            .unwrap();
        assert_eq!(pool.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn inbound_frame_requires_destination_and_tunnel_id() {
        let routing = Arc::new(RoutingCore::new());
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(HostId::from_u128(1), routing, incoming_tx);
        let id = TunnelId::new(HostId::from_u128(10), HostId::from_u128(20));

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
    async fn endpoint_frame_for_other_target_is_protocol_violation() {
        let routing = Arc::new(RoutingCore::new());
        let local = HostId::from_u128(1);
        let target = HostId::from_u128(2);
        let (incoming_tx, _incoming_rx) = mpsc::channel(1);
        let pool = TunnelPool::new(local, routing, incoming_tx);

        let error = pool
            .handle_inbound_frame(frame(
                TunnelId::new(HostId::from_u128(3), target),
                b"payload",
            ))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            TunnelPoolError::TargetMismatch {
                target: observed,
                local: observed_local
            } if observed == target && observed_local == local
        ));
    }

    #[tokio::test]
    async fn remove_host_drops_related_tunnels_and_channels() {
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
        assert_eq!(pool.counts().await, (1, 1));

        pool.remove_host(peer).await;
        assert_eq!(pool.counts().await, (0, 0));
    }

    #[tokio::test]
    async fn host_removed_event_drops_related_tunnels_and_channels() {
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
        assert_eq!(pool.counts().await, (1, 1));

        pool.handle_host_event(&HostReachabilityEvent::HostAdded {
            host: host(3, "other"),
        })
        .await;
        assert_eq!(pool.counts().await, (1, 1));

        pool.handle_host_event(&HostReachabilityEvent::HostRemoved { host_id: peer })
            .await;
        assert_eq!(pool.counts().await, (0, 0));
    }
}
