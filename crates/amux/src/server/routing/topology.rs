use std::collections::{HashMap, HashSet};

use tokio::sync::mpsc;
use uuid::Uuid;

use crate::agent::Agent;
use crate::protocol::link::Link;
use crate::protocol::message::{Host, Message, RoutingEvent};
use crate::protocol::route::Route;
use crate::server::ConnectionHandle;
use crate::server::registry::{AgentRegistry, AgentRegistryError};

/// User-scoped routing topology facts.
///
/// This owns the graph-like state that says which links exist, which links are
/// peers, and which hosts and remote agents are reachable through those peers.
/// RPC call lifecycle stays in `ServerUserState::rpc`; topology only owns
/// reachability facts.
pub(in crate::server) struct Topology {
    pub(in crate::server) routes: HashMap<Link, ConnectionHandle>,
    pub(in crate::server) registry: AgentRegistry,
    pub(in crate::server) peer_links: HashSet<Link>,
    pub(in crate::server) hosts: HashMap<Uuid, Host>,
}

impl Topology {
    pub(in crate::server) fn new() -> Self {
        Self {
            routes: HashMap::new(),
            registry: AgentRegistry::new(),
            peer_links: HashSet::new(),
            hosts: HashMap::new(),
        }
    }

    /// Atomically register a connection under `link`, returning the handle and
    /// receive half of its outgoing channel.
    pub(in crate::server) fn try_reserve_link(
        &mut self,
        link: Link,
    ) -> Result<(ConnectionHandle, mpsc::Receiver<Message>), Link> {
        if self.routes.contains_key(&link) {
            return Err(link);
        }
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Message>(256);
        let handle = ConnectionHandle::new(outgoing_tx);
        self.routes.insert(link, handle.clone());
        Ok((handle, outgoing_rx))
    }

    pub(in crate::server) fn mark_peer_link(&mut self, link: Link) {
        self.peer_links.insert(link);
    }

    pub(in crate::server) fn remove_link(&mut self, link: &Link) {
        self.routes.remove(link);
        self.peer_links.remove(link);
    }

    pub(in crate::server) fn route(&self, link: &Link) -> Option<ConnectionHandle> {
        self.routes.get(link).cloned()
    }

    pub(in crate::server) fn register_local_agent(
        &mut self,
        agent: Agent,
    ) -> Result<TopologyEvent, AgentRegistryError> {
        self.registry.register_local(agent.clone())?;
        Ok(TopologyEvent::AgentUp { agent })
    }

    pub(in crate::server) fn update_local_agent(
        &mut self,
        agent: Agent,
    ) -> Result<TopologyEvent, AgentRegistryError> {
        self.registry.update_local(agent.clone())?;
        Ok(TopologyEvent::AgentUp { agent })
    }

    pub(in crate::server) fn remove_agent(&mut self, agent_id: Uuid) -> Option<AgentRemovedChange> {
        let agent = self.registry.remove(&agent_id)?;
        Some(AgentRemovedChange {
            agent,
            event: TopologyEvent::AgentDown { agent_id },
        })
    }

    pub(in crate::server) fn apply_peer_agent_up(
        &mut self,
        from: &Link,
        mut agent: Agent,
    ) -> PeerAgentUpChange {
        if self
            .registry
            .get(&agent.id)
            .is_some_and(|existing| !existing.is_remote())
        {
            return PeerAgentUpChange::ignored(PeerAgentUpIgnored::LocalAgent);
        }

        let Some(host) = self.hosts.get(&agent.host_id) else {
            return PeerAgentUpChange::ignored(PeerAgentUpIgnored::UnknownHost);
        };
        if !matches!(host.route.peek(), Some(link) if link == from) {
            return PeerAgentUpChange::ignored(PeerAgentUpIgnored::NonSelectedHostRoute);
        }

        let agent_id = agent.id;
        agent.route = host.route.clone();
        if let Err(error) = self.registry.register_remote(agent) {
            return PeerAgentUpChange::ignored(PeerAgentUpIgnored::InvalidAgent {
                message: error.to_string(),
            });
        }
        let agent = self
            .registry
            .get(&agent_id)
            .expect("registered remote agent should be readable")
            .clone();
        PeerAgentUpChange {
            event: Some(TopologyEvent::AgentUp { agent }),
            ignored: None,
        }
    }

