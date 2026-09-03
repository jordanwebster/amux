//! The link registry: this daemon's live links and the wire-side adjacency
//! discipline.
//!
//! The registry is the single source of truth for *wire* adjacency. Every
//! `NeighborUp`/`NeighborDown` a peer ever receives from us is emitted here,
//! under one lock, in registration order — so "advertise only adjacency" is
//! structural: there is no API for broadcasting anything else. The handshake
//! snapshot is reconciled here too: registering a link diffs the neighbor
//! set the handshake advertised against the current one and sends the
//! difference down the new link, atomically with registration, which closes
//! the window between composing the snapshot and the link going live.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{Notify, RwLock, mpsc};

use crate::protocol::wire::pb;
use crate::routing::types::{Host, LinkId};
use crate::routing::wire::{neighbor_down_message, neighbor_up_message};
use crate::{HostId, audit};

pub(crate) type LinkOutputTx = mpsc::Sender<pb::Message>;

#[derive(Debug, thiserror::Error)]
#[error("no live link to host {host_id}")]
pub(crate) struct LinkUnavailable {
    pub(crate) host_id: HostId,
}

#[derive(Default)]
pub(crate) struct LinkRegistry {
    state: RwLock<LinkRegistryState>,
}

#[derive(Default)]
struct LinkRegistryState {
    writers: HashMap<LinkId, LinkWriter>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkRole {
    Peer,
    CloudRelay,
}

#[derive(Clone)]
struct LinkWriter {
    host: Host,
    tx: LinkOutputTx,
    close_tx: mpsc::Sender<LinkCloseRequest>,
    closed: Arc<Notify>,
    role: LinkRole,
}

/// A local request for the link's connect task to close the link. Distinct
/// from the wire `pb::LinkCloseReason`, which names why a *peer* closed it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum LinkCloseRequest {
    OutgoingQueueFull,
    TrustReplaced,
}

impl LinkRegistry {
    /// Registers a live link and runs the adjacency discipline atomically:
    /// other links learn `NeighborUp(peer)` if this is the first link to the
    /// peer, and this link receives the diff between `advertised_snapshot`
    /// (the neighbor set its handshake carried) and the current one.
    pub(crate) async fn register(
        &self,
        link: LinkId,
        host: Host,
        outgoing_tx: LinkOutputTx,
        role: LinkRole,
        advertised_snapshot: &[HostId],
    ) -> mpsc::Receiver<LinkCloseRequest> {
        let (close_tx, close_rx) = mpsc::channel(1);
        let closed = Arc::new(Notify::new());
        let mut state = self.state.write().await;
        let first_link_to_peer = !state
            .writers
            .values()
            .any(|writer| writer.host.id == host.id);

        // Reconcile the new link's view: the handshake snapshot plus this
        // diff equals the registry's neighbor set at this instant.
        let advertised: HashSet<HostId> = advertised_snapshot.iter().copied().collect();
        let mut current: HashMap<HostId, Host> = HashMap::new();
        for writer in state.writers.values() {
            current.entry(writer.host.id).or_insert(writer.host.clone());
        }
        for (peer, peer_host) in &current {
            if *peer != host.id && !advertised.contains(peer) {
                try_send_or_request_close(&outgoing_tx, &close_tx, neighbor_up_message(peer_host));
            }
        }
        for advertised_peer in advertised {
            if advertised_peer != host.id && !current.contains_key(&advertised_peer) {
                try_send_or_request_close(
                    &outgoing_tx,
                    &close_tx,
                    neighbor_down_message(advertised_peer),
                );
            }
        }

        if first_link_to_peer {
            let message = neighbor_up_message(&host);
            fanout(&state, message, host.id);
        }

        let old = state.writers.insert(
            link,
            LinkWriter {
                host: host.clone(),
                tx: outgoing_tx,
                close_tx,
                closed,
                role,
            },
        );
        drop(state);
        if let Some(old) = old {
            old.closed.notify_waiters();
            audit::link_down(old.host.id, &link, "replaced");
        }
        audit::link_up(host.id, &link, role);
        close_rx
    }

    /// Removes a link; if it was the last link to its peer, other links
    /// learn `NeighborDown(peer)`.
    pub(crate) async fn remove(&self, link: &LinkId) {
        let mut state = self.state.write().await;
        let Some(writer) = state.writers.remove(link) else {
            return;
        };
        let peer = writer.host.id;
        let last_link_to_peer = !state.writers.values().any(|other| other.host.id == peer);
        if last_link_to_peer {
            let message = neighbor_down_message(peer);
            fanout(&state, message, peer);
        }
        drop(state);
        writer.closed.notify_waiters();
        audit::link_down(peer, link, "removed");
    }

