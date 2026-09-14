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

pub(crate) use core::RouteUpdateOutcome;
pub use core::{RoutingCore, RoutingDebug};

pub use events::RoutingEvent;
pub(crate) use events::{EventSource, HostReachabilityEvent};
pub(crate) use host::{FEATURE_CLOUD_RELAY, LiveLocalHost, local_host, validate_remote_host};
pub use link::ConnectRole;
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, protocol_error_hello_ack, protocol_error_link_close,
};
pub(crate) use link_registry::{DirectLinkOrder, LinkAdmission, LinkCloseRequest, LinkProperties};
pub use link_registry::{LinkCarrier, LinkRegistry, LinkRole};
pub use model::{
    Capabilities, Host, HostEntry, HostEvent, HostTrustStatus, HostVia, SupportedAgentType,
};
pub use types::{LinkId, Route, host_to_wire};
pub(crate) use types::{capabilities_to_wire, host_from_wire};
pub(crate) use wire::{inbound_host_from_wire, neighbor_down_from_wire, neighbor_up_from_wire};

#[cfg(test)]
pub(crate) use crate::link::run::spawn_connector_with_bearer_token;
pub(crate) use crate::link::run::{
    AuthenticatedLinkContextProvider, LinkConnectorCtx, LinkConnectorRefreshReceiver,
    LinkConnectorRefreshRequest, LinkCtx, spawn_connector_with_auth_establishment_and_shutdown,
    spawn_connector_with_establishment_and_shutdown,
};
pub use crate::link::run::{
    AuthenticatedLinkUser, LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher,
    LinkTokenAuthenticator, bearer_token_auth, link_reauth_tier_probe,
    spawn_connector_with_establishment,
};
