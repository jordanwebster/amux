//! Codex agent runtime backed by the shared app-server daemon.

pub mod io;

#[cfg(all(feature = "local-agents", unix))]
mod session;

#[cfg(all(feature = "local-agents", unix))]
pub(crate) use session::{CodexClient, CodexSession};
