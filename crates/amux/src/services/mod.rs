//! Shared service context for protobuf-shaped application services.

mod agent;
mod client;
mod pairing;
mod reachability;
mod startup;

pub(crate) use agent::{
    AgentServiceCtx, AgentServiceState, SharedAgentServiceState, commit_server_suspend,
    prepare_server_suspend, resume_agents, shutdown_server, withdraw_agent,
};
pub(crate) use pairing::{LocalPairingIdentity, PairingService, pair_by_spake2_initiator};
pub(crate) use reachability::ReachabilityLinkConnector;
pub(crate) use startup::{
    CloudRoutingService, DeviceRuntimeSecurity, StartedUserServices, establish_cloud_connection,
    start_user_services,
};
