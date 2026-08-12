//! The chat frame renderer: a pure function of (Model, ChatView,
//! FrameContext), composing with the chrome's line helpers.
//!
//! Visual language fixed by `docs/CHAT.md` §Wireframes: user lines
//! prefixed `› `, assistant text at a two-column indent, tool lines
//! glyphed by outcome (`✔`/`▸`/`✗`) with dim `└` continuations, markers
//! and boundaries as dim `─` rules, header `agent · type @ host … chat ·
//! <phase>`, one footer hint line with the permission mode on the right.
//! Every fact rendered here comes from the Model — magnitudes, phases,
//! gates, counts — the code below formats and never decides.

use amux_ui::claude::{
    ChatPhase, FeedEntry, FeedEntryKind, InterruptionKind, PromptEcho, SuccessFacts, ToolEntry,
    ToolInvocation, ToolOutcome, TurnDuration,
};
use amux_ui::{AgentId, Model};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::chat::{ChatView, FeedScroll, entry_watermark};
use crate::chat::{composer::Composer, markdown};
use crate::render::{
    FrameContext, Theme, blank_line, finish_line, line_len, new_line, pad_to, push_span, str_width,
};

/// Column grid from the wireframes: glyphs (`›`, `✔`, `~`, `─`) at 2,
/// entry text at 4, `└` continuation text at 6.
const GLYPH_COL: usize = 2;
const TEXT_COL: usize = 4;
const CONT_COL: usize = 6;

/// Below these, the chat frame cannot lay out (7 static chrome rows + up
/// to 2 working/paused rows + a 1-row composer need 10); degrade to the
/// chrome's too-small notice.
const MIN_WIDTH: usize = 24;
const MIN_HEIGHT: usize = 10;

/// One 1 Hz Tick drives the spinner and the elapsed text together (D5);
/// the frame index derives from elapsed seconds — no renderer state.
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// The plan entry's feed preview length (B6: "truncated to its first ~6
/// lines").
const PLAN_PREVIEW_LINES: usize = 6;

// --- layout -----------------------------------------------------------------

/// The frame's fixed rows: top border, header, rule (3) above the feed;
/// blank, blank, footer, bottom border (4) below it. Everything else —
/// feed, working/paused rows, composer growth — divides the remainder.
const FIXED_ROWS: usize = 3 + 4;

/// Rows between the feed and the composer block: the reserved
/// queue-preview row (occupied by the paused rule when scrolled back)
/// plus the working line.
fn extra_rows(working: bool, paused: bool) -> usize {
    usize::from(working || paused) + usize::from(working)
}

/// The frame's row budget, shared between the renderer and the key
/// handler so scroll paging and rendering agree on the feed viewport.
pub(crate) struct ChatLayout {
    pub height: usize,
    pub composer_rows: usize,
    pub working: bool,
    pub paused: bool,
}

impl ChatLayout {
    fn feed_height_for(&self, paused: bool) -> usize {
        self.height
            .saturating_sub(FIXED_ROWS + extra_rows(self.working, paused) + self.composer_rows)
    }

    /// Feed viewport height for the current scroll state.
    pub fn feed_height(&self) -> usize {
        self.feed_height_for(self.paused)
    }

    /// Feed viewport height once scrolled back — the scroll keys target
    /// the paused layout (the paused rule takes one row).
    pub fn feed_height_when_paused(&self) -> usize {
        self.feed_height_for(true)
    }
}

pub(crate) fn layout(model: &Model, chat: &ChatView, viewport: (u16, u16)) -> ChatLayout {
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    let working = matches!(model.claude_phase(chat.agent), ChatPhase::Working);
    let paused = matches!(chat.scroll, FeedScroll::Paused { .. });
    // The composer auto-grows to six rows but never past the viewport's
    // fixed-row budget: the footer and border survive every height, the
    // feed gives way first. Never below 1: a 1-row composer always
    // renders (the too-small guard keeps heights where even that cannot
    // fit out of this path).
    let budget = height
        .saturating_sub(FIXED_ROWS + extra_rows(working, paused))
        .max(1);
    let (rows, _) = composer_display_rows(&chat.composer, text_width(width));
    ChatLayout {
        height,
        composer_rows: rows.len().clamp(1, budget.min(6)),
        working,
        paused,
    }
}

/// Cells available to content at TEXT_COL — feed text and the composer
/// share the column — inside the right border's one-cell margin.
fn text_width(width: usize) -> usize {
    width.saturating_sub(TEXT_COL + 1).max(1)
}

/// Cells available to `└` continuation text at CONT_COL.
fn cont_width(width: usize) -> usize {
    width.saturating_sub(CONT_COL + 1).max(1)
}

