//! Per-connection message loop and stream lifecycle.
//!
//! Each connection runs a [`connection_loop`] that receives messages from the
//! reader task and dispatches them via [`handle_message`](super::dispatch::handle_message).
//! Reader and writer tasks ([`reader_loop`], [`writer_loop`]) bridge the transport
//! to channels.

mod context;
mod driver;
mod heartbeat;
mod reauth;

pub(crate) use context::{ConnectionContext, ConnectionError};
pub(super) use context::{HeartbeatRole, HeartbeatSetup, Result};
pub(in crate::server) use driver::{
    RunConnection, drain_local_origin_routed_unreachable_for_route, run_connection,
};
