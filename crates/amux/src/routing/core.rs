use std::collections::{HashMap, HashSet};

use tokio::sync::{RwLock, mpsc};
use tokio::time::Instant;

use crate::HostId;
use crate::resource_limits::{
    CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW, ROUTES_PER_HOST_CAP, ROUTING_HOST_CAP,
};
use crate::routing::events::{EventSource, HostReachabilityEvent, RoutingEvent};
use crate::routing::types::Host;
use crate::routing::{Link, Route};
use crate::trust::SharedTrustStore;

#[cfg(any(test, feature = "testnet"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostEntry {
    pub(crate) host: Host,
    pub(crate) route: Route,
}

#[derive(Debug, Clone)]
struct HostRoutes {
    host: Host,
    routes: Vec<Route>,
    inserted_sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostUpOutcome {
    Inserted,
    AlreadyKnown,
    RejectedByCap,
}

#[derive(Default)]
struct RoutingState {
    hosts: HashMap<HostId, HostRoutes>,
    links: HashSet<Link>,
    active_routes: HashMap<HostId, Route>,
    client_visible_activity: HashMap<HostId, Instant>,
    next_host_sequence: u64,
    routing_events: EventSource<RoutingEvent>,
    host_events: EventSource<HostReachabilityEvent>,
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

    #[cfg(any(test, feature = "testnet"))]
    pub(crate) async fn host_entry(&self, host_id: HostId) -> Option<HostEntry> {
        self.state
            .read()
            .await
            .hosts
            .get(&host_id)
            .and_then(HostRoutes::first_entry)
    }

    pub(crate) async fn best_route(&self, host_id: HostId) -> Option<Route> {
        self.state
            .read()
            .await
            .hosts
            .get(&host_id)
            .and_then(|entry| select_best_route(&entry.routes))
    }

    #[cfg(test)]
    pub(crate) async fn hosts_snapshot(&self) -> Vec<HostEntry> {
        let state = self.state.read().await;
        let mut hosts = state
            .hosts
            .values()
            .filter_map(HostRoutes::first_entry)
            .collect::<Vec<_>>();
        hosts.sort_unstable_by_key(|entry| entry.host.id);
        hosts
    }

    pub(crate) async fn reserve_link(&self, proposed: &Link) -> Link {
        const LINK_ASSIGNMENT_ATTEMPTS: usize = 5;
        const LINK_ASSIGNMENT_SUFFIX_LEN: usize = 8;

        let mut state = self.state.write().await;
        for attempt in 0..LINK_ASSIGNMENT_ATTEMPTS {
            let candidate =
                link_assignment_candidate(proposed, attempt, LINK_ASSIGNMENT_SUFFIX_LEN);
            if !link_is_used(&state, &candidate) {
                state.links.insert(candidate.clone());
                return candidate;
            }
        }

        let candidate = link_assignment_candidate(
            proposed,
            LINK_ASSIGNMENT_ATTEMPTS,
            LINK_ASSIGNMENT_SUFFIX_LEN,
        );
        state.links.insert(candidate.clone());
        candidate
    }

    pub(crate) async fn reserve_exact_link(&self, link: &Link) -> bool {
        let mut state = self.state.write().await;
        if link_is_used(&state, link) {
            return false;
        }
        state.links.insert(link.clone());
        true
    }

    pub(crate) async fn release_link(&self, link: &Link) {
        self.state.write().await.links.remove(link);
    }

    pub(crate) async fn mark_active_route(&self, host_id: HostId, route: Option<Route>) {
        let mut state = self.state.write().await;
        match route {
            Some(route)
                if state
                    .hosts
                    .get(&host_id)
                    .is_some_and(|entry| entry.routes.contains(&route)) =>
            {
                state.active_routes.insert(host_id, route);
            }
            _ => {
                state.active_routes.remove(&host_id);
            }
        }
    }

    pub(crate) async fn mark_client_visible_hosts(&self, host_ids: &[HostId]) {
        let mut state = self.state.write().await;
        let now = Instant::now();
        prune_client_visible_activity(&mut state, now);
        for host_id in host_ids {
            if state.hosts.contains_key(host_id) {
                state.client_visible_activity.insert(*host_id, now);
            }
        }
    }

