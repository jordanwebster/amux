//! This chat's adapter onto the shared frame: it walks the session's own
//! feed kinds, formats their words, and hands the shell finished blocks.
//!
//! Nothing here draws. Every row comes from the painter kit in
//! `chat::blocks`, so the three chats cannot drift apart: this file
//! decides what a block *says* and the kit decides how it is painted.
//! Every fact rendered here comes from the Model; the code below formats
//! and never recovers meaning the fold did not keep.

use amux_ui::Model;
use amux_ui::attachments::Segment;
use amux_ui::claude::ToolInvocation;
use amux_ui::claude_sdk::{
    BoundaryEntry, FeedEntry, FeedEntryKind, Finality, SdkPhase, TaskEntry, TaskState, ToolEntry,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::chat::attachments::{attachment_key, described, echo_owner, prose};
use crate::chat::blocks::{
    self, paint_agent_message, paint_assistant, paint_attachment, paint_compaction_rule,
    paint_composer_block, paint_header, paint_plan, paint_thinking, paint_tool_line,
    paint_turn_rule, paint_unrecognized, paint_user_prompt,
};
use crate::chat::claude_sdk::{View, is_open, reader_context};
use crate::chat::claude_shared::{armed_quit_line, reader};
use crate::chat::frame::{BlockKey, ChatFrameParts, FeedBlocks, PaintCache, PaintedBlock};
use crate::chat::viewport::FeedViewport;
use crate::chat::{FeedScroll, MessageView, family_banner, message_glyph, subagent_marker};
use crate::render::{FrameContext, Theme, line_len, push_span};

/// One 1 Hz Tick drives the spinner; the frame index derives from the
/// clock, so no renderer state has to be kept for it.
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// The plan entry's feed preview length.
const PLAN_PREVIEW_LINES: usize = 6;

/// Synthetic keys for the block no entry owns. The optimistic echo counts
/// down from the top of the space so it can never collide with an entry
/// id, which counts up from zero.
const ECHO_KEY_BASE: u64 = u64::MAX;

// --- the frame --------------------------------------------------------------

/// Everything the shared shell needs to draw this chat: one header, the
/// feed as painted blocks, the liveness row, the bottom block, and the
/// overlay that replaces all of it when a reader, the review page or the
/// key list is open.
pub(crate) fn claude_sdk_frame_parts(
    model: &Model,
    chat: &View,
    viewport: &FeedViewport,
    cache: &mut PaintCache,
    ctx: &FrameContext,
) -> ChatFrameParts {
    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    let theme = ctx.theme;
    let readonly = chat.read_only(model);
    let phase = amux_ui::claude_sdk::phase(model, chat.agent);
    let banner = family_banner(model, chat.agent).map(|banner| {
        family_banner_line(
            &banner.row(banner_answerable(model, chat, &banner), chat.leader),
            theme,
        )
    });

    let overlay = if chat.help {
        Some(crate::chat::claude_shared::help_overlay(
            crate::bindings::chat_sections(
                &effective(chat),
                crate::chat::family_keys(model, chat.agent),
            ),
            chat.quit_guard.is_armed(),
            theme,
            width,
            height,
        ))
    } else if let Some(draft) = chat.review.as_ref().filter(|draft| draft.open) {
        Some(draft.view.frame(theme, ctx.viewport.0, ctx.viewport.1))
    } else if chat.reader.is_some() {
        reader_context(model, chat).and_then(|ctx| reader::reader_frame(&ctx, theme, width, height))
    } else {
        None
    };

    let paused = matches!(viewport.scroll, FeedScroll::Paused { .. });
    let working = matches!(phase, SdkPhase::Working);
    let loading = matches!(phase, SdkPhase::Replaying);

    ChatFrameParts {
        header: header_row(model, chat, theme, phase, width, readonly),
        banner,
        feed: FeedBlocks {
            blocks: if loading {
                Vec::new()
            } else {
                feed_blocks(model, chat, cache, theme, width)
            },
            history_truncated: model
                .claude_sdk(chat.agent)
                .is_some_and(|layer| layer.history_truncated()),
            loading,
        },
        activity: working.then(|| working_row(chat, ctx, readonly)),
        bottom: bottom_block(model, chat, theme, width, height, paused),
        overlay,
    }
}

// --- the header and the rows around the feed --------------------------------

/// `name · claude @ host` on the left; the model this session is running
/// and the permission mode it is running under on the right, because both
/// change under the person's hands and both change what the next turn
/// will do.
fn header_row(
    model: &Model,
    chat: &View,
    theme: Theme,
    phase: SdkPhase,
    width: usize,
    readonly: bool,
) -> Line<'static> {
    let name = match model.agent(chat.agent) {
        Some(card) => format!(
            "{} · {} @ {}{}",
            card.display_name(),
            card.agent.kind.provider(),
            model.host_name(card.agent.host_id).unwrap_or("?"),
            subagent_marker(model, chat.agent),
        ),
        None => String::new(),
    };
    let (word, style) = phase_word(phase, theme);
    // Read-only chats say so, and "needs you" becomes "needs owner" — the
    // observer is not the you who can answer.
    let word = if readonly && matches!(phase, SdkPhase::NeedsYou { .. }) {
        "needs owner".to_string()
    } else {
        word
    };
    let mut right = String::new();
    for fact in session_facts(model, chat) {
        right.push_str(&fact);
        right.push_str(" · ");
    }
    if right.is_empty() {
        right.push_str("chat · ");
    }
    if readonly {
        right.push_str("read-only · ");
    }
    paint_header(&name, (&word, style), &right, theme, width)
}

