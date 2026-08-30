//! The Codex chat's adapter onto the shared frame: it walks Codex's own
//! typed entries, formats their words, and hands the shell finished
//! blocks.
//!
//! Nothing here draws. Every row comes from the painter kit in
//! `chat::blocks`, the same one the Claude adapter uses, so the two
//! screens cannot drift apart. Phase and attention-like presentation come
//! only from `amux_ui::codex::phase`; feed blocks format the layer's typed
//! entries without reconstructing a second semantic model.

use amux_ui::codex::{
    ApprovalResolution, Ask, AskContext, BoundaryEntry, CodexPhase, ErrorSeverity, FeedEntry,
    FeedEntryKind, ItemFinality, McpStartupEntry, McpStartupStatus, MessagePhase,
    NetworkPolicyAction, NetworkPolicyAmendment, PromptPart, PromptSource, TokenUsage, TurnStatus,
    WorkEntry, WorkKind, WorkOutcome, WorkState,
};
use amux_ui::{AgentId, Model};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::View;
use crate::chat::blocks::{
    self, paint_agent_message, paint_ask_fact, paint_ask_panel, paint_assistant,
    paint_compaction_rule, paint_composer_block, paint_error, paint_header, paint_mcp_startup,
    paint_thinking, paint_tool_line, paint_turn_rule, paint_unrecognized, paint_user_prompt,
};
use crate::chat::diff::diff_rows_from_patch;
use crate::chat::frame::{
    BlockKey, ChatFrameParts, ChatGeometry, FeedBlocks, FrameSpacing, PaintCache, PaintedBlock,
    chat_geometry,
};
use crate::chat::viewport::FeedViewport;
use crate::chat::{FeedScroll, MessageView, family_banner, message_glyph};
use crate::markdown;
use crate::render::{FrameContext, Theme, clip_to_width, pad_to, push_span, str_width};
use crate::view::QuitGuard;

const GLYPH_COL: usize = blocks::GLYPH_COL;
const TEXT_COL: usize = blocks::TEXT_COL;
const DECISION_LABEL_MAX: usize = 52;
const DECISION_KIND_MAX: usize = 22;
const DECISION_DETAIL_MAX: usize = DECISION_LABEL_MAX - DECISION_KIND_MAX - 3;
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// Rows of a file change's patch a feed block shows before it says the
/// rest is in the sandbox, not on screen.
const PATCH_PREVIEW_ROWS: usize = 12;

// --- the frame --------------------------------------------------------------

/// Everything the shared shell needs to draw this chat.
pub(crate) fn codex_frame_parts(
    model: &Model,
    chat: &View,
    viewport: &FeedViewport,
    cache: &mut PaintCache,
    ctx: &FrameContext,
) -> ChatFrameParts {
    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    let theme = ctx.theme;
    let phase = amux_ui::codex::phase(model, chat.agent);
    let working = active_phase(&phase);
    let loading = matches!(phase, CodexPhase::Replaying);

    // A child waiting on a person beats the session's own configuration:
    // one is a person being blocked, the other is context that will still
    // be true a minute from now.
    let banner = match family_banner(model, chat.agent) {
        Some(banner) => Some(family_banner_line(
            &banner.row(banner_answerable(model, chat, &banner), chat.leader),
            theme,
        )),
        None => chat
            .configuration_label
            .as_deref()
            .map(|label| configuration_row(label, theme)),
    };

    let paused = matches!(viewport.scroll, FeedScroll::Paused { .. });
    ChatFrameParts {
        header: header_row(model, chat, &phase, theme, width),
        banner,
        feed: FeedBlocks {
            blocks: if loading {
                Vec::new()
            } else {
                feed_blocks(model, chat, cache, theme, width)
            },
            history_truncated: model
                .codex(chat.agent)
                .is_some_and(|layer| layer.history_truncated()),
            loading,
        },
        activity: working.then(|| working_row(model, chat, &phase, ctx)),
        bottom: bottom_block(model, chat, theme, width, height, paused),
        overlay: chat
            .help
            .then(|| help_overlay(model, chat, theme, width, height)),
    }
}

// --- geometry the key handler shares ----------------------------------------

pub(in crate::chat) fn geometry(
    model: &Model,
    chat: &View,
    viewport: (u16, u16),
    paused: bool,
) -> ChatGeometry {
    let theme = Theme::default();
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    let banner = family_banner(model, chat.agent).is_some() || chat.configuration_label.is_some();
    chat_geometry(
        viewport,
        FrameSpacing::DEFAULT,
        banner,
        active_phase(&amux_ui::codex::phase(model, chat.agent)),
        paused,
        bottom_block(model, chat, theme, width, height, paused).len(),
    )
}

// --- the header and the rows around the feed --------------------------------

fn header_row(
    model: &Model,
    chat: &View,
    phase: &CodexPhase,
    theme: Theme,
    width: usize,
) -> Line<'static> {
    let name = match model.agent(chat.agent) {
        Some(card) => format!(
            "{} · {} @ {}{}",
            card.display_name(),
            card.agent.agent_type,
            model.host_name(card.agent.host_id).unwrap_or("?"),
            crate::chat::subagent_marker(model, chat.agent),
        ),
        None => String::new(),
    };
    let (mut word, style) = phase_word(phase, theme);
    let readonly = chat.read_only(model);
    if readonly && matches!(phase, CodexPhase::AwaitingApproval { .. }) {
        word = "needs owner".to_string();
    }
    let right = if readonly {
        "chat · read-only · "
    } else {
        "chat · "
    };
    paint_header(&name, (&word, style), right, theme, width)
}

/// The session's own settings, stated once under the header: what model
/// this is, how it asks, and what it is allowed to touch.
fn configuration_row(label: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, GLYPH_COL, label.to_string(), theme.muted());
    line
}

/// Whether this banner's chord would do anything from here: the child
/// has a panel to dock, and it is not already docked.
fn banner_answerable(model: &Model, chat: &View, banner: &crate::chat::FamilyBanner) -> bool {
    chat.inline_ask.is_none() && crate::chat::inline::can_open(model, chat.agent, banner.child)
}

