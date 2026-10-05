//! Claude Code's bidirectional stream-JSON protocol. The frames, control
//! requests and option types live in `claude_protocol::stream`; this module
//! adds the error a host or client of that stream reports.

pub mod error;

pub use claude_protocol::stream::{control, init, message, options, types, *};
pub use error::{Error, ProtocolError};