    pub(crate) async fn notify_host_status_changed(&self, host_id: HostId) {
        self.state
            .write()
            .await
            .host_events
            .emit(HostReachabilityEvent::StatusChanged { host_id });
    }

    pub(crate) async fn apply_host_up(
        &self,
        host: Host,
        route: Route,
        origin_link: Option<Link>,
    ) -> HostUpOutcome {
        let trusted_hosts = self.trusted_host_ids();
        let mut state = self.state.write().await;
        let host_id = host.id;
        if !state.hosts.contains_key(&host_id)
            && !trusted_hosts.contains(&host_id)
            && untrusted_host_count(&state, &trusted_hosts) >= ROUTING_HOST_CAP
        {
            if let Some(evicted) = evict_host_for_cap(&mut state, &trusted_hosts) {
                emit_host_eviction(&mut state, evicted);
            } else {
                return HostUpOutcome::RejectedByCap;
            }
        }

        let first_route = !state.hosts.contains_key(&host_id);
        if first_route {
            let inserted_sequence = state.next_host_sequence;
            state.next_host_sequence = state.next_host_sequence.saturating_add(1);
            state.hosts.insert(
                host_id,
                HostRoutes {
                    host: host.clone(),
                    routes: Vec::new(),
                    inserted_sequence,
                },
            );
        }
        let active_route = state.active_routes.get(&host_id).cloned();
        let entry = state
            .hosts
            .get_mut(&host_id)
            .expect("host entry inserted above");
        if entry.routes.iter().any(|known| {
            known == &route || (route.len() > known.len() && route.ends_with_route(known))
        }) {
            return HostUpOutcome::AlreadyKnown;
        }
        if entry.routes.len() >= ROUTES_PER_HOST_CAP {
            if let Some(removed_route) = evict_route_for_cap(entry, active_route.as_ref()) {
                state.routing_events.emit(RoutingEvent::HostDown {
                    host_id,
                    route: removed_route,
                    origin_link: None,
                });
            } else {
                return HostUpOutcome::RejectedByCap;
            }
        }
        let entry = state
            .hosts
            .get_mut(&host_id)
            .expect("host entry retained after route eviction");
        entry.routes.push(route.clone());

        state.routing_events.emit(RoutingEvent::HostUp {
            host: host.clone(),
            route,
            origin_link,
        });
        if first_route {
            state
                .host_events
                .emit(HostReachabilityEvent::Added { host });
        }

        HostUpOutcome::Inserted
    }

    pub(crate) async fn apply_host_down(
        &self,
        host_id: HostId,
        route: &Route,
        origin_link: Option<Link>,
    ) -> bool {
        let mut state = self.state.write().await;
        let Some(current) = state.hosts.get_mut(&host_id) else {
            return false;
        };
        let Some(position) = current.routes.iter().position(|known| known == route) else {
            return false;
        };

        let removed_route = current.routes.remove(position);
        let removed_last_route = current.routes.is_empty();
        if removed_last_route {
            state.hosts.remove(&host_id);
            state.active_routes.remove(&host_id);
            state.client_visible_activity.remove(&host_id);
        } else if state.active_routes.get(&host_id) == Some(&removed_route) {
            state.active_routes.remove(&host_id);
        }
        state.routing_events.emit(RoutingEvent::HostDown {
            host_id,
            route: removed_route,
            origin_link,
        });
        if removed_last_route {
            state
                .host_events
                .emit(HostReachabilityEvent::Removed { host_id });
        }
        true
    }

