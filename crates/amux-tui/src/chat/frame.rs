//! Agent-agnostic chat geometry, windowing, and paint memoization.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

use super::FeedScroll;
use super::blocks::RunKey;
use super::viewport::FeedViewport;
use crate::render::{Theme, clip_to_width, str_width};
use crate::theme::{ColorMode, ThemeName};

/// Columns every feed row keeps clear at its left edge. Column 0 is the
/// mark column — where a block wears the accent bar of a user surface or
/// the focus bar when it is selected — and column 1 separates that mark
/// from the text. Because the mark is drawn into a column the painters
/// already left empty, selecting a block never shifts its content
/// sideways, and the screen keeps a straight left edge with no border
/// and no chrome gutter.
pub(crate) const CONTENT_INDENT: usize = 2;

/// The glyph both left-edge marks are drawn with.
pub(crate) const MARK_GLYPH: &str = "\u{258e}";

/// Renderer-local identity for one painted block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct BlockKey(pub(crate) u64);

/// What a block is, as far as the space around it is concerned.
///
/// A feed mixes things that were said with things that happened, and the two
/// want different amounts of air. The distinction lives here rather than in
/// each painter's head because it is the frame, not the painter, that owns
/// the rows between blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockKind {
    /// Someone's words: the person's message, the agent's reply, a note from
    /// another agent, a plan it wrote.
    Speech,
    /// Something the agent did: a command, a search, a file changed, an
    /// error, a thought.
    Activity,
    /// One attachment a message carries, as a row of its own so it can be
    /// focused and opened by itself. It belongs to the speech above it and
    /// hangs directly under it.
    Attachment,
    /// A rule across the feed marking a boundary rather than an event.
    Divider,
    /// A docked panel: a permission ask, a diff. Full-weight content that
    /// arrives with its own surface.
    Panel,
}

/// Finished rows and interaction metadata produced by one block painter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaintedBlock {
    pub(crate) key: BlockKey,
    pub(crate) kind: BlockKind,
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) copy_text: String,
    pub(crate) run: Option<RunKey>,
}

/// Agent-owned content handed to the shared frame shell.
pub(crate) struct ChatFrameParts {
    pub(crate) header: Line<'static>,
    pub(crate) banner: Option<Line<'static>>,
    pub(crate) feed: FeedBlocks,
    pub(crate) activity: Option<Line<'static>>,
    pub(crate) bottom: Vec<Line<'static>>,
    pub(crate) overlay: Option<Vec<Line<'static>>>,
}

impl ChatFrameParts {
    /// Derive layout from the same optional rows and bottom block that will
    /// be composed. `target_paused` may differ from the current viewport:
    /// scroll metrics deliberately measure the paused layout while following
    /// because the first scroll action adds the paused rule row, and its
    /// bounds must describe the layout that action enters.
    pub(crate) fn geometry(&self, viewport: (u16, u16), target_paused: bool) -> ChatGeometry {
        chat_geometry(
            viewport,
            FrameShape::DEFAULT,
            self.banner.is_some(),
            self.activity.is_some(),
            target_paused,
            self.bottom.len(),
        )
    }
}

/// Painted feed blocks and facts about their retained-history boundary.
pub(crate) struct FeedBlocks {
    pub(crate) blocks: Vec<PaintedBlock>,
    pub(crate) history_truncated: bool,
    pub(crate) loading: bool,
}

impl FeedBlocks {
    pub(crate) fn boundary_rows(&self) -> usize {
        usize::from(self.history_truncated) + usize::from(self.loading)
    }
}

/// Eye-judged whitespace consumed by the geometry engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameSpacing {
    pub(crate) header_gap: usize,
    /// Rows between two adjacent blocks, unless both are machinery.
    pub(crate) block_gap: usize,
    /// Rows between two consecutive actions.
    pub(crate) action_gap: usize,
    pub(crate) bottom_gap: usize,
}

/// The rows of air between two adjacent blocks.
///
/// Both the geometry pass and the compose pass ask this, and they must agree
/// exactly: a feed whose measured height disagrees with its painted height
/// scrolls to a bottom that is not the bottom.
pub(crate) fn gap_between(previous: BlockKind, next: BlockKind, spacing: FrameSpacing) -> usize {
    match (previous, next) {
        (BlockKind::Activity, BlockKind::Activity) => spacing.action_gap,
        (BlockKind::Speech | BlockKind::Attachment, BlockKind::Attachment) => 0,
        _ => spacing.block_gap,
    }
}

