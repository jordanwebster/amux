//! Shared painted-block vocabulary: one pure painter per block kind,
//! from already-formatted facts to finished rows.
//!
//! Nothing here interprets content. Each agent's own adapter decides what
//! a block is, formats its words, and hands them over; these functions
//! only decide how the chat looks. That is why the two chats cannot drift
//! apart visually — there is exactly one place a tool line, a thought
//! marker or a user prompt is drawn.
//!
//! The column grid, measured from the frame's left edge:
//!
//! ```text
//! 0  the mark column — a user surface's accent bar, or the focus bar
//! 2  glyphs: `›`, `✔`, `✗`, `~`, `─`
//! 4  entry text, and the composer's draft
//! 6  `└` continuation text under a tool line
//! ```
//!
//! Only the user prompt and the composer fill a surface; every other
//! painter leaves the background alone so the screen stays calm and the
//! style map reads a plain row or a single foreground token.

use amux_ui::attachments::{AttachmentKind, AttachmentLine};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

use super::attach::human_size;
use super::frame::{BlockKey, BlockKind, PaintedBlock};
use crate::markdown;
use crate::render::{
    INVARIANT_WARNING, Theme, clip_to_width, line_len, pad_to, push_span, str_width,
};

/// Renderer-local identity for an expandable run of related feed entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct RunKey(pub(crate) u64);

/// Claude's run counts plus the renderer-selected path preview. The layer
/// supplies every path; the cap and hidden-path arithmetic stay local because
/// they define this terminal row's density.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RunSummary {
    pub(crate) reads: usize,
    pub(crate) searches: usize,
    pub(crate) first_paths: Vec<String>,
    pub(crate) hidden: usize,
}

/// Glyphs sit here; the mark column and its separator stay clear.
pub(crate) const GLYPH_COL: usize = 2;
/// Entry text and the composer's draft share this column.
pub(crate) const TEXT_COL: usize = 4;
/// `└` continuation text under a tool line.
pub(crate) const CONT_COL: usize = 6;

/// The bar a filled surface wears in the mark column: a quarter of a cell
/// wide, drawn full height, so a stack of them is one unbroken rule.
const BAR: &str = "\u{258e}";

/// A message someone sent gets no padding at all.
///
/// Both alternatives were tried and rejected on the screen: a row on each
/// side turns a one-line message into a three-row slab that dominates a feed
/// full of them, and a row on one side reads as a block that lost its
/// bottom. A tint that hugs its words is the tightest of the three and the
/// only one that looks deliberate.
const MESSAGE_PAD: (usize, usize) = (0, 0);

/// The same, for the composer — which is a place to work rather than a thing
/// to skim past, and earns the room on both sides.
const COMPOSER_PAD: (usize, usize) = (1, 1);

/// Cells available to text at `TEXT_COL`, inside a one-cell right margin.
fn text_width(width: usize) -> usize {
    width.saturating_sub(TEXT_COL + 1).max(1)
}

/// Cells available to speech starting at `col`, inside the same one-cell
/// right margin every row keeps.
fn speech_width(width: usize, col: usize) -> usize {
    width.saturating_sub(col + 1).max(1)
}

/// Cells available to continuation text at `CONT_COL`.
fn cont_width(width: usize) -> usize {
    width.saturating_sub(CONT_COL + 1).max(1)
}

/// The text of one finished line, which is what a copy action emits.
fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn copy_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

fn block(key: BlockKey, kind: BlockKind, lines: Vec<Line<'static>>) -> PaintedBlock {
    PaintedBlock {
        key,
        kind,
        copy_text: copy_text(&lines),
        lines,
        run: None,
    }
}

/// Rows under one leading glyph: the glyph on the first row only, every
/// row's text at `TEXT_COL`.
fn glyph_rows(
    glyph: (&str, Style),
    rows: Vec<Vec<Span<'static>>>,
    theme: Theme,
) -> Vec<Line<'static>> {
    let rows = if rows.is_empty() {
        vec![Vec::new()]
    } else {
        rows
    };
    rows.into_iter()
        .enumerate()
        .map(|(index, spans)| {
            let mut line = Line::default();
            if index == 0 {
                push_span(&mut line, GLYPH_COL, glyph.0.to_string(), glyph.1);
            }
            pad_to(&mut line, TEXT_COL);
            line.spans.extend(spans);
            let _ = theme;
            line
        })
        .collect()
}

/// Dim `└ …` continuations under a tool line. Each line of the source is
/// its own fact and gets its own marker; only a fact too wide for the
/// row wraps without one, so a stack of facts never reads as one
/// sentence that happened to break.
fn continuation_rows(text: &str, theme: Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for fact in text.lines() {
        for (index, spans) in markdown::plain_rows(fact, cont_width(width), theme.muted())
            .into_iter()
            .enumerate()
        {
            let mut line = Line::default();
            if index == 0 {
                push_span(&mut line, TEXT_COL, "└", theme.muted());
            }
            pad_to(&mut line, CONT_COL);
            line.spans.extend(spans);
            lines.push(line);
        }
    }
    lines
}

