//! The routing table: who is adjacent, and who claims adjacency to whom.
//!
//! Two rules define the model:
//!
//! 1. **Advertise only adjacency.** Neighbors tell us only "I have/lost a
//!    direct link to H" — never anything they learned from someone else.
//! 2. **Forward only to adjacency.** Frames are forwarded iff we hold a
//!    direct link to their destination (see `LinkRegistry`).
//!
//! From those rules, the table is two maps: our own channel-backed links
//! (`directs`) and our neighbors' adjacency claims (`claims`). Presence is a
//! derivation — a host is online iff it is a direct peer or some neighbor
//! claims adjacency to it — so presence reaches exactly two hops.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;

use crate::HostId;
use crate::resource_limits::{CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW, ROUTING_HOST_CAP};
use crate::routing::events::{EventSource, HostReachabilityEvent, RoutingEvent};
use crate::routing::types::{Host, LinkId, Route};
use crate::routing::{LinkRegistry, LinkRole};
use crate::trust::SharedTrustStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteUpdateOutcome {
    Inserted,
    AlreadyKnown,
    RejectedByCap,
    /// The host is mid trust-replacement; updates are suppressed until
    /// `finish_replacement`.
    Replacing,
}

#[derive(Debug, Clone)]
struct DirectEntry {
    host: Host,
    links: Vec<LinkId>,
}

#[derive(Debug, Clone)]
struct ClaimedEntry {
    host: Host,
    relays: Vec<HostId>,
    inserted_sequence: u64,
}

