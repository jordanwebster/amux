//! Core routing types: hosts, link identities, and routes.
//!
//! A link is identified locally as `(peer host_id, connection instance)`;
//! nothing about a link's identity crosses the wire. A route is either
//! `Direct` (a link of our own) or `Via` (any adjacent relay).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::HostId;
use crate::protocol::wire::{self as protocol_wire, pb};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_agent_types: Vec<SupportedAgentType>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SupportedAgentType {
    pub agent_type: String,
}

/// Information about a connected host (machine running amux server).
/// Exchanged in the link handshake and in NeighborUp adjacency events.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Daemon-lifetime host ID announced to peers.
    pub id: Uuid,
    /// Human-readable hostname from config
    pub name: String,
    /// amux version of the host
    pub version: String,
    /// Host-level protocol and agent creation capabilities.
    pub capabilities: Capabilities,
}

/// Local identity of one link: the authenticated peer plus a connection
/// instance. Two links to the same peer differ only in `instance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId {
    pub(crate) peer: HostId,
    pub(crate) instance: Uuid,
}

impl LinkId {
    pub(crate) fn new(peer: HostId) -> Self {
        Self {
            peer,
            instance: Uuid::new_v4(),
        }
    }

    pub(crate) fn peer(&self) -> HostId {
        self.peer
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}#{}",
            self.peer.as_simple(),
            &self.instance.as_simple().to_string()[..8]
        )
    }
}

/// How this daemon reaches a host: over its own link, or through one
/// adjacent relay. There are no longer routes than this — forwarding is
/// non-recursive by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// Over a direct link of our own; the call's tunnel is pinned to it.
    Direct(LinkId),
    /// Through an adjacent relay that claims adjacency to the host.
    Via(HostId),
}

impl Route {
    pub(crate) fn is_direct(&self) -> bool {
        matches!(self, Route::Direct(_))
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Route::Direct(link) => write!(f, "direct({link})"),
            Route::Via(relay) => write!(f, "via({})", relay.as_simple()),
        }
    }
}

pub(crate) fn host_to_wire(host: &Host) -> pb::Host {
    pb::Host {
        host_id: host.id.as_bytes().to_vec(),
        name: host.name.clone(),
        version: host.version.clone(),
        capabilities: Some(capabilities_to_wire(&host.capabilities)),
    }
}

pub(crate) fn host_from_wire(host: pb::Host) -> Result<Host, protocol_wire::DecodeError> {
    Ok(Host {
        id: uuid_from_bytes("host_id", host.host_id)?,
        name: host.name,
        version: host.version,
        capabilities: capabilities_from_wire(host.capabilities)?,
    })
}

pub(crate) fn capabilities_to_wire(capabilities: &Capabilities) -> pb::Capabilities {
    pb::Capabilities {
        features: capabilities.features.clone(),
        supported_agents: capabilities
            .supported_agent_types
            .iter()
            .map(|agent| pb::SupportedAgentType {
                kind: Some(crate::agents::agent_kind_to_wire(
                    crate::agents::AgentKind::from_legacy(&agent.agent_type, &[])
                        .expect("locally advertised agent kind must be known"),
                )),
            })
            .collect(),
    }
}

pub(crate) fn capabilities_from_wire(
    capabilities: Option<pb::Capabilities>,
) -> Result<Capabilities, protocol_wire::DecodeError> {
    let Some(capabilities) = capabilities else {
        return Ok(Capabilities::default());
    };
    Ok(Capabilities {
        features: capabilities.features,
        supported_agent_types: capabilities
            .supported_agents
            .into_iter()
            .map(|agent| {
                let kind = crate::agents::agent_kind_from_wire(agent.kind.ok_or_else(|| {
                    protocol_wire::DecodeError::Invalid(
                        "SupportedAgentType missing kind".to_string(),
                    )
                })?)?;
                Ok(SupportedAgentType {
                    agent_type: kind.provider().to_string(),
                })
            })
            .collect::<Result<Vec<_>, protocol_wire::DecodeError>>()?,
    })
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, protocol_wire::DecodeError> {
    let bytes: [u8; 16] = bytes.try_into().map_err(|bytes: Vec<u8>| {
        protocol_wire::DecodeError::Invalid(format!("{name} must be 16 bytes, got {}", bytes.len()))
    })?;
    Ok(Uuid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_ids_to_one_peer_differ_by_instance() {
        let peer = HostId::from_u128(7);
        let first = LinkId::new(peer);
        let second = LinkId::new(peer);

        assert_eq!(first.peer(), peer);
        assert_ne!(first, second);
    }

    #[test]
    fn direct_routes_are_direct_and_via_routes_are_not() {
        assert!(Route::Direct(LinkId::new(HostId::from_u128(1))).is_direct());
        assert!(!Route::Via(HostId::from_u128(2)).is_direct());
    }
}
