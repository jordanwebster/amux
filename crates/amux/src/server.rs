//! Server runtime: listeners, routing, connection handling, and shared state.

mod accept;
mod cloud;
mod connection;
mod debug;
mod dispatch;
mod host;
#[cfg(test)]
mod protocol_harness;
#[cfg(test)]
mod protocol_tests;
mod routing;
mod rpc_dispatcher;
mod rpc_output;
mod runtime;
mod session_subscription_lifecycle;
mod state;

pub(crate) use connection::ConnectionError;
pub(crate) use debug::dump_server_debug_info;
pub(in crate::server) use host::validate_remote_host;
pub(crate) use host::{local_capabilities, local_host};
pub(crate) use routing::{
    CreateAgentError, RenameAgentError, broadcast_topology_event, create_agent_record,
    delete_local_agent, initial_routing_events, maybe_start_agent_subscription,
    rename_local_agent_record, withdraw_agent,
};
pub(crate) use rpc_dispatcher::{
    ActiveEndpointStreamSink, EndpointServerStream, EndpointServerStreamStart, EndpointUnaryStart,
    InboundCallResources, LocalOriginOutboundCall, LocalOriginOutboundStart, OutboundCallResources,
    PeerRoutingOutboundStart, RpcDispatcher, RpcInboundCloseTarget, RpcInboundClosing,
    ServerOriginOutboundStart, routing_subscription_dedup_key,
};
pub(crate) use rpc_output::{
    ServerStreamEncoder, ServerStreamSendError, ServerStreamSink, ServerStreamSnapshotSendError,
    TypedServerStreamSink,
};
pub(crate) use runtime::Server;
pub use runtime::ServerError;
#[cfg(test)]
pub(crate) use runtime::test_helpers;
pub(crate) use session_subscription_lifecycle::{
    SessionSubscriptionRuntime, begin_session_subscriptions_closing_for_agent,
    finish_session_subscriptions_with_error,
};
pub(in crate::server) use session_subscription_lifecycle::{
    cancel_session_subscription_for_route_and_call, cancel_session_subscriptions_for_closed_link,
    cancel_session_subscriptions_for_owner_link, cancel_session_subscriptions_for_route_prefix,
    finish_session_subscription_cleanup_jobs, send_terminal_and_finish_session_subscription,
    session_subscription_closing_from_rpc_closing,
};
pub(in crate::server) use state::{
    ConnectionHandle, LOCAL_USER_ID, ShutdownRequest, ensure_user_state,
};
pub(crate) use state::{ServerState, ServerUserState, SessionSubscriptionState};
