//! Immediate-mode layout: each frame lays rows out from the anchor at the
//! known width until the feed is full, so heights are exact before
//! anything is painted and the cost is the rows on screen, not the window.
//!
//! Rows come from ui-view one item at a time. An item outside any run
//! asks `chat_rows` for its own order; a run member asks `chat_rows_for`
//! by key, because `chat_rows` widens a range across a whole run and a run
//! can be thousands of calls long. Members of a collapsed run other than
//! its summary are skipped here before any row is built.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use ratatui::text::Line;
use ui_state::{Key, SessionState};
use ui_view::{ChatOptions, Row, ToolRows, chat_rows, chat_rows_for};

use super::rows::{RowState, row_lines};
use crate::theme::Theme;

/// Rows fetched per page, and the tail a chat opens with.
pub const PAGE: u32 = 40;
/// Fewer held rows than this above the screen asks for an older page.
pub const LOOKAHEAD: u64 = PAGE as u64;
/// The largest page asked for at an open collapsed run.
pub const RUN_PAGE_CAP: u32 = 1_000;

/// Where the reader is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    /// Following the newest row.
    #[default]
    Bottom,
    /// This row at the top of the feed, with `offset` of its lines above
    /// the screen.
    Top { key: Key, offset: usize },
}

/// One drawn row: its key and its lines.
#[derive(Clone, Debug)]
pub struct Block {
    pub key: Key,
    pub order: u64,
    pub row: Row,
    pub lines: Vec<Line<'static>>,
}

/// What a frame's layout produced.
#[derive(Clone, Debug, Default)]
pub struct Laid {
    /// Exactly the feed's lines, top first; blank lines pad a short chat.
    pub lines: Vec<Line<'static>>,
    /// The drawn rows, top first, with how many of the first one's lines
    /// sit above the screen.
    pub blocks: Vec<Block>,
    pub top_offset: usize,
    /// The feed shows the newest row at its bottom.
    pub at_bottom: bool,
    /// Held rows that would draw above the screen.
    pub held_above: u64,
    /// An older page to ask for, and its size, when the reader is within a
    /// page of the oldest held row and older history exists.
    pub page: Option<u32>,
}

/// The view state layout reads.
pub struct Frame<'a> {
    pub anchor: &'a Anchor,
    pub expanded: &'a HashSet<Key>,
    pub focus: Option<&'a Key>,
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
}

