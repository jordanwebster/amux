//! Reactive UI runtime for amux.
//!
//! v0 limitations: no reconnect/backoff, no input replay across reconnects,
//! single-consumer notifications, and opaque session payloads.

mod agent_cache;
mod cmd;
mod error;
mod inventory;
mod notification;
mod runtime;
mod session;

pub mod types;

pub use cmd::{Cmd, CmdId, CmdResult};
pub use error::AmuxError;
pub use notification::{
    DisconnectReason, Notification, NotificationStream, SessionFailureReason, SessionPhase,
};
pub use runtime::Runtime;
