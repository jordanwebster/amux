//! Server runtime: listeners, routing, connection handling, and shared state.

mod accept;
mod cloud;
mod connection;
mod debug;
mod handlers;
mod registry;
mod routing;
mod runtime;
mod state;

pub(crate) use runtime::Server;
pub use runtime::ServerError;
pub(in crate::server) use runtime::send_routable_via_full_dst;
#[cfg(test)]
pub(in crate::server) use runtime::test_helpers;
#[cfg(test)]
pub(in crate::server) use runtime::{handle_session_event, sweep_expired_subscriptions};
pub(in crate::server) use state::{
    ConnectionHandle, LOCAL_USER_ID, SUBSCRIPTION_LEASE_DURATION, ServerState, ServerUserState,
    ShutdownRequest, SubscriptionEntry, SubscriptionMode, ensure_user_state, subscription_lease_ms,
};