/// Everything besides content that decides the frame's arithmetic: the
/// rows of air, and the columns kept clear at the left edge.
///
/// One value rather than two arguments because they always travel together
/// and the geometry pass and the compose pass must agree on both, or a feed
/// scrolls to a bottom that is not the bottom.
///
/// A conversation is never bounded. The fleet draws a box; a conversation
/// runs to the edges, and the mark column is its only left-edge chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameShape {
    pub(crate) spacing: FrameSpacing,
    pub(crate) content_indent: usize,
}

impl FrameShape {
    pub(crate) const DEFAULT: Self = Self {
        spacing: FrameSpacing::DEFAULT,
        content_indent: CONTENT_INDENT,
    };
}

impl FrameSpacing {
    /// Chosen by eye at 120x40 against the idle Claude and Codex screens.
    /// One blank row under the header is enough to lift it off the feed
    /// once the old rule is gone; a second reads as a gap rather than a
    /// margin on a screen this short. One blank row between blocks keeps
    /// a long feed legible without halving how much of it fits, and one
    /// above the bottom block separates what the agent said from what
    /// the person is typing. No blank row between two consecutive actions:
    /// a burst of tool work is one event to a reader, and nine rows of
    /// machinery spread over eighteen read as nine separate events.
    pub(crate) const DEFAULT: Self = Self {
        header_gap: 1,
        block_gap: 1,
        action_gap: 0,
        bottom_gap: 1,
    };
}

/// The row budget shared by composition, scrolling, and hit-testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChatGeometry {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) feed_top: usize,
    pub(crate) feed_rows: usize,
    pub(crate) bottom_top: usize,
    /// Carried on the geometry so a caller that already has one does not
    /// need the design in hand to ask how wide a feed row may be.
    pub(crate) content_indent: usize,
}

impl ChatGeometry {
    /// The width a block painter may fill: everything but the left mark
    /// column and its separator. Used by the agent adapters.
    #[allow(dead_code)]
    pub(crate) fn feed_width(&self) -> usize {
        self.width.saturating_sub(self.content_indent)
    }
}

/// Compute the full-screen frame budget without inspecting agent content.
fn chat_geometry(
    viewport: (u16, u16),
    shape: FrameShape,
    banner: bool,
    activity: bool,
    paused: bool,
    bottom_rows: usize,
) -> ChatGeometry {
    let FrameShape {
        spacing,
        content_indent,
    } = shape;
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    let feed_top = (1 + usize::from(banner) + spacing.header_gap).min(height);
    let bottom_top = height.saturating_sub(bottom_rows.min(height));
    // The working row is a block of its own: air above it as well as
    // below, or it hangs off whatever the feed happened to end with.
    let activity_rows = if activity { 1 + spacing.bottom_gap } else { 0 };
    let feed_bottom =
        bottom_top.saturating_sub(spacing.bottom_gap + activity_rows + usize::from(paused));
    let feed_rows = feed_bottom.saturating_sub(feed_top);
    ChatGeometry {
        width,
        height,
        feed_top,
        feed_rows,
        bottom_top,
        content_indent,
    }
}

/// Cached feed row totals and block ranges at one frame geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FeedMetrics {
    pub(crate) total_rows: usize,
    pub(crate) feed_rows: usize,
    pub(crate) max_top: usize,
    pub(crate) ranges: Vec<(BlockKey, Range<usize>)>,
}

/// Count rows directly from painted blocks, without repainting content.
pub(crate) fn feed_metrics(
    feed: &FeedBlocks,
    spacing: FrameSpacing,
    geometry: &ChatGeometry,
) -> FeedMetrics {
    // Boundary rows share the scroll coordinate space with blocks. Their
    // offset must be present in both max_top and block ranges or a
    // truncated feed can never reach its real bottom and focus motion
    // targets the wrong rows.
    let mut cursor = feed.boundary_rows();
    let mut ranges = Vec::with_capacity(feed.blocks.len());
    for (index, block) in feed.blocks.iter().enumerate() {
        if let Some(previous) = index.checked_sub(1) {
            cursor += gap_between(feed.blocks[previous].kind, block.kind, spacing);
        }
        let start = cursor;
        cursor += block.lines.len();
        ranges.push((block.key, start..cursor));
    }
    FeedMetrics {
        total_rows: cursor,
        feed_rows: geometry.feed_rows,
        max_top: cursor.saturating_sub(geometry.feed_rows),
        ranges,
    }
}

