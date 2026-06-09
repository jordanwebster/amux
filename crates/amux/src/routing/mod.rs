//! New architecture routing primitives.
//!
//! This module owns route-level reachability and exposes the raw and logical
//! event streams described in `docs/NETWORKING.md`.

mod connect;
mod core;
mod events;
mod host;
mod link;
mod link_registry;
mod route;
mod types;
mod wire;

pub(crate) use core::{HostUpOutcome, RoutingCore, select_best_route};

#[cfg(test)]
pub(crate) use connect::spawn_connector_to_channel;
#[cfg(any(test, feature = "testnet"))]
pub(crate) use connect::spawn_connector_to_channel_with_bearer_token;
pub(crate) use connect::{
    AuthenticatedRoutingUser, RoutingAuthSession, RoutingConnectCtx, RoutingConnectorAuth,
    RoutingConnectorCtx, RoutingConnectorToken, RoutingConnectorTokenRefresher,
    RoutingTokenAuthenticator, spawn_connector_to_channel_with_auth_and_establishment,
    spawn_connector_to_channel_with_establishment,
};
pub(crate) use events::{EventSource, HostReachabilityEvent, RoutingEvent};
pub use events::{HostEntry, HostEvent, HostReachabilityStatus, HostTrustStatus};
pub(crate) use host::{
    FEATURE_CLOUD_RELAY, MAX_HOST_NAME_BYTES, local_capabilities, local_host, validate_remote_host,
};
pub(crate) use link::{
    ConnectHandshake, ConnectHandshakeEvent, protocol_error_goaway, protocol_error_hello_ack,
};
pub(crate) use link_registry::{
    LinkCloseReason, LinkOutputTx, LinkRegistry, LinkRegistryError, LinkRole,
    spawn_routing_event_fanout,
};
pub(crate) use route::{ROUTE_HOP_CAP, Route, generate_server_link};
pub use types::{Capabilities, Host, SupportedAgentType};
pub(crate) use types::{
    InvalidLinkName, Link, capabilities_from_wire, capabilities_to_wire, host_from_wire,
    host_to_wire,
};
pub(crate) use wire::{
    InboundRoutingEvent, outbound_routing_message, route_to_wire,
    should_send_routing_event_to_link, wire_routing_event_to_inbound,
};