/// The child-ask banner (U1): one warning row naming who is waiting and
/// for what, derived per frame so it leaves when the ask is answered
/// anywhere.
fn family_banner_line(text: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, GLYPH_COL, "⚠", theme.warn());
    push_span(&mut line, TEXT_COL, text.to_string(), theme.warn());
    line
}

fn working_row(
    model: &Model,
    chat: &View,
    phase: &CodexPhase,
    ctx: &FrameContext,
) -> Line<'static> {
    let label = match phase {
        CodexPhase::Responding { .. } => "responding",
        CodexPhase::Executing { .. } => "executing",
        _ => "thinking",
    };
    let spinner = SPINNER[ctx.now.timestamp().unsigned_abs() as usize % SPINNER.len()];
    let mut line = Line::default();
    push_span(
        &mut line,
        GLYPH_COL,
        format!("{spinner} {label}"),
        ctx.theme.text(),
    );
    let mut hints = Vec::new();
    // A docked child ask owns Enter and Ctrl+X while it is on screen, so
    // the activity line stops naming them: a hint that would do
    // something else than it says is worse than no hint (P10). The
    // panel's own rows say what those keys do instead.
    if chat.inline_ask.is_none() {
        if amux_ui::codex::allows_steer(model, chat.agent) {
            hints.push("enter steer");
        }
        if amux_ui::codex::allows_interrupt(model, chat.agent) {
            hints.push("ctrl+x interrupt");
        }
    }
    if !hints.is_empty() {
        line.spans.push(Span::styled(
            format!(" · {}", hints.join(" · ")),
            ctx.theme.muted(),
        ));
    }
    line
}

fn active_phase(phase: &CodexPhase) -> bool {
    matches!(
        phase,
        CodexPhase::Thinking | CodexPhase::Responding { .. } | CodexPhase::Executing { .. }
    )
}

fn phase_word(phase: &CodexPhase, theme: Theme) -> (String, Style) {
    match phase {
        CodexPhase::Replaying => ("replaying".into(), theme.muted()),
        CodexPhase::Idle => ("idle".into(), theme.muted()),
        CodexPhase::Thinking => ("thinking".into(), theme.text()),
        CodexPhase::Responding { .. } => ("responding".into(), theme.text()),
        CodexPhase::Executing { .. } => ("executing".into(), theme.text()),
        CodexPhase::AwaitingApproval { .. } => ("needs you".into(), theme.warn()),
        CodexPhase::BlockedUnsupported { .. } => ("blocked".into(), theme.warn()),
        CodexPhase::ReadOnly => ("read-only".into(), theme.warn()),
        CodexPhase::Unknown => ("unknown".into(), theme.muted()),
    }
}

// --- the bottom block -------------------------------------------------------

/// Everything below the feed: the read-only statement, a docked
/// approval — this chat's own or a child's — the blocked-input panel, or
/// the composer the person types in.
fn bottom_block(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    height: usize,
    paused: bool,
) -> Vec<Line<'static>> {
    let mut lines = if chat.read_only(model) {
        readonly_bottom(theme)
    } else if let Some(inline) = &chat.inline_ask {
        // U2: a child's ask docks where the composer is, exactly as this
        // chat's own ask would. The parent's own ask is checked first,
        // below, and reconcile drops a guest the moment one arrives —
        // one panel, one cursor, one place to look.
        crate::chat::inline::panel_lines(model, inline, width, theme, chat.quit_guard.is_armed())
    } else if let Some(ask) = model.codex(chat.agent).and_then(|layer| layer.ask_head()) {
        approval_panel(
            model,
            ApprovalView {
                agent: chat.agent,
                cursor: chat.approval_cursor,
                failure: chat.answer_failure.as_deref(),
            },
            ask,
            width,
            theme,
            chat.quit_guard.is_armed(),
        )
    } else if matches!(
        amux_ui::codex::phase(model, chat.agent),
        CodexPhase::BlockedUnsupported { .. }
    ) {
        unsupported_panel(chat, width, theme)
    } else {
        return composer_bottom(model, chat, theme, width, height, paused);
    };

    // Keep the tail: the hint and action rows survive, body rows give way
    // (mirrors the feed giving way to the composer).
    let max_rows = height.saturating_sub(4).max(1);
    if lines.len() > max_rows {
        lines.drain(..lines.len() - max_rows);
    }
    lines
}

/// Whose approval this is and how the reader is holding it — the whole
/// of what the panel needed from a `View`. Named separately so the same
/// rows can be drawn for an agent whose chat is not the one on screen
/// (U2: a child's ask, docked in its parent's chat).
#[derive(Clone, Copy)]
pub(crate) struct ApprovalView<'a> {
    pub(crate) agent: AgentId,
    pub(crate) cursor: usize,
    pub(crate) failure: Option<&'a str>,
}

pub(crate) fn approval_panel(
    model: &Model,
    view: ApprovalView<'_>,
    ask: &Ask,
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
) -> Vec<Line<'static>> {
    paint_ask_panel(
        BlockKey(ask.seq),
        &approval_title_for(model, view.agent, &ask.context),
        approval_body(model, view, ask, theme, blocks::panel_body_width(width)),
        approval_actions(model, view, ask, theme, quit_guard_armed),
        &approval_hints(model, view, ask, quit_guard_armed),
        theme,
        width,
    )
    .lines
}

/// The panel title, with the honest queue position: the panel has one
/// title row and no right margin of its own, so an approval that is one
/// of several says so where its name is.
fn approval_title_for(model: &Model, agent: AgentId, context: &AskContext) -> String {
    let count = model
        .codex(agent)
        .map(|layer| layer.ask_count())
        .unwrap_or(1);
    let title = approval_title(context);
    match count {
        0 | 1 => title,
        count => format!("{title} · 1 of {count}"),
    }
}

/// What is being asked about: the command, the files, the permissions or
/// the tool call, plus any failure the last answer reported.
fn approval_body(
    model: &Model,
    view: ApprovalView<'_>,
    ask: &Ask,
    theme: Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let _ = model;
    let mut lines = context_lines(&ask.context, width, theme);
    if let Some(message) = view.failure {
        lines.extend(panel_glyph_text(
            "✗",
            message,
            width,
            theme.error(),
            theme.text(),
        ));
    }
    lines
}

