//! Immediate-mode layout: each frame lays rows out from the anchor at the
//! known width until the feed is full, so heights are exact before
//! anything is painted and the cost is the rows on screen, not the window.
//!
//! Rows come from ui-view one item at a time. An item outside any run
//! asks `chat_rows` for its own order; a run member asks `chat_rows_for`
//! by key, because `chat_rows` widens a range across a whole run and a run
//! can be thousands of calls long. Members of a collapsed run other than
//! its summary are skipped here before any row is built.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ops::RangeInclusive;

use ratatui::text::Line;
use ui_state::{Key, SessionState};
use ui_view::{ChatOptions, Row, Stretch, ToolRows, chat_rows, chat_rows_for, stretch_at};

use super::feed::{self, Header, LIVE_STEPS, LineHits, Placement};
use super::rows::{OPEN_LINES, PATCH_HEAD_LINES, RowFacts, RowState, on_rail, row_lines};
use crate::theme::Theme;

/// The stretches one layout pass has read, with their steps' orders, so a
/// stretch is walked once per frame however many of its rows draw.
#[derive(Debug, Default)]
pub struct StretchCache(RefCell<Vec<(Stretch, Vec<u64>)>>);

impl StretchCache {
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

/// What a block's toggle key opens or closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// The row itself, or its run: the old rule.
    Row,
    /// A stretch, by its oldest step.
    Stretch(Key),
    /// A step's detail.
    Step(Key),
}

/// Rows fetched per page, and the tail a chat opens with.
pub const PAGE: u32 = 40;
/// The most rows a chat's window keeps while the reader follows the newest.
/// While following, the layout pages only to keep a page of rows above the
/// screen, and the window must still hold a full screen and that page after
/// trimming, or each live row would trim what the next page fetches again:
/// rows draw at least one line, and 160 lines is taller than a terminal in
/// use, so a screen and the 40-row lookahead fit in 200.
pub const CAP: u32 = 200;
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
    /// What clicking along each line does, one entry per line.
    pub hits: Vec<LineHits>,
    pub toggle: Toggle,
}