/// A muted rule that starts at the glyph column and runs to the right
/// margin: `─ turn · 1m 42s ─────…`.
fn rule_row(label: &str, theme: Theme, width: usize) -> Line<'static> {
    let mut text = format!("─ {label} ");
    while GLYPH_COL + str_width(&text) < width {
        text.push('─');
    }
    let mut line = Line::default();
    push_span(&mut line, GLYPH_COL, text, theme.muted());
    line
}

// --- the header -------------------------------------------------------------

/// The one header row both chats wear: who this is on the left, and the
/// screen and its phase on the right.
///
/// `name` is the whole left segment the adapter composed. Everything
/// before its first separator is the agent's own name and reads at full
/// strength; the rest — its type, its host, how many children it started
/// — is context and is muted, so a row of six facts still has one
/// subject.
pub(crate) fn paint_header(
    name: &str,
    phase: (&str, Style),
    right: &str,
    theme: Theme,
    width: usize,
) -> Line<'static> {
    let mut line = Line::default();
    match name.split_once(" · ") {
        Some((subject, context)) => {
            push_span(&mut line, GLYPH_COL, subject.to_string(), theme.emphasis());
            line.spans
                .push(Span::styled(format!(" · {context}"), theme.muted()));
        }
        None => push_span(&mut line, GLYPH_COL, name.to_string(), theme.emphasis()),
    }
    let tail = str_width(right) + str_width(phase.0);
    let col = width
        .saturating_sub(1 + tail)
        .max(line_len(&line).saturating_add(1));
    push_span(&mut line, col, right.to_string(), theme.muted());
    line.spans.push(Span::styled(phase.0.to_string(), phase.1));
    line
}

// --- the user's own words ---------------------------------------------------

/// What the person said, on the one tinted surface in the feed, with the
/// accent bar in the mark column. It is the only thing in a transcript
/// nobody else wrote, so it is the only thing that gets a surface.
pub(crate) fn paint_user_prompt(
    key: BlockKey,
    markdown_source: &str,
    sending: bool,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let col = TEXT_COL;
    let surface = theme.user_surface();
    let mut rows = markdown::plain_rows(markdown_source, speech_width(width, col), surface);
    if sending {
        match rows.last_mut() {
            Some(last) => last.push(Span::styled(" · sending…", surface.patch(theme.muted()))),
            None => rows.push(vec![Span::styled("sending…", surface.patch(theme.muted()))]),
        }
    }
    let lines = padded_surface(rows, Some(col), MESSAGE_PAD, surface, theme, width);
    block(key, BlockKind::Speech, lines)
}

/// The sticky diagnostic the chat writes over its header gap when the
/// kernel reports its own state stopped adding up.
///
/// The fleet has a row of the same words, but the fleet's rows are drawn
/// inside a border; a chat's are not, and borrowing that one would put a
/// border glyph in column 0 and again in the last cell of a full-screen
/// frame. This row keeps the chat's grid — a glyph at `GLYPH_COL`, then
/// the background out to the edge — and fills itself, because it is
/// written over an already-fitted frame.
pub(crate) fn invariant_warning_row(width: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(
        &mut line,
        GLYPH_COL,
        clip_to_width(INVARIANT_WARNING, width.saturating_sub(GLYPH_COL)).to_string(),
        theme.warn().patch(theme.background()),
    );
    fill(&mut line, width, theme.background());
    line
}

/// A filled block: the caller's rows, wearing the accent bar, with
/// [`SURFACE_PAD`] blank tinted rows above and below.
fn padded_surface(
    rows: Vec<Vec<Span<'static>>>,
    bar_col: Option<usize>,
    pad: (usize, usize),
    surface: Style,
    theme: Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let row = |spans: Vec<Span<'static>>| {
        let mut line = Line::default();
        // The bar runs the whole height, padding included, or the block
        // reads as two surfaces stacked rather than one.
        if let Some(col) = bar_col.filter(|col| *col > 0) {
            line.spans.push(Span::styled(BAR, theme.accent_bar()));
            line.spans.push(Span::styled(" ".repeat(col - 1), surface));
        }
        line.spans.extend(spans);
        fill(&mut line, width, surface);
        line
    };
    let (above, below) = pad;
    let mut lines = Vec::with_capacity(rows.len() + above + below);
    lines.extend((0..above).map(|_| row(Vec::new())));
    lines.extend(rows.into_iter().map(row));
    lines.extend((0..below).map(|_| row(Vec::new())));
    lines
}

/// Pad a surface row to the frame width so its tint reaches the edge.
fn fill(line: &mut Line<'static>, width: usize, surface: Style) {
    let used = line_len(line);
    if used < width {
        line.spans
            .push(Span::styled(" ".repeat(width - used), surface));
    }
}

// --- what the agent said ----------------------------------------------------