#[derive(Default)]
struct RoutingState {
    directs: HashMap<HostId, DirectEntry>,
    claims: HashMap<HostId, ClaimedEntry>,
    replacing: HashSet<HostId>,
    client_visible_activity: HashMap<HostId, Instant>,
    next_host_sequence: u64,
    routing_events: EventSource<RoutingEvent>,
    host_events: EventSource<HostReachabilityEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RoutingDebug {
    pub(crate) hosts: Vec<HostDebug>,
    pub(crate) routes: Vec<RouteDebug>,
    pub(crate) links: Vec<LinkDebug>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HostDebug {
    pub(crate) id: HostId,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) capabilities: crate::routing::Capabilities,
}

impl From<&Host> for HostDebug {
    fn from(host: &Host) -> Self {
        Self {
            id: host.id,
            name: host.name.clone(),
            version: host.version.clone(),
            capabilities: host.capabilities.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteDebug {
    pub(crate) dst: HostId,
    pub(crate) via: RouteVia,
    pub(crate) hops: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RouteVia {
    Direct { link: String },
    Relay { host: HostId },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkDirection {
    Bidirectional,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LinkDebug {
    pub(crate) id: String,
    pub(crate) peer: HostId,
    pub(crate) direction: LinkDirection,
    pub(crate) transport: Option<LinkRole>,
    pub(crate) established_at: Option<DateTime<Utc>>,
}

impl RoutingState {
    fn is_present(&self, host_id: HostId) -> bool {
        self.directs.contains_key(&host_id) || self.claims.contains_key(&host_id)
    }
}

#[derive(Default)]
pub(crate) struct RoutingCore {
    state: RwLock<RoutingState>,
    trust_store: Option<SharedTrustStore>,
}

impl RoutingCore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_trust_store(trust_store: SharedTrustStore) -> Self {
        Self {
            state: RwLock::new(RoutingState::default()),
            trust_store: Some(trust_store),
        }
    }

    /// The route a fresh call to `host_id` would take: a direct link when we
    /// hold one, otherwise through the first relay that claims adjacency.
    pub(crate) async fn route_to(&self, host_id: HostId) -> Option<Route> {
        let state = self.state.read().await;
        if let Some(entry) = state.directs.get(&host_id)
            && let Some(link) = entry.links.first()
        {
            return Some(Route::Direct(*link));
        }
        state
            .claims
            .get(&host_id)
            .and_then(|entry| entry.relays.first())
            .map(|relay| Route::Via(*relay))
    }

    /// Every route we hold to `host_id` (the direct link first).
    pub(crate) async fn routes_to(&self, host_id: HostId) -> Vec<Route> {
        let state = self.state.read().await;
        let mut routes = Vec::new();
        if let Some(entry) = state.directs.get(&host_id) {
            routes.extend(entry.links.iter().map(|link| Route::Direct(*link)));
        }
        if let Some(entry) = state.claims.get(&host_id) {
            routes.extend(entry.relays.iter().map(|relay| Route::Via(*relay)));
        }
        routes
    }

    /// A stable snapshot of the live routing table for diagnostics.
    pub(crate) async fn debug_view(&self, link_registry: &LinkRegistry) -> RoutingDebug {
        let state = self.state.read().await;

        let mut hosts = HashMap::new();
        for entry in state.directs.values() {
            hosts.insert(entry.host.id, HostDebug::from(&entry.host));
        }
        for entry in state.claims.values() {
            hosts
                .entry(entry.host.id)
                .or_insert_with(|| HostDebug::from(&entry.host));
        }
        let mut hosts = hosts.into_values().collect::<Vec<_>>();
        hosts.sort_unstable_by_key(|host| host.id);

        let mut routes = Vec::new();
        let mut links = Vec::new();
        for (dst, entry) in &state.directs {
            for link in &entry.links {
                let id = link.to_string();
                routes.push(RouteDebug {
                    dst: *dst,
                    via: RouteVia::Direct { link: id.clone() },
                    hops: 1,
                });
                links.push((
                    *link,
                    LinkDebug {
                        id,
                        peer: link.peer(),
                        direction: LinkDirection::Bidirectional,
                        transport: None,
                        established_at: None,
                    },
                ));
            }
        }
        for (dst, entry) in &state.claims {
            for relay in &entry.relays {
                routes.push(RouteDebug {
                    dst: *dst,
                    via: RouteVia::Relay { host: *relay },
                    hops: 2,
                });
            }
        }
        routes.sort_unstable_by_key(|route| {
            let via = match &route.via {
                RouteVia::Direct { link } => format!("0:{link}"),
                RouteVia::Relay { host } => format!("1:{host}"),
            };
            (route.dst, via)
        });
        drop(state);

        for (link_id, link) in &mut links {
            link.transport = link_registry.link_role(link_id).await;
        }
        let mut links = links.into_iter().map(|(_, link)| link).collect::<Vec<_>>();
        links.sort_unstable_by(|left, right| left.id.cmp(&right.id));

        RoutingDebug {
            hosts,
            routes,
            links,
        }
    }

    /// The relays claiming adjacency to `host_id`, in claim order.
    pub(crate) async fn relays_to(&self, host_id: HostId) -> Vec<HostId> {
        self.state
            .read()
            .await
            .claims
            .get(&host_id)
            .map(|entry| entry.relays.clone())
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) async fn host_entry(&self, host_id: HostId) -> Option<Host> {
        let state = self.state.read().await;
        state
            .directs
            .get(&host_id)
            .map(|entry| entry.host.clone())
            .or_else(|| state.claims.get(&host_id).map(|entry| entry.host.clone()))
    }

    /// Records a channel-backed direct link to `host`. Emits `NeighborUp`
    /// (and `Added` presence on first sight).
    pub(crate) async fn apply_direct_up(&self, host: Host, link: LinkId) -> RouteUpdateOutcome {
        let trusted_hosts = self.trusted_host_ids();
        let mut state = self.state.write().await;
        let host_id = host.id;
        if state.replacing.contains(&host_id) {
            return RouteUpdateOutcome::Replacing;
        }
        if let Some(outcome) = reserve_presence_capacity(&mut state, host_id, &trusted_hosts) {
            return outcome;
        }

        let newly_present = !state.is_present(host_id);
        let entry = state.directs.entry(host_id).or_insert_with(|| DirectEntry {
            host: host.clone(),
            links: Vec::new(),
        });
        if entry.links.contains(&link) {
            return RouteUpdateOutcome::AlreadyKnown;
        }
        entry.links.push(link);
        state.routing_events.emit(RoutingEvent::NeighborUp {
            host: host.clone(),
            link,
        });
        if newly_present {
            state
                .host_events
                .emit(HostReachabilityEvent::Added { host });
        }
        RouteUpdateOutcome::Inserted
    }

    /// Removes one direct link. Emits `NeighborDown`, and `Removed` when the
    /// host is no longer present at all.
    pub(crate) async fn apply_direct_down(&self, link: LinkId) {
        let mut state = self.state.write().await;
        let host_id = link.peer();
        let Some(entry) = state.directs.get_mut(&host_id) else {
            return;
        };
        let Some(position) = entry.links.iter().position(|known| known == &link) else {
            return;
        };
        entry.links.remove(position);
        let last_link = entry.links.is_empty();
        if last_link {
            state.directs.remove(&host_id);
        }
        state.routing_events.emit(RoutingEvent::NeighborDown {
            host_id,
            link,
            last_link,
        });
        emit_removed_if_absent(&mut state, host_id);
    }

    /// Records a neighbor's adjacency claim: `relay` says it has a direct
    /// link to `host`.
    pub(crate) async fn apply_claim_up(&self, relay: HostId, host: Host) -> RouteUpdateOutcome {
        let trusted_hosts = self.trusted_host_ids();
        let mut state = self.state.write().await;
        let host_id = host.id;
        if state.replacing.contains(&host_id) {
            return RouteUpdateOutcome::Replacing;
        }
        if let Some(outcome) = reserve_presence_capacity(&mut state, host_id, &trusted_hosts) {
            return outcome;
        }

        let newly_present = !state.is_present(host_id);
        if !state.claims.contains_key(&host_id) {
            let inserted_sequence = state.next_host_sequence;
            state.next_host_sequence = state.next_host_sequence.saturating_add(1);
            state.claims.insert(
                host_id,
                ClaimedEntry {
                    host: host.clone(),
                    relays: Vec::new(),
                    inserted_sequence,
                },
            );
        }
        let entry = state
            .claims
            .get_mut(&host_id)
            .expect("claim entry inserted above");
        if entry.relays.contains(&relay) {
            return RouteUpdateOutcome::AlreadyKnown;
        }
        entry.relays.push(relay);
        state.routing_events.emit(RoutingEvent::ClaimUp {
            relay,
            host: host.clone(),
        });
        if newly_present {
            state
                .host_events
                .emit(HostReachabilityEvent::Added { host });
        }
        RouteUpdateOutcome::Inserted
    }

    /// Withdraws one adjacency claim.
    pub(crate) async fn apply_claim_down(&self, relay: HostId, host_id: HostId) {
        let mut state = self.state.write().await;
        remove_claim(&mut state, relay, host_id);
        emit_removed_if_absent(&mut state, host_id);
    }

    /// Withdraws every claim made by `relay` (its last link went down).
    pub(crate) async fn remove_relay_claims(&self, relay: HostId) {
        let mut state = self.state.write().await;
        let claimed = state
            .claims
            .iter()
            .filter_map(|(host_id, entry)| entry.relays.contains(&relay).then_some(*host_id))
            .collect::<Vec<_>>();
        for host_id in claimed {
            remove_claim(&mut state, relay, host_id);
            emit_removed_if_absent(&mut state, host_id);
        }
    }

    /// Forgets everything about `host_id` (teardown / trust replacement):
    /// its direct links and every claim about it.
    pub(crate) async fn remove_host(&self, host_id: HostId) {
        let mut state = self.state.write().await;
        if let Some(entry) = state.directs.remove(&host_id) {
            for link in entry.links {
                state.routing_events.emit(RoutingEvent::NeighborDown {
                    host_id,
                    link,
                    last_link: true,
                });
            }
        }
        if let Some(entry) = state.claims.remove(&host_id) {
            for relay in entry.relays {
                state
                    .routing_events
                    .emit(RoutingEvent::ClaimDown { relay, host_id });
            }
        }
        state.client_visible_activity.remove(&host_id);
        state
            .host_events
            .emit(HostReachabilityEvent::Removed { host_id });
    }

    /// Suppresses route updates for `host_id` until `finish_replacement`
    /// (trust replacement window).
    pub(crate) async fn begin_replacement(&self, host_id: HostId) {
        self.state.write().await.replacing.insert(host_id);
    }

    pub(crate) async fn finish_replacement(&self, host_id: HostId) {
        self.state.write().await.replacing.remove(&host_id);
    }

    pub(crate) async fn mark_client_visible_hosts(&self, host_ids: &[HostId]) {
        let mut state = self.state.write().await;
        let now = Instant::now();
        prune_client_visible_activity(&mut state, now);
        for host_id in host_ids {
            if state.is_present(*host_id) {
                state.client_visible_activity.insert(*host_id, now);
            }
        }
    }

    pub(crate) async fn subscribe_routing_events(&self) -> mpsc::Receiver<RoutingEvent> {
        self.state.write().await.routing_events.subscribe()
    }

    /// The current table replayed as events, for seeding a late subscriber.
    pub(crate) async fn routing_events_snapshot(&self) -> Vec<RoutingEvent> {
        let state = self.state.read().await;
        let mut events = Vec::new();
        let mut directs = state.directs.values().collect::<Vec<_>>();
        directs.sort_unstable_by_key(|entry| entry.host.id);
        for entry in directs {
            for link in &entry.links {
                events.push(RoutingEvent::NeighborUp {
                    host: entry.host.clone(),
                    link: *link,
                });
            }
        }
        let mut claims = state.claims.values().collect::<Vec<_>>();
        claims.sort_unstable_by_key(|entry| entry.host.id);
        for entry in claims {
            for relay in &entry.relays {
                events.push(RoutingEvent::ClaimUp {
                    relay: *relay,
                    host: entry.host.clone(),
                });
            }
        }
        events
    }

    pub(crate) async fn subscribe_hosts(&self) -> mpsc::Receiver<HostReachabilityEvent> {
        self.state.write().await.host_events.subscribe()
    }

    #[cfg(test)]
    pub(crate) async fn subscribe_hosts_with_snapshot(
        &self,
    ) -> (Vec<Host>, mpsc::Receiver<HostReachabilityEvent>) {
        let mut state = self.state.write().await;
        let mut snapshot = state
            .directs
            .values()
            .map(|entry| entry.host.clone())
            .chain(state.claims.values().map(|entry| entry.host.clone()))
            .collect::<Vec<_>>();
        snapshot.sort_unstable_by_key(|host| host.id);
        snapshot.dedup_by_key(|host| host.id);
        let rx = state.host_events.subscribe();
        (snapshot, rx)
    }
}

impl RoutingCore {
    fn trusted_host_ids(&self) -> HashSet<HostId> {
        let Some(trust_store) = &self.trust_store else {
            return HashSet::new();
        };
        let Ok(trust_store) = trust_store.read() else {
            tracing::warn!("trust store lock poisoned while enforcing routing host cap");
            return HashSet::new();
        };
        trust_store.entries().map(|(host_id, _)| host_id).collect()
    }
}

fn remove_claim(state: &mut RoutingState, relay: HostId, host_id: HostId) {
    let Some(entry) = state.claims.get_mut(&host_id) else {
        return;
    };
    let Some(position) = entry.relays.iter().position(|known| known == &relay) else {
        return;
    };
    entry.relays.remove(position);
    if entry.relays.is_empty() {
        state.claims.remove(&host_id);
    }
    state
        .routing_events
        .emit(RoutingEvent::ClaimDown { relay, host_id });
}

fn emit_removed_if_absent(state: &mut RoutingState, host_id: HostId) {
    if !state.is_present(host_id) {
        state.client_visible_activity.remove(&host_id);
        state
            .host_events
            .emit(HostReachabilityEvent::Removed { host_id });
    }
}

/// Enforces [`ROUTING_HOST_CAP`] before a new untrusted host becomes
/// present: evicts the oldest evictable claimed host, or rejects.
fn reserve_presence_capacity(
    state: &mut RoutingState,
    host_id: HostId,
    trusted_hosts: &HashSet<HostId>,
) -> Option<RouteUpdateOutcome> {
    if state.is_present(host_id)
        || trusted_hosts.contains(&host_id)
        || untrusted_host_count(state, trusted_hosts) < ROUTING_HOST_CAP
    {
        return None;
    }
    let Some(evicted) = evict_host_for_cap(state, trusted_hosts) else {
        return Some(RouteUpdateOutcome::RejectedByCap);
    };
    for relay in evicted.relays {
        state.routing_events.emit(RoutingEvent::ClaimDown {
            relay,
            host_id: evicted.host_id,
        });
    }
    state.host_events.emit(HostReachabilityEvent::Removed {
        host_id: evicted.host_id,
    });
    None
}

struct EvictedHost {
    host_id: HostId,
    relays: Vec<HostId>,
}

fn evict_host_for_cap(
    state: &mut RoutingState,
    trusted_hosts: &HashSet<HostId>,
) -> Option<EvictedHost> {
    let now = Instant::now();
    prune_client_visible_activity(state, now);
    let candidate = state
        .claims
        .iter()
        .filter(|(host_id, _)| !trusted_hosts.contains(host_id))
        .filter(|(host_id, _)| !state.directs.contains_key(host_id))
        .filter(|(host_id, _)| !has_recent_client_visible_activity(state, **host_id, now))
        .min_by_key(|(_, entry)| entry.inserted_sequence)
        .map(|(host_id, _)| *host_id)?;
    let entry = state.claims.remove(&candidate)?;
    state.client_visible_activity.remove(&candidate);
    Some(EvictedHost {
        host_id: candidate,
        relays: entry.relays,
    })
}

fn untrusted_host_count(state: &RoutingState, trusted_hosts: &HashSet<HostId>) -> usize {
    state
        .directs
        .keys()
        .chain(state.claims.keys())
        .collect::<HashSet<_>>()
        .into_iter()
        .filter(|host_id| !trusted_hosts.contains(host_id))
        .count()
}

fn prune_client_visible_activity(state: &mut RoutingState, now: Instant) {
    state
        .client_visible_activity
        .retain(|_, last_seen| *last_seen + CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW > now);
}

fn has_recent_client_visible_activity(state: &RoutingState, host_id: HostId, now: Instant) -> bool {
    state
        .client_visible_activity
        .get(&host_id)
        .is_some_and(|last_seen| *last_seen + CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW > now)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{DateTime, Utc};

    use super::*;
    use crate::routing::{Capabilities, LinkRegistry, LinkRole, SupportedAgentType};
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

    fn link(peer: u128, instance: u128) -> LinkId {
        LinkId {
            peer: HostId::from_u128(peer),
            instance: uuid::Uuid::from_u128(instance),
        }
    }

    fn trust_store_with(host_id: HostId) -> TrustStore {
        let mut trust_store = TrustStore::default();
        trust_store.insert_for_test(
            host_id,
            TrustEntry {
                pubkey: vec![7; 32],
                name: "trusted".to_string(),
                paired_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
            },
        );
        trust_store
    }

    #[tokio::test]
    async fn debug_links_serialize_registered_roles_and_leave_unknown_roles_null() {
        let core = RoutingCore::new();
        let registry = LinkRegistry::default();
        let cloud_link = link(1, 1);
        let peer_link = link(2, 1);
        let unknown_link = link(3, 1);
        let (cloud_tx, _cloud_rx) = mpsc::channel(8);
        let (peer_tx, _peer_rx) = mpsc::channel(8);
        registry
            .register(
                cloud_link,
                host(1, "cloud"),
                cloud_tx,
                LinkRole::CloudRelay,
                &[],
            )
            .await;
        registry
            .register(peer_link, host(2, "peer"), peer_tx, LinkRole::Peer, &[])
            .await;
        core.apply_direct_up(host(1, "cloud"), cloud_link).await;
        core.apply_direct_up(host(2, "peer"), peer_link).await;
        core.apply_direct_up(host(3, "unknown"), unknown_link).await;

        let debug = serde_json::to_value(core.debug_view(&registry).await).unwrap();
        let links = debug["links"].as_array().unwrap();
        assert_eq!(links[0]["transport"], "cloud_relay");
        assert_eq!(links[1]["transport"], "peer");
        assert!(links[2]["transport"].is_null());
    }

    #[tokio::test]
    async fn direct_links_and_claims_combine_into_presence_and_routes() {
        let core = RoutingCore::new();
        let mut raw_rx = core.subscribe_routing_events().await;
        let mut host_rx = core.subscribe_hosts().await;
        let peer = HostId::from_u128(1);
        let relay = HostId::from_u128(9);

        assert_eq!(
            core.apply_claim_up(relay, host(1, "claimed-first")).await,
            RouteUpdateOutcome::Inserted
        );
        assert_eq!(
            core.apply_claim_up(relay, host(1, "claimed-again")).await,
            RouteUpdateOutcome::AlreadyKnown
        );
        assert_eq!(core.route_to(peer).await, Some(Route::Via(relay)));

        let direct = link(1, 100);
        assert_eq!(
            core.apply_direct_up(host(1, "direct"), direct).await,
            RouteUpdateOutcome::Inserted
        );
        assert_eq!(core.route_to(peer).await, Some(Route::Direct(direct)));

        assert!(matches!(
            raw_rx.recv().await,
            Some(RoutingEvent::ClaimUp { relay: event_relay, .. }) if event_relay == relay
        ));
        assert!(matches!(
            raw_rx.recv().await,
            Some(RoutingEvent::NeighborUp { link, .. }) if link == direct
        ));
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Added { host }) if host.id == peer)
        );
        assert!(host_rx.try_recv().is_err(), "presence is emitted once");
    }

    #[tokio::test]
    async fn a_host_stays_present_until_its_last_route_disappears() {
        let core = RoutingCore::new();
        let mut host_rx = core.subscribe_hosts().await;
        let peer = HostId::from_u128(2);
        let relay = HostId::from_u128(9);
        let direct = link(2, 100);

        core.apply_direct_up(host(2, "remote"), direct).await;
        core.apply_claim_up(relay, host(2, "remote")).await;
        assert!(matches!(
            host_rx.recv().await,
            Some(HostReachabilityEvent::Added { .. })
        ));

        core.apply_direct_down(direct).await;
        assert_eq!(core.route_to(peer).await, Some(Route::Via(relay)));
        assert!(host_rx.try_recv().is_err(), "still present via the claim");

        core.apply_claim_down(relay, peer).await;
        assert_eq!(core.route_to(peer).await, None);
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Removed { host_id }) if host_id == peer)
        );
    }

