//! What the terminal holds back, or builds from lesser facts, until the
//! wire, the daemon or the shared views carry what a feature needs. Every
//! such decision lives here and the screens ask, so that when a fact
//! arrives one entry changes and nothing on screen fakes it meanwhile.
//!
//! Each entry says what is missing. Against a real agent nothing here
//! pretends: a feature is either built from facts that exist, or it is not
//! offered.

/// Whether a new agent can start in a new worktree. The create request
/// cannot ask for one yet, so the toggle is not offered.
pub fn offers_worktree() -> bool {
    false
}

/// Whether an agent's own terminal on another host can be attached. Raw
/// attach reads the agent's directory on this machine, so an agent
/// elsewhere opens its chat with a notice instead.
pub fn attaches_elsewhere() -> bool {
    false
}