/// The two session facts the header states: what the turn will run on and
/// what it is allowed to do without asking. Each is shown only once the
/// session has reported it — an empty right side is honest about a
/// session that has not said yet.
fn session_facts(model: &Model, chat: &View) -> Vec<String> {
    let Some(session) = model.claude_sdk(chat.agent).map(|layer| layer.session()) else {
        return Vec::new();
    };
    session
        .model
        .iter()
        .chain(session.permission_mode.iter())
        .cloned()
        .collect()
}

fn banner_answerable(model: &Model, chat: &View, banner: &crate::chat::FamilyBanner) -> bool {
    chat.inline_ask.is_none() && crate::chat::inline::can_open(model, chat.agent, banner.child)
}

fn family_banner_line(text: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, blocks::GLYPH_COL, "⚠", theme.warn());
    push_span(&mut line, blocks::TEXT_COL, text.to_string(), theme.warn());
    line
}

fn phase_word(phase: SdkPhase, theme: Theme) -> (String, Style) {
    match phase {
        SdkPhase::Unavailable => ("unavailable".into(), theme.muted()),
        SdkPhase::Exited => ("exited".into(), theme.muted()),
        SdkPhase::Replaying => ("replaying".into(), theme.muted()),
        SdkPhase::Unknown => ("unknown".into(), theme.muted()),
        SdkPhase::Idle => ("idle".into(), theme.muted()),
        SdkPhase::Working => ("working".into(), theme.text()),
        SdkPhase::Finished => ("finished".into(), theme.warn()),
        SdkPhase::Errored => ("errored".into(), theme.error()),
        SdkPhase::Interrupted => ("interrupted".into(), theme.muted()),
        SdkPhase::NeedsYou { .. } => ("needs you".into(), theme.warn()),
    }
}

/// `◐ working · ctrl+x interrupt`. Read-only chats show the same liveness
/// without the interrupt hint — interrupt is a write affordance, absent
/// rather than disabled.
fn working_row(chat: &View, ctx: &FrameContext, readonly: bool) -> Line<'static> {
    let theme = ctx.theme;
    let frame = ctx.now.timestamp().rem_euclid(SPINNER.len() as i64) as usize;
    let mut line = Line::default();
    push_span(
        &mut line,
        blocks::GLYPH_COL,
        format!("{} working", SPINNER[frame]),
        theme.text(),
    );
    // A docked child ask owns Ctrl+X while it is on screen — it
    // interrupts the agent whose ask that is — so this line stops
    // claiming it: a hint that does something else than it says is worse
    // than no hint.
    if !readonly && chat.inline_ask.is_none() {
        line.spans
            .push(Span::styled(" · ctrl+x interrupt", theme.muted()));
    }
    line
}

// --- the bottom block -------------------------------------------------------

