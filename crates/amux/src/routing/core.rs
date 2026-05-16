use std::collections::{HashMap, HashSet};

use tokio::sync::{RwLock, mpsc};

use crate::HostId;
use crate::routing::events::{EventSource, HostReachabilityEvent, RoutingEvent};
use crate::routing::types::Host;
use crate::routing::{Link, Route};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostEntry {
    pub(crate) host: Host,
    pub(crate) route: Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostUpOutcome {
    Inserted,
    AlreadyKnown,
}

#[derive(Default)]
struct RoutingState {
    hosts: HashMap<HostId, HostEntry>,
    links: HashSet<Link>,
    routing_events: EventSource<RoutingEvent>,
    host_events: EventSource<HostReachabilityEvent>,
}

#[derive(Default)]
pub(crate) struct RoutingCore {
    state: RwLock<RoutingState>,
}

impl RoutingCore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn host_entry(&self, host_id: HostId) -> Option<HostEntry> {
        self.state.read().await.hosts.get(&host_id).cloned()
    }

    #[cfg(test)]
    pub(crate) async fn hosts_snapshot(&self) -> Vec<HostEntry> {
        let state = self.state.read().await;
        let mut hosts = state.hosts.values().cloned().collect::<Vec<_>>();
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

    pub(crate) async fn apply_host_up(
        &self,
        host: Host,
        route: Route,
        origin_link: Option<Link>,
    ) -> HostUpOutcome {
        let mut state = self.state.write().await;
        if state.hosts.contains_key(&host.id) {
            return HostUpOutcome::AlreadyKnown;
        }

        let host_id = host.id;
        state.hosts.insert(
            host_id,
            HostEntry {
                host: host.clone(),
                route: route.clone(),
            },
        );

        state.routing_events.emit(RoutingEvent::HostUp {
            host: host.clone(),
            route,
            origin_link,
        });
        state
            .host_events
            .emit(HostReachabilityEvent::HostAdded { host });

        HostUpOutcome::Inserted
    }

    pub(crate) async fn apply_host_down(
        &self,
        host_id: HostId,
        route: &Route,
        origin_link: Option<Link>,
    ) -> bool {
        let mut state = self.state.write().await;
        let Some(current) = state.hosts.get(&host_id) else {
            return false;
        };
        if current.route != *route {
            return false;
        }

        let removed = state
            .hosts
            .remove(&host_id)
            .expect("host existence checked above");
        state.routing_events.emit(RoutingEvent::HostDown {
            host_id,
            route: removed.route,
            origin_link,
        });
        state
            .host_events
            .emit(HostReachabilityEvent::HostRemoved { host_id });
        true
    }

    pub(crate) async fn remove_route_prefix(
        &self,
        prefix: &Route,
        origin_link: Option<Link>,
    ) -> Vec<RoutingEvent> {
        let mut state = self.state.write().await;
        let mut host_ids = state
            .hosts
            .iter()
            .filter_map(|(host_id, entry)| {
                entry.route.starts_with_route(prefix).then_some(*host_id)
            })
            .collect::<Vec<_>>();
        host_ids.sort_unstable();
        let mut events = Vec::with_capacity(host_ids.len());

        for host_id in &host_ids {
            let removed = state
                .hosts
                .remove(host_id)
                .expect("host id was collected from hosts table");
            let event = RoutingEvent::HostDown {
                host_id: *host_id,
                route: removed.route,
                origin_link: origin_link.clone(),
            };
            state.routing_events.emit(event.clone());
            state
                .host_events
                .emit(HostReachabilityEvent::HostRemoved { host_id: *host_id });
            events.push(event);
        }

        events
    }

    pub(crate) async fn remove_link_routes(&self, link: &Link) -> Vec<RoutingEvent> {
        self.remove_route_prefix(&Route::from_link(link.clone()), Some(link.clone()))
            .await
    }

    pub(crate) async fn subscribe_routing_events(&self) -> mpsc::Receiver<RoutingEvent> {
        self.state.write().await.routing_events.subscribe()
    }

    pub(crate) async fn routing_events_snapshot(&self) -> Vec<RoutingEvent> {
        let state = self.state.read().await;
        let mut snapshot = state
            .hosts
            .values()
            .map(|entry| RoutingEvent::HostUp {
                host: entry.host.clone(),
                route: entry.route.clone(),
                origin_link: None,
            })
            .collect::<Vec<_>>();
        snapshot.sort_unstable_by_key(|event| match event {
            RoutingEvent::HostUp { host, .. } => host.id,
            RoutingEvent::HostDown { host_id, .. } => *host_id,
        });
        snapshot
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

fn link_is_used(state: &RoutingState, link: &Link) -> bool {
    state.links.contains(link)
        || state
            .hosts
            .values()
            .any(|entry| entry.route.contains_link(link.as_str()))
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
    use super::*;
    use crate::routing::{Capabilities, SupportedAgentType};

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

    #[tokio::test]
    async fn first_host_up_wins_and_reannounce_is_ignored() {
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
            HostUpOutcome::AlreadyKnown
        );

        let entry = core.host_entry(HostId::from_u128(1)).await.unwrap();
        assert_eq!(entry.host.name, "first");
        assert_eq!(entry.route, route("a"));

        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostUp { host, .. }) if host.name == "first")
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::HostAdded { host }) if host.name == "first")
        );
        assert!(raw_rx.try_recv().is_err());
        assert!(host_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn host_down_only_removes_matching_route() {
        let core = RoutingCore::new();
        let mut raw_rx = core.subscribe_routing_events().await;
        let mut host_rx = core.subscribe_hosts().await;

        core.apply_host_up(host(2, "remote"), route("a"), None)
            .await;
        assert!(
            !core
                .apply_host_down(HostId::from_u128(2), &route("b"), None)
                .await
        );
        assert!(core.host_entry(HostId::from_u128(2)).await.is_some());

        assert!(
            core.apply_host_down(
                HostId::from_u128(2),
                &route("a"),
                Some(Link::new("a").unwrap())
            )
            .await
        );
        assert!(core.host_entry(HostId::from_u128(2)).await.is_none());

        assert!(matches!(
            raw_rx.recv().await,
            Some(RoutingEvent::HostUp { .. })
        ));
        assert!(matches!(
            host_rx.recv().await,
            Some(HostReachabilityEvent::HostAdded { .. })
        ));
        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostDown { host_id, .. }) if host_id == HostId::from_u128(2))
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::HostRemoved { host_id }) if host_id == HostId::from_u128(2))
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
                Some(HostReachabilityEvent::HostAdded { .. })
            ));
        }

        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostDown { host_id, route: event_route, origin_link }) if host_id == HostId::from_u128(1) && event_route == route("relay") && origin_link == Some(relay.clone()))
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::HostRemoved { host_id }) if host_id == HostId::from_u128(1))
        );
        assert!(
            matches!(raw_rx.recv().await, Some(RoutingEvent::HostDown { host_id, route: event_route, origin_link }) if host_id == HostId::from_u128(2) && event_route == relay_child && origin_link == Some(relay))
        );
        assert!(
            matches!(host_rx.recv().await, Some(HostReachabilityEvent::HostRemoved { host_id }) if host_id == HostId::from_u128(2))
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
            matches!(rx.recv().await, Some(HostReachabilityEvent::HostAdded { host }) if host.name == "two")
        );
    }
}