/// Assistant markdown, plain on the background.
pub(crate) fn paint_assistant(
    key: BlockKey,
    markdown_source: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let col = TEXT_COL;
    let lines = markdown::markdown_rows(markdown_source, speech_width(width, col), theme)
        .into_iter()
        .map(|spans| {
            let mut line = Line::default();
            pad_to(&mut line, col);
            line.spans.extend(spans);
            line
        })
        .collect();
    block(key, BlockKind::Speech, lines)
}

/// The thinking marker: one muted row, no surface. A thought is a fact
/// about how long the agent spent, not something to read — but an agent
/// that publishes a summary of what it thought has said something, and
/// that goes on the same dim continuation a tool's outcome uses.
pub(crate) fn paint_thinking(
    key: BlockKey,
    label: &str,
    detail: Option<&str>,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut line = Line::default();
    push_span(
        &mut line,
        GLYPH_COL,
        clip_to_width(label, width.saturating_sub(GLYPH_COL + 1)).to_string(),
        theme.muted(),
    );
    let mut lines = vec![line];
    if let Some(detail) = detail {
        lines.extend(continuation_rows(detail, theme, width));
    }
    block(key, BlockKind::Activity, lines)
}

/// A tool one-liner: outcome glyph, what it did, and the dim `└` line
/// that says how it went.
pub(crate) fn paint_tool_line(
    key: BlockKey,
    glyph: (&str, Style),
    label: &str,
    detail: Option<&str>,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut lines = glyph_rows(
        glyph,
        markdown::plain_rows(label, text_width(width), theme.text()),
        theme,
    );
    if let Some(detail) = detail {
        lines.extend(continuation_rows(detail, theme, width));
    }
    block(key, BlockKind::Activity, lines)
}

/// A run of reads and searches, folded to one row. Expanded, the row
/// keeps its place and the members follow it in the order they happened.
// The run painter takes the whole fold — its identity, its counts, its
// members and whether it is open — because grouping them into a struct
// would only move the same eight facts one level down.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_exploration_run(
    key: BlockKey,
    run: RunKey,
    summary: &RunSummary,
    members: &[PaintedBlock],
    expanded: bool,
    hint: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut label = match (summary.reads, summary.searches) {
        (reads, 0) => plural(reads, "read", "reads"),
        (0, searches) => plural(searches, "search", "searches"),
        (reads, searches) => format!(
            "{} · {}",
            plural(reads, "read", "reads"),
            plural(searches, "search", "searches")
        ),
    };
    if !summary.first_paths.is_empty() {
        label.push_str(&format!(" · {}", summary.first_paths.join(", ")));
    }
    if summary.hidden > 0 {
        label.push_str(&format!(" · +{} more", summary.hidden));
    }
    let mut line = Line::default();
    push_span(
        &mut line,
        GLYPH_COL,
        if expanded { "⌃" } else { "⌄" },
        theme.muted(),
    );
    push_span(&mut line, TEXT_COL, label, theme.text());
    line.spans
        .push(Span::styled(format!(" · {hint}"), theme.muted()));
    let mut lines = vec![line];
    if expanded {
        for member in members {
            lines.extend(member.lines.iter().cloned());
        }
    }
    let _ = width;
    PaintedBlock {
        key,
        kind: BlockKind::Activity,
        copy_text: copy_text(&lines),
        lines,
        run: Some(run),
    }
}

/// A file the agent changed, always on its own row: nothing consequential
/// is ever folded away.
pub(crate) fn paint_file_change(
    key: BlockKey,
    verb: &str,
    path: &str,
    added: u64,
    removed: u64,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut line = Line::default();
    // The glyph marks the row without colouring it: a file change is
    // consequential, not an outcome, and green here would read as "this
    // succeeded" next to the tool lines that mean exactly that.
    push_span(&mut line, GLYPH_COL, "✎", theme.text());
    push_span(
        &mut line,
        TEXT_COL,
        clip_to_width(
            &format!("{verb} {path}"),
            text_width(width).saturating_sub(str_width(&magnitude(added, removed)) + 3),
        )
        .to_string(),
        theme.text(),
    );
    line.spans.push(Span::styled(
        format!(" · {}", magnitude(added, removed)),
        theme.muted(),
    ));
    block(key, BlockKind::Activity, vec![line])
}

/// `+12 −3`, dropping a half that is zero. The minus is a real minus
/// sign, not a hyphen, so it lines up with the plus.
fn magnitude(added: u64, removed: u64) -> String {
    match (added, removed) {
        (0, 0) => "±0".to_string(),
        (added, 0) => format!("+{added}"),
        (0, removed) => format!("−{removed}"),
        (added, removed) => format!("+{added} −{removed}"),
    }
}

// --- what came attached ------------------------------------------------------