fn bottom_block(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    height: usize,
    paused: bool,
) -> Vec<Line<'static>> {
    let mut lines = if chat.read_only(model) {
        readonly_bottom(chat, theme)
    } else if let Some(inline) = chat.inline_ask.as_ref() {
        crate::chat::inline::panel_lines(model, inline, width, theme, chat.quit_guard.is_armed())
    } else {
        return composer_bottom(model, chat, theme, width, height, paused);
    };

    // Keep the tail: the hint and action rows survive, body rows give way.
    let max_rows = height.saturating_sub(4).max(1);
    if lines.len() > max_rows {
        lines.drain(..lines.len() - max_rows);
    }
    lines
}

/// The read-only chat's bottom block: the statement where the composer
/// would be, and the pager hints.
fn readonly_bottom(chat: &View, theme: Theme) -> Vec<Line<'static>> {
    let mut marker = Line::default();
    push_span(
        &mut marker,
        blocks::GLYPH_COL,
        "⊘ read-only — you are observing this session",
        theme.muted(),
    );
    let footer = if chat.quit_guard.is_armed() {
        armed_quit_line(theme)
    } else {
        let mut line = Line::default();
        push_span(
            &mut line,
            blocks::TEXT_COL,
            "pgup/pgdn scroll · q back to fleet",
            theme.muted(),
        );
        line
    };
    vec![marker, Line::default(), footer]
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
    lines.push(footer_line(model, chat, theme, width, paused));
    lines
}

fn text_width(width: usize) -> usize {
    width.saturating_sub(blocks::TEXT_COL + 1).max(1)
}

// --- footer -----------------------------------------------------------------

/// `? help` joins the hints exactly when `?` opens the overlay — composer
/// focus with an empty draft (with anything typed, `?` types a character,
/// and a hint would lie).
fn help_hinted(chat: &View, hints: String) -> String {
    if chat.composer.is_empty() {
        format!("{hints} · ? help")
    } else {
        hints
    }
}

/// The review chord, saying which of its two acts it would do.
fn review_hint(chat: &View) -> String {
    let action = if chat.review.is_some() {
        "resume review"
    } else {
        "review diff"
    };
    format!("{} r {action}", effective(chat).leader_label)
}

/// One hint line, derived purely from Model + ViewState. The mode-cycle
/// chord sits on the right, next to nothing else, because the mode it
/// changes is stated in the header directly above it.
fn footer_line(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    paused: bool,
) -> Line<'static> {
    let mut line = Line::default();
    if chat.quit_guard.is_armed() {
        line = armed_quit_line(theme);
    } else if let Some(message) = chat.send_failure() {
        push_span(&mut line, blocks::GLYPH_COL, "✗", theme.error());
        push_span(
            &mut line,
            blocks::TEXT_COL,
            format!("send failed: {message}"),
            theme.text(),
        );
    } else if paused {
        push_span(
            &mut line,
            blocks::TEXT_COL,
            help_hinted(chat, effective(chat).feed_hint()),
            theme.muted(),
        );
    } else if let Some(refusal) = amux_ui::claude_sdk::send_gate(model, chat.agent).refusal() {
        let hint = if chat.composer.is_empty() {
            refusal.to_string()
        } else {
            // The footer states the gate plainly and the draft is kept —
            // Enter is a no-op, never a loss.
            format!("draft kept — {refusal}")
        };
        push_span(
            &mut line,
            blocks::TEXT_COL,
            help_hinted(chat, hint),
            theme.muted(),
        );
    } else {
        push_span(
            &mut line,
            blocks::TEXT_COL,
            help_hinted(
                chat,
                format!("enter send · ctrl+j newline · {}", review_hint(chat)),
            ),
            theme.muted(),
        );
    }
    if crate::chat::claude_sdk::keys::allows_mode_cycle(model, chat.agent) {
        let label = "shift+tab mode".to_string();
        let col = width.saturating_sub(1 + label.chars().count());
        if col > line_len(&line) {
            push_span(&mut line, col, label, theme.muted());
        }
    }
    line
}

// --- the feed ---------------------------------------------------------------

