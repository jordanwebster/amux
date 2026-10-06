//! The one fold for tool steps. A run is every tool step between two pieces
//! of what the agent or the person put in the transcript: a turn reads as
//! the agent's text with its work in between, and a client folds each run
//! of that work to one line, what it did, not every call. Thinking, retry
//! notices, a subagent's own steps and items that draw nothing pass through
//! a run; anything else ends it.
//!
//! Membership is the session's run index; this module states it on each
//! row, so any client's list gets the fold with the rows. A client chooses
//! between showing every step, hiding them and folding them, and holds
//! which runs are open.

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Fold, Held, ItemClass, Key, SessionState};

use crate::rows::{ExploreVerb, RowKind, ToolStateView, kind_of, subject_of};

/// While a run is under way, how many of its newest steps show.
pub const LIVE_STEPS: u32 = 3;

/// A row's place in its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Run {
    /// The run's oldest held step: what a client holds to keep it open.
    pub id: Key,
    /// Its newest step, where a folded run draws its one line.
    pub last: Key,
    pub steps: u32,
    /// Nothing but steps follows it yet: the agent is still at it.
    pub live: bool,
    /// It starts at the oldest held rows and older history exists, so it may
    /// continue below: its counts are a floor.
    pub open_below: bool,
    /// For one of the run's newest steps, how many steps follow it (none for
    /// the newest); absent for the rest.
    pub recent: Option<u32>,
    /// This step failed and nothing later in its turn redid it, and the
    /// turn has ended: it stays in view when the run folds.
    pub unresolved_failure: bool,
    /// What the run's steps did, on its newest step's row only.
    pub counts: Option<RunCounts>,
}

impl Run {
    /// This row is the run's newest step.
    pub fn is_last(&self) -> bool {
        self.recent == Some(0)
    }

    /// This step shows while the run is under way.
    pub fn shows_live(&self) -> bool {
        self.live && self.recent.is_some_and(|after| after < LIVE_STEPS)
    }
}

/// What a run's steps did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RunCounts {
    pub commands: u32,
    /// Files changed, counting each file of a multi-file change.
    pub edits: u32,
    pub reads: u32,
    pub searches: u32,
    pub subagents: u32,
    /// Tool-server calls, fetches, listings and the rest.
    pub other: u32,
}

/// The run the item `held` sits in, stated for its row; `kind` and
/// `failed` are the row's own.
pub(crate) fn run_of(
    state: &SessionState,
    held: &Held,
    kind: &RowKind,
    failed: bool,
) -> Option<Run> {
    let transcript = state.transcript();
    let run = transcript.run_at(held.item.order)?;
    let step = held.class.fold() == Fold::Step;
    let recent = step
        .then(|| steps_after(state, &run, held.item.order))
        .flatten();
    let counts = (run.newest == held.item.order).then(|| counts(state, &run));
    Some(Run {
        id: run.oldest_key.clone(),
        last: run.newest_key.clone(),
        steps: run.steps,
        live: !run.closed,
        open_below: run.open_below,
        recent,
        unresolved_failure: step && (failed || failed_kind(kind)) && unresolved(state, held),
        counts,
    })
}

/// How many steps follow the one at `order` in `run`, when it is among the
/// run's newest few; the walk stops there, so a long run costs no more.
fn steps_after(state: &SessionState, run: &ui_state::Run, order: u64) -> Option<u32> {
    let mut after = 0;
    for held in state.transcript().range(order..=run.newest).rev() {
        if held.class.fold() != Fold::Step {
            continue;
        }
        if held.item.order == order {
            return Some(after);
        }
        after += 1;
        if after >= LIVE_STEPS {
            return None;
        }
    }
    None
}

fn counts(state: &SessionState, run: &ui_state::Run) -> RunCounts {
    let mut counts = RunCounts::default();
    for held in state.transcript().range(run.oldest..=run.newest) {
        if held.class.fold() != Fold::Step {
            continue;
        }
        match kind_of(state, held).0 {
            RowKind::Command { .. } | RowKind::Background { .. } => counts.commands += 1,
            RowKind::FileChange { files, .. } => {
                counts.edits += u32::try_from(files.len().max(1)).unwrap_or(u32::MAX);
            }
            RowKind::Explore { verb, .. } => match verb {
                ExploreVerb::Read => counts.reads += 1,
                ExploreVerb::Search | ExploreVerb::WebSearch => counts.searches += 1,
                ExploreVerb::List | ExploreVerb::Fetch => counts.other += 1,
            },
            RowKind::Subagent { .. } => counts.subagents += 1,
            _ => counts.other += 1,
        }
    }
    counts
}

/// A failed step stays unresolved when its turn has ended and no later
/// step of that turn with the same subject succeeded, as a lint rerun that
/// passes. Until the turn ends the agent may still fix it.
fn unresolved(state: &SessionState, failed: &Held) -> bool {
    let transcript = state.transcript();
    if !transcript.turn_ended_after(failed.item.order) {
        return false;
    }
    let subject = subject_of(failed);
    if subject.is_empty() {
        return true;
    }
    for later in transcript.range(failed.item.order + 1..=u64::MAX) {
        if later.class == ItemClass::Turn {
            break;
        }
        if later.class.fold() != Fold::Step || subject_of(later) != subject {
            continue;
        }
        let (kind, _, did_fail) = kind_of(state, later);
        if !did_fail && !failed_kind(&kind) {
            return false;
        }
    }
    true
}

/// Whether a step's own facts say it failed: its state, or a command's
/// non-zero exit.
fn failed_kind(kind: &RowKind) -> bool {
    match kind {
        RowKind::Command {
            state, exit_code, ..
        } => *state == ToolStateView::Failed || exit_code.is_some_and(|code| code != 0),
        RowKind::Explore { state, .. }
        | RowKind::ToolCall { state, .. }
        | RowKind::FileChange { state, .. } => *state == ToolStateView::Failed,
        _ => false,
    }
}