/// Total feed display rows at this width — the key handler's scroll bound.
pub(crate) fn feed_line_count(model: &Model, chat: &ChatView, width: usize) -> usize {
    feed_lines(model, chat.agent, Theme::default(), width).len()
}

// --- the frame --------------------------------------------------------------

pub(crate) fn build_chat_lines(
    model: &Model,
    chat: &ChatView,
    ctx: &FrameContext,
) -> Vec<Line<'static>> {
    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return vec![Line::from("amux: terminal too small")];
    }
    let theme = ctx.theme;
    let layout = layout(model, chat, ctx.viewport);
    let phase = model.claude_phase(chat.agent);
    let feed_h = layout.feed_height();

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    lines.push(top_border(width, theme));
    lines.push(header_line(model, chat, theme, phase, width));

    // The feed viewport: everything before `transcript_ready` is replay —
    // the loading band is visually distinct from an empty chat (B10).
    let loading = matches!(phase, ChatPhase::Replaying);
    let (window, at_top) = if loading {
        (loading_band(theme, width, feed_h), false)
    } else {
        let feed = feed_lines(model, chat.agent, theme, width);
        let total = feed.len();
        let max_top = total.saturating_sub(feed_h);
        let (start, at_top) = match &chat.scroll {
            FeedScroll::Following => (max_top, total <= feed_h),
            FeedScroll::Paused { top_line, .. } => {
                // Clamp a stale anchor against the Model at render — never
                // assert against it.
                let top = (*top_line).min(max_top);
                (top, top == 0)
            }
        };
        let mut window: Vec<Line<'static>> = feed.into_iter().skip(start).take(feed_h).collect();
        while window.len() < feed_h {
            window.push(blank_line(width));
        }
        (window, at_top)
    };

    // The rule row under the header carries the honest history boundary
    // when the feed's very first line is in view (B9): a statement, not an
    // apology.
    let truncated = model
        .claude(chat.agent)
        .is_some_and(|layer| layer.history_truncated());
    if truncated && at_top {
        lines.push(chrome_rule(width, theme, "─ earlier history unavailable "));
    } else {
        lines.push(chrome_rule(width, theme, ""));
    }
    lines.extend(window);

    // Reserved row above the working line (the queued-input preview door,
    // deferred): the paused rule occupies it when scrolled back. During
    // the loading band there is no feed for the rule to describe — the
    // row stays blank so the frame's row accounting holds.
    match &chat.scroll {
        FeedScroll::Paused {
            entry_watermark: mark,
            top_line,
        } if !loading => {
            lines.push(paused_rule(
                model, chat, theme, width, feed_h, *mark, *top_line,
            ));
        }
        FeedScroll::Paused { .. } => lines.push(blank_line(width)),
        FeedScroll::Following if layout.working => lines.push(blank_line(width)),
        FeedScroll::Following => {}
    }
    if layout.working {
        lines.push(working_line(model, chat, ctx, width));
    }

    lines.push(blank_line(width));
    lines.extend(composer_lines(chat, theme, width, layout.composer_rows));
    lines.push(blank_line(width));
    lines.push(footer_line(model, chat, theme, width));
    lines.push(crate::render::bottom_border(width));
    lines.truncate(height);
    lines
}

fn top_border(width: usize, theme: Theme) -> Line<'static> {
    let mut text = String::from("┌");
    while text.chars().count() < width - 1 {
        text.push('─');
    }
    text.push('┐');
    Line::from(Span::styled(text, theme.muted()))
}

fn header_line(
    model: &Model,
    chat: &ChatView,
    theme: Theme,
    phase: ChatPhase,
    width: usize,
) -> Line<'static> {
    let mut line = new_line();
    if let Some(card) = model.agent(chat.agent) {
        let host = model
            .host_name(card.agent.host_id)
            .unwrap_or("?")
            .to_string();
        push_span(&mut line, GLYPH_COL, card.display_name(), theme.text());
        line.spans.push(Span::styled(
            format!(" · {} @ {host}", card.agent.agent_type),
            theme.muted(),
        ));
    }
    let (word, style) = phase_word(phase, theme);
    let right_len = "chat · ".chars().count() + word.chars().count();
    let col = (width.saturating_sub(2 + right_len)).max(line_len(&line) + 1);
    push_span(&mut line, col, "chat · ", theme.muted());
    line.spans.push(Span::styled(word, style));
    finish_line(&mut line, width);
    line
}