/// The decisions on offer, or the one row that says an answer is already
/// on its way.
fn approval_actions(
    model: &Model,
    view: ApprovalView<'_>,
    ask: &Ask,
    theme: Theme,
    quit_guard_armed: bool,
) -> Vec<Line<'static>> {
    let answer_in_flight = model.codex(view.agent).is_some_and(|layer| {
        layer.in_flight_inputs().any(|input| {
            matches!(&input.kind, amux_ui::codex::InFlightKind::Answer { request_id, .. }
                if *request_id == ask.request_id)
        })
    });
    if answer_in_flight {
        let mut line = Line::default();
        push_span(&mut line, 0, "◌", theme.muted());
        push_span(&mut line, 2, "sending decision…", theme.muted());
        return vec![line];
    }

    let allows_answer = amux_ui::codex::allows_answer(model, view.agent);
    let mut lines = Vec::new();
    for (index, action) in ask.actions.iter().enumerate() {
        let mut line = Line::default();
        let supported = action.decision.is_some();
        let selectable = allows_answer && supported;
        if selectable && index == view.cursor {
            push_span(&mut line, 0, "›", theme.text());
        }
        let style = if selectable {
            theme.text()
        } else {
            theme.muted()
        };
        push_span(&mut line, 2, format!("{}.", index + 1), style);
        push_span(
            &mut line,
            5,
            decision_label(&ask.context, &action.wire),
            style,
        );
        if !supported {
            line.spans
                .push(Span::styled(" · unavailable in V1", theme.muted()));
        }
        lines.push(line);
    }
    // The armed guard replaces the hints entirely, in warning colour, so
    // it lands as the last action row rather than as hint text.
    if quit_guard_armed {
        lines.push(Line::default());
        lines.push(armed_quit_line(theme));
    }
    lines
}

fn approval_hints(
    model: &Model,
    view: ApprovalView<'_>,
    ask: &Ask,
    quit_guard_armed: bool,
) -> String {
    let answer_in_flight = model.codex(view.agent).is_some_and(|layer| {
        layer.in_flight_inputs().any(|input| {
            matches!(&input.kind, amux_ui::codex::InFlightKind::Answer { request_id, .. }
                if *request_id == ask.request_id)
        })
    });
    if quit_guard_armed || answer_in_flight || !amux_ui::codex::allows_answer(model, view.agent) {
        return String::new();
    }
    "↑↓/1-9 select · enter confirm · ctrl+x interrupt".to_string()
}

fn approval_title(context: &AskContext) -> String {
    match context {
        AskContext::Command { .. } => "approval — command".to_string(),
        AskContext::FileChange { changes, .. } => {
            format!("approval — {} file change(s)", changes.len())
        }
        AskContext::Permissions { .. } => "approval — permissions".to_string(),
        AskContext::DynamicTool { tool, .. } => format!("approval — {tool}"),
    }
}

/// A panel body's own columns: the painter supplies the indent, so a
/// row's glyph sits at its left edge and its text two cells in.
const PANEL_GLYPH_COL: usize = 0;
const PANEL_TEXT_COL: usize = 2;
const PANEL_CONT_COL: usize = 4;

/// `glyph text` inside a panel body.
fn panel_glyph_text(
    glyph: &str,
    text: &str,
    width: usize,
    glyph_style: Style,
    text_style: Style,
) -> Vec<Line<'static>> {
    markdown::plain_rows(
        text,
        width.saturating_sub(PANEL_TEXT_COL).max(1),
        text_style,
    )
    .into_iter()
    .enumerate()
    .map(|(index, spans)| {
        let mut line = Line::default();
        if index == 0 {
            push_span(&mut line, PANEL_GLYPH_COL, glyph.to_string(), glyph_style);
        }
        pad_to(&mut line, PANEL_TEXT_COL);
        line.spans.extend(spans);
        line
    })
    .collect()
}

/// A dim `└ …` continuation inside a panel body.
fn panel_continuation(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    markdown::plain_rows(
        text,
        width.saturating_sub(PANEL_CONT_COL).max(1),
        theme.muted(),
    )
    .into_iter()
    .enumerate()
    .map(|(index, spans)| {
        let mut line = Line::default();
        if index == 0 {
            push_span(&mut line, PANEL_TEXT_COL, "└", theme.muted());
        }
        pad_to(&mut line, PANEL_CONT_COL);
        line.spans.extend(spans);
        line
    })
    .collect()
}

fn context_lines(context: &AskContext, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match context {
        AskContext::Command {
            command,
            cwd,
            reason,
            ..
        } => {
            lines.extend(panel_glyph_text(
                "$",
                command,
                width,
                theme.muted(),
                theme.code(),
            ));
            if let Some(cwd) = cwd {
                lines.extend(panel_continuation(&format!("cwd {cwd}"), width, theme));
            }
            if let Some(reason) = reason {
                lines.extend(panel_continuation(reason, width, theme));
            }
        }
        AskContext::FileChange {
            reason, changes, ..
        } => {
            for change in changes {
                lines.extend(panel_glyph_text(
                    "▸",
                    &format!(
                        "{}{}",
                        change.path,
                        change
                            .status
                            .as_deref()
                            .map(|status| format!(" · {status}"))
                            .unwrap_or_default()
                    ),
                    width,
                    theme.text(),
                    theme.text(),
                ));
            }
            if let Some(reason) = reason {
                lines.extend(panel_continuation(reason, width, theme));
            }
        }
        AskContext::Permissions {
            reason,
            permissions,
            ..
        } => {
            lines.extend(panel_glyph_text(
                "▸",
                &json_text(permissions),
                width,
                theme.text(),
                theme.code(),
            ));
            if let Some(reason) = reason {
                lines.extend(panel_continuation(reason, width, theme));
            }
        }
        AskContext::DynamicTool {
            tool,
            namespace,
            arguments,
            ..
        } => lines.extend(panel_glyph_text(
            "▸",
            &format!(
                "{}{} {}",
                namespace
                    .as_deref()
                    .map(|namespace| format!("{namespace}::"))
                    .unwrap_or_default(),
                tool,
                json_text(arguments)
            ),
            width,
            theme.text(),
            theme.code(),
        )),
    }
    lines
}