    #[tokio::test]
    async fn a_dying_relay_takes_all_its_claims_with_it() {
        let core = RoutingCore::new();
        let relay = HostId::from_u128(9);
        let other_relay = HostId::from_u128(8);
        core.apply_claim_up(relay, host(1, "one")).await;
        core.apply_claim_up(relay, host(2, "two")).await;
        core.apply_claim_up(other_relay, host(2, "two")).await;

        core.remove_relay_claims(relay).await;

        assert_eq!(core.route_to(HostId::from_u128(1)).await, None);
        assert_eq!(
            core.route_to(HostId::from_u128(2)).await,
            Some(Route::Via(other_relay))
        );
    }

    #[tokio::test]
    async fn multiple_links_to_one_peer_survive_a_single_link_loss() {
        let core = RoutingCore::new();
        let peer = HostId::from_u128(3);
        let first = link(3, 1);
        let second = link(3, 2);

        core.apply_direct_up(host(3, "peer"), first).await;
        core.apply_direct_up(host(3, "peer"), second).await;
        core.apply_direct_down(first).await;

        assert_eq!(core.route_to(peer).await, Some(Route::Direct(second)));
    }

    #[tokio::test]
    async fn replacement_window_suppresses_route_updates() {
        let core = RoutingCore::new();
        let peer = HostId::from_u128(4);

        core.begin_replacement(peer).await;
        assert_eq!(
            core.apply_direct_up(host(4, "peer"), link(4, 1)).await,
            RouteUpdateOutcome::Replacing
        );
        assert_eq!(
            core.apply_claim_up(HostId::from_u128(9), host(4, "peer"))
                .await,
            RouteUpdateOutcome::Replacing
        );

        core.finish_replacement(peer).await;
        assert_eq!(
            core.apply_direct_up(host(4, "peer"), link(4, 1)).await,
            RouteUpdateOutcome::Inserted
        );
    }