    pub(in crate::server) fn apply_peer_agent_down(
        &mut self,
        from: &Link,
        agent_id: Uuid,
    ) -> PeerAgentDownChange {
        let Some(agent) = self.registry.get(&agent_id) else {
            return PeerAgentDownChange::ignored(PeerAgentDownIgnored::UnknownAgent);
        };
        if !agent.is_remote() {
            return PeerAgentDownChange::ignored(PeerAgentDownIgnored::LocalAgent);
        }
        if !self
            .hosts
            .get(&agent.host_id)
            .is_some_and(|host| matches!(host.route.peek(), Some(link) if link == from))
        {
            return PeerAgentDownChange::ignored(PeerAgentDownIgnored::NonSelectedHostRoute);
        }

        PeerAgentDownChange {
            removed: self.remove_agent(agent_id),
            ignored: None,
        }
    }

    pub(in crate::server) fn apply_peer_host_up(
        &mut self,
        from: &Link,
        id: Uuid,
        name: String,
        received_route: Route,
        version: String,
    ) -> PeerHostUpChange {
        let old_route = self.hosts.get(&id).map(|host| host.route.clone());

        let mut route = received_route;
        route.push(from.clone());

        let host = Host {
            id,
            name,
            route: route.clone(),
            version,
        };
        self.hosts.insert(id, host.clone());

        let rewritten_hosts = old_route
            .as_ref()
            .map(|old_route| self.rewrite_descendant_host_routes(id, old_route, &route))
            .unwrap_or_default();
        let rewritten_descendants = rewritten_hosts.len();
        let mut events = Vec::with_capacity(1 + rewritten_descendants);
        events.push(TopologyEvent::HostUp { host });
        events.extend(
            rewritten_hosts
                .into_iter()
                .map(|host| TopologyEvent::HostUp { host }),
        );

        PeerHostUpChange {
            events,
            rewritten_descendants,
        }
    }

    pub(in crate::server) fn apply_peer_host_down(
        &mut self,
        from: &Link,
        id: Uuid,
        received_route: Route,
    ) -> PeerHostDownChange {
        let mut route = received_route;
        route.push(from.clone());

        let root_matches = self.hosts.get(&id).is_some_and(|host| host.route == route);
        if !root_matches {
            return PeerHostDownChange {
                event: None,
                root_matches,
                removed_agents: 0,
                removed_descendants: 0,
                effect: None,
            };
        }

        let removed_agents = {
            let hosts = &self.hosts;
            self.registry
                .remove_where(
                    |host_id| hosts.get(&host_id).map(|host| host.route.clone()),
                    |host_route| host_route.starts_with_route(&route),
                )
                .len()
        };

        let removed_descendants = self.remove_descendant_hosts(id, &route);

        if root_matches {
            self.hosts.remove(&id);
        }

        PeerHostDownChange {
            event: Some(TopologyEvent::HostDown {
                id,
                route: route.clone(),
            }),
            root_matches,
            removed_agents,
            removed_descendants,
            effect: Some(TopologyEffect::CancelSessionsForRoute {
                route_prefix: route,
            }),
        }
    }

    pub(in crate::server) fn apply_link_closed(&mut self, link: &Link) -> LinkClosedChange {
        self.remove_link(link);

        let prefix = Route::from_link(link.clone());
        let removed_agents = {
            let hosts = &self.hosts;
            self.registry
                .remove_where(
                    |host_id| hosts.get(&host_id).map(|host| host.route.clone()),
                    |route| route.starts_with_route(&prefix),
                )
                .len()
        };

        let removed_hosts = self.disconnected_hosts(link);
        let events = disconnected_host_roots(&removed_hosts)
            .into_iter()
            .map(|(id, route)| TopologyEvent::HostDown { id, route })
            .collect();
        for (id, _) in &removed_hosts {
            self.hosts.remove(id);
        }

        LinkClosedChange {
            events,
            removed_agents,
            removed_hosts: removed_hosts.len(),
            effect: TopologyEffect::CancelSessionsForClosedLink { link: link.clone() },
        }
    }

    fn descendant_host_ids(&self, root_host_id: Uuid, route_prefix: &Route) -> Vec<Uuid> {
        let mut ids: Vec<_> = self
            .hosts
            .iter()
            .filter(|(id, host)| **id != root_host_id && host.route.starts_with_route(route_prefix))
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable_by_key(|id| id.as_u128());
        ids
    }

    fn rewrite_descendant_host_routes(
        &mut self,
        root_host_id: Uuid,
        old_route: &Route,
        new_route: &Route,
    ) -> Vec<Host> {
        if old_route == new_route {
            return Vec::new();
        }

        self.descendant_host_ids(root_host_id, old_route)
            .into_iter()
            .map(|id| {
                let host = self
                    .hosts
                    .get_mut(&id)
                    .expect("descendant host should still exist while rewriting routes");
                let replaced = host.route.replace_prefix(old_route, new_route);
                debug_assert!(replaced, "descendant route should still match old prefix");
                host.clone()
            })
            .collect()
    }

