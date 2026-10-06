//! What the terminal holds back, or builds from lesser facts, until the
//! wire, the daemon or the shared views carry what a feature needs. Every
//! such decision lives here and the screens ask, so that when a fact
//! arrives one entry changes and nothing on screen fakes it meanwhile.
//!
//! Each entry says what is missing. Against a real agent nothing here
//! pretends: a feature is either built from facts that exist, or it is not
//! offered.
//!
//! Held back outside this module, because the shared views never offer
//! them: Codex's Plan mode (amux cannot set Codex's collaboration mode),
//! a decision on a Codex plan (Codex's protocol has none, so its plan reads
//! as an ordinary message), and a Claude plan growing as it is written (the
//! interpreter does not stream a tool call's input, so the plan arrives
//! whole).

use ui_state::SessionState;
use wire::Kind;

use crate::chat::pane::Job;
use crate::setup;

/// Whether a question ask can be declined with the person's own message
/// ("Reply instead"). Neither agent's protocol has a decline for its
/// questions on the wire yet; until one exists the only ways out are
/// answering and stopping the turn.
pub fn declines_questions(_kind: Kind) -> bool {
    false
}

/// Whether questions may be sent with some left unanswered. Codex takes a
/// question with no answer; Claude's interpreter refuses one, so with
/// Claude every question is answered before sending.
pub fn skips_questions(kind: Kind) -> bool {
    kind == Kind::Codex
}

/// Whether a new agent can start in a new worktree. The create request
/// cannot ask for one yet, so the toggle is not offered.
pub fn offers_worktree() -> bool {
    false
}

/// The background jobs still running, for the row above the composer and
/// the Overview. The wire counts them but does not list them, and a list
/// rebuilt from the transcript misses what the agent did not show as a
/// step, so they are not shown until the wire lists them.
pub fn background_jobs(_state: &SessionState) -> Vec<Job> {
    Vec::new()
}

/// Whether the working tree's change counts are shown: the header's
/// `[Diff +a −b]` and the Overview's Changes. Counting needs the whole
/// working-tree patch today; until the host keeps per-file counts with the
/// agent, the header shows a plain `[Diff]` (the review page still reads
/// the patch when opened) and the Overview has no Changes.
pub fn diff_counts() -> bool {
    false
}

/// Whether an agent's own terminal on another host can be attached. Raw
/// attach reads the agent's directory on this machine, so an agent
/// elsewhere opens its chat with a notice instead.
pub fn attaches_elsewhere() -> bool {
    false
}

/// The models a new agent offers before it exists, and whether a typed
/// name is taken as one. Only a running agent lists its models; until a
/// host lists them per agent, Claude offers its aliases (every Claude takes
/// them) and Codex its model from the settings, any other by name.
pub fn models_before_start(agent: setup::Agent, configured: &str) -> (Vec<String>, bool) {
    match agent {
        setup::Agent::Claude => (
            ["opus", "sonnet", "haiku"].map(str::to_owned).to_vec(),
            false,
        ),
        setup::Agent::Codex => (vec![configured.to_owned()], true),
    }
}