    pub(crate) async fn remove_route_prefix(
        &self,
        prefix: &Route,
        origin_link: Option<Link>,
    ) -> Vec<RoutingEvent> {
        let mut state = self.state.write().await;
        let mut removals = state
            .hosts
            .iter()
            .flat_map(|(host_id, entry)| {
                entry
                    .routes
                    .iter()
                    .filter_map(|route| {
                        route
                            .starts_with_route(prefix)
                            .then_some((*host_id, route.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        removals.sort_by_key(|(host_id, _)| *host_id);
        let mut events = Vec::with_capacity(removals.len());

        for (host_id, route) in removals {
            let removed_last_route = if let Some(entry) = state.hosts.get_mut(&host_id) {
                entry.routes.retain(|known| known != &route);
                entry.routes.is_empty()
            } else {
                false
            };
            if removed_last_route {
                state.hosts.remove(&host_id);
                state.active_routes.remove(&host_id);
                state.client_visible_activity.remove(&host_id);
            } else if state.active_routes.get(&host_id) == Some(&route) {
                state.active_routes.remove(&host_id);
            }
            let event = RoutingEvent::HostDown {
                host_id,
                route,
                origin_link: origin_link.clone(),
            };
            state.routing_events.emit(event.clone());
            if removed_last_route {
                state
                    .host_events
                    .emit(HostReachabilityEvent::Removed { host_id });
            }
            events.push(event);
        }

        events
    }

    pub(crate) async fn remove_link_routes(&self, link: &Link) -> Vec<RoutingEvent> {
        self.remove_route_prefix(&Route::from_link(link.clone()), Some(link.clone()))
            .await
    }

    pub(crate) async fn remove_host_routes(
        &self,
        host_id: HostId,
        origin_link: Option<Link>,
    ) -> Vec<RoutingEvent> {
        let mut state = self.state.write().await;
        let Some(entry) = state.hosts.remove(&host_id) else {
            return Vec::new();
        };
        state.active_routes.remove(&host_id);
        state.client_visible_activity.remove(&host_id);

        let events = entry
            .routes
            .into_iter()
            .map(|route| RoutingEvent::HostDown {
                host_id,
                route,
                origin_link: origin_link.clone(),
            })
            .collect::<Vec<_>>();
        for event in &events {
            state.routing_events.emit(event.clone());
        }
        state
            .host_events
            .emit(HostReachabilityEvent::Removed { host_id });
        events
    }

    pub(crate) async fn subscribe_routing_events(&self) -> mpsc::Receiver<RoutingEvent> {
        self.state.write().await.routing_events.subscribe()
    }

    pub(crate) async fn routing_events_snapshot(&self) -> Vec<RoutingEvent> {
        let state = self.state.read().await;
        let mut hosts = state.hosts.iter().collect::<Vec<_>>();
        hosts.sort_unstable_by_key(|(host_id, _)| **host_id);
        hosts
            .into_iter()
            .flat_map(|(_, entry)| {
                entry.routes.iter().map(|route| RoutingEvent::HostUp {
                    host: entry.host.clone(),
                    route: route.clone(),
                    origin_link: None,
                })
            })
            .collect()
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
            .hosts
            .values()
            .map(|entry| entry.host.clone())
            .collect::<Vec<_>>();
        snapshot.sort_unstable_by_key(|host| host.id);
        let rx = state.host_events.subscribe();
        (snapshot, rx)
    }
}

impl HostRoutes {
    #[cfg(any(test, feature = "testnet"))]
    fn first_entry(&self) -> Option<HostEntry> {
        self.routes.first().cloned().map(|route| HostEntry {
            host: self.host.clone(),
            route,
        })
    }
}

struct EvictedHost {
    host_id: HostId,
    routes: Vec<Route>,
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

fn evict_route_for_cap(entry: &mut HostRoutes, active_route: Option<&Route>) -> Option<Route> {
    let position = entry
        .routes
        .iter()
        .position(|route| Some(route) != active_route)?;
    Some(entry.routes.remove(position))
}

fn evict_host_for_cap(
    state: &mut RoutingState,
    trusted_hosts: &HashSet<HostId>,
) -> Option<EvictedHost> {
    let now = Instant::now();
    prune_client_visible_activity(state, now);
    let candidate = state
        .hosts
        .iter()
        .filter(|(host_id, _)| !trusted_hosts.contains(host_id))
        .filter(|(host_id, _)| !state.active_routes.contains_key(host_id))
        .filter(|(host_id, _)| !has_recent_client_visible_activity(state, **host_id, now))
        .min_by_key(|(_, entry)| entry.inserted_sequence)
        .map(|(host_id, _)| *host_id)?;
    let entry = state.hosts.remove(&candidate)?;
    state.client_visible_activity.remove(&candidate);
    Some(EvictedHost {
        host_id: candidate,
        routes: entry.routes,
    })
}

fn untrusted_host_count(state: &RoutingState, trusted_hosts: &HashSet<HostId>) -> usize {
    state
        .hosts
        .keys()
        .filter(|host_id| !trusted_hosts.contains(host_id))
        .count()
}

fn emit_host_eviction(state: &mut RoutingState, evicted: EvictedHost) {
    for route in evicted.routes {
        state.routing_events.emit(RoutingEvent::HostDown {
            host_id: evicted.host_id,
            route,
            origin_link: None,
        });
    }
    state.host_events.emit(HostReachabilityEvent::Removed {
        host_id: evicted.host_id,
    });
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

fn link_is_used(state: &RoutingState, link: &Link) -> bool {
    state.links.contains(link)
}

pub(crate) fn select_best_route(routes: &[Route]) -> Option<Route> {
    routes.iter().min_by_key(|route| route.len()).cloned()
}

fn link_assignment_candidate(proposed: &Link, attempt: usize, suffix_len: usize) -> Link {
    if attempt == 0 {
        return proposed.clone();
    }

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..suffix_len];
    let max_base_len = 128 - 1 - suffix_len;
    let base = proposed.as_str();
    let base = &base[..base.len().min(max_base_len)];
    Link::new(format!("{base}-{suffix}")).expect("candidate link is derived from a valid link")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{DateTime, Utc};

    use super::*;
    use crate::routing::{Capabilities, SupportedAgentType};
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

    fn route(link: &str) -> Route {
        Route::from_link(Link::new(link).unwrap())
    }

    fn route_path(links: &[&str]) -> Route {
        Route::from_links(links.iter().map(|link| (*link).to_string())).unwrap()
    }

    async fn routes_for(core: &RoutingCore, host_id: HostId) -> Vec<Route> {
        let mut routes = core
            .routing_events_snapshot()
            .await
            .into_iter()
            .filter_map(|event| match event {
                RoutingEvent::HostUp {
                    host,
                    route,
                    origin_link: _,
                } if host.id == host_id => Some(route),
                _ => None,
            })
            .collect::<Vec<_>>();
        routes.sort_unstable_by_key(|route| {
            route
                .iter()
                .map(|link| link.as_str().to_string())
                .collect::<Vec<_>>()
        });
        routes
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
    async fn host_up_tracks_distinct_routes_and_deduplicates_exact_route() {
        let core = RoutingCore::new();
        let mut raw_rx = core.subscribe_routing_events().await;
        let mut host_rx = core.subscribe_hosts().await;

        assert_eq!(
            core.apply_host_up(host(1, "first"), route("a"), Some(Link::new("a").unwrap()))
                .await,
            HostUpOutcome::Inserted
        );
        assert_eq!(
            core.apply_host_up(host(1, "second"), route("b"), Some(Link::new("b").unwrap()))
                .await,
            HostUpOutcome::Inserted
        );
        assert_eq!(
            core.apply_host_up(host(1, "third"), route("a"), Some(Link::new("a").unwrap()))
                .await,
            HostUpOutcome::AlreadyKnown
        );
        assert_eq!(
            core.apply_host_up(host(1, "worse"), route_path(&["x", "b"]), None)
                .await,
            HostUpOutcome::AlreadyKnown
        );

        let entry = core.host_entry(HostId::from_u128(1)).await.unwrap();
        assert_eq!(entry.host.name, "first");
        assert_eq!(entry.route, route("a"));
        assert_eq!(
            core.routing_events_snapshot()
                .await
                .into_iter()
                .map(|event| match event {
                    RoutingEvent::HostUp { route, .. } => route,
                    RoutingEvent::HostDown { .. } => panic!("expected only HostUp events"),
                })
                .collect::<Vec<_>>(),
            vec![route("a"), route("b")]
        );

        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostUp { host, .. }) if host.name == "first")
        );
        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostUp { host, route: event_route, .. }) if host.id == HostId::from_u128(1) && event_route == route("b"))
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Added { host }) if host.name == "first")
        );
        assert!(raw_rx.try_recv().is_err());
        assert!(host_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn host_down_removes_one_route_and_keeps_host_until_last_route() {
        let core = RoutingCore::new();
        let mut raw_rx = core.subscribe_routing_events().await;
        let mut host_rx = core.subscribe_hosts().await;
        let peer = HostId::from_u128(2);
        let multi_hop = route_path(&["relay", "peer"]);
        let direct = route("direct");

        core.apply_host_up(host(2, "remote"), multi_hop.clone(), None)
            .await;
        core.apply_host_up(host(2, "remote"), direct.clone(), None)
            .await;
        assert!(!core.apply_host_down(peer, &route("missing"), None).await);
        assert_eq!(core.host_entry(peer).await.unwrap().route, multi_hop);

        for _ in 0..2 {
            assert!(matches!(
                raw_rx.recv().await,
                Some(RoutingEvent::HostUp { .. })
            ));
        }
        assert!(matches!(
            host_rx.recv().await,
            Some(HostReachabilityEvent::Added { .. })
        ));
        assert!(host_rx.try_recv().is_err());

        assert!(
            core.apply_host_down(peer, &direct, Some(Link::new("direct").unwrap()))
                .await
        );
        assert_eq!(core.host_entry(peer).await.unwrap().route, multi_hop);

        assert!(matches!(
            raw_rx.recv().await,
            Some(RoutingEvent::HostDown { host_id, route, .. }) if host_id == peer && route == direct
        ));
        assert!(host_rx.try_recv().is_err());

        assert!(core.apply_host_down(peer, &multi_hop, None).await);
        assert!(core.host_entry(peer).await.is_none());
        assert!(matches!(
            raw_rx.recv().await,
            Some(RoutingEvent::HostDown { host_id, route, .. }) if host_id == peer && route == multi_hop
        ));
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Removed { host_id }) if host_id == peer)
        );
    }

