//! Codex agent runtime backed by the shared app-server daemon.

pub mod io;

/// The one transient reason the raw plane is not up yet: the connect loop has
/// not published a thread. Clients retry on exactly this, so it is stated once
/// here rather than spelled out again at each call site.
pub const CODEX_RAW_THREAD_NOT_READY: &str =
    "Codex raw session is not ready: thread_id is not available yet";

#[cfg(all(feature = "local-agents", unix))]
mod session;

#[cfg(all(feature = "local-agents", unix))]
pub(crate) use session::{CodexClient, CodexRawPtyLease, CodexRawPtyTarget, CodexSession};