fn phase_word(phase: ChatPhase, theme: Theme) -> (String, Style) {
    match phase {
        ChatPhase::Replaying => ("replaying".into(), theme.muted()),
        ChatPhase::Working => ("working".into(), theme.text()),
        ChatPhase::Idle { .. } => ("idle".into(), theme.muted()),
        ChatPhase::NeedsYou { .. } => ("needs you".into(), theme.warn()),
        ChatPhase::Errored => ("errored".into(), theme.error()),
        ChatPhase::Unknown => ("unknown".into(), theme.muted()),
    }
}

/// A full-interior dim rule, optionally titled (`─ earlier history
/// unavailable ─…`). Chrome rules run border to border, unlike feed rules
/// which respect the one-space margin.
fn chrome_rule(width: usize, theme: Theme, title: &str) -> Line<'static> {
    let mut line = new_line();
    let mut text = title.to_string();
    while 1 + text.chars().count() < width - 1 {
        text.push('─');
    }
    line.spans.push(Span::styled(text, theme.muted()));
    finish_line(&mut line, width);
    line
}

fn loading_band(theme: Theme, width: usize, feed_h: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::with_capacity(feed_h);
    let band_at = feed_h.saturating_sub(1) / 2;
    for i in 0..feed_h {
        if i == band_at {
            let text = "⟳ loading transcript…";
            let col = width.saturating_sub(text.chars().count()) / 2;
            let mut line = new_line();
            push_span(&mut line, col.max(GLYPH_COL), text, theme.muted());
            finish_line(&mut line, width);
            rows.push(line);
        } else {
            rows.push(blank_line(width));
        }
    }
    rows
}

/// `─ ↓ following paused · N new entries · pgdn to resume ──… NN% ──` —
/// the honesty indicator (scrolled-back frame), with the reading position.
fn paused_rule(
    model: &Model,
    chat: &ChatView,
    theme: Theme,
    width: usize,
    feed_h: usize,
    watermark: u64,
    top_line: usize,
) -> Line<'static> {
    let new_entries = entry_watermark(model, chat.agent).saturating_sub(watermark);
    let mut left = String::from("─ ↓ following paused");
    if new_entries > 0 {
        let plural = if new_entries == 1 { "y" } else { "ies" };
        left.push_str(&format!(" · {new_entries} new entr{plural}"));
    }
    left.push_str(" · pgdn to resume ");
    let total = feed_line_count(model, chat, width);
    let max_top = total.saturating_sub(feed_h);
    let percent = if max_top == 0 {
        100
    } else {
        top_line.min(max_top) * 100 / max_top
    };
    let right = format!(" {percent}% ────");
    let interior = width.saturating_sub(2);
    let fill = interior.saturating_sub(left.chars().count() + right.chars().count());
    let mut text = left;
    text.extend(std::iter::repeat_n('─', fill));
    text.push_str(&right);
    let mut line = new_line();
    line.spans.push(Span::styled(text, theme.muted()));
    finish_line(&mut line, width);
    line
}

/// `◐ working · 24s · ctrl+x interrupt` (D5). Elapsed ticks locally from
/// the prompt row's timestamp; the authoritative duration replaces it in
/// the turn marker at close. The token count is omitted: the layer folds
/// no usage facts yet (recorded in the phase report).
fn working_line(model: &Model, chat: &ChatView, ctx: &FrameContext, width: usize) -> Line<'static> {
    let theme = ctx.theme;
    let elapsed = model
        .claude(chat.agent)
        .and_then(|layer| layer.prompt_at())
        .map(|at| (ctx.now - at).num_seconds().max(0) as u64);
    let spinner = SPINNER[elapsed.unwrap_or(0) as usize % SPINNER.len()];
    let mut line = new_line();
    let mut label = format!("{spinner} working");
    if let Some(secs) = elapsed {
        label.push_str(&format!(" · {}", fmt_secs(secs)));
    }
    push_span(&mut line, GLYPH_COL, label, theme.text());
    line.spans
        .push(Span::styled(" · ctrl+x interrupt", theme.muted()));
    finish_line(&mut line, width);
    line
}

// --- composer ---------------------------------------------------------------

