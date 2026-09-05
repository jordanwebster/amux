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

/// Finished rows and interaction metadata produced by one block painter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaintedBlock {
    pub(crate) key: BlockKey,
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) copy_text: String,
    pub(crate) run: Option<RunKey>,
}

/// Agent-owned content handed to the shared frame shell.
pub(crate) struct ChatFrameParts {
    pub(crate) header: Line<'static>,
    pub(crate) banner: Option<Line<'static>>,
    pub(crate) feed: FeedBlocks,
    pub(crate) activity: Vec<Line<'static>>,
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
            FrameSpacing::DEFAULT,
            self.banner.is_some(),
            self.activity.len(),
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
    pub(crate) block_gap: usize,
    pub(crate) bottom_gap: usize,
}

impl FrameSpacing {
    /// Chosen by eye at 120x40 against the idle Claude and Codex screens.
    /// One blank row under the header is enough to lift it off the feed
    /// once the old rule is gone; a second reads as a gap rather than a
    /// margin on a screen this short. One blank row between blocks keeps
    /// a long feed legible without halving how much of it fits, and one
    /// above the bottom block separates what the agent said from what
    /// the person is typing.
    pub(crate) const DEFAULT: Self = Self {
        header_gap: 1,
        block_gap: 1,
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
}

impl ChatGeometry {
    /// The width a block painter may fill: everything but the left mark
    /// column and its separator. Used by the agent adapters.
    #[allow(dead_code)]
    pub(crate) fn feed_width(&self) -> usize {
        self.width.saturating_sub(CONTENT_INDENT)
    }
}

/// Compute the full-screen frame budget without inspecting agent content.
fn chat_geometry(
    viewport: (u16, u16),
    spacing: FrameSpacing,
    banner: bool,
    activity: usize,
    paused: bool,
    bottom_rows: usize,
) -> ChatGeometry {
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    let feed_top = (1 + usize::from(banner) + spacing.header_gap).min(height);
    let bottom_top = height.saturating_sub(bottom_rows.min(height));
    let feed_bottom =
        bottom_top.saturating_sub(spacing.bottom_gap + activity + usize::from(paused));
    let feed_rows = feed_bottom.saturating_sub(feed_top);
    ChatGeometry {
        width,
        height,
        feed_top,
        feed_rows,
        bottom_top,
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
        if index > 0 {
            cursor += spacing.block_gap;
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
    let mut feed = Vec::with_capacity(metrics.total_rows);
    for (index, block) in parts.feed.blocks.iter().enumerate() {
        if index > 0 {
            feed.extend((0..spacing.block_gap).map(|_| (Line::default(), None)));
        }
        feed.extend(
            block
                .lines
                .iter()
                .cloned()
                .map(|line| (line, Some(block.key))),
        );
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
    lines.extend(parts.activity);
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
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::styled(MARK_GLYPH, theme.focus_bar()));
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

struct CachedBlock {
    /// `Send` so the whole view can be cloned into a diagnostic snapshot
    /// that crosses to the runtime's Msg tap. Cache content is plain data
    /// — the bound costs nothing and states where the view can travel.
    content: Box<dyn Any + Send>,
    width: usize,
    theme: (ThemeName, ColorMode),
    expanded: bool,
    block: PaintedBlock,
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
        width: usize,
        theme: Theme,
        expanded: bool,
        paint: impl FnOnce() -> PaintedBlock,
    ) -> &PaintedBlock {
        let theme_key = (theme.name, theme.mode);
        let hit = self.entries.get(&key).is_some_and(|cached| {
            cached.width == width
                && cached.theme == theme_key
                && cached.expanded == expanded
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
                width,
                theme: theme_key,
                expanded,
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
        .set_codex_configuration_label(None);

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
            activity: Vec::new(),
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

    fn paint(key: BlockKey, label: &str) -> PaintedBlock {
        block(key.0, &[label])
    }

    #[test]
    fn geometry_at_120_by_40_accounts_for_every_optional_row() {
        let mut parts = parts(Vec::new());
        parts.banner = Some(Line::from("banner"));
        parts.activity = vec![Line::from("activity")];
        parts.bottom = vec![Line::default(); 4];
        let geometry = parts.geometry((120, 40), true);
        assert_eq!(
            geometry,
            ChatGeometry {
                width: 120,
                height: 40,
                feed_top: 3,
                feed_rows: 30,
                bottom_top: 36,
            }
        );
    }

    #[test]
    fn minimum_geometry_saturates_under_two_spacing_values() {
        let roomy = chat_geometry((24, 10), FrameSpacing::DEFAULT, true, 1, true, 4);
        assert_eq!(roomy.feed_top, 3);
        assert_eq!(roomy.feed_rows, 0);
        assert_eq!(roomy.bottom_top, 6);

        let tight = chat_geometry(
            (24, 10),
            FrameSpacing {
                header_gap: 0,
                block_gap: 0,
                bottom_gap: 0,
            },
            true,
            1,
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
            FrameSpacing {
                header_gap: 0,
                block_gap: 2,
                bottom_gap: 0,
            },
            false,
            0,
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
            FrameSpacing {
                header_gap: 0,
                block_gap: 1,
                bottom_gap: 0,
            },
            false,
            0,
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

    #[test]
    fn a_full_screen_frame_is_borderless_and_starts_with_the_header() {
        let mut parts = parts(vec![
            block(1, &["  first block"]),
            block(2, &["  second block"]),
        ]);
        parts.banner = Some(Line::from("  a child is waiting"));
        parts.activity = vec![Line::from("  working · 4s")];
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
        for (row, line) in frame.iter().enumerate() {
            assert_eq!(
                str_width(&rendered[row]),
                120,
                "row {row} must fill the width: {:?}",
                rendered[row]
            );
            let first = rendered[row].chars().next().expect("a filled row");
            assert!(
                !"┌┐└┘├┤┬┴┼│─═║╭╮╰╯".contains(first),
                "row {row} starts with chrome: {:?}",
                rendered[row]
            );
            for span in &line.spans {
                assert!(
                    !span.content.contains('│'),
                    "row {row} keeps a border glyph: {:?}",
                    rendered[row]
                );
            }
        }
        assert!(
            rendered
                .iter()
                .any(|row| row.starts_with("\u{258e} first block")),
            "the focused block wears the mark in column 0: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|row| row.starts_with("  second block")),
            "an unfocused block keeps the mark column clear: {rendered:?}"
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
            80,
            Theme::default(),
            false,
            || paint(key, "first"),
        );
        cache.get_or_paint(
            key,
            &"first".to_string(),
            80,
            Theme::default(),
            false,
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
            80,
            Theme::default(),
            false,
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
        cache.get_or_paint(key, &content, 80, Theme::default(), false, || {
            paint(key, "80")
        });

        cache.reset_stats();
        cache.get_or_paint(key, &content, 81, Theme::default(), false, || {
            paint(key, "81")
        });
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
        cache.get_or_paint(key, &content, 81, Theme::default(), false, || {
            paint(key, "dark")
        });
        cache.reset_stats();
        cache.get_or_paint(
            key,
            &content,
            81,
            Theme::light(ColorMode::TrueColor),
            false,
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
        cache.get_or_paint(key, &content, 81, Theme::default(), false, || {
            paint(key, "closed")
        });
        cache.reset_stats();
        cache.get_or_paint(key, &content, 81, Theme::default(), true, || {
            paint(key, "open")
        });
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
            cache.get_or_paint(key, &value, 80, Theme::default(), false, || {
                paint(key, "collapsed")
            });
        }

        cache.reset_stats();
        for value in 1..=3 {
            let key = BlockKey(value);
            let expanded = value == 2;
            cache.get_or_paint(key, &value, 80, Theme::default(), expanded, || {
                paint(key, "expanded")
            });
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
            cache.get_or_paint(key, &value, 80, Theme::default(), false, || {
                paint(key, "painted")
            });
        }
        cache.retain(&[BlockKey(1), BlockKey(3)]);
        cache.reset_stats();
        cache.get_or_paint(BlockKey(1), &1_u64, 80, Theme::default(), false, || {
            panic!("retained key must be reused")
        });
        cache.get_or_paint(BlockKey(2), &2_u64, 80, Theme::default(), false, || {
            paint(BlockKey(2), "repainted")
        });
        assert_eq!(
            cache.stats(),
            PaintStats {
                painted: 1,
                reused: 1
            }
        );
    }
}