impl Frame<'_> {
    /// The spans of runs the reader expanded.
    fn expanded_runs(&self, state: &SessionState) -> Vec<RangeInclusive<u64>> {
        let transcript = state.transcript();
        self.expanded
            .iter()
            .filter_map(|key| transcript.get(key))
            .filter_map(|held| transcript.run_at(held.item.order))
            .map(|run| run.oldest..=run.newest)
            .collect()
    }

    /// The row for one held order, or None when it draws nothing here.
    fn block(
        &self,
        state: &SessionState,
        order: u64,
        runs: &[RangeInclusive<u64>],
    ) -> Option<Block> {
        let transcript = state.transcript();
        let held = transcript.at(order)?;
        let opts = ChatOptions {
            tools: ToolRows::CollapseRuns {
                expanded: self.expanded,
            },
        };
        let row = match transcript.run_at(order) {
            Some(run) => {
                let open = runs.iter().any(|span| span.contains(&order));
                if !open && run.newest != order {
                    return None;
                }
                chat_rows_for(state, std::slice::from_ref(&held.item.key), &opts).pop()?
            }
            None => chat_rows(state, order..=order, &opts)
                .into_iter()
                .find(|row| row.order == order)?,
        };
        let child_open = row
            .parent
            .as_ref()
            .is_some_and(|parent| self.expanded.contains(parent));
        let mut shown = row.clone();
        if child_open {
            shown.collapsed = false;
        }
        let expanded = self.expanded.contains(&row.id)
            || row
                .run
                .as_ref()
                .is_some_and(|_| runs.iter().any(|span| span.contains(&order)));
        let state = RowState {
            focused: self.focus == Some(&row.id),
            expanded,
        };
        let mut lines = row_lines(&shown, state, self.width, self.theme);
        if child_open {
            for line in &mut lines {
                line.spans.insert(0, "  ".into());
            }
        }
        if lines.is_empty() {
            return None;
        }
        Some(Block {
            key: row.id.clone(),
            order,
            row,
            lines,
        })
    }

    /// Held rows that would draw below `top`: every held order above the
    /// oldest, less the hidden members of collapsed runs.
    fn held_above(&self, state: &SessionState, top: u64, runs: &[RangeInclusive<u64>]) -> u64 {
        let transcript = state.transcript();
        let Some(oldest) = transcript.oldest_held() else {
            return 0;
        };
        let mut count = top.saturating_sub(oldest);
        for span in transcript.runs().spans() {
            if *span.start() >= top {
                break;
            }
            if runs.iter().any(|open| open.contains(span.start())) {
                continue;
            }
            let end = (*span.end()).min(top.saturating_sub(1));
            // A run's members above its summary do not draw.
            let hidden = if *span.end() < top {
                span.end() - span.start()
            } else {
                end + 1 - span.start()
            };
            count = count.saturating_sub(hidden);
        }
        count
    }

    pub fn layout(&self, state: &SessionState) -> Laid {
        let runs = self.expanded_runs(state);
        let transcript = state.transcript();
        let (Some(oldest), Some(head)) = (transcript.oldest_held(), transcript.head()) else {
            return Laid {
                lines: vec![Line::default(); self.height],
                at_bottom: true,
                page: transcript.has_older().then_some(PAGE),
                ..Laid::default()
            };
        };
        let anchored = match self.anchor {
            Anchor::Bottom => None,
            Anchor::Top { key, offset } => {
                transcript.get(key).map(|held| (held.item.order, *offset))
            }
        };
        let mut laid = match anchored {
            Some((order, offset)) => self
                .downward(state, order, offset, &runs)
                .unwrap_or_else(|| self.upward(state, head, &runs)),
            None => self.upward(state, head, &runs),
        };
        let top = laid.blocks.first().map_or(oldest, |block| block.order);
        laid.held_above = self.held_above(state, top, &runs);
        if laid.held_above < LOOKAHEAD && transcript.has_older() {
            let run_page = laid
                .blocks
                .first()
                .and_then(|block| block.row.run.as_ref())
                .filter(|run| run.is_summary && run.open_below)
                .map(|run| run.len.clamp(PAGE, RUN_PAGE_CAP));
            laid.page = Some(run_page.unwrap_or(PAGE));
        }
        laid
    }

    /// Fills the feed upward from `from`, newest at the bottom.
    fn upward(&self, state: &SessionState, from: u64, runs: &[RangeInclusive<u64>]) -> Laid {
        let mut blocks = Vec::new();
        let mut total = 0usize;
        let mut order = Some(from);
        while let Some(at) = order {
            if total >= self.height {
                break;
            }
            if let Some(block) = self.block(state, at, runs) {
                total += block.lines.len();
                blocks.push(block);
            }
            order = before(state, at).next_back().map(|held| held.item.order);
        }
        blocks.reverse();
        let top_offset = total.saturating_sub(self.height);
        let mut lines: Vec<Line<'static>> = blocks
            .iter()
            .flat_map(|block| block.lines.iter().cloned())
            .skip(top_offset)
            .collect();
        if lines.len() < self.height {
            let mut padded = vec![Line::default(); self.height - lines.len()];
            padded.append(&mut lines);
            lines = padded;
        }
        Laid {
            lines,
            blocks,
            top_offset,
            at_bottom: true,
            ..Laid::default()
        }
    }

    /// Fills the feed downward from the anchor row; None when what lies
    /// below it does not fill the feed, so the reader is at the bottom.
    fn downward(
        &self,
        state: &SessionState,
        from: u64,
        offset: usize,
        runs: &[RangeInclusive<u64>],
    ) -> Option<Laid> {
        let transcript = state.transcript();
        let head = transcript.head()?;
        let mut blocks = Vec::new();
        let mut total = 0usize;
        for held in transcript.range(from..=head) {
            if let Some(block) = self.block(state, held.item.order, runs) {
                total += block.lines.len();
                blocks.push(block);
            }
            if total >= offset + self.height {
                break;
            }
        }
        let offset = offset.min(
            blocks
                .first()
                .map_or(0, |b| b.lines.len().saturating_sub(1)),
        );
        if total < offset + self.height {
            return None;
        }
        let lines = blocks
            .iter()
            .flat_map(|block| block.lines.iter().cloned())
            .skip(offset)
            .take(self.height)
            .collect();
        Some(Laid {
            lines,
            blocks,
            top_offset: offset,
            at_bottom: false,
            ..Laid::default()
        })
    }

    /// The anchor `lines` above (negative) or below the top of `laid`;
    /// Bottom once it would pass the newest row.
    pub fn scrolled(&self, state: &SessionState, laid: &Laid, delta: isize) -> Anchor {
        let runs = self.expanded_runs(state);
        let transcript = state.transcript();
        let Some(first) = laid.blocks.first() else {
            return Anchor::Bottom;
        };
        let mut order = first.order;
        let mut offset = laid.top_offset as isize + delta;
        let mut height = first.lines.len() as isize;
        while offset < 0 {
            let above = before(state, order)
                .rev()
                .find_map(|held| self.block(state, held.item.order, &runs));
            let Some(block) = above else {
                offset = 0;
                break;
            };
            order = block.order;
            height = block.lines.len() as isize;
            offset += height;
        }
        while offset >= height {
            let below =
                after(state, order).find_map(|held| self.block(state, held.item.order, &runs));
            let Some(block) = below else {
                return Anchor::Bottom;
            };
            offset -= height;
            order = block.order;
            height = block.lines.len() as isize;
        }
        let key = transcript
            .at(order)
            .map(|held| held.item.key.clone())
            .unwrap_or_default();
        let anchor = Anchor::Top {
            key,
            offset: offset as usize,
        };
        // Scrolling down to where the rest no longer fills the feed is
        // following again.
        let probe = Frame {
            anchor: &anchor,
            ..*self
        };
        if probe
            .downward(state, order, offset as usize, &runs)
            .is_none()
        {
            return Anchor::Bottom;
        }
        anchor
    }
}

/// Held items strictly older than `order`, oldest first.
fn before(state: &SessionState, order: u64) -> impl DoubleEndedIterator<Item = &ui_state::Held> {
    let transcript = state.transcript();
    let oldest = transcript.oldest_held().unwrap_or(order);
    let end = order.saturating_sub(1);
    let empty = order <= oldest;
    transcript
        .range(if empty { order..=order } else { oldest..=end })
        .filter(move |_| !empty)
}

/// Held items strictly newer than `order`, oldest first.
fn after(state: &SessionState, order: u64) -> impl DoubleEndedIterator<Item = &ui_state::Held> {
    let transcript = state.transcript();
    let head = transcript.head().unwrap_or(order);
    let empty = order >= head;
    transcript
        .range(if empty {
            order..=order
        } else {
            order + 1..=head
        })
        .filter(move |_| !empty)
}
