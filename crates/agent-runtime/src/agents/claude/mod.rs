//! Thin host adapter over the canonical Claude provider crate.

mod delivery;
pub mod io;
mod pty_backend;
mod sdk_backend;
mod sdk_delivery;
mod sdk_facts;
pub mod sdk_io;
mod suspend;

pub(crate) use claude::version::VersionCache as ClaudeVersionCache;
pub(crate) use pty_backend::ClaudePtyBackend as ClaudeSession;
pub(crate) use sdk_backend::ClaudeSdkBackend;
