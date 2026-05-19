use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::{Notify, RwLock, mpsc};
use tokio::task::JoinHandle;

use crate::protocol::wire::pb;
use crate::routing::{
    Link, Route, RoutingCore, RoutingEvent, outbound_routing_message,
    should_send_routing_event_to_link,
};
use crate::{HostId, audit};

pub(crate) type LinkOutputTx = mpsc::Sender<pb::Message>;

const PENDING_ROUTING_EVENT_LIMIT: usize = 256;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LinkRegistryError {
    #[error("route first hop {link} has no outgoing writer")]
    Unavailable { link: Link },
    #[error("route first hop {link} is draining")]
    Draining { link: Link },
}

#[derive(Default)]
pub(crate) struct LinkRegistry {
    state: RwLock<LinkRegistryState>,
}

#[derive(Default)]
struct LinkRegistryState {
    writers: HashMap<Link, LinkWriter>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum LinkRole {
    Peer,
    CloudRelay,
}

#[derive(Clone)]
struct LinkWriter {
    peer_host_id: HostId,
    tx: LinkOutputTx,
    close_tx: mpsc::Sender<LinkCloseReason>,
    closed: Arc<Notify>,
    role: LinkRole,
    active: bool,
    draining: bool,
    pending_routing_events: VecDeque<RoutingEvent>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum LinkCloseReason {
    OutgoingQueueFull,
    TrustReplaced,
}

impl LinkRegistry {
    #[cfg(test)]
    pub(crate) async fn register(
        &self,
        link: Link,
        peer_host_id: HostId,
        outgoing_tx: LinkOutputTx,
    ) -> mpsc::Receiver<LinkCloseReason> {
        self.register_with_role(link, peer_host_id, outgoing_tx, LinkRole::Peer)
            .await
    }

    pub(crate) async fn register_with_role(
        &self,
        link: Link,
        peer_host_id: HostId,
        outgoing_tx: LinkOutputTx,
        role: LinkRole,
    ) -> mpsc::Receiver<LinkCloseReason> {
        let (close_tx, close_rx) = mpsc::channel(1);
        let closed = Arc::new(Notify::new());
        let audit_link = link.clone();
        let old = self.state.write().await.writers.insert(
            link,
            LinkWriter {
                peer_host_id,
                tx: outgoing_tx,
                close_tx,
                closed,
                role,
                active: false,
                draining: false,
                pending_routing_events: VecDeque::new(),
            },
        );
        if let Some(old) = old {
            old.closed.notify_waiters();
            if old.active {
                audit::link_down(old.peer_host_id, &audit_link, "replaced");
            }
        }
        close_rx
    }

