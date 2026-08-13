//! Codex agent runtime backed by the shared app-server daemon.

pub mod io;

#[cfg(feature = "local-agents")]
mod session;

#[cfg(feature = "local-agents")]
pub(crate) use session::{CodexClient, CodexSession};
