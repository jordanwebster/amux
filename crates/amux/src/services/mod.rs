//! Shared service context for protobuf-shaped application services.

mod agent;
pub(crate) mod client;
pub mod front_door;
mod pairing;
#[cfg(testnet)]
pub(crate) use pairing::{PeerTrustCommitContext, PeerTrustUpdate, commit_peer_trust};
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
#[cfg(testnet)]
pub(crate) use startup::start_user_services_with_artifact_clock;
pub(crate) use startup::{
    CloudConnector, CloudLinkService, DeviceRuntimeSecurity, StartedUserServices,
    establish_cloud_connection, start_user_services,
};
