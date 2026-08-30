//! Thin host adapter over the canonical Claude provider crate.

pub mod io;
#[cfg(feature = "local-agents")]
mod delivery;
#[cfg(feature = "local-agents")]
mod pty_backend;
#[cfg(feature = "local-agents")]
mod suspend;

#[cfg(feature = "local-agents")]
pub(crate) use claude::version::VersionCache as ClaudeVersionCache;
#[cfg(feature = "local-agents")]
pub(crate) use pty_backend::ClaudePtyBackend as ClaudeSession;