/// What a frame's layout produced.
#[derive(Clone, Debug, Default)]
pub struct Laid {
    /// Exactly the feed's lines, top first; blank lines pad a short chat.
    pub lines: Vec<Line<'static>>,
    /// What clicking along each of `lines` does.
    pub hits: Vec<LineHits>,
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
    pub leader: char,
    /// Draw the redesigned feed: turns, with stretches of steps folded.
    pub redesigned: bool,
    /// Stretches the reader opened, by their oldest step.
    pub stretches: &'a HashSet<Key>,
    pub cache: &'a StretchCache,
    /// The step an ask in the composer's box points at: the box shows it,
    /// so the feed does not draw it twice.
    pub asking: Option<&'a Key>,
    /// Lines after the newest row, drawn by the caller and scrolled with
    /// the rest: the running turn's live end (your prompt on its way, the
    /// activity line).
    pub tail: &'a [Line<'static>],
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

    /// The row drawn at one held order, as shown, with whether its subagent
    /// parent is open; None when it draws nothing here.
    fn shown(
        &self,
        state: &SessionState,
        order: u64,
        runs: &[RangeInclusive<u64>],
    ) -> Option<(Row, bool)> {
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
        let mut shown = row;
        if child_open {
            shown.collapsed = false;
        }
        if shown.collapsed || matches!(shown.kind, ui_view::RowKind::Hidden) {
            return None;
        }
        Some((shown, child_open))
    }

    /// Whether the nearest row drawn beside `order`, older or newer, is a
    /// tool row.
    fn tool_beside(
        &self,
        state: &SessionState,
        order: u64,
        runs: &[RangeInclusive<u64>],
        older: bool,
    ) -> bool {
        let beside = |held: &ui_state::Held| self.shown(state, held.item.order, runs);
        let row = if older {
            before(state, order).rev().find_map(beside)
        } else {
            after(state, order).find_map(beside)
        };
        row.is_some_and(|(row, _)| on_rail(&row))
    }

    /// The row for one held order, or None when it draws nothing here.
    fn block(
        &self,
        state: &SessionState,
        order: u64,
        runs: &[RangeInclusive<u64>],
    ) -> Option<Block> {
        if self.redesigned {
            return self.feed_block(state, order, runs);
        }
        let (shown, child_open) = self.shown(state, order, runs)?;
        let expanded = self.expanded.contains(&shown.id)
            || shown
                .run
                .as_ref()
                .is_some_and(|_| runs.iter().any(|span| span.contains(&order)));
        let tool = on_rail(&shown);
        let joined = tool && self.tool_beside(state, order, runs, false);
        let row_state = RowState {
            focused: self.focus == Some(&shown.id),
            expanded,
            rail: joined || (tool && self.tool_beside(state, order, runs, true)),
            joined,
        };
        let mut facts = RowFacts {
            leader: self.leader,
            ..RowFacts::default()
        };
        if shown.run.as_ref().is_some_and(|run| run.is_summary) && !expanded {
            facts.run_subjects = ui_view::run_subjects(state, order, 2);
        }
        if matches!(shown.kind, ui_view::RowKind::FileChange { .. }) {
            let lines = if expanded {
                OPEN_LINES
            } else {
                PATCH_HEAD_LINES
            };
            facts.patch = ui_view::patch_head(state, &shown.id, lines);
        }
        let mut lines = row_lines(&shown, row_state, &facts, self.width, self.theme);
        if child_open {
            for line in &mut lines {
                line.spans.insert(0, "  ".into());
            }
        }
        if lines.is_empty() {
            return None;
        }
        let hits = vec![Vec::new(); lines.len()];
        Some(Block {
            key: shown.id.clone(),
            order,
            row: shown,
            lines,
            hits,
            toggle: Toggle::Row,
        })
    }

    /// The stretch holding `order` and its steps' orders, read once a frame.
    fn stretch(&self, state: &SessionState, order: u64) -> Option<(Stretch, Vec<u64>)> {
        if let Some(found) = self
            .cache
            .0
            .borrow()
            .iter()
            .find(|(stretch, _)| (stretch.oldest_order..=stretch.newest_order).contains(&order))
        {
            return Some(found.clone());
        }
        let stretch = stretch_at(state, order)?;
        if !(stretch.oldest_order..=stretch.newest_order).contains(&order) {
            return None;
        }
        let steps = ui_view::stretch_steps(state, &stretch);
        self.cache
            .0
            .borrow_mut()
            .push((stretch.clone(), steps.clone()));
        Some((stretch, steps))
    }

    /// A row of the redesigned feed. Inside a stretch, what draws depends on
    /// whether the stretch is open, under way or folded; outside one, text
    /// and turn ends draw from [`feed`], and the rest keeps its own drawing.
    fn feed_block(
        &self,
        state: &SessionState,
        order: u64,
        runs: &[RangeInclusive<u64>],
    ) -> Option<Block> {
        let everything = ChatOptions {
            tools: ToolRows::ShowAll,
        };
        let transcript = state.transcript();
        let held = transcript.at(order)?;
        if self.asking == Some(&held.item.key) {
            return None;
        }
        let (placement, row, toggle) = match self.stretch(state, order) {
            Some((stretch, steps)) => {
                let is_step = steps.binary_search(&order).is_ok();
                // The stretch's last step drawn: its newest, unless that is
                // the one the composer's box is asking about.
                let newest = steps
                    .iter()
                    .rev()
                    .copied()
                    .find(|step| {
                        transcript
                            .at(*step)
                            .is_none_or(|held| self.asking != Some(&held.item.key))
                    })
                    .unwrap_or(stretch.newest_order);
                let open = self.stretches.contains(&stretch.oldest);
                if open {
                    if !is_step {
                        return None;
                    }
                    // Opened, a stretch lists every step on its own line:
                    // merging its reads would only repeat its folded line.
                    let row = flat_step(state, order)?;
                    let first = steps
                        .iter()
                        .copied()
                        .find(|step| flat_step(state, *step).is_some())
                        == Some(order);
                    let placement = Placement::Step {
                        header: first.then(|| Header {
                            stretch: stretch.clone(),
                            open: true,
                            earlier: 0,
                        }),
                        joined: order != newest,
                        // Opened while under way, the step running now is
                        // still the bright one, as it is folded.
                        current: !stretch.closed
                            && order == stretch.newest_order
                            && stretch.running,
                    };
                    let toggle = Toggle::Step(row.id.clone());
                    (placement, row, toggle)
                } else if !stretch.closed {
                    // Under way: the newest few steps, the newest bright
                    // while it runs.
                    let shown_from = steps.len().saturating_sub(LIVE_STEPS);
                    let at = steps.iter().position(|step| *step == order)?;
                    if at < shown_from {
                        return None;
                    }
                    let mut row =
                        chat_rows_for(state, std::slice::from_ref(&held.item.key), &everything)
                            .pop()?;
                    // Live, each read is its own step: the motion is the point.
                    row.run = None;
                    let placement = Placement::Step {
                        header: (at == shown_from && shown_from > 0).then(|| Header {
                            stretch: stretch.clone(),
                            open: false,
                            earlier: shown_from,
                        }),
                        joined: order != newest,
                        current: order == stretch.newest_order && stretch.running,
                    };
                    let toggle = Toggle::Step(row.id.clone());
                    (placement, row, toggle)
                } else {
                    if order != stretch.newest_order {
                        return None;
                    }
                    let row =
                        chat_rows_for(state, std::slice::from_ref(&held.item.key), &everything)
                            .pop()?;
                    let unresolved = chat_rows_for(state, &stretch.unresolved, &everything);
                    let toggle = Toggle::Stretch(stretch.oldest.clone());
                    (
                        Placement::Folded {
                            stretch,
                            unresolved,
                        },
                        row,
                        toggle,
                    )
                }
            }
            None => {
                let (row, _) = self.shown(state, order, runs)?;
                if !matches!(
                    row.kind,
                    ui_view::RowKind::Prompt { .. }
                        | ui_view::RowKind::Prose { .. }
                        | ui_view::RowKind::Thinking { .. }
                        | ui_view::RowKind::TurnEnd { .. }
                        | ui_view::RowKind::Stopped
                        | ui_view::RowKind::Ask(_)
                ) && !feed::is_step(&row)
                {
                    // Asks, errors, boundaries and the rest keep their own
                    // drawing for now.
                    return self.old_block(state, order, runs);
                }
                let toggle = Toggle::Step(row.id.clone());
                (Placement::Plain, row, toggle)
            }
        };
        let expanded = self.expanded.contains(&row.id)
            || row
                .run
                .as_ref()
                .is_some_and(|_| runs.iter().any(|span| span.contains(&order)));
        let mut facts = RowFacts {
            leader: self.leader,
            ..RowFacts::default()
        };
        if expanded && matches!(row.kind, ui_view::RowKind::FileChange { .. }) {
            facts.patch = ui_view::patch_head(state, &row.id, OPEN_LINES);
        }
        let drawn = feed::row_lines(&row, &placement, expanded, &facts, self.width, self.theme)?;
        let mut lines = drawn.lines;
        if self.focus == Some(&row.id) {
            for line in &mut lines {
                line.spans
                    .insert(0, ratatui::text::Span::styled("▌", self.theme.focus_bar()));
                if let Some(second) = line.spans.get_mut(1)
                    && second.content.starts_with(' ')
                {
                    second.content = second.content[1..].to_owned().into();
                }
            }
        }
        Some(Block {
            key: row.id.clone(),
            order,
            row,
            lines,
            hits: drawn.hits,
            toggle,
        })
    }

    /// A row the redesign leaves as it was.
    fn old_block(
        &self,
        state: &SessionState,
        order: u64,
        runs: &[RangeInclusive<u64>],
    ) -> Option<Block> {
        // Their glyphs already sit on the feed's left edge and their words on
        // its second column.
        let old = Frame {
            redesigned: false,
            ..*self
        };
        old.block(state, order, runs)
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
        self.cache.clear();
        let runs = self.expanded_runs(state);
        let transcript = state.transcript();
        let (Some(oldest), Some(head)) = (transcript.oldest_held(), transcript.head()) else {
            return Laid {
                lines: vec![Line::default(); self.height],
                hits: vec![Vec::new(); self.height],
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
            // Following, a page brings only what fits under the cap, so the
            // next live row never trims what it fetched.
            let room = state
                .page_room()
                .map_or(u32::MAX, |room| u32::try_from(room).unwrap_or(u32::MAX));
            let size = run_page.unwrap_or(PAGE).min(room);
            laid.page = (size > 0).then_some(size);
        }
        laid
    }

    /// Fills the feed upward from `from`, newest at the bottom.
    fn upward(&self, state: &SessionState, from: u64, runs: &[RangeInclusive<u64>]) -> Laid {
        let mut blocks = Vec::new();
        let mut total = self.tail.len();
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
            .chain(self.tail.iter().cloned())
            .skip(top_offset)
            .collect();
        let mut hits: Vec<LineHits> = blocks
            .iter()
            .flat_map(|block| block.hits.iter().cloned())
            .chain(self.tail.iter().map(|_| Vec::new()))
            .skip(top_offset)
            .collect();
        if lines.len() < self.height {
            let pad = self.height - lines.len();
            let mut padded = vec![Line::default(); pad];
            padded.append(&mut lines);
            lines = padded;
            let mut padded = vec![Vec::new(); pad];
            padded.append(&mut hits);
            hits = padded;
        }
        Laid {
            lines,
            hits,
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
        let mut reached_head = true;
        for held in transcript.range(from..=head) {
            if let Some(block) = self.block(state, held.item.order, runs) {
                total += block.lines.len();
                blocks.push(block);
            }
            if total >= offset + self.height {
                reached_head = held.item.order == head;
                break;
            }
        }
        let offset = offset.min(
            blocks
                .first()
                .map_or(0, |b| b.lines.len().saturating_sub(1)),
        );
        // The live end follows the newest row.
        let tail = if reached_head { self.tail } else { &[] };
        let total = total + tail.len();
        // What lies below fits exactly: that is the bottom too.
        if total < offset + self.height || (reached_head && total == offset + self.height) {
            return None;
        }
        let lines = blocks
            .iter()
            .flat_map(|block| block.lines.iter().cloned())
            .chain(tail.iter().cloned())
            .skip(offset)
            .take(self.height)
            .collect();
        let hits = blocks
            .iter()
            .flat_map(|block| block.hits.iter().cloned())
            .chain(tail.iter().map(|_| Vec::new()))
            .skip(offset)
            .take(self.height)
            .collect();
        Some(Laid {
            lines,
            hits,
            blocks,
            top_offset: offset,
            at_bottom: false,
            ..Laid::default()
        })
    }

    /// The anchor `lines` above (negative) or below the top of `laid`;
    /// Bottom once it would pass the newest row.
    pub fn scrolled(&self, state: &SessionState, laid: &Laid, delta: isize) -> Anchor {
        self.cache.clear();
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

/// One step of a stretch as its own row, never merged into a run: how an
/// opened stretch draws each step. None when the step draws nothing.
fn flat_step(state: &SessionState, order: u64) -> Option<Row> {
    let held = state.transcript().at(order)?;
    let everything = ChatOptions {
        tools: ToolRows::ShowAll,
    };
    let mut row = chat_rows_for(state, std::slice::from_ref(&held.item.key), &everything).pop()?;
    if row.collapsed || matches!(row.kind, ui_view::RowKind::Hidden) {
        return None;
    }
    row.run = None;
    Some(row)
}
