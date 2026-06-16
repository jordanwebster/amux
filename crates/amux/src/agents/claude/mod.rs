//! Claude agent runtime: session lifecycle, hook handling, and transcript tailing.

#[cfg(feature = "local-agents")]
mod hooks;
pub mod io;
#[cfg(feature = "local-agents")]
mod session;
pub(in crate::agents) mod transcript;

#[cfg(feature = "local-agents")]
pub(crate) use session::{ClaudeSession, ClaudeStructuredInputTarget};
