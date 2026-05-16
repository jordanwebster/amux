//! Reactive UI runtime for amux.
//!
//! UI state and command adapters over the generated ClientService API.

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
