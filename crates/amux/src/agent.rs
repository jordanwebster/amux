//! Agent runtime: session lifecycle, PTY management, and hook dispatch.

pub(crate) mod claude;
mod hook;
mod log_source;
mod naming;
mod pty;
mod session;
#[cfg(any(debug_assertions, test))]
mod test_agent;

pub(crate) use hook::{ExternalHookBootstrap, HookError, HookOutcome};
pub(crate) use log_source::StructuredLogSource;
pub(crate) use naming::LocalAgentNameSource;
pub(crate) use pty::{PtyHandle, spawn_pty_agent};
pub(crate) use session::{Agent, AgentSession, SessionEvent, StopPolicy};
#[cfg(any(debug_assertions, test))]
pub(crate) use test_agent::TestAgentSession;