/// Every feed block, in file order, the optimistic echo last.
fn feed_blocks(
    model: &Model,
    chat: &View,
    cache: &mut PaintCache,
    theme: Theme,
    width: usize,
) -> Vec<PaintedBlock> {
    let agent = chat.agent;
    let Some(layer) = model.claude_sdk(agent) else {
        return Vec::new();
    };
    // The plan reader affordance is a write-side binding; read-only chats
    // never advertise it.
    let plan_hint = !model.agent(agent).is_some_and(|card| card.agent.readonly);
    let reports = MessageView::new(model, agent, chat.reports_open, chat.leader);

    let mut blocks = Vec::new();
    for entry in layer.entries() {
        let content = entry_content(layer, entry);
        let Some(block) = entry_block_cached(
            entry,
            &content,
            cache,
            theme,
            width,
            plan_hint,
            chat.reports_open,
            reports,
        ) else {
            continue;
        };
        blocks.push(block);
        push_attachment_blocks(
            &mut blocks,
            cache,
            entry.id,
            &described(layer.attachments(), &content),
            theme,
            width,
        );
    }
    if let Some(echo) = layer.pending_echo() {
        let key = BlockKey(ECHO_KEY_BASE);
        // An echo is painted from the same segments a landed prompt is,
        // so its attachment rows appear the moment Enter is pressed and
        // simply survive reconciliation rather than arriving with it.
        let content = layer.attachments().segments(&echo.text);
        blocks.push(
            cache
                .get_or_paint(key, echo, width, theme, false, || {
                    paint_user_prompt(key, &prose(&content), true, theme, width)
                })
                .clone(),
        );
        push_attachment_blocks(
            &mut blocks,
            cache,
            echo_owner(0),
            &described(layer.attachments(), &content),
            theme,
            width,
        );
    }
    cache.retain(&blocks.iter().map(|block| block.key).collect::<Vec<_>>());
    blocks
}

// The cache keys a block by everything that changes how it reads: the
// entry itself and the attachment elements resolved out of its words.
#[allow(clippy::too_many_arguments)]
fn entry_block_cached(
    entry: &FeedEntry,
    content: &[Segment],
    cache: &mut PaintCache,
    theme: Theme,
    width: usize,
    plan_hint: bool,
    reports_open: bool,
    reports: MessageView<'_>,
) -> Option<PaintedBlock> {
    if !paints(entry) {
        return None;
    }
    let keyed = (entry.clone(), content.to_vec());
    Some(
        cache
            .get_or_paint(
                BlockKey(entry.id),
                &keyed,
                width,
                theme,
                reports_open,
                || entry_block(entry, content, theme, width, plan_hint, reports),
            )
            .clone(),
    )
}

/// Whether this row says anything to a person.
///
/// A session that just started needs no rule announcing itself; every
/// other row the fold kept has something to show, so this is the one
/// exception rather than a filter the painter has to agree with.
fn paints(entry: &FeedEntry) -> bool {
    !matches!(
        &entry.kind,
        FeedEntryKind::Boundary(BoundaryEntry::Ready { resumed: false, .. })
    )
}

/// The words one entry carries, as segments, so its attachment elements
/// become rows rather than markup in the message body.
fn entry_content(layer: &amux_ui::claude_sdk::ClaudeSdkLayer, entry: &FeedEntry) -> Vec<Segment> {
    match &entry.kind {
        FeedEntryKind::Prompt(prompt) => layer.attachments().segments(&prompt.text),
        FeedEntryKind::Message(message) => layer.attachments().segments(&message.text),
        _ => Vec::new(),
    }
}

fn push_attachment_blocks(
    blocks: &mut Vec<PaintedBlock>,
    cache: &mut PaintCache,
    owner: u64,
    attachments: &[amux_ui::attachments::AttachmentLine],
    theme: Theme,
    width: usize,
) {
    for (index, attachment) in attachments.iter().enumerate() {
        let key = attachment_key(owner, index);
        blocks.push(
            cache
                .get_or_paint(key, attachment, width, theme, false, || {
                    paint_attachment(key, attachment, theme, width)
                })
                .clone(),
        );
    }
}

/// This chat's effective binding table — the one source every hint that
/// names a leader chord reads, so a hint cannot drift from the `?`
/// overlay.
fn effective(chat: &View) -> crate::bindings::Effective {
    crate::bindings::Effective::new(chat.kitty, chat.leader)
}

