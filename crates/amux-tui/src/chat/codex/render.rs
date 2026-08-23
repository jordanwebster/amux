//! Codex-native frame formatting. Phase and attention-like presentation come
//! only from `amux_ui::codex::phase`; feed blocks format the layer's typed
//! entries without reconstructing a second semantic model.

use amux_ui::codex::{
    ApprovalResolution, Ask, AskContext, BoundaryEntry, CodexPhase, ErrorSeverity, FeedEntry,
    FeedEntryKind, ItemFinality, McpStartupEntry, McpStartupStatus, MessagePhase,
    NetworkPolicyAction, NetworkPolicyAmendment, PromptPart, PromptSource, TokenUsage, TurnStatus,
    WorkEntry, WorkKind, WorkOutcome, WorkState,
};
use amux_ui::{AgentId, AgentMessagePresentation, Model};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::View;
use crate::chat::layout::{ChatLayout, bottom_max_rows};
use crate::chat::{FeedScroll, entry_watermark};
use crate::markdown;
use crate::render::{
    FrameContext, Theme, blank_line, clip_to_width, finish_line, line_len, new_line, pad_to,
    push_right, push_span, str_width,
};
use crate::view::QuitGuard;

const GLYPH_COL: usize = 2;
const TEXT_COL: usize = 4;
const CONT_COL: usize = 6;
const MIN_WIDTH: usize = 24;
const MIN_HEIGHT: usize = 10;
const DECISION_LABEL_MAX: usize = 52;
const DECISION_KIND_MAX: usize = 22;
const DECISION_DETAIL_MAX: usize = DECISION_LABEL_MAX - DECISION_KIND_MAX - 3;
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub(crate) fn layout(model: &Model, chat: &View, viewport: (u16, u16)) -> ChatLayout {
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    let working = active_phase(&amux_ui::codex::phase(model, chat.agent));
    let paused = matches!(chat.scroll, FeedScroll::Paused { .. });
    ChatLayout {
        height,
        bottom_rows: bottom_lines(
            model,
            chat,
            Theme::default(),
            width,
            height,
            working,
            paused,
        )
        .len(),
        working,
        paused,
    }
}

pub(crate) fn build_chat_lines(
    model: &Model,
    chat: &View,
    ctx: &FrameContext,
) -> Vec<Line<'static>> {
    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        return vec![Line::from("amux: terminal too small")];
    }
    let theme = ctx.theme;
    if chat.help {
        return help_frame(chat, theme, width, height);
    }

    let phase = amux_ui::codex::phase(model, chat.agent);
    let working = active_phase(&phase);
    let paused = matches!(chat.scroll, FeedScroll::Paused { .. });
    let bottom = bottom_lines(model, chat, theme, width, height, working, paused);
    let layout = ChatLayout {
        height,
        bottom_rows: bottom.len(),
        working,
        paused,
    };
    let feed_h = layout.feed_height();

    let mut lines = Vec::with_capacity(height);
    lines.push(top_border(width, theme));
    lines.push(header_line(model, chat, &phase, width, theme));

    let loading = matches!(phase, CodexPhase::Replaying);
    let (window, at_top) = if loading {
        (loading_band(theme, width, feed_h), false)
    } else {
        let feed = feed_lines(model, chat.agent, theme, width);
        let total = feed.len();
        let max_top = total.saturating_sub(feed_h);
        let (start, at_top) = match chat.scroll {
            FeedScroll::Following => (max_top, total <= feed_h),
            FeedScroll::Paused { top_line, .. } => {
                let top = top_line.min(max_top);
                (top, top == 0)
            }
        };
        let mut window: Vec<_> = feed.into_iter().skip(start).take(feed_h).collect();
        while window.len() < feed_h {
            window.push(blank_line(width));
        }
        (window, at_top)
    };
    let truncated = model
        .codex(chat.agent)
        .is_some_and(|layer| layer.history_truncated());
    lines.push(if truncated && at_top {
        chrome_rule(width, theme, "─ earlier Codex history unavailable ")
    } else if let Some(label) = &chat.configuration_label {
        chrome_rule(width, theme, &format!("─ {label} "))
    } else {
        chrome_rule(width, theme, "")
    });
    lines.extend(window);

    match chat.scroll {
        FeedScroll::Paused {
            entry_watermark: mark,
            top_line,
        } if !loading => lines.push(paused_rule(
            model, chat, width, feed_h, mark, top_line, theme,
        )),
        FeedScroll::Paused { .. } => lines.push(blank_line(width)),
        FeedScroll::Following if working => lines.push(blank_line(width)),
        FeedScroll::Following => {}
    }
    if working {
        lines.push(working_line(model, chat, &phase, ctx, width));
    }
    lines.extend(bottom);
    lines.push(crate::render::bottom_border(width));
    lines.truncate(height);
    lines
}

