//! Routing primitives for the amux protocol.
//!
//! Two rules define routing (docs/PROTOCOL.md): advertise only adjacency,
//! forward only to adjacency. The carrier-independent link runtime lives in
//! `crate::link`; this module owns the wire-side adjacency discipline
//! (`link_registry`) and the derived local routing table (`core`).

mod core;
mod events;
mod host;
mod link;
mod link_registry;
mod types;
mod wire;

pub(crate) use core::{RouteUpdateOutcome, RoutingCore, RoutingDebug};

pub(crate) use events::{EventSource, HostReachabilityEvent, RoutingEvent};
pub use events::{HostEntry, HostEvent, HostTrustStatus, HostVia};
pub(crate) use host::{
    FEATURE_CLOUD_RELAY, LiveLocalHost, MAX_HOST_NAME_BYTES, local_capabilities, local_host,
    validate_remote_host,
};
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, ConnectRole, protocol_error_hello_ack,
    protocol_error_link_close,
};
pub(crate) use link_registry::{
    LinkAdmission, LinkCarrier, LinkCloseRequest, LinkOutputTx, LinkProperties, LinkRegistry,
    LinkRole, LinkUnavailable,
};
pub use types::{Capabilities, Host, SupportedAgentType};
pub(crate) use types::{
    LinkId, Route, capabilities_from_wire, capabilities_to_wire, host_from_wire, host_to_wire,
};
pub(crate) use wire::{inbound_host_from_wire, neighbor_down_from_wire, neighbor_up_from_wire};

#[cfg(testnet)]
pub(crate) use crate::link::run::link_reauth_tier_probe;
#[cfg(test)]
pub(crate) use crate::link::run::spawn_connector_to_channel_with_bearer_token;
pub(crate) use crate::link::run::{
    AuthenticatedLinkUser, LinkConnectorAuth, LinkConnectorCtx, LinkConnectorRefreshReceiver,
    LinkConnectorRefreshRequest, LinkConnectorToken, LinkConnectorTokenRefresher, LinkCtx,
    LinkTokenAuthenticator, spawn_connector_to_channel_with_auth_establishment_and_shutdown,
    spawn_connector_to_channel_with_establishment,
};