/// Compose one complete full-screen chat frame from agent-painted parts.
pub(crate) fn compose_chat_frame(
    parts: ChatFrameParts,
    viewport: &FeedViewport,
    theme: Theme,
    size: (u16, u16),
) -> Vec<Line<'static>> {
    let paused = matches!(viewport.scroll, FeedScroll::Paused { .. });
    let spacing = FrameSpacing::DEFAULT;
    let geometry = parts.geometry(size, paused);

    if let Some(overlay) = parts.overlay {
        return fit_frame(overlay, geometry.width, geometry.height, theme);
    }

    let metrics = feed_metrics(&parts.feed, spacing, &geometry);
    let mut boundary = Vec::with_capacity(
        usize::from(parts.feed.history_truncated) + usize::from(parts.feed.loading),
    );
    if parts.feed.history_truncated {
        boundary.push((
            indented_row("⋯ earlier history unavailable", theme.muted(), theme),
            None,
        ));
    }
    if parts.feed.loading {
        boundary.push((
            indented_row("⟳ loading session…", theme.muted(), theme),
            None,
        ));
    }
    let boundary_rows = boundary.len();
    let chained = chained_runs(&parts.feed.blocks);
    let mut feed = Vec::with_capacity(metrics.total_rows);
    for (index, block) in parts.feed.blocks.iter().enumerate() {
        if let Some(previous) = index.checked_sub(1) {
            let gap = gap_between(parts.feed.blocks[previous].kind, block.kind, spacing);
            feed.extend((0..gap).map(|_| (Line::default(), None)));
        }
        feed.extend(block.lines.iter().cloned().map(|line| {
            let line = if chained[index] {
                write_mark(line, CHAIN, theme.gutter())
            } else {
                line
            };
            (line, Some(block.key))
        }));
    }

    let max_top = metrics.max_top;
    let start = match viewport.scroll {
        FeedScroll::Following => max_top,
        FeedScroll::Paused { top_line, .. } => top_line.min(max_top),
    };
    let feed_start = start.saturating_sub(boundary_rows);
    let visible = if start == 0 {
        boundary.into_iter().chain(feed).collect::<Vec<_>>()
    } else {
        feed.into_iter().skip(feed_start).collect()
    };
    let mut window: Vec<_> = visible
        .into_iter()
        .take(geometry.feed_rows)
        .map(|(line, key)| {
            // The rows between blocks, and the boundary rows above them,
            // belong to no block; nothing focuses them, so they must not
            // match a viewport that is focusing nothing.
            if key.is_some() && key == viewport.focus {
                mark_focused(line, theme)
            } else {
                line
            }
        })
        .collect();
    window.resize_with(geometry.feed_rows, Line::default);

    let mut lines = Vec::with_capacity(geometry.height);
    lines.push(parts.header);
    if let Some(banner) = parts.banner {
        lines.push(banner);
    }
    lines.extend((0..spacing.header_gap).map(|_| Line::default()));
    lines.extend(window);
    if paused {
        lines.push(indented_row(
            crate::bindings::PAUSED_RULE,
            theme.muted(),
            theme,
        ));
    }
    if let Some(activity) = parts.activity {
        lines.extend((0..spacing.bottom_gap).map(|_| Line::default()));
        lines.push(activity);
    }
    lines.extend((0..spacing.bottom_gap).map(|_| Line::default()));
    lines.extend(parts.bottom);
    fit_frame(lines, geometry.width, geometry.height, theme)
}

fn fit_frame(
    mut lines: Vec<Line<'static>>,
    width: usize,
    height: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    lines.truncate(height);
    lines.resize_with(height, Line::default);
    lines
        .into_iter()
        .map(|line| fit_line(line, width, theme))
        .collect()
}

fn fit_line(mut line: Line<'static>, width: usize, theme: Theme) -> Line<'static> {
    // Every row of the screen is body text on the background token, so a
    // span that names no colour is still a token the style map can read
    // rather than whatever the terminal happens to default to.
    line = line.patch_style(theme.text().patch(theme.background()));
    let mut used = 0;
    for span in &mut line.spans {
        let span_width = str_width(&span.content);
        if used + span_width > width {
            let keep = width.saturating_sub(used);
            span.content = clip_to_width(&span.content, keep).to_string().into();
        }
        used += str_width(&span.content);
    }
    line.spans.retain(|span| !span.content.is_empty());
    if used < width {
        line.spans
            .push(Span::styled(" ".repeat(width - used), theme.background()));
    }
    line
}

/// One indented row of plain text — the shape the shell's own rows take,
/// so the boundary and paused rows line up with painted block content.
fn indented_row(text: &str, style: Style, theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(" ".repeat(CONTENT_INDENT), theme.background()),
        Span::styled(text.to_string(), style),
    ])
}

