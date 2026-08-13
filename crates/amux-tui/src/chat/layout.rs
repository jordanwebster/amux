//! Shared terminal row accounting for native chat screens.

/// Rows above the feed (border, header, rule).
pub(crate) const FIXED_TOP: usize = 3;

/// Rows between the feed and bottom block. A paused feed reserves its status
/// rule; a working feed additionally renders the activity line.
pub(crate) fn extra_rows(working: bool, paused: bool) -> usize {
    usize::from(working || paused) + usize::from(working)
}

/// The frame row budget shared by rendering and scroll paging.
pub(crate) struct ChatLayout {
    pub(crate) height: usize,
    pub(crate) bottom_rows: usize,
    pub(crate) working: bool,
    pub(crate) paused: bool,
}

impl ChatLayout {
    fn feed_height_for(&self, paused: bool) -> usize {
        self.height
            .saturating_sub(FIXED_TOP + 1 + extra_rows(self.working, paused) + self.bottom_rows)
    }

    pub(crate) fn feed_height(&self) -> usize {
        self.feed_height_for(self.paused)
    }

    pub(crate) fn feed_height_when_paused(&self) -> usize {
        self.feed_height_for(true)
    }
}

pub(crate) fn bottom_max_rows(height: usize, working: bool, paused: bool) -> usize {
    height
        .saturating_sub(FIXED_TOP + 1 + extra_rows(working, paused))
        .max(1)
}