    fn remove_descendant_hosts(&mut self, root_host_id: Uuid, route_prefix: &Route) -> usize {
        self.descendant_host_ids(root_host_id, route_prefix)
            .into_iter()
            .filter(|id| self.hosts.remove(id).is_some())
            .count()
    }

    fn disconnected_hosts(&self, link: &Link) -> Vec<(Uuid, Route)> {
        let prefix = Route::from_link(link.clone());
        let mut hosts: Vec<_> = self
            .hosts
            .iter()
            .filter(|(_, info)| info.route.starts_with_route(&prefix))
            .map(|(id, info)| (*id, info.route.clone()))
            .collect();
        hosts.sort_unstable_by(|(id_a, route_a), (id_b, route_b)| {
            route_a
                .to_string()
                .cmp(&route_b.to_string())
                .then_with(|| id_a.as_u128().cmp(&id_b.as_u128()))
        });
        hosts
    }
}

fn disconnected_host_roots(hosts: &[(Uuid, Route)]) -> Vec<(Uuid, Route)> {
    hosts
        .iter()
        .filter(|(_, route)| {
            !hosts.iter().any(|(_, other_route)| {
                route != other_route && route.starts_with_route(other_route)
            })
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) enum TopologyEvent {
    HostUp { host: Host },
    HostDown { id: Uuid, route: Route },
    AgentUp { agent: Agent },
    AgentDown { agent_id: Uuid },
}

#[derive(Debug, Clone)]
pub(in crate::server) enum TopologyEffect {
    CancelSessionsForRoute { route_prefix: Route },
    CancelSessionsForClosedLink { link: Link },
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PeerHostUpChange {
    pub(in crate::server) events: Vec<TopologyEvent>,
    pub(in crate::server) rewritten_descendants: usize,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PeerHostDownChange {
    pub(in crate::server) event: Option<TopologyEvent>,
    pub(in crate::server) root_matches: bool,
    pub(in crate::server) removed_agents: usize,
    pub(in crate::server) removed_descendants: usize,
    pub(in crate::server) effect: Option<TopologyEffect>,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct AgentRemovedChange {
    pub(in crate::server) agent: Agent,
    pub(in crate::server) event: TopologyEvent,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PeerAgentUpChange {
    pub(in crate::server) event: Option<TopologyEvent>,
    pub(in crate::server) ignored: Option<PeerAgentUpIgnored>,
}

impl PeerAgentUpChange {
    fn ignored(ignored: PeerAgentUpIgnored) -> Self {
        Self {
            event: None,
            ignored: Some(ignored),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::server) enum PeerAgentUpIgnored {
    LocalAgent,
    UnknownHost,
    NonSelectedHostRoute,
    InvalidAgent { message: String },
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PeerAgentDownChange {
    pub(in crate::server) removed: Option<AgentRemovedChange>,
    pub(in crate::server) ignored: Option<PeerAgentDownIgnored>,
}

impl PeerAgentDownChange {
    fn ignored(ignored: PeerAgentDownIgnored) -> Self {
        Self {
            removed: None,
            ignored: Some(ignored),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::server) enum PeerAgentDownIgnored {
    UnknownAgent,
    LocalAgent,
    NonSelectedHostRoute,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct LinkClosedChange {
    pub(in crate::server) events: Vec<TopologyEvent>,
    pub(in crate::server) removed_agents: usize,
    pub(in crate::server) removed_hosts: usize,
    pub(in crate::server) effect: TopologyEffect,
}

impl TopologyEvent {
    pub(in crate::server) fn to_routing_event(&self) -> RoutingEvent {
        match self {
            Self::HostUp { host } => RoutingEvent::HostUp {
                id: host.id,
                name: host.name.clone(),
                route: host.route.clone(),
                version: host.version.clone(),
            },
            Self::HostDown { id, route } => RoutingEvent::HostDown {
                id: *id,
                route: route.clone(),
            },
            Self::AgentUp { agent } => agent.routing_event(),
            Self::AgentDown { agent_id } => RoutingEvent::AgentDown {
                agent_id: *agent_id,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;

    use super::*;

    fn link(name: &str) -> Link {
        Link::new(name).unwrap()
    }

    fn route<const N: usize>(links: [&str; N]) -> Route {
        Route::from_links(links.into_iter().map(str::to_string)).unwrap()
    }

    fn host(id: Uuid, route: Route) -> Host {
        Host {
            id,
            name: format!("host-{id}"),
            route,
            version: "test".to_string(),
        }
    }

    fn remote_agent(id: Uuid, host_id: Uuid, route: Route) -> Agent {
        Agent {
            id,
            host_id,
            name: Some(format!("agent-{id}")),
            command: "test-agent".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route,
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        }
    }

    fn local_agent(id: Uuid, host_id: Uuid) -> Agent {
        Agent {
            id,
            host_id,
            name: Some(format!("agent-{id}")),
            command: "test-agent".to_string(),
            working_dir: PathBuf::from("/tmp"),
            route: Route::empty(),
            agent_type: "test".to_string(),
            io_protocols: Vec::new(),
            readonly: false,
            args: Vec::new(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn register_local_agent_returns_canonical_agent_up_event() {
        let mut topology = Topology::new();
        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        let agent = local_agent(agent_id, host_id);

        let event = topology.register_local_agent(agent.clone()).unwrap();

        assert!(matches!(
            event,
            TopologyEvent::AgentUp { agent: event_agent }
                if event_agent.id == agent_id && event_agent.host_id == host_id
        ));
        assert_eq!(topology.registry.get(&agent_id).unwrap().id, agent.id);
    }

    #[test]
    fn remove_agent_returns_removed_agent_and_agent_down_event() {
        let mut topology = Topology::new();
        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        topology
            .register_local_agent(local_agent(agent_id, host_id))
            .unwrap();

        let change = topology
            .remove_agent(agent_id)
            .expect("registered agent should be removed");

        assert_eq!(change.agent.id, agent_id);
        assert!(matches!(
            change.event,
            TopologyEvent::AgentDown {
                agent_id: event_agent_id,
            } if event_agent_id == agent_id
        ));
        assert!(topology.registry.get(&agent_id).is_none());
    }

    #[test]
    fn update_local_agent_returns_canonical_agent_up_event() {
        let mut topology = Topology::new();
        let agent_id = Uuid::new_v4();
        let host_id = Uuid::new_v4();
        topology
            .register_local_agent(local_agent(agent_id, host_id))
            .unwrap();

        let mut updated = topology.registry.get(&agent_id).unwrap().clone();
        updated.name = Some("renamed".to_string());
        let event = topology.update_local_agent(updated).unwrap();

        assert!(matches!(
            event,
            TopologyEvent::AgentUp { agent }
                if agent.id == agent_id && agent.name.as_deref() == Some("renamed")
        ));
        assert_eq!(
            topology
                .registry
                .get(&agent_id)
                .and_then(|agent| agent.name.as_deref()),
            Some("renamed")
        );
    }

    #[test]
    fn peer_agent_up_uses_selected_host_route_and_returns_agent_up_event() {
        let mut topology = Topology::new();
        let host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        topology
            .hosts
            .insert(host_id, host(host_id, route(["peer-a"])));

        let change = topology.apply_peer_agent_up(
            &link("peer-a"),
            remote_agent(agent_id, host_id, Route::empty()),
        );

        assert!(change.ignored.is_none());
        assert!(matches!(
            change.event,
            Some(TopologyEvent::AgentUp { ref agent })
                if agent.id == agent_id && agent.route == route(["peer-a"])
        ));
        assert_eq!(
            topology.registry.get(&agent_id).unwrap().route,
            route(["peer-a"])
        );
    }

    #[test]
    fn peer_agent_up_ignores_non_selected_host_route() {
        let mut topology = Topology::new();
        let host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        topology
            .hosts
            .insert(host_id, host(host_id, route(["peer-a"])));

        let change = topology.apply_peer_agent_up(
            &link("peer-b"),
            remote_agent(agent_id, host_id, Route::empty()),
        );

        assert!(matches!(
            change.ignored,
            Some(PeerAgentUpIgnored::NonSelectedHostRoute)
        ));
        assert!(change.event.is_none());
        assert!(topology.registry.get(&agent_id).is_none());
    }

    #[test]
    fn peer_agent_down_removes_only_when_selected_host_route_matches() {
        let mut topology = Topology::new();
        let host_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        topology
            .hosts
            .insert(host_id, host(host_id, route(["peer-a"])));
        topology.apply_peer_agent_up(
            &link("peer-a"),
            remote_agent(agent_id, host_id, Route::empty()),
        );

        let ignored = topology.apply_peer_agent_down(&link("peer-b"), agent_id);
        assert!(matches!(
            ignored.ignored,
            Some(PeerAgentDownIgnored::NonSelectedHostRoute)
        ));
        assert!(topology.registry.get(&agent_id).is_some());

        let removed = topology.apply_peer_agent_down(&link("peer-a"), agent_id);
        assert!(matches!(
            removed.removed,
            Some(AgentRemovedChange {
                event: TopologyEvent::AgentDown { agent_id: event_agent_id },
                ..
            }) if event_agent_id == agent_id
        ));
        assert!(topology.registry.get(&agent_id).is_none());
    }

    #[test]
    fn peer_host_up_rewrites_descendant_routes() {
        let mut topology = Topology::new();
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        topology
            .hosts
            .insert(root_id, host(root_id, route(["peer-a"])));
        topology
            .hosts
            .insert(child_id, host(child_id, route(["peer-a", "child"])));

        let change = topology.apply_peer_host_up(
            &link("peer-b"),
            root_id,
            "root".to_string(),
            Route::empty(),
            "v1".to_string(),
        );

        assert_eq!(change.rewritten_descendants, 1);
        assert_eq!(
            topology.hosts.get(&root_id).unwrap().route,
            route(["peer-b"])
        );
        assert_eq!(
            topology.hosts.get(&child_id).unwrap().route,
            route(["peer-b", "child"])
        );
        assert_eq!(change.events.len(), 2);
        assert!(matches!(change.events[0], TopologyEvent::HostUp { .. }));
        assert!(matches!(change.events[1], TopologyEvent::HostUp { .. }));
    }

    #[test]
    fn peer_host_down_removes_descendants_and_returns_cleanup_effect() {
        let mut topology = Topology::new();
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        topology
            .hosts
            .insert(root_id, host(root_id, route(["peer-a"])));
        topology
            .hosts
            .insert(child_id, host(child_id, route(["peer-a", "child"])));
        topology
            .registry
            .register_remote(remote_agent(agent_id, child_id, route(["peer-a", "child"])))
            .unwrap();

        let change = topology.apply_peer_host_down(&link("peer-a"), root_id, Route::empty());

        assert!(change.root_matches);
        assert_eq!(change.removed_agents, 1);
        assert_eq!(change.removed_descendants, 1);
        assert!(!topology.hosts.contains_key(&root_id));
        assert!(!topology.hosts.contains_key(&child_id));
        assert_eq!(topology.registry.count_remote(), 0);
        assert!(matches!(change.event, Some(TopologyEvent::HostDown { .. })));
        assert!(matches!(
            change.effect,
            Some(TopologyEffect::CancelSessionsForRoute { ref route_prefix })
                if *route_prefix == route(["peer-a"])
        ));
    }

    #[test]
    fn peer_host_down_with_wrong_id_does_not_remove_matching_route_subtree() {
        let mut topology = Topology::new();
        let root_id = Uuid::new_v4();
        let wrong_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();
        let agent_id = Uuid::new_v4();
        topology
            .hosts
            .insert(root_id, host(root_id, route(["peer-a"])));
        topology
            .hosts
            .insert(child_id, host(child_id, route(["peer-a", "child"])));
        topology
            .registry
            .register_remote(remote_agent(agent_id, child_id, route(["peer-a", "child"])))
            .unwrap();

        let change = topology.apply_peer_host_down(&link("peer-a"), wrong_id, Route::empty());

        assert!(!change.root_matches);
        assert!(change.event.is_none());
        assert!(change.effect.is_none());
        assert_eq!(change.removed_agents, 0);
        assert_eq!(change.removed_descendants, 0);
        assert!(topology.hosts.contains_key(&root_id));
        assert!(topology.hosts.contains_key(&child_id));
        assert_eq!(topology.registry.count_remote(), 1);
    }

    #[test]
    fn link_closed_removes_routes_and_returns_host_down_events() {
        let mut topology = Topology::new();
        let link = link("peer-a");
        let root_id = Uuid::new_v4();
        let child_id = Uuid::new_v4();

        topology.try_reserve_link(link.clone()).unwrap();
        topology.mark_peer_link(link.clone());
        topology
            .hosts
            .insert(root_id, host(root_id, route(["peer-a"])));
        topology
            .hosts
            .insert(child_id, host(child_id, route(["peer-a", "child"])));

        let change = topology.apply_link_closed(&link);

        assert!(!topology.routes.contains_key(&link));
        assert!(!topology.peer_links.contains(&link));
        assert_eq!(change.removed_hosts, 2);
        assert!(matches!(
            change.events.as_slice(),
            [TopologyEvent::HostDown { id, route: event_route }]
                if *id == root_id && *event_route == route(["peer-a"])
        ));
        assert!(matches!(
            change.effect,
            TopologyEffect::CancelSessionsForClosedLink { link: ref closed_link }
                if closed_link == &link
        ));
    }
}