/// Codex asked for something structured chat V1 cannot answer. It goes
/// on the panel surface, like an approval, because it is the same kind
/// of fact: the turn is waiting on a person and this screen cannot be
/// the person.
fn unsupported_panel(chat: &View, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let body_width = blocks::panel_body_width(width);
    let mut body = markdown::plain_rows(
        "Codex requested user input that structured chat V1 cannot answer.",
        body_width,
        theme.text(),
    )
    .into_iter()
    .map(Line::from)
    .collect::<Vec<_>>();
    body.push(Line::from(Span::styled(
        "This turn is blocked — it is not idle.",
        theme.muted(),
    )));
    let mut actions = Vec::new();
    if chat.quit_guard.is_armed() {
        actions.push(armed_quit_line(theme));
    }
    paint_ask_panel(
        BlockKey(u64::MAX),
        "user input requested",
        body,
        actions,
        &if chat.quit_guard.is_armed() {
            String::new()
        } else {
            format!("ctrl+x interrupt · C-{} s then open raw mode", chat.leader)
        },
        theme,
        width,
    )
    .lines
}

fn readonly_bottom(theme: Theme) -> Vec<Line<'static>> {
    let mut marker = Line::default();
    push_span(
        &mut marker,
        GLYPH_COL,
        "⊘ read-only — you are observing this Codex session",
        theme.muted(),
    );
    let mut hint = Line::default();
    push_span(
        &mut hint,
        TEXT_COL,
        "pgup/pgdn scroll · q back to fleet",
        theme.muted(),
    );
    vec![marker, Line::default(), hint]
}

/// The composer as the person's own surface, with one hint row under it.
fn composer_bottom(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    height: usize,
    paused: bool,
) -> Vec<Line<'static>> {
    // The composer grows from one row to six, never past what the frame
    // can spare: the hint row and a feed row survive every height.
    let budget = height.saturating_sub(6).clamp(1, 6);
    let (rows, cursor_row) = chat.composer.display_rows(text_width(width));
    let mut lines = if chat.composer.is_empty() {
        paint_composer_block(
            vec![String::new()],
            Some((0, 0)),
            Some("Type a message"),
            theme,
            width,
        )
    } else {
        let visible = rows.len().min(budget);
        // Past the visible rows, the window follows the cursor.
        let start = if rows.len() <= visible {
            0
        } else {
            (cursor_row + 1)
                .saturating_sub(visible)
                .min(rows.len() - visible)
        };
        paint_composer_block(
            rows[start..start + visible].to_vec(),
            None,
            None,
            theme,
            width,
        )
    };
    lines.push(Line::default());
    lines.push(footer_line(model, chat, theme, paused));
    lines
}

fn footer_line(model: &Model, chat: &View, theme: Theme, paused: bool) -> Line<'static> {
    let mut line = if chat.quit_guard.is_armed() {
        armed_quit_line(theme)
    } else {
        Line::default()
    };
    if !chat.quit_guard.is_armed() {
        if let Some(message) = &chat.send_failure {
            push_span(&mut line, GLYPH_COL, "✗", theme.error());
            push_span(
                &mut line,
                TEXT_COL,
                format!("send failed: {message}"),
                theme.text(),
            );
        } else if paused {
            // The rule above the footer already says how to catch up, so
            // the footer spends its width on what a stopped reader came
            // for: putting the focus on a block and taking it out.
            push_span(
                &mut line,
                TEXT_COL,
                crate::bindings::Effective::new(chat.kitty, chat.leader).feed_hint(),
                theme.muted(),
            );
        } else if amux_ui::codex::allows_steer(model, chat.agent) {
            let suffix = "enter steer · ctrl+j newline";
            push_span(&mut line, TEXT_COL, suffix, theme.muted());
        } else if let Some(refusal) = amux_ui::codex::send_gate(model, chat.agent).refusal() {
            let text = if chat.composer.is_empty() {
                refusal.to_string()
            } else {
                format!("draft kept — {refusal}")
            };
            push_span(&mut line, TEXT_COL, text, theme.muted());
        } else {
            push_span(
                &mut line,
                TEXT_COL,
                "enter send · ctrl+j newline · ? help",
                theme.muted(),
            );
        }
    }
    line
}

fn armed_quit_line(theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, 2, QuitGuard::HINT, theme.warn());
    line
}

/// Every feed block, in file order. Codex has no read-only exploration
/// kind to fold: every command and every file change is consequential,
/// so each one keeps a block of its own.
fn feed_blocks(
    model: &Model,
    chat: &View,
    cache: &mut PaintCache,
    theme: Theme,
    width: usize,
) -> Vec<PaintedBlock> {
    let Some(layer) = model.codex(chat.agent) else {
        return Vec::new();
    };
    let reports = MessageView::new(model, chat.agent, chat.reports_open, chat.leader);
    let blocks: Vec<_> = layer
        .entries()
        .map(|entry| {
            cache
                .get_or_paint(
                    BlockKey(entry.id),
                    entry,
                    width,
                    theme,
                    chat.reports_open,
                    || entry_block(entry, theme, width, reports),
                )
                .clone()
        })
        .collect();
    cache.retain(&blocks.iter().map(|block| block.key).collect::<Vec<_>>());
    blocks
}

/// Join a second painted block onto the first: some entries say one
/// thing in two shapes — a patch under its file list, a streaming marker
/// under a message — and they are still one block to focus and copy.
fn merged(mut block: PaintedBlock, tail: PaintedBlock) -> PaintedBlock {
    block.lines.extend(tail.lines);
    if !tail.copy_text.is_empty() {
        block.copy_text.push('\n');
        block.copy_text.push_str(&tail.copy_text);
    }
    block
}

