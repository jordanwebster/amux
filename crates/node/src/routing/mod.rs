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

pub(crate) use core::RouteUpdateOutcome;
pub use core::{RoutingCore, RoutingDebug};

pub use connect::{
    AuthenticatedLinkUser, LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher,
    LinkTokenAuthenticator,
};
pub(crate) use connect::{
    LinkAuthSession, LinkConnectorCtx, LinkServiceCtx,
    spawn_connector_to_channel_with_auth_establishment_and_shutdown,
    spawn_connector_to_channel_with_bearer_token_and_shutdown,
    spawn_connector_to_channel_with_establishment,
};
#[cfg(test)]
pub(crate) use connect::{
    spawn_connector_to_channel, spawn_connector_to_channel_with_bearer_token,
};
pub use events::RoutingEvent;
pub(crate) use events::{EventSource, HostReachabilityEvent};
pub(crate) use host::{FEATURE_CLOUD_RELAY, local_host, validate_remote_host};
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, protocol_error_hello_ack, protocol_error_link_close,
};
pub use link_registry::LinkRegistry;
pub(crate) use link_registry::{LinkCloseRequest, LinkOutputTx, LinkRole, LinkUnavailable};
pub use model::{Capabilities, Host, HostEntry, HostEvent, HostTrustStatus, SupportedAgentType};
pub use types::{LinkId, Route, host_to_wire};
pub(crate) use types::{capabilities_to_wire, host_from_wire};
pub(crate) use wire::{inbound_host_from_wire, neighbor_down_from_wire, neighbor_up_from_wire};
