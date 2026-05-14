//! Server runtime: listeners, routing, connection handling, and shared state.

mod accept;
mod cloud;
mod connection;
mod debug;
mod dispatch;
mod host;
mod open_session_lifecycle;
#[cfg(test)]
mod protocol_harness;
#[cfg(test)]
mod protocol_tests;
mod routing;
mod rpc_dispatcher;
mod runtime;
mod state;

pub(crate) use connection::ConnectionError;
pub(crate) use debug::dump_server_debug_info;
pub(in crate::server) use host::validate_remote_host;
pub(crate) use host::{local_capabilities, local_host};
pub(crate) use open_session_lifecycle::{
    OpenSessionRuntime, OpenSessionStructuredInput, OpenSessionStructuredInputJob,
    OpenSessionStructuredInputPayload, begin_open_sessions_closing_for_agent,
    finish_open_sessions_with_error,
};
pub(in crate::server) use open_session_lifecycle::{
    cancel_open_session_for_route_and_call, cancel_open_sessions_for_closed_link,
    cancel_open_sessions_for_owner_link, cancel_open_sessions_for_route_prefix,
    finish_open_session_cleanup_jobs, open_session_closing_from_rpc_closing,
    send_terminal_and_finish_open_session,
};
pub(crate) use routing::{
    CreateAgentError, RenameAgentError, broadcast_topology_event, create_agent_record,
    delete_local_agent, initial_routing_events, maybe_start_agent_subscription,
    rename_local_agent_record, withdraw_agent,
};
pub(crate) use rpc_dispatcher::RpcDispatcher;
pub(in crate::server) use rpc_dispatcher::RpcInboundCloseTarget;
pub(crate) use runtime::Server;
pub use runtime::ServerError;
#[cfg(test)]
pub(crate) use runtime::test_helpers;
pub(in crate::server) use state::{
    ConnectionHandle, LOCAL_USER_ID, ShutdownRequest, ensure_user_state,
};
pub(crate) use state::{ServerState, ServerUserState};