/// The draft (cursor glyph inserted) as hard-wrapped display rows, plus
/// the row holding the cursor — the auto-grow and windowing base. Rows
/// wrap by display cells at grapheme boundaries (CJK/emoji are two cells,
/// combining marks zero); cursor tracking stays char-indexed because the
/// `▌` glyph is a real char in the display string.
fn composer_display_rows(composer: &Composer, width: usize) -> (Vec<String>, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    let width = width.max(1);
    let display = composer.display_with_cursor();
    let cursor_pos = composer.cursor();
    let mut rows: Vec<String> = Vec::new();
    let mut cursor_row = 0usize;
    let mut chars_seen = 0usize;
    for logical in display.split('\n') {
        let mut row = String::new();
        let mut row_cells = 0usize;
        for grapheme in logical.graphemes(true) {
            let cells = str_width(grapheme);
            if row_cells + cells > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                row_cells = 0;
            }
            if cursor_pos >= chars_seen && cursor_pos < chars_seen + grapheme.chars().count() {
                cursor_row = rows.len();
            }
            row.push_str(grapheme);
            row_cells += cells;
            chars_seen += grapheme.chars().count();
        }
        rows.push(row);
        chars_seen += 1; // the newline
    }
    (rows, cursor_row)
}

fn composer_lines(
    chat: &ChatView,
    theme: Theme,
    width: usize,
    visible_rows: usize,
) -> Vec<Line<'static>> {
    if chat.composer.is_empty() {
        // Empty chat/composer: the placeholder, dim, cursor after it —
        // the wireframes' empty state.
        let mut line = new_line();
        push_span(&mut line, GLYPH_COL, "›", theme.text());
        push_span(&mut line, TEXT_COL, "Type a message", theme.muted());
        line.spans.push(Span::styled("▌", theme.text()));
        finish_line(&mut line, width);
        return vec![line];
    }
    let (rows, cursor_row) = composer_display_rows(&chat.composer, text_width(width));
    // Auto-grow one to six rows; past six, the window follows the cursor.
    let start = if rows.len() <= visible_rows {
        0
    } else {
        (cursor_row + 1)
            .saturating_sub(visible_rows)
            .min(rows.len() - visible_rows)
    };
    rows.iter()
        .enumerate()
        .skip(start)
        .take(visible_rows)
        .map(|(index, row)| {
            let mut line = new_line();
            if index == 0 {
                push_span(&mut line, GLYPH_COL, "›", theme.text());
            }
            push_span(&mut line, TEXT_COL, row.clone(), theme.text());
            finish_line(&mut line, width);
            line
        })
        .collect()
}

// --- footer -----------------------------------------------------------------

/// One hint line, at most four items, derived purely from Model +
/// ViewState (no stored footer mode); permission mode on the right (D4,
/// hook-fact sourced).
fn footer_line(model: &Model, chat: &ChatView, theme: Theme, width: usize) -> Line<'static> {
    let mut line = new_line();
    if let Some(message) = chat.send_failure() {
        push_span(&mut line, GLYPH_COL, "✗", theme.error());
        push_span(
            &mut line,
            TEXT_COL,
            format!("send failed: {message}"),
            theme.text(),
        );
    } else if matches!(chat.scroll, FeedScroll::Paused { .. }) {
        let mut hints = String::from("pgup/pgdn scroll");
        if chat.composer.is_empty() {
            hints.push_str(" · esc newest");
        }
        push_span(&mut line, TEXT_COL, hints, theme.muted());
    } else if let Some(refusal) = model.claude_send_gate(chat.agent).refusal() {
        let hint = if chat.composer.is_empty() {
            refusal.to_string()
        } else {
            // D2: the footer states the gate plainly, and the draft is
            // kept — Enter is a no-op, never a loss.
            format!("draft kept — {refusal}")
        };
        push_span(&mut line, TEXT_COL, hint, theme.muted());
    } else {
        push_span(
            &mut line,
            TEXT_COL,
            "enter send · ctrl+j newline",
            theme.muted(),
        );
    }
    if let Some(mode) = model
        .claude(chat.agent)
        .and_then(|layer| layer.session().permission_mode.as_deref())
    {
        let label = format!("mode {mode}");
        let col = width.saturating_sub(2 + label.chars().count());
        if col > line_len(&line) {
            push_span(&mut line, col, label, theme.muted());
        }
    }
    finish_line(&mut line, width);
    line
}

// --- the feed ---------------------------------------------------------------

/// One rendered feed block: an entry's rows plus whether it is a tool
/// line (tool runs stack without blank separators, B4).
struct Block {
    lines: Vec<Line<'static>>,
    tool: bool,
}