    /// The current neighbor set, as a handshake snapshot.
    pub(crate) async fn neighbor_snapshot(&self) -> Vec<Host> {
        let state = self.state.read().await;
        let mut seen = HashSet::new();
        let mut snapshot = Vec::new();
        for writer in state.writers.values() {
            if seen.insert(writer.host.id) {
                snapshot.push(writer.host.clone());
            }
        }
        snapshot.sort_unstable_by_key(|host| host.id);
        snapshot
    }

    /// Requests closure of every link to `host_id` and waits until they are
    /// gone from the registry.
    pub(crate) async fn close_host(&self, host_id: HostId) -> Vec<LinkId> {
        let closing = {
            let state = self.state.read().await;
            state
                .writers
                .iter()
                .filter(|(_, writer)| writer.host.id == host_id)
                .map(|(link, writer)| {
                    let _ = writer.close_tx.try_send(LinkCloseRequest::TrustReplaced);
                    (*link, writer.closed.clone())
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

    /// The writer for one specific link.
    pub(crate) async fn outgoing_tx(&self, link: &LinkId) -> Result<LinkOutputTx, LinkUnavailable> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.tx.clone())
            .ok_or(LinkUnavailable {
                host_id: link.peer(),
            })
    }

    pub(crate) async fn has_link(&self, link: &LinkId) -> bool {
        self.state.read().await.writers.contains_key(link)
    }

    /// A writer for any live link to `peer` — the forwarding rule's "do I
    /// have a direct link to dst" lookup.
    pub(crate) async fn link_to_peer(&self, peer: HostId) -> Option<(LinkId, LinkOutputTx)> {
        self.state
            .read()
            .await
            .writers
            .iter()
            .find(|(_, writer)| writer.host.id == peer)
            .map(|(link, writer)| (*link, writer.tx.clone()))
    }

    /// Rule 2's action: enqueue `message` on any live link to `peer`,
    /// applying the full-queue close policy — forwarding never awaits a
    /// congested link (a slow peer must not stall the origin link's
    /// inbound processing). Returns false when no direct link to `peer`
    /// exists.
    pub(crate) async fn forward_to_peer(&self, peer: HostId, message: pb::Message) -> bool {
        let state = self.state.read().await;
        let Some(writer) = state.writers.values().find(|writer| writer.host.id == peer) else {
            return false;
        };
        try_send_or_request_close(&writer.tx, &writer.close_tx, message);
        true
    }

    /// Best-effort enqueue on one specific link (proactive `TunnelClose`):
    /// never awaits a full queue inline and never escalates — a close the
    /// peer misses is recovered by the unknown-id drop rule or link death.
    pub(crate) async fn send_best_effort(&self, link: &LinkId, message: pb::Message) {
        let outgoing = self
            .state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.tx.clone());
        if let Some(outgoing_tx) = outgoing {
            try_send_or_spawn(outgoing_tx, message);
        }
    }

    pub(crate) async fn is_cloud_relay(&self, link: &LinkId) -> bool {
        self.link_role(link)
            .await
            .is_some_and(|role| role == LinkRole::CloudRelay)
    }

    pub(crate) async fn link_role(&self, link: &LinkId) -> Option<LinkRole> {
        self.state
            .read()
            .await
            .writers
            .get(link)
            .map(|writer| writer.role)
    }

    /// Whether any live link to `peer` is the authenticated cloud link.
    /// Pairing route selection keys on the link role, never on the peer's
    /// self-asserted capabilities.
    pub(crate) async fn has_cloud_relay_link_to(&self, peer: HostId) -> bool {
        self.state
            .read()
            .await
            .writers
            .values()
            .any(|writer| writer.host.id == peer && writer.role == LinkRole::CloudRelay)
    }

    pub(crate) async fn send_link_close_to_all(&self, reason: pb::LinkCloseReason) {
        let outgoing = {
            let state = self.state.read().await;
            state
                .writers
                .values()
                .map(|writer| writer.tx.clone())
                .collect::<Vec<_>>()
        };
        let message = link_close_message(reason);
        for outgoing_tx in outgoing {
            try_send_or_spawn(outgoing_tx, message.clone());
        }
    }

    pub(crate) async fn send_link_close_to_host(
        &self,
        host_id: HostId,
        reason: pb::LinkCloseReason,
    ) {
        let outgoing = {
            let state = self.state.read().await;
            state
                .writers
                .values()
                .filter(|writer| writer.host.id == host_id)
                .map(|writer| writer.tx.clone())
                .collect::<Vec<_>>()
        };
        let message = link_close_message(reason);
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
}

/// Enqueues `message` to every writer except links to `skip_peer` (a host is
/// never told about itself). A full queue requests the link's closure; the
/// connect task then removes it through the normal path.
fn fanout(state: &LinkRegistryState, message: pb::Message, skip_peer: HostId) {
    for writer in state.writers.values() {
        if writer.host.id == skip_peer {
            continue;
        }
        try_send_or_request_close(&writer.tx, &writer.close_tx, message.clone());
    }
}

fn try_send_or_request_close(
    tx: &LinkOutputTx,
    close_tx: &mpsc::Sender<LinkCloseRequest>,
    message: pb::Message,
) {
    match tx.try_send(message) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("link outgoing queue full; requesting link close");
            let _ = close_tx.try_send(LinkCloseRequest::OutgoingQueueFull);
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {}
    }
}

fn link_close_message(reason: pb::LinkCloseReason) -> pb::Message {
    pb::Message {
        body: Some(pb::message::Body::LinkClose(pb::LinkClose {
            reason: reason as i32,
            error: None,
        })),
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use uuid::Uuid;

    use super::*;
    use crate::routing::Capabilities;

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

    fn link(peer: u128, instance: u128) -> LinkId {
        LinkId {
            peer: Uuid::from_u128(peer),
            instance: Uuid::from_u128(instance),
        }
    }

    async fn recv_message(rx: &mut mpsc::Receiver<pb::Message>) -> pb::Message {
        tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for a registry message")
            .expect("registry writer channel closed")
    }

    fn neighbor_up_host_id(message: &pb::Message) -> Vec<u8> {
        let Some(pb::message::Body::NeighborUp(up)) = &message.body else {
            panic!("expected NeighborUp, got {message:?}");
        };
        up.host.as_ref().expect("NeighborUp.host").host_id.clone()
    }

    fn neighbor_down_host_id(message: &pb::Message) -> Vec<u8> {
        let Some(pb::message::Body::NeighborDown(down)) = &message.body else {
            panic!("expected NeighborDown, got {message:?}");
        };
        down.host_id.clone()
    }

    #[tokio::test]
    async fn registering_a_link_announces_the_new_neighbor_to_existing_links() {
        let registry = LinkRegistry::default();
        let (first_tx, mut first_rx) = mpsc::channel(8);
        registry
            .register(link(1, 1), host(1), first_tx, LinkRole::Peer, &[])
            .await;

        let (second_tx, _second_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), second_tx, LinkRole::Peer, &[])
            .await;

        assert_eq!(
            neighbor_up_host_id(&recv_message(&mut first_rx).await),
            Uuid::from_u128(2).as_bytes()
        );
        assert!(first_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn registering_a_link_reconciles_its_handshake_snapshot() {
        let registry = LinkRegistry::default();
        let (existing_tx, _existing_rx) = mpsc::channel(8);
        registry
            .register(link(1, 1), host(1), existing_tx, LinkRole::Peer, &[])
            .await;

        // The new link's handshake advertised a now-gone neighbor (3) and
        // missed the existing one (1): registration sends the difference.
        let (new_tx, mut new_rx) = mpsc::channel(8);
        registry
            .register(
                link(2, 1),
                host(2),
                new_tx,
                LinkRole::Peer,
                &[Uuid::from_u128(3)],
            )
            .await;

        let first = recv_message(&mut new_rx).await;
        let second = recv_message(&mut new_rx).await;
        let (up, down) = match &first.body {
            Some(pb::message::Body::NeighborUp(_)) => (first, second),
            _ => (second, first),
        };
        assert_eq!(neighbor_up_host_id(&up), Uuid::from_u128(1).as_bytes());
        assert_eq!(neighbor_down_host_id(&down), Uuid::from_u128(3).as_bytes());
        assert!(new_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn only_the_last_link_to_a_peer_announces_neighbor_down() {
        let registry = LinkRegistry::default();
        let (observer_tx, mut observer_rx) = mpsc::channel(8);
        registry
            .register(link(9, 1), host(9), observer_tx, LinkRole::Peer, &[])
            .await;

        let (first_tx, _first_rx) = mpsc::channel(8);
        let (second_tx, _second_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), first_tx, LinkRole::Peer, &[])
            .await;
        registry
            .register(
                link(2, 2),
                host(2),
                second_tx,
                LinkRole::Peer,
                &[Uuid::from_u128(9)],
            )
            .await;
        // One NeighborUp for the peer's first link; the second link is silent.
        assert_eq!(
            neighbor_up_host_id(&recv_message(&mut observer_rx).await),
            Uuid::from_u128(2).as_bytes()
        );
        assert!(observer_rx.try_recv().is_err());

        registry.remove(&link(2, 1)).await;
        assert!(observer_rx.try_recv().is_err(), "one link is still up");

        registry.remove(&link(2, 2)).await;
        assert_eq!(
            neighbor_down_host_id(&recv_message(&mut observer_rx).await),
            Uuid::from_u128(2).as_bytes()
        );
    }

    #[tokio::test]
    async fn a_neighbor_is_never_told_about_itself() {
        let registry = LinkRegistry::default();
        let (peer_tx, mut peer_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), peer_tx, LinkRole::Peer, &[])
            .await;

        // A second link to the same peer: no announcement to the peer.
        let (second_tx, mut second_rx) = mpsc::channel(8);
        registry
            .register(
                link(2, 2),
                host(2),
                second_tx,
                LinkRole::Peer,
                &[Uuid::from_u128(2)],
            )
            .await;

        assert!(peer_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn snapshot_lists_each_neighbor_once() {
        let registry = LinkRegistry::default();
        let (first_tx, _first_rx) = mpsc::channel(8);
        let (second_tx, _second_rx) = mpsc::channel(8);
        let (other_tx, _other_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), first_tx, LinkRole::Peer, &[])
            .await;
        registry
            .register(link(2, 2), host(2), second_tx, LinkRole::Peer, &[])
            .await;
        registry
            .register(link(3, 1), host(3), other_tx, LinkRole::Peer, &[])
            .await;

        let snapshot = registry.neighbor_snapshot().await;
        assert_eq!(
            snapshot.iter().map(|host| host.id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(2), Uuid::from_u128(3)]
        );
    }

    #[tokio::test]
    async fn full_outgoing_queue_requests_link_close() {
        let registry = LinkRegistry::default();
        let (full_tx, _full_rx) = mpsc::channel(1);
        full_tx.try_send(pb::Message { body: None }).unwrap();
        let mut close_rx = registry
            .register(link(1, 1), host(1), full_tx, LinkRole::Peer, &[])
            .await;

        let (new_tx, _new_rx) = mpsc::channel(8);
        registry
            .register(link(2, 1), host(2), new_tx, LinkRole::Peer, &[])
            .await;

        assert_eq!(
            close_rx.recv().await,
            Some(LinkCloseRequest::OutgoingQueueFull)
        );
    }

    #[tokio::test]
    async fn link_close_notification_is_best_effort_for_full_link_queues() {
        let registry = LinkRegistry::default();
        let (tx, _rx) = mpsc::channel(1);
        tx.try_send(pb::Message { body: None }).unwrap();
        registry
            .register(link(1, 1), host(1), tx, LinkRole::Peer, &[])
            .await;

        tokio::time::timeout(
            Duration::from_millis(50),
            registry.send_link_close_to_all(pb::LinkCloseReason::UserShutdown),
        )
        .await
        .expect("link close send must not wait on a full queue");
    }

    #[tokio::test]
    async fn send_link_close_to_all_sends_typed_reason() {
        let registry = LinkRegistry::default();
        let (tx, mut rx) = mpsc::channel(8);
        registry
            .register(link(99, 1), host(99), tx, LinkRole::Peer, &[])
            .await;

        registry
            .send_link_close_to_all(pb::LinkCloseReason::UserShutdown)
            .await;

        let Some(pb::Message {
            body: Some(pb::message::Body::LinkClose(close)),
        }) = rx.recv().await
        else {
            panic!("expected LinkClose message");
        };
        assert_eq!(close.reason, pb::LinkCloseReason::UserShutdown as i32);
        assert!(close.error.is_none());
    }

    #[tokio::test]
    async fn cloud_relay_role_is_tracked_per_link() {
        let registry = LinkRegistry::default();
        let (cloud_tx, _cloud_rx) = mpsc::channel(1);
        let (peer_tx, _peer_rx) = mpsc::channel(1);
        registry
            .register(link(1, 1), host(1), cloud_tx, LinkRole::CloudRelay, &[])
            .await;
        registry
            .register(link(2, 1), host(2), peer_tx, LinkRole::Peer, &[])
            .await;

        assert!(registry.is_cloud_relay(&link(1, 1)).await);
        assert!(!registry.is_cloud_relay(&link(2, 1)).await);
        assert!(registry.has_cloud_relay_link_to(Uuid::from_u128(1)).await);
        assert!(!registry.has_cloud_relay_link_to(Uuid::from_u128(2)).await);
    }
}
