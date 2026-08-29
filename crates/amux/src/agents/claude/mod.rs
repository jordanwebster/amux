//! Claude agent runtime: session lifecycle, hook handling, and transcript tailing.

pub mod io;
#[cfg(feature = "local-agents")]
mod session;
#[cfg(feature = "local-agents")]
mod transcript_ingest;

#[cfg(feature = "local-agents")]
pub(crate) use claude::version::VersionCache as ClaudeVersionCache;
#[cfg(feature = "local-agents")]
pub(crate) use session::ClaudeSession;
