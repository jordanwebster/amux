use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

use crate::HostId;
use crate::protocol::wire::pb;
use crate::routing::{
    Link, RoutingCore, RoutingEvent, outbound_routing_message, should_send_routing_event_to_link,
};

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

#[derive(Clone)]
struct LinkWriter {
    peer_host_id: HostId,
    tx: LinkOutputTx,
    close_tx: mpsc::Sender<LinkCloseReason>,
    active: bool,
    draining: bool,
    pending_routing_events: VecDeque<RoutingEvent>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum LinkCloseReason {
    OutgoingQueueFull,
}

impl LinkRegistry {
    pub(crate) async fn register(
        &self,
        link: Link,
        peer_host_id: HostId,
        outgoing_tx: LinkOutputTx,
    ) -> mpsc::Receiver<LinkCloseReason> {
        let (close_tx, close_rx) = mpsc::channel(1);
        self.state.write().await.writers.insert(
            link,
            LinkWriter {
                peer_host_id,
                tx: outgoing_tx,
                close_tx,
                active: false,
                draining: false,
                pending_routing_events: VecDeque::new(),
            },
        );
        close_rx
    }

    pub(crate) async fn remove(&self, link: &Link) {
        self.state.write().await.writers.remove(link);
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

    pub(crate) async fn send_goaway_to_all(&self, reason: pb::GoAwayReason, drain_timeout_ms: u32) {
        let outgoing = {
            let state = self.state.read().await;
            state
                .writers
                .values()
                .map(|writer| writer.tx.clone())
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
            for link in &overflowed {
                state.writers.remove(link);
            }
            overflowed
        };
        for link in overflowed {
            tracing::warn!(%link, "closing routing link after full outgoing event queue");
        }
    }

    pub(crate) async fn activate(
        &self,
        link: &Link,
        snapshot_hosts: impl IntoIterator<Item = HostId>,
    ) -> bool {
        let mut known_hosts = snapshot_hosts.into_iter().collect::<HashSet<_>>();
        loop {
            let (tx, close_tx, pending) = {
                let mut state = self.state.write().await;
                let Some(writer) = state.writers.get_mut(link) else {
                    return false;
                };
                if writer.pending_routing_events.is_empty() {
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
                    RoutingEvent::HostUp { host, .. } if known_hosts.contains(&host.id) => {
                        continue;
                    }
                    RoutingEvent::HostUp { host, .. } => {
                        known_hosts.insert(host.id);
                    }
                    RoutingEvent::HostDown { host_id, .. } => {
                        if !known_hosts.remove(host_id) {
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
}