    #[tokio::test]
    async fn remove_host_forgets_links_and_claims_and_emits_removed() {
        let core = RoutingCore::new();
        let mut host_rx = core.subscribe_hosts().await;
        let peer = HostId::from_u128(5);
        core.apply_direct_up(host(5, "peer"), link(5, 1)).await;
        core.apply_claim_up(HostId::from_u128(9), host(5, "peer"))
            .await;
        assert!(matches!(
            host_rx.recv().await,
            Some(HostReachabilityEvent::Added { .. })
        ));

        core.remove_host(peer).await;

        assert_eq!(core.route_to(peer).await, None);
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Removed { host_id }) if host_id == peer)
        );
    }

    #[tokio::test]
    async fn host_cap_evicts_oldest_claimed_host_without_trust_or_recent_activity() {
        let trusted = HostId::from_u128(10_000);
        let core = RoutingCore::with_trust_store(Arc::new(std::sync::RwLock::new(
            trust_store_with(trusted),
        )));
        let relay = HostId::from_u128(20_000);
        assert_eq!(
            core.apply_claim_up(relay, host(10_000, "trusted")).await,
            RouteUpdateOutcome::Inserted
        );
        for id in 1..=ROUTING_HOST_CAP as u128 {
            assert_eq!(
                core.apply_claim_up(relay, host(id, &format!("host-{id}")))
                    .await,
                RouteUpdateOutcome::Inserted
            );
        }
        core.mark_client_visible_hosts(&[HostId::from_u128(2)])
            .await;

        assert_eq!(
            core.apply_claim_up(relay, host(1001, "new")).await,
            RouteUpdateOutcome::Inserted
        );

        assert!(core.host_entry(HostId::from_u128(2)).await.is_some());
        assert!(core.host_entry(trusted).await.is_some());
        assert!(core.host_entry(HostId::from_u128(1)).await.is_none());
        assert!(core.host_entry(HostId::from_u128(1001)).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn host_cap_allows_client_visible_activity_to_expire() {
        let core = RoutingCore::new();
        let relay = HostId::from_u128(20_000);
        for id in 1..=ROUTING_HOST_CAP as u128 {
            core.apply_claim_up(relay, host(id, &format!("host-{id}")))
                .await;
        }
        core.mark_client_visible_hosts(&[HostId::from_u128(1)])
            .await;
        tokio::time::advance(CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW + Duration::from_secs(1)).await;

        assert_eq!(
            core.apply_claim_up(relay, host(1001, "new")).await,
            RouteUpdateOutcome::Inserted
        );

        assert!(core.host_entry(HostId::from_u128(1)).await.is_none());
        assert!(core.host_entry(HostId::from_u128(1001)).await.is_some());
    }

    #[tokio::test]
    async fn subscribe_with_snapshot_registers_before_later_events() {
        let core = RoutingCore::new();
        core.apply_direct_up(host(1, "one"), link(1, 1)).await;

        let (snapshot, mut rx) = core.subscribe_hosts_with_snapshot().await;
        assert_eq!(
            snapshot
                .into_iter()
                .map(|host| host.name)
                .collect::<Vec<_>>(),
            ["one"]
        );

        core.apply_direct_up(host(2, "two"), link(2, 1)).await;
        assert!(
            matches!(rx.recv().await, Some(HostReachabilityEvent::Added { host }) if host.name == "two")
        );
    }
}