fn entry_block(
    entry: &FeedEntry,
    theme: Theme,
    width: usize,
    reports: MessageView<'_>,
) -> PaintedBlock {
    let key = BlockKey(entry.id);
    match &entry.kind {
        FeedEntryKind::Prompt(prompt) => {
            let mut text = match prompt.source {
                PromptSource::Protocol => String::new(),
                PromptSource::SteerEcho => "steer · ".to_string(),
            };
            text.push_str(&prompt_parts(&prompt.parts));
            paint_user_prompt(
                key,
                &text,
                prompt.finality == ItemFinality::Open,
                theme,
                width,
            )
        }
        FeedEntryKind::Message(message) => {
            let mut block = paint_assistant(key, &message.text, theme, width);
            if message.phase == MessagePhase::Commentary {
                block = merged(
                    paint_thinking(key, "· commentary", None, theme, width),
                    block,
                );
            }
            if message.finality == ItemFinality::Open {
                block = merged(
                    block,
                    paint_thinking(key, "· streaming…", None, theme, width),
                );
            }
            block
        }
        FeedEntryKind::Reasoning(reasoning) => {
            let mut detail = reasoning
                .summary
                .iter()
                .map(|summary| format!("summary: {summary}"))
                .collect::<Vec<_>>();
            if !reasoning.text.is_empty() {
                detail.push(reasoning.text.clone());
            }
            paint_thinking(
                key,
                if reasoning.finality == ItemFinality::Open {
                    "~ reasoning…"
                } else {
                    "~ reasoning"
                },
                (!detail.is_empty()).then(|| detail.join("\n")).as_deref(),
                theme,
                width,
            )
        }
        FeedEntryKind::Work(work) => work_block(key, work, theme, width),
        FeedEntryKind::McpStartup(startup) => {
            paint_mcp_startup(key, mcp_startup_rows(startup, theme, width), theme, width)
        }
        // One directional glyph, the sender, then the body — in the shape
        // the kernel gives the message's kind, so this chat and every
        // other draw a completion the same way.
        FeedEntryKind::AgentMessage(message) => {
            let glyph = message_glyph(message.kind.presentation(), theme);
            let body = reports.body(message.kind.presentation(), &message.text);
            paint_agent_message(
                key,
                glyph,
                &reports.sender(&message.from),
                &body.text,
                body.affordance.as_deref(),
                theme,
                width,
            )
        }
        FeedEntryKind::Turn(turn) => {
            let status = match &turn.status {
                TurnStatus::Completed => "completed".to_string(),
                TurnStatus::Interrupted => "interrupted".to_string(),
                TurnStatus::Failed { message } => format!("failed · {message}"),
            };
            let mut label = format!("turn {status}");
            if let Some(usage) = &turn.token_usage {
                label.push_str(&format!(" · {}", usage_text(usage)));
            }
            paint_turn_rule(key, &label, theme, width)
        }
        FeedEntryKind::Boundary(boundary) => match boundary {
            BoundaryEntry::Compacted { turn_id } => paint_compaction_rule(
                key,
                &turn_id
                    .as_deref()
                    .map(|id| format!("context compacted · {id}"))
                    .unwrap_or_else(|| "context compacted".to_string()),
                theme,
                width,
            ),
            BoundaryEntry::Resumed => paint_turn_rule(
                key,
                "resumed · earlier history not re-rendered · context intact",
                theme,
                width,
            ),
            BoundaryEntry::Ready => paint_turn_rule(key, "Codex re-synchronized", theme, width),
            BoundaryEntry::Gap { reason } => {
                paint_turn_rule(key, &format!("stream gap · {reason}"), theme, width)
            }
        },
        FeedEntryKind::Error(error) => match error.severity {
            ErrorSeverity::Error => {
                paint_error(key, &error.message, error.will_retry, theme, width)
            }
            severity => {
                let (glyph, style, label) = match severity {
                    ErrorSeverity::Warning => ("⚠", theme.warn(), "warning"),
                    _ => ("·", theme.muted(), "notice"),
                };
                paint_tool_line(
                    key,
                    (glyph, style),
                    &format!("{label} · {}", error.message),
                    error.will_retry.then_some("retrying"),
                    theme,
                    width,
                )
            }
        },
        FeedEntryKind::Unrecognized(row) => paint_unrecognized(
            key,
            "unrecognized Codex row",
            Some(&format!(
                "{}{}",
                row.method,
                row.detail
                    .as_deref()
                    .map(|detail| format!(" · {detail}"))
                    .unwrap_or_default()
            )),
            theme,
            width,
        ),
    }
}

fn mcp_startup_rows(startup: &McpStartupEntry, theme: Theme, width: usize) -> Vec<Line<'static>> {
    let count = |status| {
        startup
            .servers
            .values()
            .filter(|server| server.status == status)
            .count()
    };
    let starting = count(McpStartupStatus::Starting);
    let ready = count(McpStartupStatus::Ready);
    let failed = count(McpStartupStatus::Failed);
    let cancelled = count(McpStartupStatus::Cancelled);
    let mut text = format!("MCP servers · {starting} starting · {ready} ready · {failed} failed");
    if cancelled > 0 {
        text.push_str(&format!(" · {cancelled} cancelled"));
    }
    let (glyph, style) = if failed > 0 {
        ("✗", theme.error())
    } else if starting > 0 {
        ("◌", theme.muted())
    } else if cancelled > 0 {
        ("⚠", theme.warn())
    } else {
        ("✓", theme.ok())
    };
    glyph_text(glyph, &text, width, style, theme.text())
}