    #[tokio::test]
    async fn link_route_removal_cascades_matching_host_routes() {
        let core = RoutingCore::new();
        let mut raw_rx = core.subscribe_routing_events().await;
        let mut host_rx = core.subscribe_hosts().await;

        let relay = Link::new("relay").unwrap();
        let other = Link::new("other").unwrap();
        let relay_child = Route::from_links(["relay".to_string(), "child".to_string()]).unwrap();
        core.apply_host_up(host(1, "direct"), Route::from_link(relay.clone()), None)
            .await;
        core.apply_host_up(host(2, "child"), relay_child.clone(), None)
            .await;
        core.apply_host_up(host(3, "other"), Route::from_link(other), None)
            .await;

        let removed_events = core.remove_link_routes(&relay).await;
        assert_eq!(
            removed_events
                .iter()
                .map(|event| match event {
                    RoutingEvent::HostDown { host_id, .. } => *host_id,
                    RoutingEvent::HostUp { .. } => panic!("expected only HostDown events"),
                })
                .collect::<Vec<_>>(),
            vec![HostId::from_u128(1), HostId::from_u128(2)]
        );
        assert!(core.host_entry(HostId::from_u128(1)).await.is_none());
        assert!(core.host_entry(HostId::from_u128(2)).await.is_none());
        assert!(core.host_entry(HostId::from_u128(3)).await.is_some());

        for _ in 0..3 {
            assert!(matches!(
                raw_rx.recv().await,
                Some(RoutingEvent::HostUp { .. })
            ));
            assert!(matches!(
                host_rx.recv().await,
                Some(HostReachabilityEvent::Added { .. })
            ));
        }

        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostDown { host_id, route: event_route, origin_link }) if host_id == HostId::from_u128(1) && event_route == route("relay") && origin_link == Some(relay.clone()))
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Removed { host_id }) if host_id == HostId::from_u128(1))
        );
        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostDown { host_id, route: event_route, origin_link }) if host_id == HostId::from_u128(2) && event_route == relay_child && origin_link == Some(relay))
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::Removed { host_id }) if host_id == HostId::from_u128(2))
        );
        assert!(raw_rx.try_recv().is_err());
        assert!(host_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn link_reservation_suffixes_collisions_and_releases_names() {
        let core = RoutingCore::new();
        let proposed = Link::new("peer").unwrap();

        assert!(core.reserve_exact_link(&proposed).await);
        assert!(!core.reserve_exact_link(&proposed).await);

        let assigned = core.reserve_link(&proposed).await;
        assert_ne!(assigned, proposed);
        assert!(assigned.as_str().starts_with("peer-"));

        core.release_link(&proposed).await;
        assert!(core.reserve_exact_link(&proposed).await);
    }

    #[tokio::test]
    async fn link_reservation_ignores_downstream_route_hop_names() {
        let core = RoutingCore::new();
        let downstream = Link::new("downstream").unwrap();
        core.apply_host_up(
            host(1, "remote"),
            route_path(&["relay", "downstream"]),
            None,
        )
        .await;

        assert!(core.reserve_exact_link(&downstream).await);
    }

    #[tokio::test]
    async fn subscribe_with_snapshot_registers_before_later_events() {
        let core = RoutingCore::new();
        core.apply_host_up(host(1, "one"), route("a"), None).await;

        let (snapshot, mut rx) = core.subscribe_hosts_with_snapshot().await;
        assert_eq!(
            snapshot
                .into_iter()
                .map(|host| host.name)
                .collect::<Vec<_>>(),
            ["one"]
        );

        core.apply_host_up(host(2, "two"), route("b"), None).await;
        assert!(
            matches!(rx.recv().await, Some(HostReachabilityEvent::Added { host }) if host.name == "two")
        );
    }

    #[tokio::test]
    async fn route_cap_evicts_oldest_non_active_route() {
        let core = RoutingCore::new();
        let peer = HostId::from_u128(1);
        for index in 0..ROUTES_PER_HOST_CAP {
            assert_eq!(
                core.apply_host_up(host(1, "peer"), route(&format!("r{index}")), None)
                    .await,
                HostUpOutcome::Inserted
            );
        }
        core.mark_active_route(peer, Some(route("r0"))).await;

        assert_eq!(
            core.apply_host_up(host(1, "peer"), route("r16"), None)
                .await,
            HostUpOutcome::Inserted
        );

        let routes = routes_for(&core, peer).await;
        assert_eq!(routes.len(), ROUTES_PER_HOST_CAP);
        assert!(routes.contains(&route("r0")));
        assert!(routes.contains(&route("r16")));
        assert!(!routes.contains(&route("r1")));
    }

    #[tokio::test]
    async fn host_cap_evicts_oldest_host_without_active_trust_or_recent_activity() {
        let trusted = HostId::from_u128(10_000);
        let core = RoutingCore::with_trust_store(Arc::new(std::sync::RwLock::new(
            trust_store_with(trusted),
        )));
        assert_eq!(
            core.apply_host_up(host(10_000, "trusted"), route("trusted"), None)
                .await,
            HostUpOutcome::Inserted
        );
        for id in 1..=ROUTING_HOST_CAP as u128 {
            assert_eq!(
                core.apply_host_up(
                    host(id, &format!("host-{id}")),
                    route(&format!("r{id}")),
                    None
                )
                .await,
                HostUpOutcome::Inserted
            );
        }
        core.mark_active_route(HostId::from_u128(1), Some(route("r1")))
            .await;
        core.mark_client_visible_hosts(&[HostId::from_u128(2)])
            .await;

        assert_eq!(
            core.apply_host_up(host(1001, "new"), route("r1001"), None)
                .await,
            HostUpOutcome::Inserted
        );

        assert!(core.host_entry(HostId::from_u128(1)).await.is_some());
        assert!(core.host_entry(HostId::from_u128(2)).await.is_some());
        assert!(core.host_entry(trusted).await.is_some());
        assert!(core.host_entry(HostId::from_u128(3)).await.is_none());
        assert!(core.host_entry(HostId::from_u128(1001)).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn host_cap_allows_client_visible_activity_to_expire() {
        let core = RoutingCore::new();
        for id in 1..=ROUTING_HOST_CAP as u128 {
            core.apply_host_up(
                host(id, &format!("host-{id}")),
                route(&format!("r{id}")),
                None,
            )
            .await;
        }
        core.mark_active_route(HostId::from_u128(1), Some(route("r1")))
            .await;
        core.mark_client_visible_hosts(&[HostId::from_u128(2)])
            .await;
        tokio::time::advance(CLIENT_VISIBLE_ACTIVITY_RECENT_WINDOW + Duration::from_secs(1)).await;

        assert_eq!(
            core.apply_host_up(host(1001, "new"), route("r1001"), None)
                .await,
            HostUpOutcome::Inserted
        );

        assert!(core.host_entry(HostId::from_u128(1)).await.is_some());
        assert!(core.host_entry(HostId::from_u128(2)).await.is_none());
        assert!(core.host_entry(HostId::from_u128(1001)).await.is_some());
    }
}
