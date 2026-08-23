//! Claude agent runtime: session lifecycle, hook handling, and transcript tailing.

#[cfg(feature = "local-agents")]
mod hooks;
pub mod io;
#[cfg(feature = "local-agents")]
mod session;
#[cfg(feature = "local-agents")]
mod transcript;
#[cfg(feature = "local-agents")]
mod transcript_ingest;
#[cfg(feature = "local-agents")]
mod version;

#[cfg(feature = "local-agents")]
pub(crate) use session::ClaudeSession;
#[cfg(feature = "local-agents")]
pub(crate) use version::ClaudeVersionCache;