/// Draw the focus bar into the mark column of one row of the focused
/// block. It overwrites that column instead of being inserted, so
/// focusing a block never pushes its text one cell to the right.
fn mark_focused(line: Line<'static>, theme: Theme) -> Line<'static> {
    write_mark(line, MARK_GLYPH, theme.focus_bar())
}

/// The line linking a run of consecutive actions into one thing.
///
/// A burst of tool work stacked with no air between the rows reads as a
/// list of unrelated events that happen to be adjacent. The line says they
/// are one stretch of work — and it lives in the mark column, so the focus
/// bar simply replaces it on the row it lands on.
const CHAIN: &str = "\u{2502}";

/// Which blocks sit inside a run of two or more consecutive actions.
fn chained_runs(blocks: &[PaintedBlock]) -> Vec<bool> {
    let neighbour = |index: usize| {
        blocks
            .get(index)
            .is_some_and(|block| block.kind == BlockKind::Activity)
    };
    (0..blocks.len())
        .map(|index| {
            blocks[index].kind == BlockKind::Activity
                && (index.checked_sub(1).is_some_and(neighbour) || neighbour(index + 1))
        })
        .collect()
}

/// Overwrite a row's first cell with a mark, rather than inserting one.
///
/// Every feed row leaves that column clear for exactly this: marking a block
/// must never push its text one cell to the right, or focusing something
/// would reflow it.
fn write_mark(line: Line<'static>, glyph: &str, style: Style) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    // The mark takes the surface of the cell it replaces: a bar or a chain
    // drawn on a panel stays on the panel rather than punching a hole of
    // bare ground in its first column.
    let under = line
        .spans
        .iter()
        .find(|span| str_width(&span.content) > 0)
        .map(|span| span.style)
        .unwrap_or_default();
    spans.push(Span::styled(glyph.to_string(), under.patch(style)));
    let mut marked = false;
    for span in line.spans {
        if marked || str_width(&span.content) == 0 {
            spans.push(span);
            continue;
        }
        marked = true;
        let text = span.content.into_owned();
        let Some(first) = text.graphemes(true).next() else {
            continue;
        };
        // A wide grapheme that gave up one of its two cells leaves a hole;
        // a space keeps everything after it in the same column.
        let mut rest = String::new();
        if str_width(first) > 1 {
            rest.push(' ');
        }
        rest.push_str(&text[first.len()..]);
        if !rest.is_empty() {
            spans.push(Span::styled(rest, span.style));
        }
    }
    Line { spans, ..line }
}

/// Paint/reuse counters for the most recently measured frame interval.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaintStats {
    pub painted: usize,
    pub reused: usize,
}

/// Everything besides a block's own content that decides what it looks
/// like. The cache validates against all of it, which is the whole reason it
/// is one value: an input added here and forgotten at a comparison site is
/// how a cache starts serving rows painted for a different screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaintInputs {
    pub(crate) width: usize,
    pub(crate) theme: Theme,
    pub(crate) expanded: bool,
}

/// The comparable projection of those inputs. A `Theme` carries a whole
/// palette; what decides a repaint is which palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PaintKey {
    width: usize,
    theme: (ThemeName, ColorMode),
    expanded: bool,
}

impl From<PaintInputs> for PaintKey {
    fn from(inputs: PaintInputs) -> Self {
        Self {
            width: inputs.width,
            theme: (inputs.theme.name, inputs.theme.mode),
            expanded: inputs.expanded,
        }
    }
}

struct CachedBlock {
    /// `Send` so the whole view can be cloned into a diagnostic snapshot
    /// that crosses to the runtime's Msg tap. Cache content is plain data
    /// — the bound costs nothing and states where the view can travel.
    content: Box<dyn Any + Send>,
    key: PaintKey,
    block: PaintedBlock,
}

/// A cache key held as borrowed parts. Comparing costs nothing; the
/// owned copy the cache keeps is built only when a block is repainted.
pub(crate) trait CacheView: Copy {
    type Owned: PartialEq<Self> + Send + 'static;

    fn to_owned_key(self) -> Self::Owned;
}

/// Memoized block painting, validated against all paint-affecting inputs.
#[derive(Default)]
pub(crate) struct PaintCache {
    entries: HashMap<BlockKey, CachedBlock>,
    stats: PaintStats,
}

impl fmt::Debug for PaintCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaintCache")
            .field("entries", &self.entries.len())
            .field("stats", &self.stats)
            .finish()
    }
}

