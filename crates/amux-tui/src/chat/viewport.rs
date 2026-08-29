//! Shared feed viewport state.

use std::collections::BTreeSet;

use super::FeedScroll;
use super::blocks::RunKey;
use super::frame::{BlockKey, FeedMetrics, PaintedBlock};

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

/// A request to move the shared feed viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollIntent {
    /// Move by display rows; negative values move toward older content.
    Rows(i32),
    /// Move by viewport pages; negative values move toward older content.
    Page(i32),
    /// Show the oldest retained content.
    Oldest,
    /// Return to sticky-bottom following.
    Follow,
}

/// Apply one scroll request against the metrics produced by the frame's
/// paint pass. Reaching the newest possible top resumes sticky following;
/// entering a paused state records the supplied entry watermark once.
pub(crate) fn apply_scroll(
    viewport: &mut FeedViewport,
    metrics: &FeedMetrics,
    intent: ScrollIntent,
    watermark: u64,
) -> bool {
    let before = viewport.scroll.clone();
    let max_top = metrics.max_top;
    let current = match viewport.scroll {
        FeedScroll::Following => max_top,
        FeedScroll::Paused { top_line, .. } => top_line.min(max_top),
    };
    let paused_watermark = match viewport.scroll {
        FeedScroll::Paused {
            entry_watermark, ..
        } => entry_watermark,
        FeedScroll::Following => watermark,
    };

    let target = match intent {
        ScrollIntent::Follow => None,
        ScrollIntent::Oldest if max_top > 0 => Some(0),
        ScrollIntent::Oldest => None,
        ScrollIntent::Rows(delta) => Some(offset(current, delta, max_top)),
        ScrollIntent::Page(delta) => {
            let page = metrics.feed_rows.saturating_sub(1).max(1);
            Some(offset_by_pages(current, delta, page, max_top))
        }
    };

    viewport.scroll = match target {
        Some(top_line) if top_line < max_top => FeedScroll::Paused {
            top_line,
            entry_watermark: paused_watermark,
        },
        _ => FeedScroll::Following,
    };
    viewport.scroll != before
}

fn offset(current: usize, delta: i32, max_top: usize) -> usize {
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        current.saturating_add(delta as usize).min(max_top)
    }
}

fn offset_by_pages(current: usize, delta: i32, page: usize, max_top: usize) -> usize {
    let magnitude = (delta.unsigned_abs() as usize).saturating_mul(page);
    if delta < 0 {
        current.saturating_sub(magnitude)
    } else {
        current.saturating_add(magnitude).min(max_top)
    }
}

/// Move focus through painted blocks and keep the selected range visible.
pub(crate) fn move_focus(
    viewport: &mut FeedViewport,
    metrics: &FeedMetrics,
    delta: i32,
    watermark: u64,
) -> bool {
    if metrics.ranges.is_empty() || delta == 0 {
        return false;
    }
    let before = (viewport.focus, viewport.scroll.clone());
    let last = metrics.ranges.len() - 1;
    let current = viewport.focus.and_then(|key| {
        metrics
            .ranges
            .iter()
            .position(|(candidate, _)| *candidate == key)
    });
    let next = match current {
        None => last,
        Some(index) if delta < 0 => index.saturating_sub(delta.unsigned_abs() as usize),
        Some(index) => index.saturating_add(delta as usize).min(last),
    };
    let (key, range) = &metrics.ranges[next];
    viewport.focus = Some(*key);

    let top = match viewport.scroll {
        FeedScroll::Following => metrics.max_top,
        FeedScroll::Paused { top_line, .. } => top_line.min(metrics.max_top),
    };
    if range.start < top {
        set_focus_top(viewport, range.start, metrics.max_top, watermark);
    } else if range.end > top.saturating_add(metrics.feed_rows) {
        set_focus_top(
            viewport,
            range.end.saturating_sub(metrics.feed_rows),
            metrics.max_top,
            watermark,
        );
    }
    (viewport.focus, viewport.scroll.clone()) != before
}

fn set_focus_top(viewport: &mut FeedViewport, top_line: usize, max_top: usize, watermark: u64) {
    if top_line >= max_top {
        viewport.scroll = FeedScroll::Following;
        return;
    }
    let entry_watermark = match viewport.scroll {
        FeedScroll::Paused {
            entry_watermark, ..
        } => entry_watermark,
        FeedScroll::Following => watermark,
    };
    viewport.scroll = FeedScroll::Paused {
        top_line,
        entry_watermark,
    };
}