/// One unit of Codex work: what it is, how it went, and — for a file
/// change — the patch itself, through the shared diff rows.
fn work_block(key: BlockKey, work: &WorkEntry, theme: Theme, width: usize) -> PaintedBlock {
    let (glyph, glyph_style, state) = work_state(&work.state, theme);

    // A decision already made is history, not a question: a denied or
    // abandoned unit states what was settled instead of wearing a work
    // glyph as though it were still on its way.
    if let WorkState::Denied | WorkState::Abandoned { .. } = &work.state {
        return paint_ask_fact(
            key,
            (glyph, glyph_style),
            &format!("{state} — {}", work_subject(&work.kind)),
            theme,
            width,
        );
    }

    let mut detail: Vec<String> = Vec::new();
    let mut patch: Option<(String, &str, bool)> = None;
    let label = match &work.kind {
        WorkKind::Command {
            command,
            cwd,
            exit_code,
        } => {
            let mut label = format!("$ {command} · {state}");
            if let Some(code) = exit_code {
                label.push_str(&format!(" · exit {code}"));
            }
            if let Some(cwd) = cwd {
                detail.push(format!("cwd {cwd}"));
            }
            label
        }
        WorkKind::FileChange {
            changes,
            patch_head,
            patch_truncated,
        } => {
            let label = format!("file changes · {} · {state}", changes.len());
            for change in changes {
                detail.push(format!(
                    "{}{}",
                    change.path,
                    change
                        .status
                        .as_deref()
                        .map(|status| format!(" · {status}"))
                        .unwrap_or_default()
                ));
            }
            if !patch_head.is_empty() {
                let title = match changes.as_slice() {
                    [change] => change.path.clone(),
                    changes => format!("{} files", changes.len()),
                };
                patch = Some((title, patch_head.as_str(), *patch_truncated));
            }
            label
        }
        WorkKind::Plan {
            text,
            explanation,
            steps,
        } => {
            if let Some(explanation) = explanation {
                detail.push(explanation.clone());
            }
            if !text.is_empty() {
                detail.push(text.clone());
            }
            for step in steps {
                detail.push(format!("[{}] {}", step.status, step.step));
            }
            format!("plan update · {state}")
        }
        WorkKind::McpTool {
            server,
            tool,
            arguments,
            result,
            error,
        } => {
            detail.push(json_text(arguments));
            if let Some(result) = result {
                detail.push(format!("result {}", json_text(result)));
            }
            if let Some(error) = error {
                detail.push(format!("error {}", json_text(error)));
            }
            format!("MCP {server}::{tool} · {state}")
        }
        WorkKind::AmuxTool {
            tool,
            arguments,
            success,
        } => {
            // U4: a send is the outbound half of a conversation — one
            // directional glyph, who it went to, and a summary of what
            // left. The other amux tools keep the generic tool shape:
            // spawning and stopping are work, not talk.
            let label = match (tool.as_str(), send_summary(arguments)) {
                ("send", Some(summary)) => summary,
                _ => {
                    detail.push(json_text(arguments));
                    format!("amux {tool} · {state}")
                }
            };
            if let Some(success) = success {
                detail.push(format!("success {success}"));
            }
            label
        }
        WorkKind::DynamicTool {
            tool,
            namespace,
            arguments,
            success,
        } => {
            let name = namespace
                .as_deref()
                .map(|namespace| format!("{namespace}::{tool}"))
                .unwrap_or_else(|| tool.clone());
            detail.push(json_text(arguments));
            if let Some(success) = success {
                detail.push(format!("success {success}"));
            }
            format!("tool {name} · {state}")
        }
        WorkKind::WebSearch { query, action } => {
            if let Some(action) = action {
                detail.push(json_text(action));
            }
            format!("web search “{query}” · {state}")
        }
        WorkKind::UnsupportedUserInput { questions } => {
            return paint_unrecognized(
                key,
                "user input requested · blocked in structured chat V1",
                Some(&json_text(questions)),
                theme,
                width,
            );
        }
        WorkKind::Other { item_type, raw } => {
            detail.push(json_text(raw));
            format!("Codex item {item_type} · {state}")
        }
    };

    if !work.stdout_head.is_empty() {
        detail.push(format!("stdout: {}", work.stdout_head));
    }
    if !work.stderr_head.is_empty() {
        detail.push(format!("stderr: {}", work.stderr_head));
    }
    if work.output_truncated {
        detail.push("output preview truncated".to_string());
    }

    let mut block = paint_tool_line(
        key,
        (glyph, glyph_style),
        &label,
        (!detail.is_empty()).then(|| detail.join("\n")).as_deref(),
        theme,
        width,
    );
    if let Some((title, patch_head, truncated)) = patch {
        let rows = diff_rows_from_patch(patch_head, truncated);
        if !rows.is_empty() {
            let shown = rows.len().min(PATCH_PREVIEW_ROWS);
            let title = if truncated || rows.len() > shown {
                format!("{title} · patch preview")
            } else {
                title
            };
            block = merged(
                block,
                blocks::paint_unified_diff(key, &title, &rows[..shown], theme, width),
            );
        }
    }
    block
}

/// What a settled decision was about, in as few words as the row can
/// carry: the command, the files, the tool.
fn work_subject(kind: &WorkKind) -> String {
    match kind {
        WorkKind::Command { command, .. } => format!("$ {command}"),
        WorkKind::FileChange { changes, .. } => match changes.as_slice() {
            [change] => change.path.clone(),
            changes => format!("{} file changes", changes.len()),
        },
        WorkKind::Plan { .. } => "plan update".to_string(),
        WorkKind::McpTool { server, tool, .. } => format!("MCP {server}::{tool}"),
        WorkKind::AmuxTool { tool, .. } => format!("amux {tool}"),
        WorkKind::DynamicTool {
            tool, namespace, ..
        } => namespace
            .as_deref()
            .map(|namespace| format!("{namespace}::{tool}"))
            .unwrap_or_else(|| tool.clone()),
        WorkKind::WebSearch { query, .. } => format!("web search “{query}”"),
        WorkKind::UnsupportedUserInput { .. } => "user input request".to_string(),
        WorkKind::Other { item_type, .. } => format!("Codex item {item_type}"),
    }
}

/// `→ name · what left`, from a send call's own arguments. `None` when
/// the call did not name a recipient — an argument shape amux did not
/// write is better shown raw than summarized into a claim.
fn send_summary(arguments: &Value) -> Option<String> {
    let to = arguments.get("to")?.as_str()?;
    match arguments
        .get("text")
        .and_then(Value::as_str)
        .and_then(|text| text.lines().find(|line| !line.trim().is_empty()))
    {
        Some(head) => Some(format!("→ {to} · {}", head.trim())),
        None => Some(format!("→ {to}")),
    }
}

fn work_state(state: &WorkState, theme: Theme) -> (&'static str, Style, String) {
    match state {
        WorkState::Proposed => ("▸", theme.muted(), "proposed".into()),
        WorkState::AwaitingApproval { .. } => ("⚠", theme.warn(), "awaiting approval".into()),
        WorkState::Running => ("▸", theme.text(), "running".into()),
        WorkState::Done { outcome } => match outcome {
            WorkOutcome::Succeeded => ("✔", theme.ok(), "done".into()),
            WorkOutcome::Failed => ("✗", theme.error(), "failed".into()),
            WorkOutcome::Declined => ("✗", theme.error(), "declined".into()),
            WorkOutcome::Unknown => ("·", theme.muted(), "done · unknown outcome".into()),
        },
        WorkState::Denied => ("✗", theme.error(), "denied".into()),
        WorkState::Abandoned { reason } => (
            "✗",
            theme.error(),
            format!("abandoned · {}", resolution_label(*reason)),
        ),
        WorkState::BlockedUnsupported => ("?", theme.warn(), "blocked".into()),
    }
}