impl PaintCache {
    pub(crate) fn get_or_paint<K: PartialEq + Clone + Send + 'static>(
        &mut self,
        key: BlockKey,
        content: &K,
        inputs: PaintInputs,
        paint: impl FnOnce() -> PaintedBlock,
    ) -> &PaintedBlock {
        let paint_key = PaintKey::from(inputs);
        let hit = self.entries.get(&key).is_some_and(|cached| {
            cached.key == paint_key
                && cached
                    .content
                    .downcast_ref::<K>()
                    .is_some_and(|cached_content| cached_content == content)
        });
        if hit {
            self.stats.reused += 1;
            return &self.entries.get(&key).expect("cache hit exists").block;
        }

        self.stats.painted += 1;
        self.entries.insert(
            key,
            CachedBlock {
                content: Box::new(content.clone()),
                key: paint_key,
                block: paint(),
            },
        );
        &self.entries.get(&key).expect("painted entry exists").block
    }

    /// [`Self::get_or_paint`] for a key that has to be assembled out of
    /// several borrowed parts. The frame compares what the layer already
    /// holds; the owned copy is built only when the block is actually
    /// repainted, so a steady frame copies nothing.
    pub(crate) fn get_or_paint_view<V: CacheView>(
        &mut self,
        key: BlockKey,
        view: V,
        inputs: PaintInputs,
        paint: impl FnOnce() -> PaintedBlock,
    ) -> &PaintedBlock {
        let paint_key = PaintKey::from(inputs);
        let hit = self.entries.get(&key).is_some_and(|cached| {
            cached.key == paint_key
                && cached
                    .content
                    .downcast_ref::<V::Owned>()
                    .is_some_and(|cached_content| *cached_content == view)
        });
        if hit {
            self.stats.reused += 1;
            return &self.entries.get(&key).expect("cache hit exists").block;
        }

        self.stats.painted += 1;
        self.entries.insert(
            key,
            CachedBlock {
                content: Box::new(view.to_owned_key()),
                key: paint_key,
                block: paint(),
            },
        );
        &self.entries.get(&key).expect("painted entry exists").block
    }

    pub(crate) fn retain(&mut self, live: &[BlockKey]) {
        let live: HashSet<_> = live.iter().copied().collect();
        self.entries.retain(|key, _| live.contains(key));
    }

    /// Read by tests and by the fixtures' `paint_stats`.
    pub(crate) fn stats(&self) -> PaintStats {
        self.stats
    }

    pub(crate) fn reset_stats(&mut self) {
        self.stats = PaintStats::default();
    }
}

