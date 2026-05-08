use uuid::Uuid;

use crate::agent::Agent;
use crate::protocol::message::{Host, RoutingEvent};
use crate::protocol::route::Route;

#[derive(Debug, Clone)]
pub(crate) enum TopologyEvent {
    HostUp { host: Host, route: Route },
    HostDown { id: Uuid, route: Route },
    AgentUp { agent: Agent },
    AgentDown { agent_id: Uuid },
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
}

#[derive(Debug, Clone)]
pub(in crate::server) struct AgentRemovedChange {
    pub(in crate::server) event: TopologyEvent,
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PeerAgentUpChange {
    pub(in crate::server) event: Option<TopologyEvent>,
    pub(in crate::server) ignored: Option<PeerAgentUpIgnored>,
}

impl PeerAgentUpChange {
    pub(in crate::server) fn ignored(ignored: PeerAgentUpIgnored) -> Self {
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
}

#[derive(Debug, Clone)]
pub(in crate::server) struct PeerAgentDownChange {
    pub(in crate::server) removed: Option<AgentRemovedChange>,
    pub(in crate::server) ignored: Option<PeerAgentDownIgnored>,
}

impl PeerAgentDownChange {
    pub(in crate::server) fn ignored(ignored: PeerAgentDownIgnored) -> Self {
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
}

impl TopologyEvent {
    pub(in crate::server) fn to_routing_event(&self) -> RoutingEvent {
        match self {
            Self::HostUp { host, route } => RoutingEvent::HostUp {
                id: host.id,
                name: host.name.clone(),
                route: route.clone(),
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
