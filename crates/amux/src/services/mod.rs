//! Shared service context for protobuf-shaped application services.

mod agent;
mod client;
mod pairing;
mod reachability;
mod startup;

#[cfg(feature = "local-agents")]
pub(crate) use agent::PtyAgentHost;
pub(crate) use agent::{AgentServiceCtx, DebugAgent, LocalAgentHost};
#[cfg(all(feature = "local-agents", debug_assertions))]
pub(crate) use agent::{create_sdk_in_process, open_in_process_protocol_plane};
#[cfg(testnet)]
pub(crate) use client::ClientService;
pub(crate) use pairing::{LocalPairingIdentity, PairingService, pair_initiator};
pub(crate) use reachability::ReachabilityLinkConnector;
pub(crate) use startup::{
    CloudLinkService, DeviceRuntimeSecurity, StartedUserServices, establish_cloud_connection,
    start_user_services,
};
