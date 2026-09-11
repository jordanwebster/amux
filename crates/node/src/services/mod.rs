//! Shared service context for protobuf-shaped application services.

mod agent;
pub(crate) mod client;
pub mod front_door;
mod pairing;
#[cfg(test)]
pub(crate) use pairing::{PeerTrustCommitContext, PeerTrustUpdate, commit_peer_trust};
mod reachability;
mod startup;

pub(crate) use agent::AgentServiceCtx;
#[cfg(test)]
pub(crate) use client::ClientService;
pub(crate) use pairing::{LocalPairingIdentity, PairingService, pair_initiator};
pub(crate) use reachability::ReachabilityLinkConnector;
pub(crate) use startup::{
    CloudConnector, CloudLinkService, DeviceRuntimeSecurity, StartedUserServices,
    establish_cloud_connection, start_user_services,
};
