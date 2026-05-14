//! Agent lifecycle, peer management, and route-level operations.

mod agents;
mod forwarding;
mod naming;
mod peers;
mod topology;

pub(crate) use agents::{
    CreateAgentError, create_agent_record, delete_local_agent, withdraw_agent,
};
pub(super) use agents::{resume_agents, shutdown_server, suspend_server};
pub(in crate::server) use forwarding::{FrameForwardingResult, forward_frame_or_endpoint};
pub(super) use naming::apply_local_name_candidate;
pub(crate) use naming::{RenameAgentError, rename_local_agent_record};
pub(crate) use peers::{
    broadcast_topology_event, initial_routing_events, maybe_start_agent_subscription,
};
pub(crate) use topology::TopologyEvent;
pub(in crate::server) use topology::{
    LinkClosedChange, PeerAgentDownChange, PeerAgentDownIgnored, PeerAgentUpChange,
    PeerAgentUpIgnored, PeerHostDownChange, PeerHostUpChange,
};
