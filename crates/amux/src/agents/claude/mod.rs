//! Thin host adapter over the canonical Claude provider crate.

#[cfg(feature = "local-agents")]
mod delivery;
pub mod io;
#[cfg(feature = "local-agents")]
mod pty_backend;
pub mod sdk_io;
#[cfg(feature = "local-agents")]
mod suspend;

#[cfg(feature = "local-agents")]
pub(crate) use claude::version::VersionCache as ClaudeVersionCache;
#[cfg(feature = "local-agents")]
pub(crate) use pty_backend::ClaudePtyBackend as ClaudeSession;
