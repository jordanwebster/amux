//! Routing primitives for the amux protocol.
//!
//! Two rules define routing (docs/PROTOCOL.md): advertise only adjacency,
//! forward only to adjacency. This module owns the link runtime
//! (`connect`), the wire-side adjacency discipline (`link_registry`), and
//! the derived local routing table (`core`).

mod connect;
mod core;
mod events;
mod host;
mod link;
mod link_registry;
mod types;
mod wire;

pub(crate) use core::{RouteUpdateOutcome, RoutingCore, RoutingDebug};

#[cfg(test)]
pub(crate) use connect::spawn_connector_to_channel;
#[cfg(test)]
pub(crate) use connect::spawn_connector_to_channel_with_bearer_token;
#[cfg(testnet)]
pub(crate) use connect::spawn_connector_to_channel_with_bearer_token_and_shutdown;
pub(crate) use connect::{
    AuthenticatedLinkUser, LinkAuthSession, LinkConnectorAuth, LinkConnectorCtx,
    LinkConnectorToken, LinkConnectorTokenRefresher, LinkServiceCtx, LinkTokenAuthenticator,
    spawn_connector_to_channel_with_auth_establishment_and_shutdown,
    spawn_connector_to_channel_with_establishment,
};
pub(crate) use events::{EventSource, HostReachabilityEvent, RoutingEvent};
pub use events::{HostEntry, HostEvent, HostTrustStatus};
pub(crate) use host::{
    FEATURE_CLOUD_RELAY, MAX_HOST_NAME_BYTES, local_capabilities, local_host, validate_remote_host,
};
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, protocol_error_hello_ack, protocol_error_link_close,
};
pub(crate) use link_registry::{
    LinkCloseRequest, LinkOutputTx, LinkRegistry, LinkRole, LinkUnavailable,
};
pub use types::{Capabilities, Host, SupportedAgentType};
pub(crate) use types::{
    LinkId, Route, capabilities_from_wire, capabilities_to_wire, host_from_wire, host_to_wire,
};
pub(crate) use wire::{inbound_host_from_wire, neighbor_down_from_wire, neighbor_up_from_wire};
