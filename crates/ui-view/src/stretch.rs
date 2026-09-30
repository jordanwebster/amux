//! Stretches: the runs of tool steps between two pieces of what the agent
//! said. A turn reads as the agent's text with its work in between, and a
//! client folds each stretch of that work to one line: what it did, not
//! every call.
//!
//! A stretch is found from any of its members, the way a run is, so row ids
//! never move and the stretch grows only at its edges: paging adds steps
//! at its oldest end, a live call at its newest. Thinking, retry notices,
//! a subagent's own steps and items that draw nothing pass through a
//! stretch without ending it; anything else the agent or the person put in
//! the transcript ends it.

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Held, ItemClass, Key, SessionState};

use crate::rows::{ExploreVerb, RowKind, ToolStateView, kind_of, parent_of, subject_of};

/// One stretch of tool steps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Stretch {
    pub oldest: Key,
    pub newest: Key,
    pub oldest_order: u64,
    pub newest_order: u64,
    /// The steps in it, counted by what they did.
    pub counts: StretchCounts,
    /// A step is still waiting or running.
    pub running: bool,
    /// Something other than a step follows it: the agent's text, an ask, a
    /// prompt or the turn's end. A stretch at the transcript's head with
    /// nothing after it is still open.
    pub closed: bool,
    /// Steps that failed and that nothing later in the same turn redid
    /// successfully, oldest first. Reported only once the turn has ended:
    /// until then the agent may still fix them.
    pub unresolved: Vec<Key>,
    /// The stretch starts at the oldest held row and older history exists,
    /// so it may continue below.
    pub open_below: bool,
}

/// What a stretch's steps did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StretchCounts {
    pub steps: u32,
    pub commands: u32,
    /// Files changed, counting each file of a multi-file change.
    pub edits: u32,
    pub reads: u32,
    pub searches: u32,
    pub subagents: u32,
    /// Tool-server calls, fetches, listings and the rest.
    pub other: u32,
}

/// How an item stands toward a stretch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Step,
    /// Inside a stretch, it neither counts nor ends it.
    Through,
    /// Ends a stretch.
    Break,
}

fn role(state: &SessionState, held: &Held) -> Role {
    match &held.class {
        ItemClass::Tool(_) => {
            if parent_of(held).is_some() {
                return Role::Through;
            }
            match kind_of(state, held).0 {
                RowKind::Hidden => Role::Through,
                RowKind::Ask(_) => Role::Break,
                _ => Role::Step,
            }
        }
        ItemClass::Thinking { .. } | ItemClass::Retrying { .. } => Role::Through,
        ItemClass::Other if matches!(kind_of(state, held).0, RowKind::Hidden) => Role::Through,
        _ => Role::Break,
    }
}

/// The stretch holding the item at `order`, or None when that item is not a
/// step and sits between no two steps of one stretch.
pub fn stretch_at(state: &SessionState, order: u64) -> Option<Stretch> {
    let transcript = state.transcript();
    let held = transcript.at(order)?;
    if role(state, held) == Role::Break {
        return None;
    }
    let oldest_held = transcript.oldest_held()?;
    let head = transcript.head()?;

    // Out to the breaks on either side, then in to the outermost steps.
    let mut low = order;
    let mut reached_oldest = true;
    for held in transcript.range(oldest_held..=order).rev().skip(1) {
        if role(state, held) == Role::Break {
            reached_oldest = false;
            break;
        }
        low = held.item.order;
    }
    let mut high = order;
    let mut closed = false;
    for held in transcript.range(order..=head).skip(1) {
        if role(state, held) == Role::Break {
            closed = true;
            break;
        }
        high = held.item.order;
    }

    let mut counts = StretchCounts::default();
    let mut running = false;
    let mut first: Option<&Held> = None;
    let mut last: Option<&Held> = None;
    let mut failed: Vec<(&Held, String)> = Vec::new();
    for held in transcript.range(low..=high) {
        if role(state, held) != Role::Step {
            continue;
        }
        first.get_or_insert(held);
        last = Some(held);
        let (kind, _, did_fail) = kind_of(state, held);
        counts.steps += 1;
        let tool_state = match &kind {
            RowKind::Command { state, .. }
            | RowKind::Explore { state, .. }
            | RowKind::ToolCall { state, .. }
            | RowKind::FileChange { state, .. } => Some(*state),
            RowKind::Subagent { running, .. } => Some(if *running {
                ToolStateView::Running
            } else {
                ToolStateView::Succeeded
            }),
            RowKind::Background { running, .. } if *running => Some(ToolStateView::Running),
            _ => None,
        };
        if matches!(
            tool_state,
            Some(ToolStateView::Pending | ToolStateView::Running)
        ) && !matches!(kind, RowKind::Background { .. })
        {
            running = true;
        }
        match &kind {
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
        if did_fail || failed_kind(&kind) {
            failed.push((held, subject_of(held)));
        }
    }
    let (first, last) = (first?, last?);

    // A failure is resolved when a later step in the same turn with the same
    // subject succeeded, as a lint rerun that passes. Only an ended turn
    // reports what is left.
    let mut unresolved = Vec::new();
    let turn_end = transcript
        .range(last.item.order..=head)
        .skip(1)
        .find(|held| {
            matches!(
                held.class,
                ItemClass::Turn | ItemClass::Prompt | ItemClass::Steer
            )
        })
        .filter(|held| matches!(held.class, ItemClass::Turn));
    if let Some(end) = turn_end {
        for (held, subject) in &failed {
            let redone = !subject.is_empty()
                && transcript
                    .range(held.item.order..=end.item.order)
                    .skip(1)
                    .filter(|later| {
                        matches!(later.class, ItemClass::Tool(_)) && parent_of(later).is_none()
                    })
                    .any(|later| {
                        let (kind, _, did_fail) = kind_of(state, later);
                        subject_of(later) == *subject && !did_fail && !failed_kind(&kind)
                    });
            if !redone {
                unresolved.push(held.item.key.clone());
            }
        }
    }

    Some(Stretch {
        oldest: first.item.key.clone(),
        newest: last.item.key.clone(),
        oldest_order: first.item.order,
        newest_order: last.item.order,
        counts,
        running,
        closed,
        unresolved,
        open_below: reached_oldest && first.item.order == oldest_held && transcript.has_older(),
    })
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

/// The orders of a stretch's steps, oldest first: what a client draws one
/// line each when the stretch is open or under way.
pub fn stretch_steps(state: &SessionState, stretch: &Stretch) -> Vec<u64> {
    state
        .transcript()
        .range(stretch.oldest_order..=stretch.newest_order)
        .filter(|held| role(state, held) == Role::Step)
        .map(|held| held.item.order)
        .collect()
}