fn active_phase(phase: &CodexPhase) -> bool {
    matches!(
        phase,
        CodexPhase::Thinking | CodexPhase::Responding { .. } | CodexPhase::Executing { .. }
    )
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
    chat: &View,
    phase: &CodexPhase,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = new_line();
    if let Some(card) = model.agent(chat.agent) {
        let host = model.host_name(card.agent.host_id).unwrap_or("?");
        push_span(&mut line, GLYPH_COL, card.display_name(), theme.text());
        line.spans.push(Span::styled(
            format!(" · {} @ {host}", card.agent.agent_type),
            theme.muted(),
        ));
    }
    let (mut word, style) = phase_word(phase, theme);
    let readonly = chat.read_only(model);
    if readonly && matches!(phase, CodexPhase::AwaitingApproval { .. }) {
        word = "needs owner".to_string();
    }
    let left = if readonly {
        "chat · read-only · "
    } else {
        "chat · "
    };
    let col = width
        .saturating_sub(2 + str_width(left) + str_width(&word))
        .max(line_len(&line) + 1);
    push_span(&mut line, col, left, theme.muted());
    line.spans.push(Span::styled(word, style));
    finish_line(&mut line, width);
    line
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

fn chrome_rule(width: usize, theme: Theme, title: &str) -> Line<'static> {
    let mut line = new_line();
    let mut text = title.to_string();
    while 1 + str_width(&text) < width - 1 {
        text.push('─');
    }
    line.spans.push(Span::styled(text, theme.muted()));
    finish_line(&mut line, width);
    line
}

fn loading_band(theme: Theme, width: usize, height: usize) -> Vec<Line<'static>> {
    (0..height)
        .map(|row| {
            if row == height.saturating_sub(1) / 2 {
                let text = "⟳ loading Codex session…";
                let mut line = new_line();
                push_span(
                    &mut line,
                    (width.saturating_sub(str_width(text)) / 2).max(GLYPH_COL),
                    text,
                    theme.muted(),
                );
                finish_line(&mut line, width);
                line
            } else {
                blank_line(width)
            }
        })
        .collect()
}

fn working_line(
    model: &Model,
    chat: &View,
    phase: &CodexPhase,
    ctx: &FrameContext,
    width: usize,
) -> Line<'static> {
    let label = match phase {
        CodexPhase::Responding { .. } => "responding",
        CodexPhase::Executing { .. } => "executing",
        _ => "thinking",
    };
    let spinner = SPINNER[ctx.now.timestamp().unsigned_abs() as usize % SPINNER.len()];
    let mut line = new_line();
    push_span(
        &mut line,
        GLYPH_COL,
        format!("{spinner} {label}"),
        ctx.theme.text(),
    );
    let mut hints = Vec::new();
    if amux_ui::codex::allows_steer(model, chat.agent) {
        hints.push("enter steer");
    }
    if amux_ui::codex::allows_interrupt(model, chat.agent) {
        hints.push("ctrl+x interrupt");
    }
    if !hints.is_empty() {
        line.spans.push(Span::styled(
            format!(" · {}", hints.join(" · ")),
            ctx.theme.muted(),
        ));
    }
    finish_line(&mut line, width);
    line
}

fn paused_rule(
    model: &Model,
    chat: &View,
    width: usize,
    feed_h: usize,
    watermark: u64,
    top_line: usize,
    theme: Theme,
) -> Line<'static> {
    let new_entries = entry_watermark(model, chat.agent).saturating_sub(watermark);
    let mut left = String::from("─ ↓ following paused");
    if new_entries > 0 {
        left.push_str(&format!(" · {new_entries} new"));
    }
    left.push_str(" · pgdn resume ");
    let total = feed_line_count(model, chat.agent, width);
    let max_top = total.saturating_sub(feed_h);
    let percent = top_line
        .min(max_top)
        .saturating_mul(100)
        .checked_div(max_top)
        .unwrap_or(100);
    let right = format!(" {percent}% ────");
    let fill = width
        .saturating_sub(2)
        .saturating_sub(str_width(&left) + str_width(&right));
    left.extend(std::iter::repeat_n('─', fill));
    left.push_str(&right);
    let mut line = new_line();
    line.spans.push(Span::styled(left, theme.muted()));
    finish_line(&mut line, width);
    line
}

