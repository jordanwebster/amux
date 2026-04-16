//! Claude agent runtime: session lifecycle, hook handling, and transcript tailing.

pub(crate) mod hooks;
mod session;
pub(crate) mod transcript;

pub(crate) use session::ClaudeSession;