fn resolution_label(reason: ApprovalResolution) -> &'static str {
    match reason {
        ApprovalResolution::Answered => "answered",
        ApprovalResolution::AnsweredElsewhere => "answered elsewhere",
        ApprovalResolution::ResponseFailed => "response failed",
        ApprovalResolution::ConnectionLost => "connection lost",
        ApprovalResolution::QueueOverflow => "queue overflow",
        ApprovalResolution::EventStreamError => "event stream error",
        ApprovalResolution::SessionStopped => "session stopped",
        ApprovalResolution::Unknown => "unknown reason",
    }
}

fn prompt_parts(parts: &[PromptPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            PromptPart::Text { text } => text.clone(),
            PromptPart::Image { url } => format!(
                "[image{}]",
                url.as_deref()
                    .map(|url| format!(": {url}"))
                    .unwrap_or_default()
            ),
            PromptPart::LocalImage { path } => format!(
                "[local image{}]",
                path.as_deref()
                    .map(|path| format!(": {path}"))
                    .unwrap_or_default()
            ),
            PromptPart::Other { item_type, raw } => format!(
                "[{}: {}]",
                item_type.as_deref().unwrap_or("input"),
                json_text(raw)
            ),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn usage_text(usage: &TokenUsage) -> String {
    let mut parts = Vec::new();
    if let Some(total) = usage.total_tokens {
        parts.push(format!("{total} tok"));
    }
    if let Some(input) = usage.input_tokens {
        parts.push(format!("{input} in"));
    }
    if let Some(output) = usage.output_tokens {
        parts.push(format!("{output} out"));
    }
    if let Some(reasoning) = usage.reasoning_output_tokens {
        parts.push(format!("{reasoning} reasoning"));
    }
    if let Some(window) = usage.model_context_window {
        parts.push(format!("{window} window"));
    }
    if parts.is_empty() {
        "token usage unavailable".to_string()
    } else {
        parts.join(" · ")
    }
}

fn json_text(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".to_string())
}

fn decision_label(context: &AskContext, value: &Value) -> String {
    match value.as_str() {
        Some("accept") => "accept once".to_string(),
        Some("acceptForSession") => "accept for session".to_string(),
        Some("decline") => "decline".to_string(),
        Some("cancel") => "cancel".to_string(),
        Some(other) => other.to_string(),
        None => object_decision_label(context, value),
    }
}

fn object_decision_label(context: &AskContext, value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return bounded_decision_label(&scalar_detail(value));
    };
    let Some((kind, body)) = object.iter().next() else {
        return "unavailable choice".to_string();
    };
    if object.len() == 1 {
        match (kind.as_str(), context) {
            (
                "acceptWithExecpolicyAmendment",
                AskContext::Command {
                    proposed_execpolicy_amendment: Some(proposed),
                    ..
                },
            ) if wire_execpolicy_amendment(body).as_ref() == Some(proposed) => {
                return "accept and allow similar commands".to_string();
            }
            (
                "applyNetworkPolicyAmendment",
                AskContext::Command {
                    proposed_network_policy_amendments,
                    ..
                },
            ) => {
                if let Some(amendment) = wire_network_policy_amendment(body)
                    && proposed_network_policy_amendments.contains(&amendment)
                {
                    let action = match amendment.action {
                        NetworkPolicyAction::Allow => "allow",
                        NetworkPolicyAction::Deny => "deny",
                    };
                    return bounded_decision_label(&format!(
                        "apply network policy change · {action} {}",
                        sanitize_label_text(&amendment.host)
                    ));
                }
                return bounded_decision_label(&sanitize_label_text(kind));
            }
            ("acceptWithExecpolicyAmendment", _) => {
                return bounded_decision_label(&sanitize_label_text(kind));
            }
            _ => {}
        }
    }

    let kind = bounded_label_segment(&sanitize_label_text(kind), DECISION_KIND_MAX);
    let detail = bounded_label_segment(&scalar_detail(body), DECISION_DETAIL_MAX);
    let label = if detail.is_empty() {
        kind
    } else {
        format!("{kind} · {detail}")
    };
    bounded_decision_label(&label)
}

fn wire_execpolicy_amendment(value: &Value) -> Option<Vec<String>> {
    value
        .get("execpolicy_amendment")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .map(|value| value.map(str::to_owned))
        .collect()
}

fn wire_network_policy_amendment(value: &Value) -> Option<NetworkPolicyAmendment> {
    let amendment = value.get("network_policy_amendment")?;
    let host = amendment.get("host")?.as_str()?.to_string();
    let action = match amendment.get("action")?.as_str()? {
        "allow" => NetworkPolicyAction::Allow,
        "deny" => NetworkPolicyAction::Deny,
        _ => return None,
    };
    Some(NetworkPolicyAmendment { host, action })
}

fn scalar_detail(value: &Value) -> String {
    fn collect(value: &Value, scalars: &mut Vec<String>) {
        match value {
            Value::Null => {}
            Value::Bool(value) => scalars.push(value.to_string()),
            Value::Number(value) => scalars.push(value.to_string()),
            Value::String(value) => {
                let value = sanitize_label_text(value);
                if !value.is_empty() {
                    scalars.push(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    collect(value, scalars);
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    collect(value, scalars);
                }
            }
        }
    }

    let mut scalars = Vec::new();
    collect(value, &mut scalars);
    sanitize_label_text(&scalars.join(" · "))
}

fn sanitize_label_text(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '{' | '}' | '"' => ' ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn bounded_decision_label(label: &str) -> String {
    if str_width(label) <= DECISION_LABEL_MAX {
        return label.to_string();
    }
    format!(
        "{}…",
        clip_to_width(label, DECISION_LABEL_MAX.saturating_sub(1))
    )
}

fn bounded_label_segment(label: &str, max: usize) -> String {
    if str_width(label) <= max {
        return label.to_string();
    }
    format!("{}…", clip_to_width(label, max.saturating_sub(1)))
}

fn text_width(width: usize) -> usize {
    width.saturating_sub(TEXT_COL + 1).max(1)
}

fn glyph_text(
    glyph: &str,
    text: &str,
    width: usize,
    glyph_style: Style,
    text_style: Style,
) -> Vec<Line<'static>> {
    markdown::plain_rows(text, text_width(width), text_style)
        .into_iter()
        .enumerate()
        .map(|(index, spans)| {
            let mut line = Line::default();
            if index == 0 {
                push_span(&mut line, GLYPH_COL, glyph.to_string(), glyph_style);
            }
            pad_to(&mut line, TEXT_COL);
            line.spans.extend(spans);
            line
        })
        .collect()
}

/// The `?` overlay: this chat's full effective key list, fullscreen like
/// the Claude chat's. On short viewports the tail gives way and a `⋮` row
/// states the cut honestly.
fn help_overlay(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let sections = crate::bindings::codex_chat_sections(
        &crate::bindings::Effective::new(chat.kitty, chat.leader),
        crate::chat::family_keys(model, chat.agent),
    );
    let key_col = TEXT_COL
        + 2
        + sections
            .iter()
            .flat_map(|section| &section.bindings)
            .map(|binding| str_width(&binding.keys))
            .max()
            .unwrap_or(0)
        + 3;
    let mut rows: Vec<Line<'static>> = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            rows.push(Line::default());
        }
        let mut title = Line::default();
        push_span(&mut title, GLYPH_COL, section.title, theme.muted());
        rows.push(title);
        for binding in &section.bindings {
            let mut line = Line::default();
            push_span(&mut line, TEXT_COL + 2, binding.keys.clone(), theme.text());
            push_span(&mut line, key_col, binding.action.clone(), theme.muted());
            if let Some(mark) = crate::render::tier_mark(binding.tier) {
                line.spans
                    .push(Span::styled(format!(" · {mark}"), theme.muted()));
            }
            rows.push(line);
        }
    }

    // Fixed chrome is five rows: the title, the gap under it, two rules
    // and the hint. The body consumes every remaining viewport row.
    let body_h = height.saturating_sub(5).max(1);
    if rows.len() > body_h {
        rows.truncate(body_h.saturating_sub(1));
        let mut more = Line::default();
        push_span(
            &mut more,
            GLYPH_COL,
            "⋮ more — a taller terminal shows the full list",
            theme.muted(),
        );
        rows.push(more);
    }
    while rows.len() < body_h {
        rows.push(Line::default());
    }

    let mut title = Line::default();
    push_span(&mut title, GLYPH_COL, "keys", theme.emphasis());
    let hint = if chat.quit_guard.is_armed() {
        let mut line = Line::default();
        push_span(&mut line, TEXT_COL, QuitGuard::HINT, theme.warn());
        line
    } else {
        let mut line = Line::default();
        push_span(&mut line, TEXT_COL, "any key to close", theme.muted());
        line
    };

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    lines.push(title);
    lines.push(Line::default());
    lines.push(rule_row(width, theme));
    lines.extend(rows);
    lines.push(rule_row(width, theme));
    lines.push(hint);
    lines.truncate(height);
    lines
}