/// All feed display rows, in file order, echoes last (B1) — unwindowed;
/// the frame slices by scroll. Block builders leave their lines open so
/// grouping can extend a main line; everything is finished (padded,
/// clipped, right-bordered) here, once.
pub(crate) fn feed_lines(
    model: &Model,
    agent: AgentId,
    theme: Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(layer) = model.claude(agent) else {
        return Vec::new();
    };
    let mut blocks: Vec<Block> = Vec::new();
    for entry in layer.entries() {
        // B4's grouping fact: consecutive read/search one-liners join onto
        // one line (idle wireframe: `✔ Read … · Grep …`). Joining is
        // layout; the fact comes from the fold.
        if let FeedEntryKind::Tool(tool) = &entry.kind
            && tool.group_with_previous
            && let Some(previous) = blocks.last_mut().filter(|block| block.tool)
            && append_grouped_tool(previous, tool, theme, width)
        {
            continue;
        }
        blocks.push(entry_block(entry, theme, width));
    }
    for echo in layer.pending_echoes() {
        blocks.push(echo_block(echo, theme, width));
    }

    let mut lines = Vec::new();
    let mut prev_tool = false;
    for (index, block) in blocks.into_iter().enumerate() {
        if index > 0 && !(prev_tool && block.tool) {
            lines.push(new_line());
        }
        prev_tool = block.tool;
        lines.extend(block.lines);
    }
    for line in &mut lines {
        finish_line(line, width);
    }
    lines
}

/// Join a grouped read/search one-liner onto the previous tool's main
/// line when it fits; refuse (fall back to its own line) when it would
/// clip — grouping compresses, it never hides. A grouped member that is
/// not a plain success keeps its own outcome glyph inline.
fn append_grouped_tool(previous: &mut Block, tool: &ToolEntry, theme: Theme, width: usize) -> bool {
    let Some(main) = previous.lines.first_mut() else {
        return false;
    };
    let (glyph, glyph_style) = outcome_glyph(tool, theme);
    let plain_success = glyph == "✔";
    let mut rest = String::new();
    if !plain_success {
        rest.push_str(glyph);
        rest.push(' ');
    }
    rest.push_str(&tool_main_text(tool));
    let added = 3 + str_width(&rest);
    if line_len(main) + added > width.saturating_sub(2) {
        return false;
    }
    let style = if plain_success {
        theme.text()
    } else {
        glyph_style
    };
    main.spans.push(Span::styled(" · ", theme.muted()));
    main.spans.push(Span::styled(rest, style));
    true
}

fn echo_block(echo: &PromptEcho, theme: Theme, width: usize) -> Block {
    let mut lines = glyph_block(
        "›",
        theme.text(),
        markdown::plain_rows(&echo.text, text_width(width), theme.text()),
    );
    let mut sending = new_line();
    push_span(&mut sending, TEXT_COL, "sending…", theme.muted());
    lines.push(sending);
    Block { lines, tool: false }
}

fn entry_block(entry: &FeedEntry, theme: Theme, width: usize) -> Block {
    match &entry.kind {
        FeedEntryKind::Prompt(prompt) => Block {
            lines: glyph_block(
                "›",
                theme.text(),
                markdown::plain_rows(&prompt.text, text_width(width), theme.text()),
            ),
            tool: false,
        },
        // One markdown source per message: blocks joined the way the API
        // separates them.
        FeedEntryKind::Message(message) => Block {
            lines: markdown_block(&message.segments.join("\n\n"), theme, width),
            tool: false,
        },
        FeedEntryKind::Thinking(thinking) => {
            let mut text = match thinking.duration_ms {
                Some(ms) => format!("~ thought for {}", fmt_secs(ms.max(0) as u64 / 1000)),
                None => "~ thought".to_string(),
            };
            if thinking.redacted {
                text.push_str(" · redacted");
            }
            Block {
                lines: vec![marker_line(&text, theme)],
                tool: false,
            }
        }
        FeedEntryKind::Turn(turn) => {
            let duration = match &turn.duration {
                TurnDuration::Measured { ms } => fmt_secs(ms / 1000),
                // Inferred elapsed-from-prompt (B3): marked `~` until the
                // authority reconciles it in place.
                TurnDuration::SincePrompt { ms } => {
                    format!("~{}", fmt_secs((*ms).max(0) as u64 / 1000))
                }
            };
            let mut title = format!("─ turn · {duration} ");
            if let Some(agents) = turn.pending_background_agents.filter(|n| *n > 0) {
                title = format!("─ turn · {duration} · {agents} bg running ");
            }
            Block {
                lines: vec![feed_rule(&title, theme, width)],
                tool: false,
            }
        }
        FeedEntryKind::Compaction(compaction) => {
            let mut title = String::from("─ compacted");
            if let Some(trigger) = &compaction.trigger {
                title.push_str(&format!(" ({trigger})"));
            }
            if let (Some(pre), Some(post)) = (compaction.pre_tokens, compaction.post_tokens) {
                title.push_str(&format!(
                    " · {} → {} tok",
                    fmt_tokens(pre),
                    fmt_tokens(post)
                ));
            }
            title.push(' ');
            Block {
                lines: vec![feed_rule(&title, theme, width)],
                tool: false,
            }
        }
        FeedEntryKind::CompactSummary(summary) => Block {
            lines: markdown_block(&summary.text, theme, width),
            tool: false,
        },
        FeedEntryKind::Tool(tool) => tool_block(tool, theme, width),
        FeedEntryKind::TaskNotification(notification) => Block {
            lines: glyph_block(
                "✔",
                theme.ok(),
                markdown::plain_rows(&notification.text, text_width(width), theme.muted()),
            ),
            tool: false,
        },
        FeedEntryKind::Interruption(interruption) => {
            let text = match interruption.kind {
                InterruptionKind::Turn => "interrupted",
                InterruptionKind::ToolUse => "interrupted (tool use)",
            };
            let mut line = new_line();
            push_span(&mut line, GLYPH_COL, "✗", theme.error());
            push_span(&mut line, TEXT_COL, text, theme.muted());
            Block {
                lines: vec![line],
                tool: false,
            }
        }
        FeedEntryKind::ApiError(error) => {
            let mut label = String::from("api error");
            if let Some(kind) = &error.error {
                label.push_str(&format!(" ({kind})"));
            }
            let mut line = new_line();
            push_span(&mut line, GLYPH_COL, "✗", theme.error());
            push_span(&mut line, TEXT_COL, label, theme.text());
            let mut lines = vec![line];
            if let Some(text) = &error.text {
                lines.extend(continuation_lines(text, theme, width));
            }
            Block { lines, tool: false }
        }
        FeedEntryKind::Unrecognized(row) => {
            // Explicit, never silently dropped (G1).
            let mut text = String::from("· unrecognized row");
            match (&row.row_type, &row.detail) {
                (Some(kind), Some(detail)) => text.push_str(&format!(" ({kind} · {detail})")),
                (Some(kind), None) => text.push_str(&format!(" ({kind})")),
                (None, Some(detail)) => text.push_str(&format!(" ({detail})")),
                (None, None) => {}
            }
            Block {
                lines: vec![marker_line(&text, theme)],
                tool: false,
            }
        }
    }
}

