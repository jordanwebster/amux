//! The Claude chat's adapter onto the shared frame: it walks Claude's own
//! feed kinds, formats their words, and hands the shell finished blocks.
//!
//! Nothing here draws. Every row comes from the painter kit in
//! `chat::blocks`, so the Claude and Codex screens cannot drift apart:
//! this file decides what a block *looks like* — what a tool line says and
//! which ask is docked — and the kit decides how it is painted. Every fact
//! rendered here comes from the Model; the code below formats and never
//! recovers provider meaning.

use std::collections::HashMap;

use amux_ui::Model;
use amux_ui::claude::{
    ChatPhase, FeedEntry, FeedEntryKind, FeedItem, InterruptionKind, SuccessFacts, ToolEntry,
    ToolInvocation, ToolOutcome, TurnDuration,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::chat::attachments::{attachment_key, described, echo_owner, prose, words};
use crate::chat::blocks::{
    self, Carrier, fmt_tokens, paint_agent_message, paint_ask_fact, paint_assistant,
    paint_attachment, paint_compaction_rule, paint_composer_block, paint_error,
    paint_exploration_run, paint_file_change, paint_header, paint_plan, paint_subagent,
    paint_thinking, paint_tool_line, paint_turn_rule, paint_unrecognized, paint_user_prompt,
};
use crate::chat::claude::{View, reader_context, shared_ask};
use crate::chat::claude_shared::{armed_quit_line, panel, reader};
use crate::chat::frame::{
    BlockKey, ChatFrameParts, FeedBlocks, PaintCache, PaintInputs, PaintedBlock,
};
use crate::chat::viewport::FeedViewport;
use crate::chat::{
    FeedScroll, MessageView, diff as diff_painter, family_banner, message_glyph, subagent_marker,
};
use crate::render::{FrameContext, Theme, line_len, push_span};

/// One 1 Hz Tick drives the spinner and the elapsed text together (D5);
/// the frame index derives from elapsed seconds — no renderer state.
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

/// The plan entry's feed preview length (B6: "truncated to its first ~6
/// lines").
const PLAN_PREVIEW_LINES: usize = 6;

/// Screen rows a diff preview may occupy, including its remainder row.
const DIFF_PREVIEW_BUDGET: usize = 8;

/// Synthetic keys for the blocks no entry owns. Pending echoes count
/// down from the top of the space so they can never collide with an
/// entry id, which counts up from zero.
const ECHO_KEY_BASE: u64 = u64::MAX;

// --- the frame --------------------------------------------------------------

/// Everything the shared shell needs to draw this chat: one header, the
/// feed as painted blocks, the liveness row, the bottom block, and the
/// overlay that replaces all of it when a reader or the key list is open.
pub(crate) fn claude_frame_parts(
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
    let phase = amux_ui::claude::phase(model, chat.agent);
    let banner = family_banner(model, chat.agent).map(|banner| {
        family_banner_line(
            &banner.row(banner_answerable(model, chat, &banner), chat.leader),
            theme,
        )
    });

    // The `?` overlay and the fullscreen reader each replace the whole
    // frame while open (any key closes the overlay; the reader falls back
    // to the chat when its source no longer resolves).
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
    let bottom = bottom_block(model, chat, theme, width, height, paused);
    let working = matches!(phase, ChatPhase::Working);
    let loading = matches!(phase, ChatPhase::Replaying);

    ChatFrameParts {
        header: header_row(model, chat, theme, phase, width, readonly),
        banner,
        feed: FeedBlocks {
            blocks: if loading {
                Vec::new()
            } else {
                feed_blocks(model, chat, viewport, cache, theme, width)
            },
            history_truncated: model
                .claude(chat.agent)
                .is_some_and(|layer| layer.history_truncated()),
            loading,
        },
        activity: crate::chat::queue::strip(
            model,
            chat.agent,
            activity_row(model, chat, ctx, readonly, working),
            theme,
            width,
            chat.inline_ask.is_none(),
        ),
        bottom,
        overlay,
    }
}

// --- the header and the rows around the feed --------------------------------

fn header_row(
    model: &Model,
    chat: &View,
    theme: Theme,
    phase: ChatPhase,
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
    // Read-only chats say so in the header, and "needs you" becomes
    // "needs owner" — the observer is not the you who can answer (F1).
    let word = if readonly && matches!(phase, ChatPhase::NeedsYou { .. }) {
        "needs owner".to_string()
    } else {
        word
    };
    let mut facts = session_facts(model, chat);
    if readonly {
        facts.push("read-only".to_string());
    }
    // Facts are context and the phase word is not, so a line too narrow
    // to hold both drops facts from the least important end — the model
    // first, then the mode; that a chat is read-only survives longest.
    let mut right = blocks::fit_header_facts(&name, facts, &word, width);
    if right.is_empty() {
        right.push_str("chat · ");
    }
    paint_header(&name, (&word, style), &right, theme, width)
}

/// The two session facts the header states: what the turn will run on and
/// what it is allowed to do without asking. Each is shown only once a row
/// has stated it — an empty right side is honest about a session that has
/// not said yet.
fn session_facts(model: &Model, chat: &View) -> Vec<String> {
    let Some(session) = model.claude(chat.agent).map(|layer| layer.session()) else {
        return Vec::new();
    };
    session
        .model
        .iter()
        .chain(session.permission_mode.iter())
        .cloned()
        .collect()
}

/// Whether this banner's chord would do anything from here: the child
/// has a panel to dock, and it is not already docked.
fn banner_answerable(model: &Model, chat: &View, banner: &crate::chat::FamilyBanner) -> bool {
    chat.inline_ask.is_none() && crate::chat::inline::can_open(model, chat.agent, banner.child)
}

/// The child-ask banner (U1): one warning row naming who is waiting and
/// for what. It is derived per frame from the child's own card, so it
/// leaves the moment the ask is answered — anywhere.
fn family_banner_line(text: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, blocks::GLYPH_COL, "⚠", theme.warn());
    push_span(&mut line, blocks::TEXT_COL, text.to_string(), theme.warn());
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

/// `◐ working · 24s · ctx 31.6k · ctrl+x interrupt` (D5). Elapsed ticks
/// locally from the prompt row's timestamp; the authoritative duration
/// replaces it in the turn marker at close. Read-only chats show the same
/// liveness without the interrupt hint — interrupt is a write affordance,
/// absent not disabled (F1).
///
/// The meter is a passive fact — whatever the last message's own usage
/// reported — so it is stated whenever this session has a layer at all,
/// working or not, and says `unknown` rather than a guess before any
/// message has arrived.
fn activity_row(
    model: &Model,
    chat: &View,
    ctx: &FrameContext,
    readonly: bool,
    working: bool,
) -> Option<Line<'static>> {
    let layer = model.claude(chat.agent)?;
    let theme = ctx.theme;
    let mut line = Line::default();
    if working {
        let elapsed = layer
            .prompt_at()
            .map(|at| (ctx.now - at).num_seconds().max(0) as u64);
        let spinner = SPINNER[elapsed.unwrap_or(0) as usize % SPINNER.len()];
        let mut label = format!("{spinner} working");
        if let Some(secs) = elapsed {
            label.push_str(&format!(" · {}", fmt_secs(secs)));
        }
        push_span(&mut line, blocks::GLYPH_COL, label, theme.text());
    } else {
        push_span(&mut line, blocks::TEXT_COL, "", theme.muted());
    }

    let mut facts = vec![meter_text(layer.session().context_used_tokens)];
    // A docked child ask owns Ctrl+X while it is on screen — it
    // interrupts the agent whose ask that is — so this line stops
    // claiming it: a hint that would do something else than it says is
    // worse than no hint (P10).
    if working && !readonly && chat.inline_ask.is_none() {
        facts.push("ctrl+x interrupt".to_string());
    }
    let joined = facts.join(" · ");
    if working {
        line.spans
            .push(Span::styled(format!(" · {joined}"), theme.muted()));
    } else {
        line.spans.push(Span::styled(joined, theme.muted()));
    }
    Some(line)
}