/// One attached thing, on its own row under the message that carries it.
///
/// Everything on the row is already in the stream: the daemon states an
/// artifact's name and size on the refs row, and an inline attachment
/// states its own length, so a viewer paints the row without fetching a
/// byte — a feed that scrolls past a hundred attachments downloads none
/// of them.
pub(crate) fn paint_attachment(
    key: BlockKey,
    attachment: &AttachmentLine,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let detail = attachment_detail(attachment);
    let mut line = Line::default();
    push_span(
        &mut line,
        GLYPH_COL,
        attachment_glyph(attachment.kind),
        theme.text(),
    );
    let room = text_width(width).saturating_sub(str_width(&detail) + 3);
    push_span(
        &mut line,
        TEXT_COL,
        clip_to_width(&attachment.name, room).to_string(),
        theme.text(),
    );
    if !detail.is_empty() {
        line.spans
            .push(Span::styled(format!(" \u{b7} {detail}"), theme.muted()));
    }
    block(key, BlockKind::Activity, vec![line])
}

/// The kind, in one cell. An attachment row has no other space for its
/// kind: the name is the useful half of the row and the size is the
/// other, so the glyph carries what an image is and a file is not.
fn attachment_glyph(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "\u{25a3}",
        AttachmentKind::File => "\u{25a4}",
        AttachmentKind::Text => "\u{b6}",
        AttachmentKind::Review => "\u{2317}",
    }
}

/// The one measurement the kind makes available: bytes for stored
/// artifacts, lines for pasted text, and for a review both halves of what
/// was written — how many comments, over how many files — because either
/// alone leaves the reader guessing at the size of the thing.
fn attachment_detail(attachment: &AttachmentLine) -> String {
    if let Some(lines) = attachment.lines {
        return plural(lines as usize, "line", "lines");
    }
    if let Some(comments) = attachment.comments {
        let mut detail = plural(comments, "comment", "comments");
        if let Some(files) = attachment.files {
            detail.push_str(&format!(" \u{b7} {}", plural(files, "file", "files")));
        }
        return detail;
    }
    attachment.size.map(human_size).unwrap_or_default()
}

/// What an ask settled, once it is settled: one plain row, because a
/// decision already made is history, not a question.
pub(crate) fn paint_ask_fact(
    key: BlockKey,
    glyph: (&str, Style),
    fact: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let lines = glyph_rows(
        glyph,
        markdown::plain_rows(fact, text_width(width), theme.text()),
        theme,
    );
    block(key, BlockKind::Activity, lines)
}

/// A plan, shown down to its first rows with the chord that opens the
/// rest. A plan is long by nature; the feed shows enough to recognise it.
pub(crate) fn paint_plan(
    key: BlockKey,
    markdown_source: &str,
    preview_rows: usize,
    reader_hint: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let rows = markdown::markdown_rows(markdown_source, text_width(width), theme);
    let hidden = rows.len().saturating_sub(preview_rows);
    let mut lines: Vec<Line<'static>> = rows
        .into_iter()
        .take(preview_rows)
        .map(|spans| {
            let mut line = Line::default();
            pad_to(&mut line, TEXT_COL);
            line.spans.extend(spans);
            line
        })
        .collect();
    let tail = match hidden {
        0 => reader_hint.to_string(),
        1 => format!("1 more line · {reader_hint}"),
        n => format!("{n} more lines · {reader_hint}"),
    };
    let mut hint = Line::default();
    push_span(&mut hint, TEXT_COL, format!("⌄ {tail}"), theme.muted());
    lines.push(hint);
    block(key, BlockKind::Speech, lines)
}

/// A subagent starting, reporting or finishing: one plain row.
pub(crate) fn paint_subagent(
    key: BlockKey,
    glyph: (&str, Style),
    text: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let lines = glyph_rows(
        glyph,
        markdown::plain_rows(text, text_width(width), theme.text()),
        theme,
    );
    block(key, BlockKind::Activity, lines)
}

/// A message from another agent: who sent it, what it said, and the one
/// row naming what is not being shown.
pub(crate) fn paint_agent_message(
    key: BlockKey,
    glyph: (&str, Style),
    sender: &str,
    body: &str,
    affordance: Option<&str>,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut rows = vec![vec![Span::styled(sender.to_string(), theme.emphasis())]];
    rows.extend(markdown::plain_rows(body, text_width(width), theme.text()));
    let mut lines = glyph_rows(glyph, rows, theme);
    if let Some(affordance) = affordance {
        let mut line = Line::default();
        push_span(&mut line, TEXT_COL, affordance.to_string(), theme.muted());
        lines.push(line);
    }
    block(key, BlockKind::Speech, lines)
}

/// The rule that closes a turn.
pub(crate) fn paint_turn_rule(
    key: BlockKey,
    label: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    block(key, BlockKind::Divider, vec![rule_row(label, theme, width)])
}

/// The rule that marks where the transcript was compacted.
pub(crate) fn paint_compaction_rule(
    key: BlockKey,
    label: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    block(key, BlockKind::Divider, vec![rule_row(label, theme, width)])
}

