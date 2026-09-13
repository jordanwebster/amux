//! Consecutive read-only exploration, folded under its first entry.
//!
//! Both Claude feeds — the terminal transcript and the stream-JSON
//! session — state the same grouping, so the walk lives here once and
//! each feed supplies only what its own entries know: their identity,
//! the tool they invoke, and whether they follow another exploration.

use std::collections::VecDeque;
use std::iter::Peekable;

use super::facts::ToolInvocation;

/// What a feed entry has to say for itself before it can join a run.
pub trait RunEntry {
    /// This entry's identity in its own feed.
    fn run_id(&self) -> u64;
    /// The exploration this entry invokes, when it invokes one.
    fn exploration(&self) -> Option<&ToolInvocation>;
    /// This entry and the one immediately before it are both exploration.
    /// A grouping fact the fold states; never renderer layout introspection.
    fn groups_with_previous(&self) -> bool;
}

/// Whether one invocation is the kind of read-only look that folds away.
pub fn groupable(invocation: &ToolInvocation) -> bool {
    invocation.is_exploration()
}

/// One native entry, or a consecutive run of read-only exploration.
/// Raw entries remain available through each layer's own accessor; this
/// projection states Claude's grouping semantics without imposing a
/// shared feed vocabulary on other agent layers.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedItem<'a, E> {
    Entry(&'a E),
    ExplorationRun {
        /// Stable identity of the run: its first retained entry.
        id: u64,
        /// Entry identities in feed order.
        member_ids: Vec<u64>,
        reads: usize,
        searches: usize,
        /// Every path stated by a Read invocation, without a presentation cap.
        read_paths: Vec<&'a str>,
    },
}

/// Lazy projection over one Claude feed's declared exploration runs.
pub struct FeedItems<'a, E> {
    entries: Peekable<std::collections::vec_deque::Iter<'a, E>>,
}

impl<'a, E> FeedItems<'a, E> {
    pub(crate) fn new(entries: &'a VecDeque<E>) -> Self {
        Self {
            entries: entries.iter().peekable(),
        }
    }
}

impl<'a, E: RunEntry> Iterator for FeedItems<'a, E> {
    type Item = FeedItem<'a, E>;

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.entries.next()?;
        let Some(first_invocation) = first.exploration() else {
            return Some(FeedItem::Entry(first));
        };

        let mut member_ids = vec![first.run_id()];
        let mut reads = 0;
        let mut searches = 0;
        let mut read_paths = Vec::new();
        count_exploration(first_invocation, &mut reads, &mut searches, &mut read_paths);

        while self
            .entries
            .peek()
            .is_some_and(|entry| entry.groups_with_previous() && entry.exploration().is_some())
        {
            let entry = self
                .entries
                .next()
                .expect("a peeked exploration entry remains available");
            let invocation = entry
                .exploration()
                .expect("grouped exploration entries retain their classification");
            member_ids.push(entry.run_id());
            count_exploration(invocation, &mut reads, &mut searches, &mut read_paths);
        }

        if member_ids.len() == 1 {
            Some(FeedItem::Entry(first))
        } else {
            Some(FeedItem::ExplorationRun {
                id: first.run_id(),
                member_ids,
                reads,
                searches,
                read_paths,
            })
        }
    }
}

fn count_exploration<'a>(
    invocation: &'a ToolInvocation,
    reads: &mut usize,
    searches: &mut usize,
    read_paths: &mut Vec<&'a str>,
) {
    match invocation {
        ToolInvocation::Read { file_path } => {
            *reads += 1;
            if let Some(path) = file_path {
                read_paths.push(path);
            }
        }
        ToolInvocation::Query { .. } => *searches += 1,
        ToolInvocation::Edit { .. }
        | ToolInvocation::Write { .. }
        | ToolInvocation::Bash { .. }
        | ToolInvocation::AmuxSend { .. }
        | ToolInvocation::Task { .. }
        | ToolInvocation::Question { .. }
        | ToolInvocation::Plan { .. }
        | ToolInvocation::Other => {}
    }
}
