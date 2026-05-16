//! Claude agent runtime: session lifecycle, hook handling, and transcript tailing.

mod hooks;
pub mod io;
mod session;
pub(in crate::agents) mod transcript;

pub(crate) use session::{ClaudeSession, ClaudeStructuredInputTarget};