#[cfg(test)]
#[test]
fn geometry_identical_for_claude_and_codex_fixtures_at_120_by_40() {
    use crate::fixtures::{NamedState, fixture};
    use crate::render::FrameContext;

    let claude = fixture(NamedState::ClaudeIdle);
    let mut codex = fixture(NamedState::CodexIdle);
    codex
        .view
        .chat
        .as_mut()
        .expect("Codex fixture opens a chat")
        .set_codex_configuration(None);

    let geometry = |fixture: &crate::fixtures::Fixture| {
        let chat = fixture.view.chat.as_ref().expect("fixture opens a chat");
        let ctx = FrameContext {
            viewport: (120, 40),
            theme: Theme::default(),
            now: chrono::Utc::now(),
        };
        let mut cache = PaintCache::default();
        super::frame_parts(&fixture.model, chat, &mut cache, &ctx).geometry((120, 40), false)
    };

    assert_eq!(geometry(&claude), geometry(&codex));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorMode;

    fn block(key: u64, rows: &[&str]) -> PaintedBlock {
        PaintedBlock {
            key: BlockKey(key),
            kind: BlockKind::Activity,
            lines: rows
                .iter()
                .map(|row| Line::from((*row).to_string()))
                .collect(),
            copy_text: rows.join("\n"),
            run: None,
        }
    }

    fn parts(blocks: Vec<PaintedBlock>) -> ChatFrameParts {
        ChatFrameParts {
            header: Line::from("header"),
            banner: None,
            feed: FeedBlocks {
                blocks,
                history_truncated: false,
                loading: false,
            },
            activity: None,
            bottom: vec![Line::from("bottom")],
            overlay: None,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(line_text).collect()
    }

    /// The chat's left margin: column 0 is the mark column and column 1
    /// separates it from the text.
    const CONTENT_INDENT: usize = 2;

    /// The frame arithmetic these tests measure against: the shipped shape,
    /// varied only in what each test is about.
    const SHIPPED_SHAPE: FrameShape = FrameShape {
        spacing: FrameSpacing::DEFAULT,
        content_indent: CONTENT_INDENT,
    };

    fn paint(key: BlockKey, label: &str) -> PaintedBlock {
        block(key.0, &[label])
    }

    /// Every optional row is accounted for: the header, a banner, the gap
    /// under them, the paused rule, the activity row with the gap above it,
    /// and the bottom block.
    #[test]
    fn geometry_at_120_by_40_accounts_for_every_optional_row() {
        let mut parts = parts(Vec::new());
        parts.banner = Some(Line::from("banner"));
        parts.activity = Some(Line::from("activity"));
        parts.bottom = vec![Line::default(); 4];
        let geometry = parts.geometry((120, 40), true);
        assert_eq!(
            geometry,
            ChatGeometry {
                content_indent: CONTENT_INDENT,
                width: 120,
                height: 40,
                feed_top: 3,
                feed_rows: 29,
                bottom_top: 36,
            }
        );
        assert_eq!(geometry.feed_width(), 118);
    }

    #[test]
    fn minimum_geometry_saturates_under_two_spacing_values() {
        let roomy = chat_geometry((24, 10), SHIPPED_SHAPE, true, true, true, 4);
        assert_eq!(roomy.feed_top, 3);
        assert_eq!(roomy.feed_rows, 0);
        assert_eq!(roomy.bottom_top, 6);

        let tight = chat_geometry(
            (24, 10),
            FrameShape {
                spacing: FrameSpacing {
                    header_gap: 0,
                    block_gap: 0,
                    action_gap: 0,
                    bottom_gap: 0,
                },
                ..SHIPPED_SHAPE
            },
            true,
            true,
            true,
            4,
        );
        assert_eq!(tight.feed_top, 2);
        assert_eq!(tight.feed_rows, 2);
        assert_eq!(tight.bottom_top, 6);
    }

    #[test]
    fn feed_metrics_include_only_inter_block_gaps() {
        let blocks = vec![block(1, &["a", "b"]), block(2, &["c"]), block(3, &[])];
        let geometry = chat_geometry(
            (20, 8),
            FrameShape {
                spacing: FrameSpacing {
                    header_gap: 0,
                    block_gap: 2,
                    action_gap: 2,
                    bottom_gap: 0,
                },
                ..SHIPPED_SHAPE
            },
            false,
            false,
            false,
            1,
        );
        let metrics = feed_metrics(
            &FeedBlocks {
                blocks,
                history_truncated: false,
                loading: false,
            },
            FrameSpacing {
                header_gap: 0,
                block_gap: 2,
                action_gap: 2,
                bottom_gap: 0,
            },
            &geometry,
        );
        assert_eq!(metrics.total_rows, 7);
        assert_eq!(
            metrics.ranges,
            vec![
                (BlockKey(1), 0..2),
                (BlockKey(2), 4..5),
                (BlockKey(3), 7..7)
            ]
        );
    }

    #[test]
    fn feed_metrics_include_boundary_rows_in_bounds_and_ranges() {
        let feed = FeedBlocks {
            blocks: vec![block(7, &["one", "two", "three"])],
            history_truncated: true,
            loading: true,
        };
        let geometry = chat_geometry(
            (20, 5),
            FrameShape {
                spacing: FrameSpacing {
                    header_gap: 0,
                    block_gap: 1,
                    action_gap: 1,
                    bottom_gap: 0,
                },
                ..SHIPPED_SHAPE
            },
            false,
            false,
            false,
            1,
        );
        let metrics = feed_metrics(&feed, FrameSpacing::DEFAULT, &geometry);
        assert_eq!(metrics.total_rows, 5);
        assert_eq!(metrics.max_top, 2);
        assert_eq!(metrics.ranges, vec![(BlockKey(7), 2..5)]);
    }

    #[test]
    fn windowing_selects_top_middle_and_bottom() {
        let feed = vec![block(1, &["row 0", "row 1", "row 2", "row 3", "row 4"])];
        let theme = Theme::default();
        let mut viewport = FeedViewport::following();

        viewport.scroll = FeedScroll::Paused {
            top_line: 0,
            entry_watermark: 0,
        };
        let top = texts(&compose_chat_frame(
            parts(feed.clone()),
            &viewport,
            theme,
            (12, 7),
        ));
        assert!(top[2].starts_with("row 0"));
        assert!(top[3].starts_with("row 1"));

        viewport.scroll = FeedScroll::Paused {
            top_line: 2,
            entry_watermark: 0,
        };
        let middle = texts(&compose_chat_frame(
            parts(feed.clone()),
            &viewport,
            theme,
            (12, 7),
        ));
        assert!(middle[2].starts_with("row 2"));
        assert!(middle[3].starts_with("row 3"));

        viewport.scroll = FeedScroll::Following;
        let bottom = texts(&compose_chat_frame(parts(feed), &viewport, theme, (12, 7)));
        assert!(bottom[2].starts_with("row 2"));
        assert!(bottom[4].starts_with("row 4"));
        assert_eq!(bottom.len(), 7);
    }

    #[test]
    fn boundary_rows_only_appear_at_the_first_window() {
        let mut top_parts = parts(vec![block(1, &["one", "two", "three", "four"])]);
        top_parts.feed.history_truncated = true;
        top_parts.feed.loading = true;
        let mut viewport = FeedViewport::following();
        viewport.scroll = FeedScroll::Paused {
            top_line: 0,
            entry_watermark: 0,
        };
        let top = texts(&compose_chat_frame(
            top_parts,
            &viewport,
            Theme::default(),
            (24, 8),
        ));
        assert!(top.iter().any(|line| line.contains("earlier history")));
        assert!(top.iter().any(|line| line.contains("loading session")));

        let mut middle_parts = parts(vec![block(1, &["one", "two", "three", "four"])]);
        middle_parts.feed.history_truncated = true;
        middle_parts.feed.loading = true;
        viewport.scroll = FeedScroll::Paused {
            top_line: 1,
            entry_watermark: 0,
        };
        let middle = texts(&compose_chat_frame(
            middle_parts,
            &viewport,
            Theme::default(),
            (24, 8),
        ));
        assert!(!middle.iter().any(|line| line.contains("earlier history")));
        assert!(!middle.iter().any(|line| line.contains("loading session")));
    }

    /// A borderless frame spends nothing on chrome: the header is the
    /// first row, every row fills the width, and no cell anywhere is a box
    /// glyph. The mark column is the screen's own first column.
    #[test]
    fn a_borderless_frame_starts_with_the_header_and_draws_no_box() {
        let mut parts = parts(vec![
            block(1, &["  first block"]),
            block(2, &["  second block"]),
        ]);
        parts.banner = Some(Line::from("  a child is waiting"));
        parts.activity = Some(Line::from("  working · 4s"));
        parts.feed.history_truncated = true;
        parts.feed.loading = true;
        let mut viewport = FeedViewport::following();
        viewport.scroll = FeedScroll::Paused {
            top_line: 0,
            entry_watermark: 0,
        };
        viewport.focus = Some(BlockKey(1));

        let theme = Theme::default();
        let frame = compose_chat_frame(parts, &viewport, theme, (120, 40));
        let rendered = texts(&frame);

        assert_eq!(frame.len(), 40);
        assert!(rendered[0].starts_with("header"), "{:?}", rendered[0]);
        for (row, rendered) in rendered.iter().enumerate() {
            assert_eq!(
                str_width(rendered),
                120,
                "row {row} must fill the width: {rendered:?}"
            );
            let first = rendered.chars().next().expect("a filled row");
            // The chain is the one glyph allowed in the mark column: it ties
            // a run of actions together and stops where the run does, which
            // is the opposite of a border that encloses everything.
            let chain = first.to_string() == CHAIN;
            assert!(
                chain || !"┌┐└┘├┤┬┴┼│─═║╭╮╰╯".contains(first),
                "row {row} starts with chrome: {rendered:?}"
            );
            // A vertical rule is not a border. The chat draws two of them
            // on purpose — the chain tying a run of actions together and the
            // diff's spine — and both live in a column of their own rather
            // than around anything. What this test forbids is a box, which
            // is the first-cell check above plus the horizontal runs below.
            assert!(
                !rendered.contains("──"),
                "row {row} draws a border rule: {rendered:?}"
            );
        }
        assert!(
            rendered
                .iter()
                .any(|row| row.starts_with("\u{258e} first block")),
            "the focused block wears the mark in column 0: {rendered:?}"
        );
        // The unfocused block is the second of two consecutive actions, so
        // the chain has its column — and the focus mark, not the chain, is
        // what says which block is selected.
        assert!(
            rendered
                .iter()
                .any(|row| row.starts_with(&format!("{CHAIN} second block"))),
            "an unfocused block in a run wears the chain, not the mark: {rendered:?}"
        );
        assert_eq!(
            rendered
                .iter()
                .filter(|row| row.starts_with(MARK_GLYPH))
                .count(),
            1,
            "only the focused block wears the mark: {rendered:?}"
        );
    }

    #[test]
    fn an_unfocused_frame_marks_nothing() {
        let mut viewport = FeedViewport::following();
        viewport.scroll = FeedScroll::Paused {
            top_line: 0,
            entry_watermark: 0,
        };
        let mut parts = parts(vec![block(1, &["  first"]), block(2, &["  second"])]);
        parts.feed.history_truncated = true;
        let rendered = texts(&compose_chat_frame(
            parts,
            &viewport,
            Theme::default(),
            (40, 12),
        ));
        assert!(
            !rendered.iter().any(|row| row.starts_with(MARK_GLYPH)),
            "a viewport focusing nothing must not mark the gaps: {rendered:?}"
        );
    }

    #[test]
    fn the_focus_mark_does_not_shift_the_row_it_marks() {
        let theme = Theme::default();
        let marked = mark_focused(Line::from("  wide 世 row".to_string()), theme);
        let plain = Line::from("  wide 世 row".to_string());
        assert_eq!(
            str_width(&line_text(&marked)),
            str_width(&line_text(&plain))
        );
        assert_eq!(line_text(&marked), "\u{258e} wide 世 row");
    }

    #[test]
    fn cache_hits_and_content_changes_miss() {
        let mut cache = PaintCache::default();
        let key = BlockKey(1);
        cache.get_or_paint(
            key,
            &"first".to_string(),
            PaintInputs {
                width: 80,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(key, "first"),
        );
        cache.get_or_paint(
            key,
            &"first".to_string(),
            PaintInputs {
                width: 80,
                theme: Theme::default(),
                expanded: false,
            },
            || panic!("cache hit must not repaint"),
        );
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 1
            }
        );

        cache.reset_stats();
        cache.get_or_paint(
            key,
            &"changed".to_string(),
            PaintInputs {
                width: 80,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(key, "changed"),
        );
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 0
            }
        );
    }

    #[test]
    fn cache_width_change_misses() {
        let mut cache = PaintCache::default();
        let key = BlockKey(1);
        let content = "same".to_string();
        cache.get_or_paint(
            key,
            &content,
            PaintInputs {
                width: 80,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(key, "80"),
        );

        cache.reset_stats();
        cache.get_or_paint(
            key,
            &content,
            PaintInputs {
                width: 81,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(key, "81"),
        );
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 0
            }
        );
    }

    #[test]
    fn cache_theme_change_misses() {
        let mut cache = PaintCache::default();
        let key = BlockKey(1);
        let content = "same".to_string();
        cache.get_or_paint(
            key,
            &content,
            PaintInputs {
                width: 81,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(key, "dark"),
        );
        cache.reset_stats();
        cache.get_or_paint(
            key,
            &content,
            PaintInputs {
                width: 81,
                theme: Theme::light(ColorMode::TrueColor),
                expanded: false,
            },
            || paint(key, "light"),
        );
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 0
            }
        );
    }

    #[test]
    fn cache_expansion_change_misses() {
        let mut cache = PaintCache::default();
        let key = BlockKey(1);
        let content = "same".to_string();
        cache.get_or_paint(
            key,
            &content,
            PaintInputs {
                width: 81,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(key, "closed"),
        );
        cache.reset_stats();
        cache.get_or_paint(
            key,
            &content,
            PaintInputs {
                width: 81,
                theme: Theme::default(),
                expanded: true,
            },
            || paint(key, "open"),
        );
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 0
            }
        );
    }

    #[test]
    fn cache_repaints_only_the_toggled_exploration_block() {
        let mut cache = PaintCache::default();
        for value in 1..=3 {
            let key = BlockKey(value);
            cache.get_or_paint(
                key,
                &value,
                PaintInputs {
                    width: 80,
                    theme: Theme::default(),
                    expanded: false,
                },
                || paint(key, "collapsed"),
            );
        }

        cache.reset_stats();
        for value in 1..=3 {
            let key = BlockKey(value);
            let expanded = value == 2;
            cache.get_or_paint(
                key,
                &value,
                PaintInputs {
                    width: 80,
                    theme: Theme::default(),
                    expanded,
                },
                || paint(key, "expanded"),
            );
        }
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 2
            }
        );
    }

    #[test]
    fn cache_retain_drops_only_stale_keys() {
        let mut cache = PaintCache::default();
        for value in 1..=3 {
            let key = BlockKey(value);
            cache.get_or_paint(
                key,
                &value,
                PaintInputs {
                    width: 80,
                    theme: Theme::default(),
                    expanded: false,
                },
                || paint(key, "painted"),
            );
        }
        cache.retain(&[BlockKey(1), BlockKey(3)]);
        cache.reset_stats();
        cache.get_or_paint(
            BlockKey(1),
            &1_u64,
            PaintInputs {
                width: 80,
                theme: Theme::default(),
                expanded: false,
            },
            || panic!("retained key must be reused"),
        );
        cache.get_or_paint(
            BlockKey(2),
            &2_u64,
            PaintInputs {
                width: 80,
                theme: Theme::default(),
                expanded: false,
            },
            || paint(BlockKey(2), "repainted"),
        );
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 1
            }
        );
    }
}