/// Something went wrong. The accent is on the glyph alone: a red wall of
/// text is harder to read at exactly the moment reading matters.
pub(crate) fn paint_error(
    key: BlockKey,
    message: &str,
    retrying: bool,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut rows = markdown::plain_rows(message, text_width(width), theme.text());
    if retrying {
        match rows.last_mut() {
            Some(last) => last.push(Span::styled(" · retrying", theme.muted())),
            None => rows.push(vec![Span::styled("retrying", theme.muted())]),
        }
    }
    block(
        key,
        BlockKind::Activity,
        glyph_rows(("✗", theme.error()), rows, theme),
    )
}

/// Codex's MCP startup report, exactly as its adapter formatted it.
pub(crate) fn paint_mcp_startup(
    key: BlockKey,
    rows: Vec<Line<'static>>,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let _ = (theme, width);
    block(key, BlockKind::Activity, rows)
}

/// A row this build does not know how to draw. It says so rather than
/// dropping it: a transcript with a silent hole in it is worse than one
/// that admits the hole.
pub(crate) fn paint_unrecognized(
    key: BlockKey,
    label: &str,
    detail: Option<&str>,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let mut lines = glyph_rows(
        ("⚠", theme.warn()),
        markdown::plain_rows(label, text_width(width), theme.text()),
        theme,
    );
    if let Some(detail) = detail {
        lines.extend(continuation_rows(detail, theme, width));
    }
    block(key, BlockKind::Activity, lines)
}

// --- the bottom block -------------------------------------------------------

/// The composer as a filled block, not a bordered strip: the same surface
/// and the same accent bar the person's own messages wear, so what they
/// are about to say looks like what they already said.
///
/// `rows` are the draft's display rows at `TEXT_COL`; `cursor` is the row
/// and column the caret sits at, in display cells. An empty draft shows
/// the placeholder with the caret after it.
pub(crate) fn paint_composer_block(
    rows: Vec<String>,
    cursor: Option<(usize, usize)>,
    placeholder: Option<&str>,
    theme: Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let surface = theme.user_surface();
    let rows = if rows.is_empty() {
        vec![String::new()]
    } else {
        rows
    };
    let spans: Vec<Vec<Span<'static>>> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if index == 0
                && row.is_empty()
                && let Some(placeholder) = placeholder
            {
                spans.push(Span::styled(
                    placeholder.to_string(),
                    surface.patch(theme.muted()),
                ));
            }
            match cursor.filter(|(caret_row, _)| *caret_row == index) {
                Some((_, column)) => {
                    let head = clip_to_width(row, column);
                    let tail = &row[head.len()..];
                    if !head.is_empty() {
                        spans.push(Span::styled(head.to_string(), surface));
                    }
                    spans.push(Span::styled("▌", surface));
                    if !tail.is_empty() {
                        spans.push(Span::styled(tail.to_string(), surface));
                    }
                }
                None => {
                    if !row.is_empty() {
                        spans.push(Span::styled(row.clone(), surface));
                    }
                }
            }
            spans
        })
        .collect();
    padded_surface(spans, Some(TEXT_COL), COMPOSER_PAD, surface, theme, width)
}

// --- the two panelled surfaces ----------------------------------------------

/// The column a panel's tint starts at. A panel runs to the left edge like
/// every other filled surface; a focused panel's bar is drawn onto it, the
/// way the user surface carries its own bar.
const PANEL_COL: usize = 0;

/// Put a panel's tint under rows another layer formatted. The spans keep
/// their own colours; the surface only supplies what they left unsaid.
fn tinted(mut line: Line<'static>, surface: Style, width: usize) -> Line<'static> {
    for span in &mut line.spans {
        span.style = surface.patch(span.style);
    }
    let mut out = Line::default();
    out.spans.push(Span::raw(" ".repeat(PANEL_COL)));
    out.spans
        .push(Span::styled(" ".repeat(TEXT_COL - PANEL_COL), surface));
    out.spans.append(&mut line.spans);
    fill(&mut out, width, surface);
    out
}

/// A blank row of panel, used to keep a panel's parts apart.
fn panel_gap(surface: Style, width: usize) -> Line<'static> {
    let mut line = Line::default();
    line.spans.push(Span::raw(" ".repeat(PANEL_COL)));
    fill(&mut line, width, surface);
    line
}

/// An ask, on the one surface that means "this is waiting for you":
/// what is being asked, the agent layer's own body rows, the answers on
/// offer, and the keys that give them.
pub(crate) fn paint_ask_panel(
    key: BlockKey,
    title: &str,
    body: Vec<Line<'static>>,
    actions: Vec<Line<'static>>,
    hints: &str,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let surface = theme.panel();
    let mut lines = vec![tinted(
        Line::from(Span::styled(title.to_string(), theme.emphasis())),
        surface,
        width,
    )];
    if !body.is_empty() {
        lines.push(panel_gap(surface, width));
        lines.extend(body.into_iter().map(|line| tinted(line, surface, width)));
    }
    if !actions.is_empty() {
        lines.push(panel_gap(surface, width));
        lines.extend(actions.into_iter().map(|line| tinted(line, surface, width)));
    }
    if !hints.is_empty() {
        // The keys sit a row below the answers, as the page's own hint row
        // sits a row below the composer.
        lines.push(panel_gap(surface, width));
        // Hints wrap rather than clip: a hint cut in half at the right
        // edge names a key the reader cannot finish reading, which is
        // worse than the row it costs.
        lines.extend(
            markdown::plain_rows(hints, panel_body_width(width), theme.muted())
                .into_iter()
                .map(|spans| tinted(Line::from(spans), surface, width)),
        );
    }
    block(key, BlockKind::Panel, lines)
}

