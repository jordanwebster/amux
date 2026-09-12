//! Shared service context for protobuf-shaped application services.

mod agent;
pub(crate) mod client;
pub mod front_door;
mod pairing;
pub use pairing::{PeerTrustCommitContext, PeerTrustUpdate, commit_peer_trust};
mod reachability;
mod startup;

pub use agent::AgentServiceCtx;
pub use client::ClientService;
pub(crate) use pairing::{LocalPairingIdentity, PairingService};
pub use reachability::ReachabilityLinkConnector;
pub use startup::{
    CloudLink, CloudLinkServer, CloudTransport, FREE_TIER_REFRESH_INTERVAL, StartedUserServices,
    TestCloudTransport, UDP_BLOCKED_MEMORY, UdpBlockedMemory, establish_cloud_link,
};
pub(crate) use startup::{DeviceRuntimeSecurity, start_user_services};