    pub(crate) async fn remove(&self, link: &Link) {
        if let Some(writer) = self.state.write().await.writers.remove(link) {
            writer.closed.notify_waiters();
            if writer.active {
                audit::link_down(writer.peer_host_id, link, "removed");
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn close_host(&self, host_id: HostId) -> Vec<Link> {
        let closing = {
            let mut state = self.state.write().await;
            state
                .writers
                .iter_mut()
                .filter_map(|(link, writer)| {
                    if writer.peer_host_id == host_id {
                        writer.draining = true;
                        request_link_close(writer, LinkCloseReason::TrustReplaced);
                        Some((link.clone(), writer.closed.clone()))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        for (link, closed) in &closing {
            loop {
                let notified = closed.notified();
                if !self.state.read().await.writers.contains_key(link) {
                    break;
                }
                notified.await;
            }
        }
        closing.into_iter().map(|(link, _)| link).collect()
    }

    pub(crate) async fn mark_draining(&self, link: &Link) -> bool {
        let mut state = self.state.write().await;
        let Some(writer) = state.writers.get_mut(link) else {
            return false;
        };
        writer.draining = true;
        true
    }

    pub(crate) async fn outgoing_tx(&self, link: &Link) -> Result<LinkOutputTx, LinkRegistryError> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| {
                if writer.draining {
                    Err(LinkRegistryError::Draining { link: link.clone() })
                } else {
                    Ok(writer.tx.clone())
                }
            })
            .unwrap_or_else(|| Err(LinkRegistryError::Unavailable { link: link.clone() }))
    }

    pub(crate) async fn existing_tx(&self, link: &Link) -> Option<LinkOutputTx> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.tx.clone())
    }

    pub(crate) async fn is_cloud_relay(&self, link: &Link) -> bool {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .is_some_and(|writer| writer.role == LinkRole::CloudRelay)
    }

    pub(crate) async fn send_goaway_to_all(&self, reason: pb::GoAwayReason, drain_timeout_ms: u32) {
        let outgoing = {
            let mut state = self.state.write().await;
            state
                .writers
                .values_mut()
                .map(|writer| {
                    writer.draining = true;
                    writer.tx.clone()
                })
                .collect::<Vec<_>>()
        };
        let message = pb::Message {
            body: Some(pb::message::Body::Goaway(pb::GoAway {
                reason: reason as i32,
                error: None,
                drain_timeout_ms,
            })),
        };
        for outgoing_tx in outgoing {
            try_send_or_spawn(outgoing_tx, message.clone());
        }
    }

    pub(crate) async fn send_goaway_to_host(
        &self,
        host_id: HostId,
        reason: pb::GoAwayReason,
        drain_timeout_ms: u32,
    ) {
        let outgoing = {
            let mut state = self.state.write().await;
            state
                .writers
                .values_mut()
                .filter_map(|writer| {
                    if writer.peer_host_id == host_id {
                        writer.draining = true;
                        Some(writer.tx.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let message = pb::Message {
            body: Some(pb::message::Body::Goaway(pb::GoAway {
                reason: reason as i32,
                error: None,
                drain_timeout_ms,
            })),
        };
        for outgoing_tx in outgoing {
            try_send_or_spawn(outgoing_tx, message.clone());
        }
    }

    #[cfg(test)]
    pub(crate) async fn outgoing_writers(&self) -> Vec<LinkOutputTx> {
        self.state
            .read()
            .await
            .writers
            .values()
            .map(|writer| writer.tx.clone())
            .collect()
    }

    pub(crate) async fn broadcast_routing_event(&self, event: &RoutingEvent) {
        let message = outbound_routing_message(event);
        let overflowed = {
            let mut state = self.state.write().await;
            let mut overflowed = Vec::new();
            for (link, writer) in &mut state.writers {
                if writer.draining {
                    continue;
                }
                if !should_send_routing_event_to_link(event, link, Some(writer.peer_host_id)) {
                    continue;
                }
                if writer.active {
                    match writer.tx.try_send(message.clone()) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            request_link_close(writer, LinkCloseReason::OutgoingQueueFull);
                            overflowed.push(link.clone());
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            overflowed.push(link.clone());
                        }
                    }
                } else {
                    if writer.pending_routing_events.len() >= PENDING_ROUTING_EVENT_LIMIT {
                        request_link_close(writer, LinkCloseReason::OutgoingQueueFull);
                        overflowed.push(link.clone());
                    } else {
                        writer.pending_routing_events.push_back(event.clone());
                    }
                }
            }
            let mut removed = Vec::new();
            for link in &overflowed {
                if let Some(writer) = state.writers.remove(link) {
                    writer.closed.notify_waiters();
                    if writer.active {
                        removed.push((link.clone(), writer.peer_host_id));
                    }
                }
            }
            removed
        };
        for (link, host_id) in overflowed {
            audit::link_down(host_id, &link, "outgoing routing event queue full");
            tracing::warn!(%link, "closing routing link after full outgoing event queue");
        }
    }

    pub(crate) async fn activate(
        &self,
        link: &Link,
        snapshot_routes: impl IntoIterator<Item = (HostId, Route)>,
    ) -> bool {
        let mut known_routes = snapshot_routes.into_iter().collect::<HashSet<_>>();
        loop {
            let (tx, close_tx, pending) = {
                let mut state = self.state.write().await;
                let Some(writer) = state.writers.get_mut(link) else {
                    return false;
                };
                if writer.pending_routing_events.is_empty() {
                    if !writer.active {
                        audit::link_up(writer.peer_host_id, link, writer.role);
                    }
                    writer.active = true;
                    return true;
                }
                (
                    writer.tx.clone(),
                    writer.close_tx.clone(),
                    writer.pending_routing_events.drain(..).collect::<Vec<_>>(),
                )
            };

            for event in pending {
                match &event {
                    RoutingEvent::HostUp { host, route, .. } => {
                        if !known_routes.insert((host.id, route.clone())) {
                            continue;
                        }
                    }
                    RoutingEvent::HostDown { host_id, route, .. } => {
                        if !known_routes.remove(&(*host_id, route.clone())) {
                            continue;
                        }
                    }
                }
                match tx.try_send(outbound_routing_message(&event)) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        let _ = close_tx.try_send(LinkCloseReason::OutgoingQueueFull);
                        return false;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => return false,
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn is_draining(&self, link: &Link) -> bool {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .is_some_and(|writer| writer.draining)
    }
}

fn request_link_close(writer: &LinkWriter, reason: LinkCloseReason) {
    let _ = writer.close_tx.try_send(reason);
}

fn try_send_or_spawn(tx: LinkOutputTx, message: pb::Message) {
    match tx.try_send(message) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(message)) => {
            tokio::spawn(async move {
                let _ = tx.send(message).await;
            });
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

pub(crate) async fn spawn_routing_event_fanout(
    routing: Arc<RoutingCore>,
    links: Arc<LinkRegistry>,
) -> JoinHandle<()> {
    let mut rx = routing.subscribe_routing_events().await;
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            links.broadcast_routing_event(&event).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::routing::{Capabilities, Host, Route};

    fn host(id: u128) -> Host {
        Host {
            id: Uuid::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities {
                features: Vec::new(),
                supported_agent_types: Vec::new(),
            },
        }
    }

    fn route(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    async fn recv_routing_event(rx: &mut mpsc::Receiver<pb::Message>) -> pb::RoutingEvent {
        let message = rx.recv().await.unwrap();
        let Some(pb::message::Body::RoutingEvent(event)) = message.body else {
            panic!("expected routing event");
        };
        event
    }

    #[tokio::test]
    async fn goaway_notification_is_best_effort_for_full_link_queues() {
        let registry = LinkRegistry::default();
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(pb::Message { body: None }).unwrap();
        registry
            .register(Link::new("full").unwrap(), Uuid::new_v4(), tx)
            .await;

        tokio::time::timeout(
            Duration::from_millis(50),
            registry.send_goaway_to_all(pb::GoAwayReason::UserShutdown, 200),
        )
        .await
        .expect("goaway send must not wait on a full queue");
    }

    #[tokio::test]
    async fn send_goaway_marks_links_draining_before_notifying() {
        let registry = LinkRegistry::default();
        let link = Link::new("peer").unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        registry
            .register(link.clone(), Uuid::from_u128(99), tx)
            .await;

        registry
            .send_goaway_to_all(pb::GoAwayReason::UserShutdown, 200)
            .await;

        assert!(registry.is_draining(&link).await);
        assert!(matches!(
            registry.outgoing_tx(&link).await,
            Err(LinkRegistryError::Draining { link: observed }) if observed == link
        ));
        let Some(pb::Message {
            body: Some(pb::message::Body::Goaway(goaway)),
        }) = rx.recv().await
        else {
            panic!("expected GoAway message");
        };
        assert_eq!(goaway.reason, pb::GoAwayReason::UserShutdown as i32);
        assert_eq!(goaway.drain_timeout_ms, 200);
    }

    #[tokio::test]
    async fn register_with_role_marks_cloud_relay_links() {
        let registry = LinkRegistry::default();
        let cloud = Link::new("cloud").unwrap();
        let peer = Link::new("peer").unwrap();
        let (cloud_tx, _cloud_rx) = mpsc::channel(1);
        let (peer_tx, _peer_rx) = mpsc::channel(1);

        registry
            .register_with_role(
                cloud.clone(),
                Uuid::from_u128(1),
                cloud_tx,
                LinkRole::CloudRelay,
            )
            .await;
        registry
            .register(peer.clone(), Uuid::from_u128(2), peer_tx)
            .await;

        assert!(registry.is_cloud_relay(&cloud).await);
        assert!(!registry.is_cloud_relay(&peer).await);
    }

    #[tokio::test]
    async fn routing_event_overflow_requests_link_close_and_removes_writer() {
        let registry = LinkRegistry::default();
        let link = Link::new("full").unwrap();
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(pb::Message { body: None }).unwrap();
        let mut close_rx = registry.register(link.clone(), Uuid::new_v4(), tx).await;
        assert!(registry.activate(&link, []).await);

        registry
            .broadcast_routing_event(&RoutingEvent::HostUp {
                host: Host {
                    id: Uuid::from_u128(2),
                    name: "remote".to_string(),
                    version: "test".to_string(),
                    capabilities: Capabilities {
                        features: Vec::new(),
                        supported_agent_types: Vec::new(),
                    },
                },
                route: Route::from_link(link.clone()),
                origin_link: None,
            })
            .await;

        assert_eq!(
            close_rx.recv().await,
            Some(LinkCloseReason::OutgoingQueueFull)
        );
        assert!(matches!(
            registry.outgoing_tx(&link).await,
            Err(LinkRegistryError::Unavailable { .. })
        ));
    }

    #[tokio::test]
    async fn activate_flushes_pending_host_up_for_distinct_route() {
        let registry = LinkRegistry::default();
        let link = Link::new("peer").unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        registry
            .register(link.clone(), Uuid::from_u128(99), tx)
            .await;
        let host = host(2);
        let snapshot_route = route(&["a"]);
        let pending_route = route(&["b"]);

        registry
            .broadcast_routing_event(&RoutingEvent::HostUp {
                host: host.clone(),
                route: pending_route.clone(),
                origin_link: None,
            })
            .await;

        assert!(registry.activate(&link, [(host.id, snapshot_route)]).await);
        let event = recv_routing_event(&mut rx).await;
        assert!(matches!(
            event.event,
            Some(pb::routing_event::Event::HostUp(up))
                if up.host.as_ref().is_some_and(|host| host.host_id == Uuid::from_u128(2).as_bytes())
                    && up.route.as_ref().is_some_and(|route| route.links == ["b"])
        ));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn activate_flushes_route_specific_host_downs() {
        let registry = LinkRegistry::default();
        let link = Link::new("peer").unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        registry
            .register(link.clone(), Uuid::from_u128(99), tx)
            .await;
        let host = host(2);
        let first = route(&["a"]);
        let second = route(&["b"]);

        for route in [first.clone(), second.clone()] {
            registry
                .broadcast_routing_event(&RoutingEvent::HostDown {
                    host_id: host.id,
                    route,
                    origin_link: None,
                })
                .await;
        }

        assert!(
            registry
                .activate(&link, [(host.id, first), (host.id, second)])
                .await
        );
        let first_event = recv_routing_event(&mut rx).await;
        let second_event = recv_routing_event(&mut rx).await;
        let routes = [first_event, second_event]
            .into_iter()
            .map(|event| match event.event {
                Some(pb::routing_event::Event::HostDown(down)) => {
                    down.route.unwrap().links.join(".")
                }
                _ => panic!("expected HostDown"),
            })
            .collect::<Vec<_>>();
        assert_eq!(routes, ["a", "b"]);
        assert!(rx.try_recv().is_err());
    }
}