/// A dim rule across the whole screen: the overlay's one boundary
/// between a title, a body and the keys that act on it.
fn rule_row(width: usize, theme: Theme) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), theme.muted()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use amux_ui::codex::McpServerStartup;
    use serde_json::json;

    use super::*;

    fn command_context() -> AskContext {
        AskContext::Command {
            item_id: "exec-1".to_string(),
            command: "cargo test".to_string(),
            cwd: Some("/work".to_string()),
            reason: Some("run tests?".to_string()),
            proposed_execpolicy_amendment: Some(vec!["cargo".to_string(), "test".to_string()]),
            proposed_network_policy_amendments: vec![NetworkPolicyAmendment {
                host: "crates.io".to_string(),
                action: NetworkPolicyAction::Allow,
            }],
        }
    }

    #[test]
    fn object_decision_labels_use_typed_proposals_and_bound_unknown_scalars() {
        let context = command_context();
        assert_eq!(
            decision_label(
                &context,
                &json!({"acceptWithExecpolicyAmendment":{
                    "execpolicy_amendment":["cargo","test"]
                }})
            ),
            "accept and allow similar commands"
        );
        assert_eq!(
            decision_label(
                &context,
                &json!({"applyNetworkPolicyAmendment":{
                    "network_policy_amendment":{"host":"crates.io","action":"allow"}
                }})
            ),
            "apply network policy change · allow crates.io"
        );
        assert_eq!(
            decision_label(
                &context,
                &json!({"acceptWithExecpolicyAmendment":{
                    "execpolicy_amendment":["mismatched"]
                }})
            ),
            "acceptWithExecpolicyAmendment",
            "a known wire kind is not trusted without its typed proposal"
        );

        let fallback = decision_label(
            &context,
            &json!({"future{Policy}":{"nested":{
                "detail":"deploy {quoted} \"value\" with a deliberately very long scalar explanation"
            },"attempt":7}}),
        );
        assert!(fallback.starts_with("future Policy · "));
        assert!(fallback.contains("deploy quoted value"));
        assert!(fallback.ends_with('…'));
        assert!(str_width(&fallback) <= DECISION_LABEL_MAX);
        assert!(
            !fallback
                .chars()
                .any(|character| matches!(character, '{' | '}' | '"'))
        );
    }

    #[test]
    fn cancelled_mcp_startup_never_renders_as_success() {
        let server = |status| McpServerStartup {
            status,
            error: None,
            failure_reason: None,
        };
        for (name, servers) in [
            (
                "cancelled only",
                BTreeMap::from([("legacy".to_string(), server(McpStartupStatus::Cancelled))]),
            ),
            (
                "ready and cancelled",
                BTreeMap::from([
                    ("ready".to_string(), server(McpStartupStatus::Ready)),
                    ("legacy".to_string(), server(McpStartupStatus::Cancelled)),
                ]),
            ),
        ] {
            let lines = mcp_startup_rows(&McpStartupEntry { servers }, Theme::default(), 88);
            let rendered = lines[0]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            assert!(rendered.contains('⚠'), "{name}: {rendered}");
            assert!(!rendered.contains('✓'), "{name}: {rendered}");
        }
    }
}