fn bottom_lines(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    height: usize,
    working: bool,
    paused: bool,
) -> Vec<Line<'static>> {
    let max_rows = bottom_max_rows(height, working, paused);
    let mut lines = if chat.read_only(model) {
        readonly_bottom(theme)
    } else if let Some(ask) = model.codex(chat.agent).and_then(|layer| layer.ask_head()) {
        approval_panel(model, chat, ask, width, theme)
    } else if matches!(
        amux_ui::codex::phase(model, chat.agent),
        CodexPhase::BlockedUnsupported { .. }
    ) {
        unsupported_panel(chat, width, theme)
    } else {
        return composer_bottom(model, chat, width, theme, max_rows);
    };
    if chat.quit_guard.is_armed()
        && let Some(last) = lines.last_mut()
    {
        *last = armed_quit_line(theme);
    }
    for line in &mut lines {
        finish_line(line, width);
    }
    if lines.len() > max_rows {
        lines.drain(..lines.len() - max_rows);
    }
    lines
}

fn approval_panel(
    model: &Model,
    chat: &View,
    ask: &Ask,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let count = model
        .codex(chat.agent)
        .map(|layer| layer.ask_count())
        .unwrap_or(1);
    let mut lines = vec![panel_rule(width, theme)];
    let mut header = new_line();
    push_span(&mut header, GLYPH_COL, "⚠", theme.warn());
    push_span(
        &mut header,
        TEXT_COL,
        approval_title(&ask.context),
        theme.text(),
    );
    if count > 1 {
        push_right(&mut header, format!("(1 of {count})"), width, theme.muted());
    }
    lines.push(header);
    lines.extend(context_lines(&ask.context, width, theme));
    if let Some(message) = &chat.answer_failure {
        lines.extend(glyph_text("✗", message, width, theme.error(), theme.text()));
    }
    lines.push(new_line());
    let answer_in_flight = model.codex(chat.agent).is_some_and(|layer| {
        layer.in_flight_inputs().any(|input| {
            matches!(&input.kind, amux_ui::codex::InFlightKind::Answer { request_id, .. }
                if *request_id == ask.request_id)
        })
    });
    if answer_in_flight {
        lines.extend(glyph_text(
            "◌",
            "sending decision…",
            width,
            theme.muted(),
            theme.muted(),
        ));
    } else {
        let allows_answer = amux_ui::codex::allows_answer(model, chat.agent);
        for (index, action) in ask.actions.iter().enumerate() {
            let mut line = new_line();
            let supported = action.decision.is_some();
            let selectable = allows_answer && supported;
            if selectable && index == chat.approval_cursor {
                push_span(&mut line, GLYPH_COL, "›", theme.text());
            }
            let style = if selectable {
                theme.text()
            } else {
                theme.muted()
            };
            push_span(&mut line, TEXT_COL, format!("{}.", index + 1), style);
            push_span(
                &mut line,
                TEXT_COL + 3,
                decision_label(&ask.context, &action.wire),
                style,
            );
            if !supported {
                line.spans
                    .push(Span::styled(" · unavailable in V1", theme.muted()));
            }
            lines.push(line);
        }
        if allows_answer {
            lines.push(new_line());
            let mut hint = new_line();
            push_span(
                &mut hint,
                TEXT_COL,
                "↑↓/1-9 select · enter confirm · ctrl+x interrupt",
                theme.muted(),
            );
            lines.push(hint);
        }
    }
    lines
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

fn context_lines(context: &AskContext, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match context {
        AskContext::Command {
            command,
            cwd,
            reason,
            ..
        } => {
            lines.extend(glyph_text("$", command, width, theme.muted(), theme.code()));
            if let Some(cwd) = cwd {
                lines.extend(continuation(&format!("cwd {cwd}"), width, theme));
            }
            if let Some(reason) = reason {
                lines.extend(continuation(reason, width, theme));
            }
        }
        AskContext::FileChange {
            reason, changes, ..
        } => {
            for change in changes {
                lines.extend(glyph_text(
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
                lines.extend(continuation(reason, width, theme));
            }
        }
        AskContext::Permissions {
            reason,
            permissions,
            ..
        } => {
            lines.extend(glyph_text(
                "▸",
                &json_text(permissions),
                width,
                theme.text(),
                theme.code(),
            ));
            if let Some(reason) = reason {
                lines.extend(continuation(reason, width, theme));
            }
        }
        AskContext::DynamicTool {
            tool,
            namespace,
            arguments,
            ..
        } => lines.extend(glyph_text(
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

fn unsupported_panel(chat: &View, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = vec![panel_rule(width, theme)];
    lines.extend(glyph_text(
        "?",
        "Codex requested user input that structured chat V1 cannot answer.",
        width,
        theme.warn(),
        theme.text(),
    ));
    lines.extend(continuation(
        "This turn is blocked — it is not idle.",
        width,
        theme,
    ));
    lines.push(new_line());
    let mut hint = new_line();
    push_span(
        &mut hint,
        TEXT_COL,
        format!("ctrl+x interrupt · C-{} s then open raw mode", chat.leader),
        theme.muted(),
    );
    lines.push(hint);
    lines
}

fn readonly_bottom(theme: Theme) -> Vec<Line<'static>> {
    let mut marker = new_line();
    push_span(
        &mut marker,
        GLYPH_COL,
        "⊘ read-only — you are observing this Codex session",
        theme.muted(),
    );
    let mut hint = new_line();
    push_span(
        &mut hint,
        TEXT_COL,
        "pgup/pgdn scroll · q back to fleet",
        theme.muted(),
    );
    vec![new_line(), marker, new_line(), hint]
}

fn composer_bottom(
    model: &Model,
    chat: &View,
    width: usize,
    theme: Theme,
    max_rows: usize,
) -> Vec<Line<'static>> {
    let budget = max_rows.saturating_sub(3).max(1);
    let (rows, cursor_row) = chat.composer.display_rows(text_width(width));
    let visible = rows.len().clamp(1, budget.min(6));
    let start = if rows.len() <= visible {
        0
    } else {
        (cursor_row + 1)
            .saturating_sub(visible)
            .min(rows.len() - visible)
    };
    let mut lines = vec![blank_line(width)];
    if chat.composer.is_empty() {
        let mut line = new_line();
        push_span(&mut line, GLYPH_COL, "›", theme.text());
        push_span(&mut line, TEXT_COL, "Type a message", theme.muted());
        line.spans.push(Span::styled("▌", theme.text()));
        finish_line(&mut line, width);
        lines.push(line);
    } else {
        for (index, row) in rows.iter().enumerate().skip(start).take(visible) {
            let mut line = new_line();
            if index == 0 {
                push_span(&mut line, GLYPH_COL, "›", theme.text());
            }
            push_span(&mut line, TEXT_COL, row.clone(), theme.text());
            finish_line(&mut line, width);
            lines.push(line);
        }
    }
    lines.push(blank_line(width));
    lines.push(footer_line(model, chat, width, theme));
    lines
}

fn footer_line(model: &Model, chat: &View, width: usize, theme: Theme) -> Line<'static> {
    let mut line = if chat.quit_guard.is_armed() {
        armed_quit_line(theme)
    } else {
        new_line()
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
        } else if matches!(chat.scroll, FeedScroll::Paused { .. }) {
            push_span(
                &mut line,
                TEXT_COL,
                "pgup/pgdn scroll · esc newest",
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
    finish_line(&mut line, width);
    line
}

fn armed_quit_line(theme: Theme) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, TEXT_COL, QuitGuard::HINT, theme.warn());
    line
}

fn panel_rule(width: usize, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    line.spans.push(Span::styled(
        "─".repeat(width.saturating_sub(2)),
        theme.muted(),
    ));
    line
}

pub(crate) fn feed_line_count(model: &Model, agent: AgentId, width: usize) -> usize {
    feed_lines(model, agent, Theme::default(), width).len()
}

pub(crate) fn feed_lines(
    model: &Model,
    agent: AgentId,
    theme: Theme,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(layer) = model.codex(agent) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for (index, entry) in layer.entries().enumerate() {
        if index > 0 {
            lines.push(new_line());
        }
        lines.extend(entry_lines(entry, width, theme));
    }
    for line in &mut lines {
        finish_line(line, width);
    }
    lines
}

fn entry_lines(entry: &FeedEntry, width: usize, theme: Theme) -> Vec<Line<'static>> {
    match &entry.kind {
        FeedEntryKind::Prompt(prompt) => {
            let (glyph, prefix, style) = match prompt.source {
                PromptSource::Protocol => ("›", "", theme.text()),
                PromptSource::SteerEcho => ("↪", "steer · ", theme.muted()),
            };
            let mut text = prefix.to_string();
            text.push_str(&prompt_parts(&prompt.parts));
            if prompt.finality == ItemFinality::Open {
                text.push_str(" …");
            }
            glyph_text(glyph, &text, width, style, theme.text())
        }
        FeedEntryKind::Message(message) => {
            let mut lines = Vec::new();
            if message.phase == MessagePhase::Commentary {
                lines.push(marker_line("· commentary", theme));
            }
            lines.extend(markdown_block(&message.text, width, theme));
            if message.finality == ItemFinality::Open {
                lines.push(marker_line("· streaming…", theme));
            }
            lines
        }
        FeedEntryKind::Reasoning(reasoning) => {
            let mut lines = vec![marker_line(
                if reasoning.finality == ItemFinality::Open {
                    "~ reasoning…"
                } else {
                    "~ reasoning"
                },
                theme,
            )];
            for summary in &reasoning.summary {
                lines.extend(continuation(&format!("summary: {summary}"), width, theme));
            }
            if !reasoning.text.is_empty() {
                lines.extend(continuation(&reasoning.text, width, theme));
            }
            lines
        }
        FeedEntryKind::Work(work) => work_lines(work, width, theme),
        FeedEntryKind::McpStartup(startup) => mcp_startup_lines(startup, width, theme),
        // One directional glyph, the sender, then the body — in the shape
        // the kernel gives the message's kind, so this chat and every
        // other draw a completion the same way.
        FeedEntryKind::AgentMessage(message) => {
            let presentation = message.kind.presentation();
            let (glyph, glyph_style) = match presentation {
                AgentMessagePresentation::Finished => ("✔", theme.ok()),
                AgentMessagePresentation::Notice => ("·", theme.muted()),
                AgentMessagePresentation::Inbound => ("←", theme.emphasis()),
            };
            let mut lines = glyph_text(glyph, &message.from, width, glyph_style, theme.muted());
            let body = match presentation {
                // A notice has no body to open, so its one line is
                // whatever the envelope managed to say — usually nothing.
                AgentMessagePresentation::Notice => amux_ui::message_digest(&message.text).head,
                _ => message.text.as_str(),
            };
            if !body.is_empty() {
                lines.extend(continuation(body, width, theme));
            }
            lines
        }
        FeedEntryKind::Turn(turn) => {
            let status = match &turn.status {
                TurnStatus::Completed => "completed".to_string(),
                TurnStatus::Interrupted => "interrupted".to_string(),
                TurnStatus::Failed { message } => format!("failed · {message}"),
            };
            let mut title = format!("─ turn {status}");
            if let Some(usage) = &turn.token_usage {
                title.push_str(&format!(" · {}", usage_text(usage)));
            }
            title.push(' ');
            vec![feed_rule(&title, width, theme)]
        }
        FeedEntryKind::Boundary(boundary) => {
            let title = match boundary {
                BoundaryEntry::Resumed => {
                    "─ resumed · earlier history not re-rendered · context intact ".to_string()
                }
                BoundaryEntry::Ready => "─ Codex re-synchronized ".to_string(),
                BoundaryEntry::Gap { reason } => format!("─ stream gap · {reason} "),
                BoundaryEntry::Compacted { turn_id } => turn_id
                    .as_deref()
                    .map(|id| format!("─ context compacted · {id} "))
                    .unwrap_or_else(|| "─ context compacted ".to_string()),
            };
            vec![feed_rule(&title, width, theme)]
        }
        FeedEntryKind::Error(error) => {
            let (glyph, style, label) = match error.severity {
                ErrorSeverity::Notice => ("·", theme.muted(), "notice"),
                ErrorSeverity::Warning => ("⚠", theme.warn(), "warning"),
                ErrorSeverity::Error => ("✗", theme.error(), "error"),
            };
            let mut text = format!("{label} · {}", error.message);
            if error.will_retry {
                text.push_str(" · retrying");
            }
            glyph_text(glyph, &text, width, style, theme.text())
        }
        FeedEntryKind::Unrecognized(row) => vec![marker_line(
            &format!(
                "· unrecognized Codex row · {}{}",
                row.method,
                row.detail
                    .as_deref()
                    .map(|detail| format!(" · {detail}"))
                    .unwrap_or_default()
            ),
            theme,
        )],
    }
}

fn mcp_startup_lines(startup: &McpStartupEntry, width: usize, theme: Theme) -> Vec<Line<'static>> {
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

fn work_lines(work: &WorkEntry, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (glyph, glyph_style, state) = work_state(&work.state, theme);
    let mut lines = match &work.kind {
        WorkKind::Command {
            command,
            cwd,
            exit_code,
        } => {
            let mut title = format!("$ {command} · {state}");
            if let Some(code) = exit_code {
                title.push_str(&format!(" · exit {code}"));
            }
            let mut lines = glyph_text(glyph, &title, width, glyph_style, theme.code());
            if let Some(cwd) = cwd {
                lines.extend(continuation(&format!("cwd {cwd}"), width, theme));
            }
            lines
        }
        WorkKind::FileChange {
            changes,
            patch_head,
            patch_truncated,
        } => {
            let mut lines = glyph_text(
                glyph,
                &format!("file changes · {} · {state}", changes.len()),
                width,
                glyph_style,
                theme.text(),
            );
            for change in changes {
                lines.extend(continuation(
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
                    theme,
                ));
            }
            if !patch_head.is_empty() {
                lines.extend(code_continuation(patch_head, width, theme));
            }
            if *patch_truncated {
                lines.extend(continuation("patch preview truncated", width, theme));
            }
            lines
        }
        WorkKind::Plan {
            text,
            explanation,
            steps,
        } => {
            let mut lines = glyph_text(
                glyph,
                &format!("plan update · {state}"),
                width,
                glyph_style,
                theme.text(),
            );
            if let Some(explanation) = explanation {
                lines.extend(continuation(explanation, width, theme));
            }
            if !text.is_empty() {
                lines.extend(continuation(text, width, theme));
            }
            for step in steps {
                lines.extend(continuation(
                    &format!("[{}] {}", step.status, step.step),
                    width,
                    theme,
                ));
            }
            lines
        }
        WorkKind::McpTool {
            server,
            tool,
            arguments,
            result,
            error,
        } => {
            let mut lines = glyph_text(
                glyph,
                &format!("MCP {server}::{tool} · {state}"),
                width,
                glyph_style,
                theme.text(),
            );
            lines.extend(continuation(&json_text(arguments), width, theme));
            if let Some(result) = result {
                lines.extend(continuation(
                    &format!("result {}", json_text(result)),
                    width,
                    theme,
                ));
            }
            if let Some(error) = error {
                lines.extend(continuation(
                    &format!("error {}", json_text(error)),
                    width,
                    theme,
                ));
            }
            lines
        }
        WorkKind::AmuxTool {
            tool,
            arguments,
            success,
        } => {
            let mut lines = glyph_text(
                glyph,
                &format!("amux {tool} · {state}"),
                width,
                glyph_style,
                theme.text(),
            );
            lines.extend(continuation(&json_text(arguments), width, theme));
            if let Some(success) = success {
                lines.extend(continuation(&format!("success {success}"), width, theme));
            }
            lines
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
            let mut lines = glyph_text(
                glyph,
                &format!("tool {name} · {state}"),
                width,
                glyph_style,
                theme.text(),
            );
            lines.extend(continuation(&json_text(arguments), width, theme));
            if let Some(success) = success {
                lines.extend(continuation(&format!("success {success}"), width, theme));
            }
            lines
        }
        WorkKind::WebSearch { query, action } => {
            let mut lines = glyph_text(
                glyph,
                &format!("web search “{query}” · {state}"),
                width,
                glyph_style,
                theme.text(),
            );
            if let Some(action) = action {
                lines.extend(continuation(&json_text(action), width, theme));
            }
            lines
        }
        WorkKind::UnsupportedUserInput { questions } => {
            let mut lines = glyph_text(
                "?",
                "user input requested · blocked in structured chat V1",
                width,
                theme.warn(),
                theme.text(),
            );
            lines.extend(continuation(&json_text(questions), width, theme));
            lines
        }
        WorkKind::Other { item_type, raw } => {
            let mut lines = glyph_text(
                glyph,
                &format!("Codex item {item_type} · {state}"),
                width,
                glyph_style,
                theme.text(),
            );
            lines.extend(continuation(&json_text(raw), width, theme));
            lines
        }
    };
    if !work.stdout_head.is_empty() {
        lines.extend(continuation(
            &format!("stdout: {}", work.stdout_head),
            width,
            theme,
        ));
    }
    if !work.stderr_head.is_empty() {
        lines.extend(continuation(
            &format!("stderr: {}", work.stderr_head),
            width,
            theme,
        ));
    }
    if work.output_truncated {
        lines.extend(continuation("output preview truncated", width, theme));
    }
    lines
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

fn cont_width(width: usize) -> usize {
    width.saturating_sub(CONT_COL + 1).max(1)
}

fn markdown_block(source: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
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

fn continuation(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
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

fn code_continuation(text: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    text.lines()
        .flat_map(|row| {
            markdown::plain_rows(row, cont_width(width), theme.code())
                .into_iter()
                .map(|spans| {
                    let mut line = new_line();
                    pad_to(&mut line, CONT_COL);
                    line.spans.extend(spans);
                    line
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn marker_line(text: &str, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, text.to_string(), theme.muted());
    line
}

fn feed_rule(title: &str, width: usize, theme: Theme) -> Line<'static> {
    let mut text = title.to_string();
    while GLYPH_COL + str_width(&text) < width - 1 {
        text.push('─');
    }
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, text, theme.muted());
    line
}

fn help_frame(chat: &View, theme: Theme, width: usize, height: usize) -> Vec<Line<'static>> {
    let sections = crate::bindings::codex_chat_sections(&crate::bindings::Effective::new(
        chat.kitty,
        chat.leader,
    ));
    let key_col = TEXT_COL
        + 2
        + sections
            .iter()
            .flat_map(|section| &section.bindings)
            .map(|binding| str_width(&binding.keys))
            .max()
            .unwrap_or(0)
        + 3;
    let mut rows = Vec::new();
    for (section_index, section) in sections.iter().enumerate() {
        if section_index > 0 {
            rows.push(new_line());
        }
        let mut title = new_line();
        push_span(&mut title, GLYPH_COL, section.title, theme.muted());
        rows.push(title);
        for binding in &section.bindings {
            let mut line = new_line();
            push_span(&mut line, TEXT_COL + 2, binding.keys.clone(), theme.text());
            push_span(&mut line, key_col, binding.action.clone(), theme.muted());
            if let Some(mark) = crate::render::tier_mark(binding.tier) {
                line.spans
                    .push(Span::styled(format!(" · {mark}"), theme.muted()));
            }
            rows.push(line);
        }
    }
    let body_h = height.saturating_sub(6).max(1);
    if rows.len() > body_h {
        rows.truncate(body_h.saturating_sub(1));
        let mut more = new_line();
        push_span(&mut more, GLYPH_COL, "⋮ more", theme.muted());
        rows.push(more);
    }
    while rows.len() < body_h {
        rows.push(new_line());
    }
    let mut title = new_line();
    push_span(&mut title, GLYPH_COL, "keys · codex", theme.text());
    let mut hint = new_line();
    push_span(&mut hint, TEXT_COL, "any key to close", theme.muted());
    let mut lines = vec![
        top_border(width, theme),
        title,
        chrome_rule(width, theme, ""),
    ];
    lines.extend(rows);
    lines.push(chrome_rule(width, theme, ""));
    lines.push(if chat.quit_guard.is_armed() {
        armed_quit_line(theme)
    } else {
        hint
    });
    for line in lines.iter_mut().skip(1) {
        finish_line(line, width);
    }
    lines.truncate(height.saturating_sub(1));
    lines.push(crate::render::bottom_border(width));
    lines
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
            let lines = mcp_startup_lines(&McpStartupEntry { servers }, 88, Theme::Dark);
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
