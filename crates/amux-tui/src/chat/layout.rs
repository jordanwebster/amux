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
    /// One row when a child of this agent is asking for the human (U1).
    /// It costs the feed a line rather than floating over it: a banner
    /// that hides a message is trading one thing the human needs to read
    /// for another.
    pub(crate) banner: bool,
}

impl ChatLayout {
    fn feed_height_for(&self, paused: bool) -> usize {
        self.height.saturating_sub(
            FIXED_TOP
                + 1
                + usize::from(self.banner)
                + extra_rows(self.working, paused)
                + self.bottom_rows,
        )
    }

    pub(crate) fn feed_height(&self) -> usize {
        self.feed_height_for(self.paused)
    }

    pub(crate) fn feed_height_when_paused(&self) -> usize {
        self.feed_height_for(true)
    }
}

/// The frame facts everything below the header reads: how tall the frame
/// is and which optional rows are present. Passed as one value so the
/// bottom block and the row budget cannot disagree about them.
#[derive(Clone, Copy)]
pub(crate) struct FrameRows {
    pub(crate) height: usize,
    pub(crate) working: bool,
    pub(crate) paused: bool,
    pub(crate) banner: bool,
}

impl FrameRows {
    pub(crate) fn bottom_max(self) -> usize {
        self.height
            .saturating_sub(
                FIXED_TOP + 1 + usize::from(self.banner) + extra_rows(self.working, self.paused),
            )
            .max(1)
    }
}