/// `ctx 31.6k`, and `ctx unknown` before any message has stated a usage.
/// There is no denominator: no transcript row states the context window.
fn meter_text(used_tokens: Option<u64>) -> String {
    match used_tokens {
        None => "ctx unknown".to_string(),
        Some(used) => format!("ctx {}", fmt_tokens(used)),
    }
}

// --- the bottom block -------------------------------------------------------

/// Everything below the feed: the read-only statement, a docked ask —
/// this chat's own or a child's — or the composer the person types in.
fn bottom_block(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    height: usize,
    paused: bool,
) -> Vec<Line<'static>> {
    let head = chat.ask_head(model);
    let mut lines = if chat.read_only(model) {
        readonly_bottom(model, chat, theme, width)
    } else if let Some(inline) = chat.inline_ask.as_ref().filter(|_| head.is_none()) {
        // U2: a child's ask docks where the composer is, exactly as this
        // chat's own ask would. This chat's own ask comes first — and
        // reconcile drops a guest the moment one arrives, so the filter
        // is a belt on top of that brace, never a state that persists.
        crate::chat::inline::panel_lines(model, inline, width, theme, chat.quit_guard.is_armed())
    } else if let Some(ask) = head {
        let count = model
            .claude(chat.agent)
            .map(|layer| layer.ask_count())
            .unwrap_or(1);
        let shared = shared_ask(ask);
        panel::paint(
            &shared,
            panel::ask_panel(
                &shared,
                count,
                chat.ask_ui.as_ref(),
                chat.ask_failure.as_deref(),
                blocks::panel_body_width(width),
                theme,
                chat.quit_guard.is_armed(),
            ),
            theme,
            width,
        )
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

/// The read-only chat's bottom block (F1): the ask fact panel when one
/// pends, the `⊘ read-only` statement where the composer would be, and
/// the pager hints.
fn readonly_bottom(model: &Model, chat: &View, theme: Theme, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(ask) = chat.ask_head(model) {
        let count = model
            .claude(chat.agent)
            .map(|layer| layer.ask_count())
            .unwrap_or(1);
        let shared = shared_ask(ask);
        lines.extend(panel::paint(
            &shared,
            panel::readonly_ask_panel(&shared, count, blocks::panel_body_width(width), theme),
            theme,
            width,
        ));
        lines.push(Line::default());
    }
    let mut marker = Line::default();
    push_span(
        &mut marker,
        blocks::GLYPH_COL,
        "⊘ read-only — you are observing this session",
        theme.muted(),
    );
    lines.push(marker);
    lines.push(Line::default());
    let mut hints = String::from("pgup/pgdn scroll");
    if let Some(ask) = chat.ask_head(model)
        && shared_ask(ask).has_readable()
    {
        hints.push_str(" · f view document");
    }
    hints.push_str(" · q back to fleet");
    lines.push(if chat.quit_guard.is_armed() {
        armed_quit_line(theme)
    } else {
        let mut footer = Line::default();
        push_span(&mut footer, blocks::TEXT_COL, hints, theme.muted());
        footer
    });
    lines
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
    lines.push(footer_line(model, chat, theme, width, paused));
    lines
}

/// Cells available to content at the kit's text column, inside the
/// frame's one-cell right margin.
fn text_width(width: usize) -> usize {
    width.saturating_sub(blocks::TEXT_COL + 1).max(1)
}

// --- footer -----------------------------------------------------------------

/// `? help` joins the footer hints exactly when `?` opens the overlay —
/// composer focus, empty draft (with anything typed, `?` types; a hint
/// would lie, P10).
fn help_hinted(chat: &View, hints: String) -> String {
    if chat.composer.is_empty() {
        format!("{hints} · ? help")
    } else {
        hints
    }
}
/// The review chord, saying which of its two acts it would do. A draft
/// that already holds a review resumes the page it froze; an empty one
/// asks for a fresh diff.
fn review_hint(chat: &View) -> String {
    let action = if chat.review.is_some() {
        "resume review"
    } else {
        "review diff"
    };
    format!("{} r {action}", effective(chat).leader_label)
}

/// One hint line, at most four items, derived purely from Model +
/// ViewState (no stored footer mode); the key that cycles permission mode
/// on the right, where the mode itself used to sit before the header
/// took over stating it (D4).
fn footer_line(
    model: &Model,
    chat: &View,
    theme: Theme,
    width: usize,
    paused: bool,
) -> Line<'static> {
    let mut line = Line::default();
    if chat.quit_guard.is_armed() {
        // The armed quit guard replaces the hints (warning color); the
        // right-hand key hint stays.
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
        // The rule above the footer already says how to catch up, so the
        // footer spends its width on what a stopped reader came for:
        // putting the focus on a block and taking it out.
        let hints = effective(chat).feed_hint();
        push_span(
            &mut line,
            blocks::TEXT_COL,
            help_hinted(chat, hints),
            theme.muted(),
        );
    } else if let Some(refusal) = amux_ui::claude::send_gate(model, chat.agent).refusal() {
        let hint = if chat.composer.is_empty() {
            refusal.to_string()
        } else {
            // D2: the footer states the gate plainly, and the draft is
            // kept — Enter is a no-op, never a loss.
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
    if amux_ui::claude::mode_cycle_gate(model, chat.agent).is_none() {
        let label = "shift+tab mode".to_string();
        let col = width.saturating_sub(1 + label.chars().count());
        if col > line_len(&line) {
            push_span(&mut line, col, label, theme.muted());
        }
    }
    line
}

// --- the feed ---------------------------------------------------------------

/// Every feed block, in file order, echoes last (B1). Consecutive
/// read/search entries fold into one exploration run first, so the feed
/// shows what the agent did rather than every step it took to do it.
fn feed_blocks(
    model: &Model,
    chat: &View,
    viewport: &FeedViewport,
    cache: &mut PaintCache,
    theme: Theme,
    width: usize,
) -> Vec<PaintedBlock> {
    let agent = chat.agent;
    let Some(layer) = model.claude(agent) else {
        return Vec::new();
    };
    let entries: HashMap<u64, &FeedEntry> =
        layer.entries().map(|entry| (entry.id, entry)).collect();
    // The plan reader affordance is a write-side binding; read-only chats
    // never advertise it (hints tell the truth, F1).
    let plan_hint = !model.agent(agent).is_some_and(|card| card.agent.readonly);
    let reports = MessageView::new(model, agent, chat.reports_open, chat.leader);
    let eff = effective(chat);

    let mut blocks = Vec::new();
    for item in layer.feed_items() {
        match item {
            FeedItem::Entry(entry) => {
                blocks.push(
                    cache
                        .get_or_paint(
                            BlockKey(entry.id),
                            entry,
                            PaintInputs {
                                width,
                                theme,
                                expanded: chat.reports_open,
                            },
                            || {
                                entry_block(
                                    entry,
                                    layer.attachments(),
                                    theme,
                                    width,
                                    plan_hint,
                                    reports,
                                )
                            },
                        )
                        .clone(),
                );
                push_attachment_blocks(
                    &mut blocks,
                    cache,
                    entry.id,
                    &entry_attachments(layer, entry),
                    carrier_of(entry),
                    theme,
                    width,
                );
            }
            FeedItem::ExplorationRun {
                id,
                member_ids,
                reads,
                searches,
                read_paths,
            } => {
                let key = blocks::RunKey(id);
                let summary = blocks::run_summary(reads, searches, &read_paths);
                let member_entries: Vec<FeedEntry> = member_ids
                    .iter()
                    .filter_map(|id| entries.get(id))
                    .map(|entry| (*entry).clone())
                    .collect();
                // An open run offers to shut again, not to open twice.
                let expanded = viewport.expanded.contains(&key);
                let hint = eff.fold_hint(expanded);
                let content = (
                    summary.clone(),
                    member_entries.clone(),
                    plan_hint,
                    chat.reports_open,
                    chat.leader,
                    hint.clone(),
                );
                blocks.push(
                    cache
                        .get_or_paint(
                            BlockKey(key.0),
                            &content,
                            PaintInputs {
                                width,
                                theme,
                                expanded,
                            },
                            || {
                                let painted: Vec<PaintedBlock> = member_entries
                                    .iter()
                                    .map(|entry| {
                                        entry_block(
                                            entry,
                                            layer.attachments(),
                                            theme,
                                            width,
                                            plan_hint,
                                            reports,
                                        )
                                    })
                                    .collect();
                                paint_exploration_run(
                                    BlockKey(key.0),
                                    key,
                                    &summary,
                                    &painted,
                                    expanded,
                                    &hint,
                                    theme,
                                    width,
                                )
                            },
                        )
                        .clone(),
                );
            }
        }
    }
    for (index, echo) in layer.pending_echoes().iter().enumerate() {
        let key = BlockKey(ECHO_KEY_BASE - index as u64);
        // An echo is painted from the same segments a landed prompt is:
        // what was sent already carries its elements, so the attachment
        // rows appear the moment Enter is pressed and simply survive
        // reconciliation rather than arriving with it.
        let content = layer.attachments().segments(&echo.text);
        blocks.push(
            cache
                .get_or_paint(
                    key,
                    echo,
                    PaintInputs {
                        width,
                        theme,
                        expanded: false,
                    },
                    || {
                        paint_user_prompt(
                            key,
                            &words(layer.attachments(), &content),
                            true,
                            theme,
                            width,
                        )
                    },
                )
                .clone(),
        );
        push_attachment_blocks(
            &mut blocks,
            cache,
            echo_owner(index),
            &described(layer.attachments(), &content),
            Carrier::Person,
            theme,
            width,
        );
    }
    cache.retain(&blocks.iter().map(|block| block.key).collect::<Vec<_>>());
    blocks
}

/// The attachments one entry carries, described from the layer's index.
fn entry_attachments(
    layer: &amux_ui::claude::ClaudeLayer,
    entry: &FeedEntry,
) -> Vec<amux_ui::attachments::AttachmentLine> {
    match &entry.kind {
        FeedEntryKind::Prompt(prompt) => described(layer.attachments(), &prompt.content),
        FeedEntryKind::Message(message) => described(layer.attachments(), &message.content),
        _ => Vec::new(),
    }
}

/// Whose message this is, for the surface its attachment rows take.
fn carrier_of(entry: &FeedEntry) -> Carrier {
    match &entry.kind {
        FeedEntryKind::Prompt(_) => Carrier::Person,
        _ => Carrier::Agent,
    }
}

/// Append one focusable row per attachment under the block that carries
/// them, so the feed can put the focus on a single attachment and open
/// exactly that one.
fn push_attachment_blocks(
    blocks: &mut Vec<PaintedBlock>,
    cache: &mut PaintCache,
    owner: u64,
    attachments: &[amux_ui::attachments::AttachmentLine],
    carrier: Carrier,
    theme: Theme,
    width: usize,
) {
    for (index, attachment) in attachments.iter().enumerate() {
        let key = attachment_key(owner, index);
        blocks.push(
            cache
                .get_or_paint(
                    key,
                    attachment,
                    PaintInputs {
                        width,
                        theme,
                        expanded: false,
                    },
                    || paint_attachment(key, attachment, carrier, theme, width),
                )
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
    index: &amux_ui::attachments::AttachmentIndex,
    theme: Theme,
    width: usize,
    plan_hint: bool,
    reports: MessageView<'_>,
) -> PaintedBlock {
    let key = BlockKey(entry.id);
    match &entry.kind {
        FeedEntryKind::Prompt(prompt) => {
            paint_user_prompt(key, &words(index, &prompt.content), false, theme, width)
        }
        // One markdown source per message: blocks joined the way the API
        // separates them.
        FeedEntryKind::Message(message) => {
            paint_assistant(key, &prose(&message.content), theme, width)
        }
        FeedEntryKind::Thinking(thinking) => {
            let mut text = match thinking.duration_ms {
                Some(ms) => format!("~ thought for {}", fmt_secs(ms.max(0) as u64 / 1000)),
                None => "~ thought".to_string(),
            };
            if thinking.redacted {
                text.push_str(" · redacted");
            }
            paint_thinking(key, &text, None, theme, width)
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
            let mut label = format!("turn · {duration}");
            if let Some(agents) = turn.pending_background_agents.filter(|n| *n > 0) {
                label.push_str(&format!(" · {agents} bg running"));
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
        FeedEntryKind::CompactSummary(summary) => paint_assistant(key, &summary.text, theme, width),
        FeedEntryKind::Tool(tool) => tool_block(key, tool, theme, width, plan_hint),
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
        FeedEntryKind::TaskNotification(notification) => {
            paint_subagent(key, ("✔", theme.ok()), &notification.text, theme, width)
        }
        FeedEntryKind::Interruption(interruption) => paint_error(
            key,
            match interruption.kind {
                InterruptionKind::Turn => "interrupted",
                InterruptionKind::ToolUse => "interrupted (tool use)",
            },
            false,
            theme,
            width,
        ),
        FeedEntryKind::ApiError(error) => {
            let mut message = String::from("api error");
            if let Some(kind) = &error.error {
                message.push_str(&format!(" ({kind})"));
            }
            if let Some(text) = &error.text {
                message.push('\n');
                message.push_str(text.lines().next().unwrap_or_default());
            }
            paint_error(key, &message, false, theme, width)
        }
        FeedEntryKind::Unrecognized(row) => {
            // Explicit, never silently dropped (G1).
            let detail = match (&row.row_type, &row.detail) {
                (Some(kind), Some(detail)) => Some(format!("{kind} · {detail}")),
                (Some(kind), None) => Some(kind.clone()),
                (None, Some(detail)) => Some(detail.clone()),
                (None, None) => None,
            };
            paint_unrecognized(key, "unrecognized row", detail.as_deref(), theme, width)
        }
    }
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

fn tool_block(
    key: BlockKey,
    tool: &ToolEntry,
    theme: Theme,
    width: usize,
    plan_hint: bool,
) -> PaintedBlock {
    // An answered question is history: one plain row per answer, no panel.
    if let ToolOutcome::Success {
        facts: SuccessFacts::Answers { answers },
    } = &tool.outcome
    {
        let fact = if answers.is_empty() {
            "answered".to_string()
        } else {
            answers
                .iter()
                .map(|answer| format!("{} → {}", answer.question, answer.answer))
                .collect::<Vec<_>>()
                .join("\n")
        };
        return paint_ask_fact(key, ("?", theme.warn()), &fact, theme, width);
    }

    // A landed edit is a file change, not a tool outcome: it says what
    // moved and by how much, on its own row, never folded away.
    if let (
        ToolInvocation::Edit { .. } | ToolInvocation::Write { .. },
        ToolOutcome::Success {
            facts:
                SuccessFacts::Edit {
                    file_path,
                    added,
                    removed,
                    document,
                    ..
                },
        },
    ) = (&tool.invocation, &tool.outcome)
    {
        let mut block = paint_file_change(
            key,
            tool.name.as_deref().unwrap_or("edit"),
            file_path,
            *added,
            *removed,
            theme,
            width,
        );
        let rows = document.rows();
        if !rows.is_empty() {
            let painted =
                diff_painter::paint_rows(&rows, theme, blocks::panel_body_width(width), 0, true);
            let (body, screen_cut) = painted.into_screen_head(DIFF_PREVIEW_BUDGET);
            let title = if document.truncated || screen_cut {
                format!("{file_path} · patch preview")
            } else {
                file_path.to_string()
            };
            append_block(
                &mut block,
                blocks::paint_unified_diff(key, &title, body, theme, width),
            );
        }
        return block;
    }

    let (glyph, glyph_style) = outcome_glyph(tool, theme);
    let mut block = paint_tool_line(
        key,
        (glyph, glyph_style),
        &tool_main_text(tool),
        tool_continuation(tool).as_deref(),
        theme,
        width,
    );

    // The accepted plan stays readable in the feed, truncated to its
    // preview with the reader affordance (B6); read-only chats state the
    // truncation without the dead binding.
    if let (
        ToolInvocation::Plan {
            plan: Some(plan), ..
        },
        ToolOutcome::Success {
            facts: SuccessFacts::PlanApproved { .. },
        },
    ) = (&tool.invocation, &tool.outcome)
    {
        let hint = if plan_hint { "ctrl+t to read" } else { "plan" };
        let preview = paint_plan(key, plan, PLAN_PREVIEW_LINES, hint, theme, width);
        block.lines.extend(preview.lines);
        block.copy_text.push('\n');
        block.copy_text.push_str(&preview.copy_text);
    }
    block
}

fn append_block(block: &mut PaintedBlock, tail: PaintedBlock) {
    block.lines.extend(tail.lines);
    if !tail.copy_text.is_empty() {
        block.copy_text.push('\n');
        block.copy_text.push_str(&tail.copy_text);
    }
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
                        ..
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
        // One directional glyph and the target, then a summary of what left —
        // the outbound half of a conversation, not a tool name.
        (ToolInvocation::AmuxSend { to, text }, _) => {
            crate::chat::format_amux_send(to.as_deref(), text.as_deref())
        }
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

// --- the overlays -----------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::Composer;

    #[test]
    fn tall_help_overlay_renders_every_effective_binding() {
        let model = Model::default();
        let chat = View::open(uuid::Uuid::nil(), 'a', false);
        let sections = crate::bindings::chat_sections(
            &crate::bindings::Effective::new(chat.kitty, chat.leader),
            crate::bindings::FamilyKeys::default(),
        );
        let body_rows = sections
            .iter()
            .map(|section| 1 + section.bindings.len())
            .sum::<usize>()
            + sections.len().saturating_sub(1);
        let _ = &model;
        let lines = crate::chat::claude_shared::help_overlay(
            crate::bindings::chat_sections(
                &crate::bindings::Effective::new(chat.kitty, chat.leader),
                crate::bindings::FamilyKeys::default(),
            ),
            chat.quit_guard.is_armed(),
            Theme::default(),
            120,
            body_rows + 5,
        );
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();

        assert!(rendered.iter().all(|line| !line.contains("⋮ more")));
        for section in sections {
            assert!(
                rendered.iter().any(|line| line.trim() == section.title),
                "missing help section {:?}",
                section.title
            );
            for binding in section.bindings {
                assert!(
                    rendered.iter().any(|line| {
                        line.contains(&binding.keys) && line.contains(&binding.action)
                    }),
                    "missing help row {:?}: {:?}",
                    binding.keys,
                    binding.action
                );
            }
        }
    }

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
        let (rows, cursor_row) = composer.display_rows(4);
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
        let (rows, cursor_row) = composer.display_rows(40);
        assert_eq!(rows, vec!["one", "▌two"]);
        assert_eq!(cursor_row, 1);
    }
}