fn entry_block(
    entry: &FeedEntry,
    content: &[Segment],
    theme: Theme,
    width: usize,
    plan_hint: bool,
    reports: MessageView<'_>,
) -> PaintedBlock {
    let key = BlockKey(entry.id);
    match &entry.kind {
        FeedEntryKind::Prompt(_) => paint_user_prompt(key, &prose(content), false, theme, width),
        // A block still arriving carries the caret a person reads as
        // "more is coming"; a block the session gave up on says so.
        FeedEntryKind::Message(message) => {
            let text = match message.finality {
                Finality::Streaming | Finality::Stopped => format!("{}▌", prose(content)),
                _ => prose(content),
            };
            let mut block = paint_assistant(key, &text, theme, width);
            if matches!(message.finality, Finality::Interrupted) {
                append_note(&mut block, "interrupted", theme);
            }
            block
        }
        FeedEntryKind::Thinking(thinking) => {
            let mut label = if is_open(thinking.finality) {
                "~ thinking".to_string()
            } else {
                "~ thought".to_string()
            };
            if thinking.redacted {
                label.push_str(" · redacted");
            }
            let detail = (!thinking.redacted)
                .then(|| first_line(&thinking.text))
                .filter(|line| !line.is_empty());
            paint_thinking(key, &label, detail.as_deref(), theme, width)
        }
        FeedEntryKind::Tool(tool) => tool_block(key, tool, theme, width, plan_hint),
        FeedEntryKind::Task(task) => task_block(key, task, theme, width),
        FeedEntryKind::Turn(turn) => {
            let mut label = String::from("turn");
            if turn.is_error {
                label.push_str(" · errored");
            }
            if let Some(ms) = turn.duration_ms {
                label.push_str(&format!(" · {}", fmt_secs(ms / 1000)));
            }
            paint_turn_rule(key, &label, theme, width)
        }
        FeedEntryKind::Compaction(compaction) => {
            let mut label = String::from("compacted");
            if let Some(trigger) = &compaction.trigger {
                label.push_str(&format!(" ({trigger})"));
            }
            if let (Some(pre), Some(post)) = (compaction.pre_tokens, compaction.post_tokens) {
                label.push_str(&format!(
                    " · {} → {} tok",
                    fmt_tokens(pre),
                    fmt_tokens(post)
                ));
            }
            paint_compaction_rule(key, &label, theme, width)
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
        FeedEntryKind::Status(status) => {
            paint_thinking(key, &format!("· {}", status.status), None, theme, width)
        }
        FeedEntryKind::Boundary(boundary) => match boundary {
            // A session picking an older conversation back up says so:
            // the rows above the rule were not written in this sitting.
            // A fresh one is filtered out before it reaches here.
            BoundaryEntry::Ready { resumed, .. } => paint_compaction_rule(
                key,
                if *resumed { "resumed" } else { "ready" },
                theme,
                width,
            ),
            BoundaryEntry::Gap { .. } => paint_compaction_rule(key, "history gap", theme, width),
            BoundaryEntry::ConversationReset { .. } => {
                paint_compaction_rule(key, "conversation reset", theme, width)
            }
        },
        // Explicit, never silently dropped.
        FeedEntryKind::Unrecognized(row) => {
            let detail = if row.detail.is_empty() {
                row.row_type.clone()
            } else {
                format!("{} · {}", row.row_type, row.detail)
            };
            paint_unrecognized(key, "unrecognized row", Some(&detail), theme, width)
        }
    }
}

/// A dim row under a block, in the words the continuation column uses.
fn append_note(block: &mut PaintedBlock, note: &str, theme: Theme) {
    let mut line = Line::default();
    push_span(&mut line, blocks::CONT_COL, note.to_string(), theme.muted());
    block.lines.push(line);
}

// --- tool and task lines ----------------------------------------------------

fn tool_block(
    key: BlockKey,
    tool: &ToolEntry,
    theme: Theme,
    width: usize,
    plan_hint: bool,
) -> PaintedBlock {
    let (glyph, glyph_style) = match (&tool.result, tool.finality) {
        (Some(result), _) if result.is_error => ("✗", theme.error()),
        (Some(_), _) => ("✔", theme.ok()),
        (None, Finality::Interrupted) => ("✗", theme.error()),
        (None, _) => ("▸", theme.text()),
    };
    let mut block = paint_tool_line(
        key,
        (glyph, glyph_style),
        &tool_main_text(tool),
        tool_continuation(tool).as_deref(),
        theme,
        width,
    );

    // An accepted plan stays readable in the feed, truncated to its
    // preview with the reader affordance; read-only chats state the
    // truncation without the dead binding.
    if let ToolInvocation::Plan {
        plan: Some(plan), ..
    } = &tool.invocation
        && tool.result.as_ref().is_some_and(|result| !result.is_error)
    {
        let hint = if plan_hint { "ctrl+t to read" } else { "plan" };
        let preview = paint_plan(key, plan, PLAN_PREVIEW_LINES, hint, theme, width);
        block.lines.extend(preview.lines);
        block.copy_text.push('\n');
        block.copy_text.push_str(&preview.copy_text);
    }
    block
}

/// The tool line's text: what ran, and on what.
fn tool_main_text(tool: &ToolEntry) -> String {
    let name = tool.name.as_str();
    match &tool.invocation {
        ToolInvocation::Edit {
            file_path: Some(path),
            ..
        }
        | ToolInvocation::Write {
            file_path: Some(path),
        }
        | ToolInvocation::Read {
            file_path: Some(path),
        } => format!("{name} {path}"),
        ToolInvocation::Bash {
            command: Some(command),
            ..
        } => {
            let mut text = first_line(command);
            if command.lines().count() > 1 {
                text.push_str(" …");
            }
            format!("{name} {text}")
        }
        // One directional glyph and the target, then a summary of what
        // left — the outbound half of a conversation, not a tool name.
        ToolInvocation::AmuxSend { to, text } => {
            crate::chat::format_amux_send(to.as_deref(), text.as_deref())
        }
        ToolInvocation::Query { text: Some(text) } => format!("{name} \"{text}\""),
        ToolInvocation::Task {
            description,
            background,
            ..
        } => {
            let mut text = match description {
                Some(description) => format!("{name} {description}"),
                None => name.to_string(),
            };
            if *background {
                text.push_str(" (background)");
            }
            text
        }
        ToolInvocation::Plan { .. } if tool.result.is_some() => "plan approved".to_string(),
        _ => name.to_string(),
    }
}

/// The dim `└` continuation, when the row has one.
fn tool_continuation(tool: &ToolEntry) -> Option<String> {
    let Some(result) = &tool.result else {
        return Some(
            match (&tool.invocation, tool.finality) {
                (_, Finality::Interrupted) => "interrupted",
                // A pending plan blocks on the person, not on a run.
                (ToolInvocation::Plan { .. } | ToolInvocation::Question { .. }, _) => "pending",
                (_, finality) if is_open(finality) => "running",
                _ => "running",
            }
            .to_string(),
        );
    };
    if result.text.trim().is_empty() {
        return result.is_error.then(|| "failed".to_string());
    }
    // A read or a search already names its target on the line above; its
    // output head would be raw file content.
    if !result.is_error
        && matches!(
            tool.invocation,
            ToolInvocation::Read { .. } | ToolInvocation::Query { .. }
        )
    {
        return None;
    }
    let mut head = first_line(&result.text);
    if result.text.lines().count() > 1 {
        head.push_str(" …");
    }
    Some(head)
}

/// One subagent task: what it was asked to do, how it is going, and the
/// last thing it was seen doing.
fn task_block(key: BlockKey, task: &TaskEntry, theme: Theme, width: usize) -> PaintedBlock {
    let (glyph, style) = match &task.state {
        TaskState::Running => ("▸", theme.text()),
        TaskState::Completed => ("✔", theme.ok()),
        TaskState::Failed => ("✗", theme.error()),
        TaskState::Stopped => ("✗", theme.warn()),
        TaskState::Unknown(_) => ("·", theme.muted()),
    };
    let mut label = format!("task {}", task.description);
    if let Some(kind) = &task.subagent_type {
        label.push_str(&format!(" · {kind}"));
    }
    let detail = match (&task.summary, &task.last_tool, &task.state) {
        (Some(summary), _, _) if !summary.trim().is_empty() => Some(first_line(summary)),
        (_, Some(tool), TaskState::Running) => Some(format!("running · {tool}")),
        (_, _, TaskState::Running) => Some("running".to_string()),
        (_, _, TaskState::Unknown(state)) => Some(state.clone()),
        _ => None,
    };
    paint_tool_line(key, (glyph, style), &label, detail.as_deref(), theme, width)
}

// --- formatting -------------------------------------------------------------

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

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