fn markdown_block(source: &str, theme: Theme, width: usize) -> Vec<Line<'static>> {
    markdown::markdown_rows(source, text_width(width), theme)
        .into_iter()
        .map(|spans| {
            let mut line = new_line();
            pad_to(&mut line, TEXT_COL);
            line.spans.extend(spans);
            line
        })
        .collect()
}

// --- tool lines -------------------------------------------------------------

fn outcome_glyph(tool: &ToolEntry, theme: Theme) -> (&'static str, Style) {
    match &tool.outcome {
        ToolOutcome::Pending => ("▸", theme.text()),
        ToolOutcome::Success { facts } => match facts {
            // The collapsed question fact leads with its own `?` (B5:
            // `? storage → trust store…`).
            SuccessFacts::Answers { .. } => ("?", theme.warn()),
            _ => ("✔", theme.ok()),
        },
        ToolOutcome::Denied { .. } | ToolOutcome::Failed { .. } => ("✗", theme.error()),
    }
}

fn tool_block(tool: &ToolEntry, theme: Theme, width: usize) -> Block {
    // Question answers collapse to per-answer fact lines.
    if let ToolOutcome::Success {
        facts: SuccessFacts::Answers { answers },
    } = &tool.outcome
    {
        let mut lines = Vec::new();
        for answer in answers {
            let text = format!("{} → {}", answer.question, answer.answer);
            lines.extend(glyph_block(
                "?",
                theme.warn(),
                markdown::plain_rows(&text, text_width(width), theme.text()),
            ));
        }
        if lines.is_empty() {
            lines = glyph_block(
                "?",
                theme.warn(),
                markdown::plain_rows("answered", text_width(width), theme.text()),
            );
        }
        return Block { lines, tool: true };
    }

    let (glyph, glyph_style) = outcome_glyph(tool, theme);
    let main = tool_main_text(tool);
    let mut lines = glyph_block(
        glyph,
        glyph_style,
        markdown::plain_rows(&main, text_width(width), theme.text()),
    );

    // The accepted plan stays readable in the feed, truncated (B6). The
    // `ctrl+t` reader affordance is Phase 5's; until it exists no hint
    // advertises it (hints tell the truth).
    if let (
        ToolInvocation::Plan {
            plan: Some(plan), ..
        },
        ToolOutcome::Success {
            facts: SuccessFacts::PlanApproved { .. },
        },
    ) = (&tool.invocation, &tool.outcome)
    {
        let total = plan.lines().count();
        for line in plan.lines().take(PLAN_PREVIEW_LINES) {
            for spans in markdown::plain_rows(line, text_width(width), theme.muted()) {
                let mut out = new_line();
                pad_to(&mut out, TEXT_COL);
                out.spans.extend(spans);
                lines.push(out);
            }
        }
        if total > PLAN_PREVIEW_LINES {
            let mut out = new_line();
            push_span(
                &mut out,
                TEXT_COL,
                format!("⋮ {} more lines", total - PLAN_PREVIEW_LINES),
                theme.muted(),
            );
            lines.push(out);
        }
    }

    if let Some(text) = tool_continuation(tool) {
        lines.extend(continuation_lines(&text, theme, width));
    }
    Block { lines, tool: true }
}

