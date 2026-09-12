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
pub(crate) use pairing::{LocalPairingIdentity, PairingService, pair_initiator};
pub use reachability::ReachabilityLinkConnector;
pub(crate) use startup::{
    CloudConnector, DeviceRuntimeSecurity, establish_cloud_connection, start_user_services,
};
pub use startup::{CloudLinkService, StartedRoutingServices, StartedUserServices};
