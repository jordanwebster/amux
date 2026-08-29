//! Shared feed viewport state.

use std::collections::BTreeSet;

use super::FeedScroll;
use super::blocks::RunKey;
use super::frame::BlockKey;

/// Renderer-local position, focus, and expansion state for a chat feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FeedViewport {
    pub(crate) scroll: FeedScroll,
    pub(crate) focus: Option<BlockKey>,
    pub(crate) expanded: BTreeSet<RunKey>,
}

impl FeedViewport {
    pub(crate) fn following() -> Self {
        Self {
            scroll: FeedScroll::Following,
            focus: None,
            expanded: BTreeSet::new(),
        }
    }
}