/// Toggle expansion for the run owned by the focused painted block.
#[allow(dead_code)]
pub(crate) fn toggle_focused_run(viewport: &mut FeedViewport, blocks: &[PaintedBlock]) -> bool {
    let Some(focused) = viewport.focus else {
        return false;
    };
    let Some(run) = blocks
        .iter()
        .find(|block| block.key == focused)
        .and_then(|block| block.run)
    else {
        return false;
    };
    if !viewport.expanded.remove(&run) {
        viewport.expanded.insert(run);
    }
    true
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::*;

    fn metrics(total_rows: usize, feed_rows: usize) -> FeedMetrics {
        FeedMetrics {
            total_rows,
            feed_rows,
            max_top: total_rows.saturating_sub(feed_rows),
            ranges: Vec::new(),
        }
    }

    fn paused(viewport: &FeedViewport) -> (usize, u64) {
        match viewport.scroll {
            FeedScroll::Paused {
                top_line,
                entry_watermark,
            } => (top_line, entry_watermark),
            FeedScroll::Following => panic!("expected paused viewport"),
        }
    }

    #[test]
    fn scrolling_back_pauses_with_the_current_watermark() {
        let mut viewport = FeedViewport::following();
        assert!(apply_scroll(
            &mut viewport,
            &metrics(100, 20),
            ScrollIntent::Rows(-3),
            41,
        ));
        assert_eq!(paused(&viewport), (77, 41));

        apply_scroll(&mut viewport, &metrics(120, 20), ScrollIntent::Rows(-3), 99);
        assert_eq!(paused(&viewport), (74, 41));
    }

    #[test]
    fn stale_paused_tops_clamp_and_resume_at_the_bottom() {
        let mut viewport = FeedViewport::following();
        viewport.scroll = FeedScroll::Paused {
            top_line: 500,
            entry_watermark: 7,
        };
        assert!(apply_scroll(
            &mut viewport,
            &metrics(30, 20),
            ScrollIntent::Rows(0),
            99,
        ));
        assert_eq!(viewport.scroll, FeedScroll::Following);
    }

    #[test]
    fn reaching_the_bottom_resumes_following() {
        let mut viewport = FeedViewport::following();
        viewport.scroll = FeedScroll::Paused {
            top_line: 70,
            entry_watermark: 7,
        };
        assert!(apply_scroll(
            &mut viewport,
            &metrics(100, 20),
            ScrollIntent::Page(1),
            99,
        ));
        assert_eq!(viewport.scroll, FeedScroll::Following);
    }

    #[test]
    fn oldest_and_follow_are_explicit_endpoints() {
        let mut viewport = FeedViewport::following();
        assert!(apply_scroll(
            &mut viewport,
            &metrics(100, 20),
            ScrollIntent::Oldest,
            23,
        ));
        assert_eq!(paused(&viewport), (0, 23));
        assert!(apply_scroll(
            &mut viewport,
            &metrics(100, 20),
            ScrollIntent::Follow,
            99,
        ));
        assert_eq!(viewport.scroll, FeedScroll::Following);
    }

    #[test]
    fn focus_moves_across_cached_block_ranges() {
        let mut viewport = FeedViewport::following();
        let metrics = FeedMetrics {
            total_rows: 30,
            feed_rows: 10,
            max_top: 20,
            ranges: vec![
                (BlockKey(1), Range { start: 0, end: 4 }),
                (BlockKey(2), Range { start: 12, end: 16 }),
                (BlockKey(3), Range { start: 25, end: 30 }),
            ],
        };
        assert!(move_focus(&mut viewport, &metrics, -1, 47));
        assert_eq!(viewport.focus, Some(BlockKey(3)));
        assert!(move_focus(&mut viewport, &metrics, -1, 99));
        assert_eq!(viewport.focus, Some(BlockKey(2)));
        assert_eq!(paused(&viewport), (12, 99));
    }

    #[test]
    fn only_a_focused_run_toggles_expansion() {
        let run = RunKey(11);
        let blocks = vec![PaintedBlock {
            key: BlockKey(7),
            lines: Vec::new(),
            copy_text: String::new(),
            run: Some(run),
        }];
        let mut viewport = FeedViewport::following();
        assert!(!toggle_focused_run(&mut viewport, &blocks));
        viewport.focus = Some(BlockKey(7));
        assert!(toggle_focused_run(&mut viewport, &blocks));
        assert!(viewport.expanded.contains(&run));
        assert!(toggle_focused_run(&mut viewport, &blocks));
        assert!(!viewport.expanded.contains(&run));
    }
}