/// The tool line's text: name, target, FACT magnitude (never recomputed —
/// the sidecar states every landed edit).
fn tool_main_text(tool: &ToolEntry) -> String {
    let name = tool.name.as_deref().unwrap_or("earlier tool");
    match (&tool.invocation, &tool.outcome) {
        (
            ToolInvocation::Edit { .. } | ToolInvocation::Write { .. },
            ToolOutcome::Success {
                facts:
                    SuccessFacts::Edit {
                        file_path,
                        added,
                        removed,
                    },
            },
        ) => format!("{name} {file_path} {}", fmt_magnitude(*added, *removed)),
        (ToolInvocation::Edit { file_path, .. }, _)
        | (ToolInvocation::Write { file_path }, _)
        | (ToolInvocation::Read { file_path }, _) => match file_path {
            Some(path) => format!("{name} {path}"),
            None => name.to_string(),
        },
        (ToolInvocation::Bash { command, .. }, _) => match command {
            Some(command) => {
                let mut text = command.lines().next().unwrap_or_default().to_string();
                if command.lines().count() > 1 {
                    text.push_str(" …");
                }
                format!("{name} {text}")
            }
            None => name.to_string(),
        },
        (ToolInvocation::Query { text }, _) => match text {
            Some(text) => format!("{name} \"{text}\""),
            None => name.to_string(),
        },
        (
            ToolInvocation::Task {
                description,
                background,
                ..
            },
            _,
        ) => {
            let mut text = match description {
                Some(description) => format!("{name} {description}"),
                None => name.to_string(),
            };
            if *background {
                text.push_str(" (background)");
            }
            text
        }
        (
            ToolInvocation::Plan { .. },
            ToolOutcome::Success {
                facts: SuccessFacts::PlanApproved { .. },
            },
        ) => "plan approved".to_string(),
        (ToolInvocation::Plan { .. }, _) => name.to_string(),
        (ToolInvocation::Question { .. }, _) | (ToolInvocation::Other, _) => name.to_string(),
    }
}

/// `(+9 -2)` from the sidecar's FACT magnitude; zero halves drop out
/// (`(+20)` for a Write-create).
fn fmt_magnitude(added: u64, removed: u64) -> String {
    match (added, removed) {
        (0, 0) => "(±0)".to_string(),
        (added, 0) => format!("(+{added})"),
        (0, removed) => format!("(-{removed})"),
        (added, removed) => format!("(+{added} -{removed})"),
    }
}

/// The dim `└` continuation, when the outcome has one.
fn tool_continuation(tool: &ToolEntry) -> Option<String> {
    match &tool.outcome {
        ToolOutcome::Pending => Some(
            // A pending question/plan blocks on the user, not on a run.
            match &tool.invocation {
                ToolInvocation::Question { .. } | ToolInvocation::Plan { .. } => {
                    "pending".to_string()
                }
                _ => "running".to_string(),
            },
        ),
        ToolOutcome::Success { facts } => match facts {
            SuccessFacts::Output { head, truncated } => {
                // Read/search one-liners suppress their output head — the
                // line already names the target, and the head would be raw
                // file content.
                if matches!(
                    tool.invocation,
                    ToolInvocation::Read { .. } | ToolInvocation::Query { .. }
                ) {
                    return None;
                }
                let mut first = head.lines().next().unwrap_or_default().to_string();
                if *truncated || head.lines().count() > 1 {
                    first.push_str(" …");
                }
                Some(first)
            }
            SuccessFacts::TaskCompleted {
                duration_ms,
                tool_count,
                ..
            } => {
                let mut text = String::from("done");
                if let Some(ms) = duration_ms {
                    text.push_str(&format!(" · {}", fmt_secs(ms / 1000)));
                }
                if let Some(tools) = tool_count {
                    text.push_str(&format!(" · {tools} tools"));
                }
                Some(text)
            }
            SuccessFacts::TaskLaunched { .. } => Some("launched in background".to_string()),
            SuccessFacts::Edit { .. }
            | SuccessFacts::Answers { .. }
            | SuccessFacts::PlanApproved { .. }
            | SuccessFacts::None => None,
        },
        // Denial is a typed fact, never an error-string sniff (B5).
        ToolOutcome::Denied { kind } => Some(match kind {
            Some(kind) => format!("denied ({kind})"),
            None => "denied".to_string(),
        }),
        ToolOutcome::Failed { message } => Some(match message {
            Some(message) => {
                let mut first = message.lines().next().unwrap_or_default().to_string();
                if message.lines().count() > 1 {
                    first.push_str(" …");
                }
                first
            }
            None => "failed".to_string(),
        }),
    }
}