/// Put already-painted unified-diff rows under a titled panel surface.
pub(crate) fn paint_unified_diff(
    key: BlockKey,
    title: &str,
    body: Vec<Line<'static>>,
    theme: Theme,
    width: usize,
) -> PaintedBlock {
    let surface = theme.panel();
    let mut lines = vec![tinted(
        Line::from(Span::styled(title.to_string(), theme.muted())),
        surface,
        width,
    )];
    lines.extend(body.into_iter().map(|line| tinted(line, surface, width)));
    block(key, BlockKind::Panel, lines)
}

/// The cells a panel's own rows may fill: everything right of the column
/// its tint starts at. Adapters formatting panel bodies measure with it.
pub(crate) fn panel_body_width(width: usize) -> usize {
    width.saturating_sub(TEXT_COL)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    }
}

#[cfg(test)]
mod tests {
    use amux_ui::diff::{RowFact as DiffRow, RowKind as DiffRowKind};

    use super::*;
    use crate::chat::diff;
    use crate::render::str_width;

    fn key() -> BlockKey {
        BlockKey(7)
    }

    fn classes(lines: &[Line<'_>], theme: Theme) -> String {
        lines
            .iter()
            .flat_map(|line| {
                line.spans
                    .iter()
                    .flat_map(move |span| {
                        let class = theme.classify(line.style.patch(span.style));
                        std::iter::repeat_n(class, str_width(&span.content))
                    })
                    .chain(std::iter::once('\n'))
            })
            .collect()
    }

    /// Every painter in the kit, at one width, so a new painter cannot be
    /// added without deciding whether it is allowed a surface.
    fn every_plain_block(theme: Theme, width: usize) -> Vec<(&'static str, PaintedBlock)> {
        vec![
            (
                "assistant",
                paint_assistant(key(), "I added exponential backoff.", theme, width),
            ),
            (
                "thinking",
                paint_thinking(key(), "~ thought for 6s", None, theme, width),
            ),
            (
                "tool",
                paint_tool_line(
                    key(),
                    ("✔", theme.ok()),
                    "read src/sync/client.rs",
                    Some("128 lines"),
                    theme,
                    width,
                ),
            ),
            (
                "run",
                paint_exploration_run(
                    key(),
                    RunKey(1),
                    &RunSummary {
                        reads: 3,
                        searches: 1,
                        first_paths: vec!["src/sync".to_string()],
                        hidden: 2,
                    },
                    &[],
                    false,
                    "C-a o open",
                    theme,
                    width,
                ),
            ),
            (
                "file change",
                paint_file_change(key(), "edit", "src/sync/client.rs", 12, 3, theme, width),
            ),
            (
                "ask fact",
                paint_ask_fact(key(), ("✔", theme.ok()), "allowed once", theme, width),
            ),
            (
                "plan",
                paint_plan(
                    key(),
                    "# Plan\n\n- one\n- two",
                    2,
                    "C-a p read",
                    theme,
                    width,
                ),
            ),
            (
                "subagent",
                paint_subagent(
                    key(),
                    ("⋯", theme.muted()),
                    "started reviewer",
                    theme,
                    width,
                ),
            ),
            (
                "agent message",
                paint_agent_message(
                    key(),
                    ("←", theme.emphasis()),
                    "reviewer",
                    "the retry path looks right",
                    Some("⌄ 3 more lines · C-a m"),
                    theme,
                    width,
                ),
            ),
            (
                "turn rule",
                paint_turn_rule(key(), "turn · 6s", theme, width),
            ),
            (
                "compaction rule",
                paint_compaction_rule(key(), "compacted · 31.6k", theme, width),
            ),
            (
                "error",
                paint_error(key(), "the API returned 529", true, theme, width),
            ),
            (
                "mcp startup",
                paint_mcp_startup(
                    key(),
                    vec![{
                        let mut line = Line::default();
                        push_span(&mut line, GLYPH_COL, "✔", theme.ok());
                        push_span(&mut line, TEXT_COL, "linear · connected", theme.text());
                        line
                    }],
                    theme,
                    width,
                ),
            ),
            (
                "unrecognized",
                paint_unrecognized(
                    key(),
                    "unrecognized row",
                    Some("kind=weather"),
                    theme,
                    width,
                ),
            ),
        ]
    }

    #[test]
    fn only_the_prompt_and_the_composer_wear_a_surface() {
        let theme = Theme::default();
        for (name, painted) in every_plain_block(theme, 80) {
            let map = classes(&painted.lines, theme);
            assert!(
                !map.contains('U') && !map.contains('A') && !map.contains('P'),
                "{name} must stay plain, got:\n{map}"
            );
            assert!(
                !map.contains('?'),
                "{name} paints a style outside the token vocabulary:\n{map}"
            );
        }

        let prompt = paint_user_prompt(key(), "next task", true, theme, 80);
        let prompt_map = classes(&prompt.lines, theme);
        assert!(prompt_map.contains('A'), "the prompt wears an accent bar");
        assert!(prompt_map.contains('U'), "the prompt fills a surface");

        let composer =
            paint_composer_block(vec!["draft".to_string()], Some((0, 5)), None, theme, 80);
        let composer_map = classes(&composer, theme);
        assert!(
            composer_map.contains('A'),
            "the composer wears an accent bar"
        );
        assert!(composer_map.contains('U'), "the composer fills a surface");
    }

    #[test]
    fn plain_painters_keep_the_mark_column_clear() {
        let theme = Theme::default();
        for (name, painted) in every_plain_block(theme, 80) {
            for line in &painted.lines {
                let text: String = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect();
                assert!(
                    text.is_empty() || text.starts_with("  "),
                    "{name} draws into the mark column: {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_surface_reaches_both_edges_at_every_row() {
        let theme = Theme::default();
        let prompt = paint_user_prompt(
            key(),
            "a prompt long enough to wrap over more than one row of the feed at this width",
            false,
            theme,
            40,
        );
        assert!(prompt.lines.len() > 1);
        for line in &prompt.lines {
            assert_eq!(line_len(line), 40);
            assert_eq!(line.spans[0].content.as_ref(), BAR);
        }
    }

    #[test]
    fn wrapping_counts_display_cells_not_characters() {
        let theme = Theme::default();
        let painted = paint_assistant(
            key(),
            "指数バックオフを実装しました。テストは全部緑です。",
            theme,
            40,
        );
        assert!(painted.lines.len() > 1, "wide text wraps");
        for line in &painted.lines {
            assert!(
                line_len(line) <= 40,
                "a wide grapheme overran the width: {}",
                line_len(line)
            );
        }
    }

    #[test]
    fn the_composer_caret_sits_after_wide_graphemes() {
        let theme = Theme::default();
        let lines = paint_composer_block(vec!["繁体字".to_string()], Some((0, 6)), None, theme, 20);
        // The draft is inside the surface, not the first row of it.
        let text: String = lines[COMPOSER_PAD.0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.starts_with("\u{258e}   繁体字▌"), "{text:?}");
        for line in &lines {
            assert_eq!(line_len(line), 20, "every row of the surface is filled");
        }
        assert_eq!(lines.len(), 1 + COMPOSER_PAD.0 + COMPOSER_PAD.1);
    }

    #[test]
    fn an_empty_composer_shows_its_placeholder_and_caret() {
        let theme = Theme::default();
        let lines =
            paint_composer_block(Vec::new(), Some((0, 0)), Some("Type a message"), theme, 40);
        let text: String = lines[COMPOSER_PAD.0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.starts_with("\u{258e}   Type a message▌"), "{text:?}");
    }

    #[test]
    fn the_header_names_its_subject_then_its_context() {
        let theme = Theme::default();
        let line = paint_header(
            "fix-auth · claude @ mbp",
            ("idle", theme.muted()),
            "chat · ",
            theme,
            60,
        );
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.starts_with("  fix-auth · claude @ mbp"), "{text:?}");
        assert!(text.ends_with("chat · idle"), "{text:?}");
        assert_eq!(theme.classify(line.spans[1].style), 'e');
        assert_eq!(theme.classify(line.spans[2].style), 'm');
    }

    #[test]
    fn a_folded_run_says_what_is_behind_it_and_opens_in_place() {
        let theme = Theme::default();
        let summary = RunSummary {
            reads: 1,
            searches: 0,
            first_paths: vec!["src/lib.rs".to_string()],
            hidden: 0,
        };
        let member = paint_tool_line(
            BlockKey(2),
            ("✔", theme.ok()),
            "read src/lib.rs",
            None,
            theme,
            80,
        );
        let folded = paint_exploration_run(
            key(),
            RunKey(1),
            &summary,
            std::slice::from_ref(&member),
            false,
            "C-a o open",
            theme,
            80,
        );
        assert_eq!(folded.lines.len(), 1);
        assert_eq!(folded.run, Some(RunKey(1)));
        assert!(folded.copy_text.contains("1 read · src/lib.rs"));

        let opened = paint_exploration_run(
            key(),
            RunKey(1),
            &summary,
            std::slice::from_ref(&member),
            true,
            "C-a o close",
            theme,
            80,
        );
        assert_eq!(opened.lines.len(), 1 + member.lines.len());
    }

    fn a_hunk() -> Vec<DiffRow> {
        vec![
            DiffRow {
                old: None,
                new: None,
                kind: DiffRowKind::Note,
                text: "@@ -1,3 +1,4 @@".to_string(),
            },
            DiffRow {
                old: Some(1),
                new: Some(1),
                kind: DiffRowKind::Context,
                text: " fn reconnect() {".to_string(),
            },
            DiffRow {
                old: Some(2),
                new: None,
                kind: DiffRowKind::Removed,
                text: "-    sleep(1);".to_string(),
            },
            DiffRow {
                old: None,
                new: Some(2),
                kind: DiffRowKind::Added,
                text: "+    sleep(backoff());".to_string(),
            },
        ]
    }

    #[test]
    fn only_changed_rows_are_tinted_and_the_gutter_stays_on_the_panel() {
        let theme = Theme::default();
        let rows = a_hunk();
        let body = diff::paint_rows(&rows, theme, panel_body_width(60), 0, true).into_lines();
        let painted = paint_unified_diff(key(), "✎ src/sync/client.rs", body, theme, 60);
        let rows: Vec<String> = classes(&painted.lines, theme)
            .lines()
            .map(str::to_string)
            .collect();

        // The title, the hunk header and the context row are panel only.
        for (index, row) in rows.iter().take(3).enumerate() {
            assert!(
                !row.contains('+') && !row.contains('-'),
                "row {index} must not be tinted: {row}"
            );
            assert!(
                row.contains('P'),
                "row {index} must sit on the panel: {row}"
            );
        }
        assert!(
            rows[3].contains('-'),
            "the removed row is tinted: {}",
            rows[3]
        );
        assert!(!rows[3].contains('+'), "{}", rows[3]);
        assert!(
            rows[4].contains('+'),
            "the added row is tinted: {}",
            rows[4]
        );
        assert!(!rows[4].contains('-'), "{}", rows[4]);

        // Every row reaches both edges, and the gutter columns line up.
        for line in &painted.lines {
            assert_eq!(line_len(line), 60);
        }
        assert!(
            painted
                .copy_text
                .contains("2 \u{2502} +    sleep(backoff());")
        );
    }

    #[test]
    fn a_long_diff_line_wraps_with_blank_continuation_gutters() {
        let theme = Theme::default();
        let rows = vec![DiffRow {
            old: None,
            new: Some(7),
            kind: DiffRowKind::Added,
            text: format!("+{}", "x".repeat(200)),
        }];
        let body = diff::paint_rows(&rows, theme, panel_body_width(40), 0, true).into_lines();
        let painted = paint_unified_diff(key(), "✎ long.rs", body, theme, 40);
        assert!(painted.lines.len() > 2, "the long source row wraps");
        assert!(
            painted.lines[2]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .starts_with("    "),
            "the continuation has no repeated gutter"
        );
        for line in &painted.lines {
            assert_eq!(line_len(line), 40);
        }
    }

    #[test]
    fn an_ask_panel_puts_every_part_on_the_panel_token() {
        let theme = Theme::default();
        let painted = paint_ask_panel(
            key(),
            "cargo test --workspace",
            vec![Line::from(Span::styled(
                "run the whole suite once",
                theme.text(),
            ))],
            vec![Line::from(Span::styled("1 allow once", theme.ok()))],
            "enter allow · esc deny",
            theme,
            60,
        );
        let map = classes(&painted.lines, theme);
        assert!(!map.contains('?'), "{map}");
        assert!(!map.contains('+') && !map.contains('-'), "{map}");
        for row in map.lines() {
            assert!(row.contains('P'), "every row sits on the panel: {row}");
        }
        for line in &painted.lines {
            assert_eq!(line_len(line), 60);
        }
    }

    #[test]
    fn the_panel_class_belongs_to_panels_alone() {
        let theme = Theme::default();
        for (name, painted) in every_plain_block(theme, 80) {
            assert!(
                !classes(&painted.lines, theme).contains('P'),
                "{name} is not a panel"
            );
        }
        let prompt = paint_user_prompt(key(), "next task", false, theme, 80);
        assert!(!classes(&prompt.lines, theme).contains('P'));
    }

    #[test]
    fn magnitudes_drop_a_half_that_is_zero() {
        assert_eq!(magnitude(12, 3), "+12 −3");
        assert_eq!(magnitude(12, 0), "+12");
        assert_eq!(magnitude(0, 3), "−3");
        assert_eq!(magnitude(0, 0), "±0");
    }

    #[test]
    fn a_plan_says_how_much_of_it_is_not_shown() {
        let theme = Theme::default();
        let painted = paint_plan(key(), "one\ntwo\nthree\nfour", 2, "C-a p read", theme, 60);
        assert!(painted.copy_text.contains("2 more lines · C-a p read"));
    }

    #[test]
    fn copy_text_is_what_the_rows_say() {
        let theme = Theme::default();
        let painted = paint_tool_line(
            key(),
            ("✔", theme.ok()),
            "read src/lib.rs",
            Some("128 lines"),
            theme,
            80,
        );
        assert_eq!(painted.copy_text, "  ✔ read src/lib.rs\n    └ 128 lines");
    }
}
