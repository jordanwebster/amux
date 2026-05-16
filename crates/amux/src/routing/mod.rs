//! New architecture routing primitives.
//!
//! This module owns the first-route-only host table and exposes the raw and
//! logical event streams described in `docs/NEW_ARCHITECTURE.md`.

mod connect;
mod core;
mod events;
mod host;
mod link;
mod link_registry;
mod route;
mod types;
mod wire;

pub(crate) use core::{HostUpOutcome, RoutingCore};

pub(crate) use connect::{
    AuthenticatedRoutingUser, RoutingAuthSession, RoutingConnectCtx, RoutingConnectorAuth,
    RoutingConnectorCtx, RoutingConnectorToken, RoutingConnectorTokenRefresher,
    RoutingTokenAuthenticator, spawn_connector_to_channel_with_auth_and_establishment,
    spawn_connector_to_channel_with_establishment,
};
#[cfg(test)]
pub(crate) use connect::{
    spawn_connector_to_channel, spawn_connector_to_channel_with_bearer_token,
};
pub use events::HostEvent;
pub(crate) use events::{EventSource, HostReachabilityEvent, RoutingEvent};
pub(crate) use host::{local_capabilities, local_host, validate_remote_host};
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, protocol_error_goaway, protocol_error_hello_ack,
};
pub(crate) use link_registry::{
    LinkCloseReason, LinkOutputTx, LinkRegistry, LinkRegistryError, spawn_routing_event_fanout,
};
pub(crate) use route::{Route, generate_server_link};
pub use types::{Capabilities, Host, SupportedAgentType};
pub(crate) use types::{InvalidLinkName, Link, host_from_wire, host_to_wire};
pub(crate) use wire::{
    InboundRoutingEvent, outbound_routing_message, route_to_wire,
    should_send_routing_event_to_link, wire_routing_event_to_inbound,
};