// --- line builders ----------------------------------------------------------

/// A glyph at column 2, wrapped text rows at column 4.
fn glyph_block(
    glyph: &str,
    glyph_style: Style,
    rows: Vec<Vec<Span<'static>>>,
) -> Vec<Line<'static>> {
    rows.into_iter()
        .enumerate()
        .map(|(index, spans)| {
            let mut line = new_line();
            if index == 0 {
                push_span(&mut line, GLYPH_COL, glyph.to_string(), glyph_style);
            }
            pad_to(&mut line, TEXT_COL);
            line.spans.extend(spans);
            line
        })
        .collect()
}

/// A dim marker at the glyph column (`~ thought for 6s`).
fn marker_line(text: &str, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, text.to_string(), theme.muted());
    line
}

/// A feed rule: dim, starting at the glyph column, filled to the right
/// border (`─ turn · 1m 42s ────…`).
fn feed_rule(title: &str, theme: Theme, width: usize) -> Line<'static> {
    let mut text = title.to_string();
    while GLYPH_COL + text.chars().count() < width - 1 {
        text.push('─');
    }
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, text, theme.muted());
    line
}

/// Dim `└ text` continuation rows under a tool line.
fn continuation_lines(text: &str, theme: Theme, width: usize) -> Vec<Line<'static>> {
    markdown::plain_rows(text, cont_width(width), theme.muted())
        .into_iter()
        .enumerate()
        .map(|(index, spans)| {
            let mut line = new_line();
            if index == 0 {
                push_span(&mut line, TEXT_COL, "└", theme.muted());
            }
            pad_to(&mut line, CONT_COL);
            line.spans.extend(spans);
            line
        })
        .collect()
}

// --- formatting -------------------------------------------------------------

/// `24s`, `1m 42s`, `1h 2m` — durations floor to whole units.
fn fmt_secs(total: u64) -> String {
    if total >= 3600 {
        format!("{}h {}m", total / 3600, (total % 3600) / 60)
    } else if total >= 60 {
        format!("{}m {}s", total / 60, total % 60)
    } else {
        format!("{total}s")
    }
}

/// `31.6k` / `421` token counts (the compaction rule).
fn fmt_tokens(count: u64) -> String {
    if count >= 1000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else {
        count.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_floor_to_whole_units() {
        assert_eq!(fmt_secs(24), "24s");
        assert_eq!(fmt_secs(102), "1m 42s");
        assert_eq!(fmt_secs(3735), "1h 2m");
    }

    #[test]
    fn token_counts_humanize_at_a_thousand() {
        assert_eq!(fmt_tokens(421), "421");
        assert_eq!(fmt_tokens(31_641), "31.6k");
        assert_eq!(fmt_tokens(1_795), "1.8k");
    }

    #[test]
    fn magnitudes_drop_zero_halves() {
        assert_eq!(fmt_magnitude(9, 2), "(+9 -2)");
        assert_eq!(fmt_magnitude(20, 0), "(+20)");
        assert_eq!(fmt_magnitude(0, 3), "(-3)");
        assert_eq!(fmt_magnitude(0, 0), "(±0)");
    }

    #[test]
    fn composer_rows_track_the_cursor_across_wraps() {
        let mut composer = Composer::default();
        composer.insert_str("abcdefgh");
        let (rows, cursor_row) = composer_display_rows(&composer, 4);
        // "abcdefgh▌" hard-wrapped at 4: abcd | efgh | ▌
        assert_eq!(rows, vec!["abcd", "efgh", "▌"]);
        assert_eq!(cursor_row, 2);
    }

    #[test]
    fn composer_rows_split_on_newlines() {
        let mut composer = Composer::default();
        composer.insert_str("one\ntwo");
        for _ in 0..3 {
            composer.left();
        }
        let (rows, cursor_row) = composer_display_rows(&composer, 40);
        assert_eq!(rows, vec!["one", "▌two"]);
        assert_eq!(cursor_row, 1);
    }
}
