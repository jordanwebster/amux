//! The fleet renderer: a pure function of (Model, ViewState, FrameContext).
//!
//! The layout grid reproduces the aligned frames in the TUI V1 spec
//! verbatim; the tier-3 golden tests lock every screen. The chrome draws on
//! the alternate screen only and never writes terminal scrollback.

use amux_ui::{
    AgentId, AgentPhase, Attention, Command, Connection, DisconnectReason, Model, Why,
    agent_type_label, format_relative_age,
};
use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub use crate::theme::Theme;
use crate::view::{Mode, QuitGuard, ViewState, VisibleRow, visible_rows};

// Column grid (0-indexed), from the aligned 68-column frames in the spec.
const MARKER_COL: usize = 2;
const BADGE_COL: usize = 4;
const NAME_COL: usize = 6;
const NAME_WIDTH: usize = 21;
const TYPE_COL: usize = 28;
const TYPE_WIDTH: usize = 8;
const HOST_COL: usize = 37;
const HOST_WIDTH: usize = 10;
const AGE_COL: usize = 48;
const AGE_WIDTH: usize = 5;
const STATUS_COL: usize = 54;
/// Room for the longest word the column is designed around (`permission`),
/// and one clear cell before `working_on`. Not every status word is one of
/// that closed set: an exited agent states its code, and an operating
/// system's abort code is long enough to run through the next column, so
/// the cell is clipped like every other one on the row.
const STATUS_WIDTH: usize = WORKING_COL - STATUS_COL - 1;
/// The status word is the second column to collapse on narrow terminals:
/// shown only when the full grid fits.
const STATUS_MIN_FRAME_WIDTH: usize = 68;
/// `working_on` sits past the widest status word (`permission`), and is
/// the FIRST column to collapse — what an agent says it is doing is the
/// most expendable cell on a cramped screen, because every other column
/// answers a question this one only elaborates on.
const WORKING_COL: usize = 65;
/// Enough room past `WORKING_COL` for a clipped phrase and its age.
const WORKING_MIN_FRAME_WIDTH: usize = 78;
/// Indent per generation for an unfolded family's descendants.
const FAMILY_INDENT: usize = 2;
/// Header right block ("5 agents" / "1/5") is left-anchored here.
const RIGHT_INFO_FROM_EDGE: usize = 13;
/// Below this width the column grid cannot lay out (the right-info block
/// anchors at `width - RIGHT_INFO_FROM_EDGE`, which must not underflow):
/// the frame degrades to the too-small notice instead.
const MIN_FRAME_WIDTH: usize = RIGHT_INFO_FROM_EDGE;
/// Key hints in the status line (col 25 leaves two clear columns after the
/// widest normal left status, and `q quit` still fits the 68-col frame).
const HINTS_COL: usize = 25;

/// Persistent diagnostic chrome shown after the runtime observes structural
/// Model incoherence. The Model supplies only the sticky fact; all native
/// render paths format the same concise message here.
pub(crate) const INVARIANT_WARNING: &str = "⚠ internal consistency error — see recorder dump/log";

/// Rows of chrome that are not list rows: two borders, filter line, spacer,
/// banner/spacer, status line.
const CHROME_ROWS: usize = 6;

/// The frame's environment: everything a render may depend on besides the
/// Model and the ViewState.
#[derive(Clone, Copy, Debug)]
pub struct FrameContext {
    pub viewport: (u16, u16),
    pub theme: Theme,
    pub now: DateTime<Utc>,
}

pub fn list_capacity(height: u16) -> usize {
    (height as usize).saturating_sub(CHROME_ROWS)
}

pub fn render(model: &Model, view: &ViewState, ctx: &FrameContext, frame: &mut Frame<'_>) {
    let lines = build_lines(model, view, ctx);
    frame.render_widget(Paragraph::new(lines), frame.area());
}

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

fn plain() -> Style {
    Style::default()
}

/// Build the full frame as styled lines (the whole chrome is text — no
/// nested widgets, so goldens control every cell). The chat screen, when
/// open, replaces the fleet inside the same chrome.
pub fn build_lines(model: &Model, view: &ViewState, ctx: &FrameContext) -> Vec<Line<'static>> {
    match &view.chat {
        Some(chat) => crate::chat::build_chat_lines(model, chat, ctx),
        None => build_fleet_lines(model, view, ctx),
    }
}

fn build_fleet_lines(model: &Model, view: &ViewState, ctx: &FrameContext) -> Vec<Line<'static>> {
    let width = ctx.viewport.0 as usize;
    let height = ctx.viewport.1 as usize;
    if width < MIN_FRAME_WIDTH || height < CHROME_ROWS {
        return vec![Line::from("amux: terminal too small")];
    }

    let rows = visible_rows(model, view);
    let capacity = list_capacity(ctx.viewport.1);
    let mut lines = Vec::with_capacity(height);

    lines.push(title_line(width));
    lines.push(filter_line(model, view, width, rows.len()));
    lines.push(blank_line(width));

    let mut list_lines = match screen_state(model, view, &rows) {
        ScreenState::Fleet => {
            // Key handling clamps scroll/selection, but a subscription-driven
            // fleet shrink can land between keypresses: clamp the stale
            // ViewState values against the rows actually being rendered, so
            // the list never draws empty (or loses the selection marker)
            // until the next keypress. Clamping a stale value against the
            // Model is formatting, not deciding — render stays pure.
            let selected = view.selected.min(rows.len().saturating_sub(1));
            let scroll = view.scroll.min(rows.len().saturating_sub(capacity));
            let mut list = Vec::with_capacity(capacity);
            let window = rows.iter().enumerate().skip(scroll).take(capacity);
            for (index, row) in window {
                list.push(fleet_row_line(model, view, ctx, row, index == selected));
            }
            list
        }
        ScreenState::Help => help_lines(model, view, width),
        ScreenState::ConfirmDelete { agent } => {
            confirm_delete_lines(model, ctx, agent, width, capacity)
        }
        ScreenState::Message(message_lines) => centered_lines(&message_lines, width, capacity),
    };
    list_lines.truncate(capacity);
    for line in &mut list_lines {
        finish_line(line, width);
    }
    lines.extend(list_lines);
    while lines.len() < height - 3 {
        lines.push(blank_line(width));
    }

    lines.push(banner_line(model, width));
    lines.push(status_line(model, view, width));
    lines.push(bottom_border(width));
    lines
}

enum ScreenState {
    Fleet,
    Help,
    /// The cascade a delete would perform, listed before it happens (U6).
    /// It takes the list area rather than the status line because a
    /// folded family is exactly one row on screen and the whole point is
    /// to show what that row was standing in for.
    ConfirmDelete {
        agent: AgentId,
    },
    /// Full-screen message (chrome frame stays), centered in the list area.
    Message(Vec<(String, Style)>),
}

fn screen_state(model: &Model, view: &ViewState, rows: &[VisibleRow<'_>]) -> ScreenState {
    if view.mode == Mode::Help {
        return ScreenState::Help;
    }
    match model.connection() {
        Connection::Connecting => {
            return ScreenState::Message(vec![("Starting daemon… ◌".to_string(), plain())]);
        }
        Connection::Disconnected { reason } => {
            let detail = match reason {
                DisconnectReason::AuthenticationRequired => {
                    "✗ authentication required — run `amux init`".to_string()
                }
                DisconnectReason::SubscriptionRequired => {
                    "✗ subscription required — amux.sh/account".to_string()
                }
                DisconnectReason::ServerShutdown { detail } => {
                    format!("✗ daemon shut down: {detail}")
                }
                DisconnectReason::TransportError { .. } | DisconnectReason::ApplicationShutdown => {
                    "✗ daemon unreachable".to_string()
                }
            };
            return ScreenState::Message(vec![
                (detail, Style::default().fg(Color::Red)),
                ("start it with: amux server start".to_string(), dim()),
            ]);
        }
        Connection::Connected { .. } => {}
    }
    // Deleting a family takes everything below it (row 9), so the
    // confirmation names everything below it. An agent that started
    // nobody keeps the one-line status prompt it has always had.
    if let Mode::ConfirmDelete { agent, .. } = &view.mode
        && !model.family_of(*agent).is_empty()
    {
        return ScreenState::ConfirmDelete { agent: *agent };
    }
    if !rows.is_empty() {
        return ScreenState::Fleet;
    }
    if !model.is_synchronized() {
        return ScreenState::Message(vec![("Loading… ◌".to_string(), dim())]);
    }
    if model.fleet_agent_count() == 0 && view.filter.is_empty() {
        let host = model
            .local_host_id()
            .and_then(|id| model.host_name(id))
            .unwrap_or("this host")
            .to_string();
        return ScreenState::Message(vec![
            (format!("Press n to create one on {host},"), plain()),
            ("or pair a device: amux pair --help".to_string(), dim()),
        ]);
    }
    if !view.filter.is_empty() {
        return ScreenState::Message(vec![("no matches".to_string(), dim())]);
    }
    ScreenState::Fleet
}

fn title_line(width: usize) -> Line<'static> {
    let mut line = String::from("┌ amux ");
    while line.chars().count() < width - 1 {
        line.push('─');
    }
    line.push('┐');
    Line::from(Span::styled(line, dim()))
}

pub(crate) fn bottom_border(width: usize) -> Line<'static> {
    let mut line = String::from("└");
    while line.chars().count() < width - 1 {
        line.push('─');
    }
    line.push('┘');
    Line::from(Span::styled(line, dim()))
}

pub(crate) fn blank_line(width: usize) -> Line<'static> {
    let mut line = new_line();
    finish_line(&mut line, width);
    line
}

/// A content line: left border plus spans; `finish_line` pads and closes it.
pub(crate) fn new_line() -> Line<'static> {
    Line::from(vec![Span::styled("│", dim())])
}

/// Display width in terminal cells — the measurement every wrap, pad, and
/// clip in this crate uses. CJK and emoji occupy two cells, combining
/// marks zero; ratatui renders with the same `unicode-width` version, so
/// this arithmetic and the backend never disagree.
pub(crate) fn str_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// The longest prefix of `text` fitting `max` display cells, cut at a
/// grapheme boundary — a wide grapheme never straddles the cut.
pub(crate) fn clip_to_width(text: &str, max: usize) -> &str {
    use unicode_segmentation::UnicodeSegmentation;
    let mut used = 0usize;
    let mut end = 0usize;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let width = str_width(grapheme);
        if used + width > max {
            break;
        }
        used += width;
        end = offset + grapheme.len();
    }
    &text[..end]
}

/// Line width in display cells.
pub(crate) fn line_len(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| str_width(&span.content)).sum()
}

pub(crate) fn pad_to(line: &mut Line<'static>, col: usize) {
    let len = line_len(line);
    if col > len {
        line.spans.push(Span::raw(" ".repeat(col - len)));
    }
}

pub(crate) fn push_span(
    line: &mut Line<'static>,
    col: usize,
    text: impl Into<String>,
    style: Style,
) {
    pad_to(line, col);
    line.spans.push(Span::styled(text.into(), style));
}

/// A right-aligned annotation inside the border margin — the panel
/// headers' `(1 of N)` queue count, the reader's position indicator.
/// Skipped when it would collide with the line's left content.
pub(crate) fn push_right(line: &mut Line<'static>, text: String, width: usize, style: Style) {
    let col = width.saturating_sub(2 + text.chars().count());
    if col > line_len(line) {
        push_span(line, col, text, style);
    }
}

/// Close a line at `width`: pad to the last column and set the right
/// border there. The arithmetic saturates because a width is not always a
/// width a frame could be drawn at — a chat's `layout` measures its bottom
/// block at whatever viewport it was handed, before the too-small notice
/// takes over — and a measurement must not be able to bring the process
/// down.
pub(crate) fn finish_line(line: &mut Line<'static>, width: usize) {
    let budget = width.saturating_sub(1);
    pad_to(line, budget);
    // Drop overflow defensively, by display cells: goldens keep us honest
    // about fit, and the right border must land in the last column even
    // when wide graphemes are in play.
    let mut used = 0usize;
    for span in line.spans.iter_mut() {
        let span_width = str_width(&span.content);
        if used + span_width > budget {
            let keep = budget.saturating_sub(used);
            span.content = clip_to_width(&span.content, keep).to_string().into();
        }
        used += str_width(&span.content);
    }
    line.spans.retain(|span| !span.content.is_empty());
    // A clipped wide grapheme can leave a one-cell gap; re-pad so the
    // border never drifts out of the last column.
    pad_to(line, budget);
    line.spans.push(Span::styled("│", dim()));
}

fn clip(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

fn filter_line(model: &Model, view: &ViewState, width: usize, visible: usize) -> Line<'static> {
    let mut line = new_line();
    match &view.mode {
        Mode::Filter => {
            push_span(&mut line, MARKER_COL, ">", plain());
            push_span(&mut line, BADGE_COL, format!("{}▌", view.filter), plain());
        }
        _ if !view.filter.is_empty() => {
            push_span(&mut line, MARKER_COL, "/", dim());
            push_span(&mut line, BADGE_COL, view.filter.clone(), plain());
        }
        _ => {
            push_span(&mut line, MARKER_COL, "/", dim());
        }
    }
    let info = if view.mode == Mode::Filter || !view.filter.is_empty() {
        format!("{visible}/{}", model.fleet_agent_count())
    } else {
        format!("{} agents", model.fleet_agent_count())
    };
    push_span(&mut line, width - RIGHT_INFO_FROM_EDGE, info, dim());
    finish_line(&mut line, width);
    line
}

fn badge_for(model: &Model, card: &amux_ui::AgentCard) -> (&'static str, Style) {
    if let AgentPhase::Exited { .. } = card.phase
        && model.host_online(card.agent.host_id)
    {
        return (" ", plain());
    }
    badge_glyph(model.effective_attention(card))
}

/// The badge an attention wears. A folded family's row wears the loudest
/// one anywhere inside it, drawn from this same table — so a shut family
/// and the child hiding in it never disagree about how loud it is.
fn badge_glyph(attention: Attention) -> (&'static str, Style) {
    match attention {
        Attention::Unknown => ("–", dim()),
        Attention::Idle => (" ", plain()),
        Attention::Working => ("⋯", dim()),
        Attention::NeedsYou {
            why: Why::Permission,
        } => ("!", Style::default().fg(Color::Red)),
        Attention::NeedsYou { why: Why::Question } => ("?", Style::default().fg(Color::Yellow)),
        Attention::NeedsYou { why: Why::Finished } => ("✓", Style::default().fg(Color::Green)),
    }
}

/// The `working_on` cell: what the agent last said it was doing, clipped
/// to the room left over, then how long ago it said so. Empty when it has
/// said nothing — silence reads as silence, not as an idle phrase.
fn working_text(card: &amux_ui::AgentCard, now: DateTime<Utc>, budget: usize) -> Option<String> {
    let working = card.working_on()?;
    let age = format_relative_age(now, working.updated_at);
    let room = budget.saturating_sub(age.chars().count() + 1);
    if room < 2 {
        return None;
    }
    let text = clip(working.text.lines().next().unwrap_or_default().trim(), room);
    (!text.is_empty()).then(|| format!("{text} {age}"))
}

fn fleet_row_line(
    model: &Model,
    view: &ViewState,
    ctx: &FrameContext,
    row: &VisibleRow<'_>,
    selected: bool,
) -> Line<'static> {
    let width = ctx.viewport.0 as usize;
    let show_status = width >= STATUS_MIN_FRAME_WIDTH;
    let show_working = width >= WORKING_MIN_FRAME_WIDTH;
    let renaming = matches!(
        (&view.mode, row),
        (Mode::Rename { agent, .. }, VisibleRow::Agent(agent_row)) if *agent == agent_row.card.agent.id
    );
    let mut line = new_line();
    if selected && !renaming {
        push_span(&mut line, MARKER_COL, "▸", plain());
    }
    match row {
        VisibleRow::Agent(agent_row) => {
            let card = agent_row.card;
            let offline = !model.host_online(card.agent.host_id);
            let base = if offline { dim() } else { plain() };
            // A folded family wears the family's badge, not the parent's:
            // the row is standing in for everyone behind it.
            let (badge, badge_style) = match agent_row.folded {
                Some(folded) => badge_glyph(folded.attention),
                None => badge_for(model, card),
            };
            if badge != " " {
                push_span(&mut line, BADGE_COL, badge, badge_style);
            }
            // Descendants indent one step per generation, and the `⋯N`
            // marker on a folded row eats into the same name column, so a
            // family never pushes the grid out of alignment.
            let indent = agent_row.depth * FAMILY_INDENT;
            let marker = agent_row
                .folded
                .map(|folded| format!(" ⋯{}", folded.hidden))
                .unwrap_or_default();
            let name_width = NAME_WIDTH.saturating_sub(indent + str_width(&marker));
            let name = match &view.mode {
                Mode::Rename { agent, draft } if *agent == card.agent.id => {
                    clip(&format!("{draft}▌"), name_width)
                }
                _ => clip(&card.display_name(), name_width),
            };
            push_span(&mut line, NAME_COL + indent, name, base);
            if !marker.is_empty() {
                line.spans
                    .push(Span::styled(marker, base.add_modifier(Modifier::DIM)));
            }
            push_span(
                &mut line,
                TYPE_COL,
                clip(&card.agent.agent_type, TYPE_WIDTH),
                base.add_modifier(Modifier::DIM),
            );
            let host = model
                .host_name(card.agent.host_id)
                .map(str::to_string)
                .unwrap_or_else(|| "?".to_string());
            push_span(&mut line, HOST_COL, clip(&host, HOST_WIDTH), base);
            push_span(
                &mut line,
                AGE_COL,
                clip(&format_relative_age(ctx.now, card.last_activity), AGE_WIDTH),
                base.add_modifier(Modifier::DIM),
            );
            if show_status {
                push_span(
                    &mut line,
                    STATUS_COL,
                    clip(&model.status_label_for(card), STATUS_WIDTH),
                    base.add_modifier(Modifier::DIM),
                );
            }
            if show_working
                && let Some(text) =
                    working_text(card, ctx.now, width.saturating_sub(2 + WORKING_COL))
            {
                push_span(
                    &mut line,
                    WORKING_COL,
                    text,
                    base.add_modifier(Modifier::DIM),
                );
            }
        }
        VisibleRow::PendingCreate {
            name,
            agent_type,
            host,
        } => {
            push_span(&mut line, BADGE_COL, "◌", dim());
            push_span(
                &mut line,
                NAME_COL,
                clip(&format!("{name} (creating…)"), NAME_WIDTH),
                dim(),
            );
            push_span(
                &mut line,
                TYPE_COL,
                clip(agent_type_label(agent_type), TYPE_WIDTH),
                dim(),
            );
            let host = host
                .or(model.local_host_id())
                .and_then(|id| model.host_name(id))
                .unwrap_or("?")
                .to_string();
            push_span(&mut line, HOST_COL, clip(&host, HOST_WIDTH), dim());
            push_span(&mut line, AGE_COL, "—", dim());
        }
    }
    line
}

fn banner_line(model: &Model, width: usize) -> Line<'static> {
    if model.has_invariant_warning() {
        return invariant_warning_line(width, Style::default().fg(Color::Yellow));
    }
    let mut line = new_line();
    if model.cloud_subscription_required() && model.is_connected() {
        push_span(
            &mut line,
            MARKER_COL,
            "⚠ subscription required · amux.sh/account · local agents fine",
            Style::default().fg(Color::Yellow),
        );
    } else if model.cloud_auth_required() && model.is_connected() {
        push_span(
            &mut line,
            MARKER_COL,
            "⚠ cloud: auth required — run `amux init` · local agents fine",
            Style::default().fg(Color::Yellow),
        );
    }
    finish_line(&mut line, width);
    line
}

pub(crate) fn invariant_warning_line(width: usize, style: Style) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, MARKER_COL, INVARIANT_WARNING, style);
    finish_line(&mut line, width);
    line
}

/// The status-line failure to surface, if any: the most recent op failure
/// the view has not dismissed.
fn active_failure(model: &Model, view: &ViewState) -> Option<(String, String)> {
    let failure = model.latest_op_failure()?;
    if failure.seq <= view.dismissed_error_seq {
        return None;
    }
    let amux_ui::OpOutcome::Error { error } = &failure.outcome else {
        return None;
    };
    Some((
        command_verb(&failure.command).to_string(),
        error.message.clone(),
    ))
}

fn command_verb(command: &Command) -> &'static str {
    match command {
        Command::CreateAgent { .. } => "create",
        Command::RenameAgent { .. } => "rename",
        Command::DeleteAgent { .. } => "delete",
        // Chat write-path commands (`docs/CHAT.md` C5/D3/D4); the chat
        // screen itself arrives in Phase 4 — until then a failure still
        // states its verb honestly in the chrome status line.
        Command::Claude(amux_ui::ClaudeCommand::SendPrompt { .. }) => "send",
        Command::Claude(amux_ui::ClaudeCommand::AnswerAsk { .. }) => "answer",
        Command::Claude(amux_ui::ClaudeCommand::Interrupt { .. }) => "interrupt",
        Command::Claude(amux_ui::ClaudeCommand::CyclePermissionMode { .. }) => "mode cycle",
        Command::Codex(amux_ui::CodexCommand::Prompt { .. }) => "send",
        Command::Codex(amux_ui::CodexCommand::Steer { .. }) => "steer",
        Command::Codex(amux_ui::CodexCommand::Answer { .. }) => "answer",
        Command::Codex(amux_ui::CodexCommand::Interrupt { .. }) => "interrupt",
    }
}

fn status_line(model: &Model, view: &ViewState, width: usize) -> Line<'static> {
    let mut line = new_line();

    // The armed quit guard replaces the hint line in warning color: a
    // second Ctrl+C within the window quits; any other key (or the
    // timeout tick) disarms and the hints return.
    if view.quit_guard.is_armed() {
        push_span(
            &mut line,
            MARKER_COL,
            "⚠",
            Style::default().fg(Color::Yellow),
        );
        push_span(
            &mut line,
            BADGE_COL,
            QuitGuard::HINT,
            Style::default().fg(Color::Yellow),
        );
        finish_line(&mut line, width);
        return line;
    }
    if let Some(notice) = &view.notice {
        push_span(&mut line, MARKER_COL, "✗", Style::default().fg(Color::Red));
        push_span(&mut line, BADGE_COL, notice.clone(), plain());
        finish_line(&mut line, width);
        return line;
    }
    if let Some((verb, message)) = active_failure(model, view) {
        push_span(&mut line, MARKER_COL, "✗", Style::default().fg(Color::Red));
        push_span(
            &mut line,
            BADGE_COL,
            format!("{verb} failed: {message}"),
            plain(),
        );
        finish_line(&mut line, width);
        return line;
    }
    if let Mode::ConfirmDelete { name, .. } = &view.mode {
        push_span(
            &mut line,
            MARKER_COL,
            "●",
            Style::default().fg(Color::Green),
        );
        push_span(&mut line, BADGE_COL, format!("delete {name}? y/n"), plain());
        finish_line(&mut line, width);
        return line;
    }

    let (dot, dot_style, summary) = match model.connection() {
        Connection::Connected { .. } => {
            let hosts = model.host_count();
            let plural = if hosts == 1 { "" } else { "s" };
            (
                "●",
                Style::default().fg(Color::Green),
                format!("connected · {hosts} host{plural}"),
            )
        }
        Connection::Connecting => ("◌", dim(), "connecting".to_string()),
        Connection::Disconnected { .. } => (
            "✗",
            Style::default().fg(Color::Red),
            "disconnected".to_string(),
        ),
    };
    push_span(&mut line, MARKER_COL, dot, dot_style);
    push_span(&mut line, BADGE_COL, summary, plain());

    let hints = match &view.mode {
        // `z` joins the row only where something folds, and only when
        // the row still fits: a hint that names a dead key is a lie, and
        // a hint block that vanishes wholesale to make room for one more
        // is a worse trade than leaving that one to the `?` overlay.
        Mode::Normal => {
            let plain = "n new  r rename  d delete  q quit  ? help";
            let with_fold = "n new  r rename  d delete  z fold  q quit  ? help";
            match has_families(model) && fits(with_fold, width) {
                true => with_fold,
                false => plain,
            }
        }
        // "open", not "attach": Enter opens the settings-default mode
        // (A1), which may be the chat — the hint stays truthful either
        // way.
        Mode::Filter => "esc nav-mode  enter open",
        Mode::Rename { .. } => "enter apply  esc cancel",
        Mode::ConfirmDelete { .. } => "",
        Mode::Help => "any key to close",
    };
    if !hints.is_empty() && fits(hints, width) {
        push_span(&mut line, HINTS_COL, hints, dim());
    }
    finish_line(&mut line, width);
    line
}

/// Whether a hint block fits the status line beside the connection
/// summary.
fn fits(hints: &str, width: usize) -> bool {
    HINTS_COL + hints.chars().count() <= width.saturating_sub(2)
}

/// Whether anything on this fleet has children — the fact `z` exists on:
/// with no family on screen the fold key does nothing anywhere, so the
/// overlay does not name it (P10).
fn has_families(model: &Model) -> bool {
    model
        .fleet()
        .iter()
        .any(|item| matches!(item, amux_ui::FleetItem::Family { .. }))
}

/// The fleet help overlay, derived from the one binding table — kitty
/// rows appear only when the probe succeeded, the configured leader
/// substitutes into chords, and the entry rows name the effective modes
/// (P10: hints tell the truth).
fn help_lines(model: &Model, view: &ViewState, _width: usize) -> Vec<Line<'static>> {
    let sections = crate::bindings::fleet_sections(
        &crate::bindings::Effective::new(view.kitty, view.leader),
        view.default_open_mode,
        has_families(model),
    );
    let mut lines = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            lines.push(new_line());
        }
        for binding in &section.bindings {
            let mut line = new_line();
            push_span(
                &mut line,
                BADGE_COL,
                format!("{:<14}", binding.keys),
                plain(),
            );
            push_span(&mut line, BADGE_COL + 14, binding.action.clone(), dim());
            if let Some(mark) = tier_mark(binding.tier) {
                line.spans.push(Span::styled(format!(" · {mark}"), dim()));
            }
            lines.push(line);
        }
    }
    lines
}

/// The overlay's tier annotation: ext is marked terminal-dependent,
/// kitty named (a kitty row only exists when delivered); plain is the
/// unmarked default.
pub(crate) fn tier_mark(tier: crate::bindings::Tier) -> Option<&'static str> {
    match tier {
        crate::bindings::Tier::Plain => None,
        crate::bindings::Tier::Ext => Some("terminal-dependent"),
        crate::bindings::Tier::Kitty => Some("kitty"),
    }
}

/// The cascade, before it happens (U6): who else goes, and which of them
/// is mid-task. Deleting a parent takes its whole subtree, and a folded
/// family is one row on screen — so a confirmation that named only the
/// selected agent would be asking the human to approve something they
/// cannot see.
///
/// Nothing here blocks. An idle child is listed and costs no extra
/// keystroke, and a working one is flagged rather than refused: the
/// person is looking straight at the list, which is a better guard than
/// a second prompt. (The CLI, where nobody is looking, refuses instead.)
fn confirm_delete_lines(
    model: &Model,
    ctx: &FrameContext,
    agent: AgentId,
    width: usize,
    capacity: usize,
) -> Vec<Line<'static>> {
    let family = model.family_of(agent);
    let name = model
        .agent(agent)
        .map(amux_ui::AgentCard::display_name)
        .unwrap_or_else(|| "this agent".to_string());
    let working = family
        .iter()
        .filter(|member| model.effective_attention(member.card) == Attention::Working)
        .count();

    let mut lines = vec![blank_line(width)];
    let mut heading = new_line();
    push_span(
        &mut heading,
        MARKER_COL,
        "⚠",
        Style::default().fg(Color::Yellow),
    );
    push_span(
        &mut heading,
        BADGE_COL,
        match family.len() {
            // "under", not "it started": the cascade recurses, so most of
            // this list is somebody else's children.
            1 => format!("deleting {name} also deletes the agent under it:"),
            n => format!("deleting {name} also deletes the {n} agents under it:"),
        },
        plain(),
    );
    lines.push(heading);
    lines.push(blank_line(width));

    // Three rows are spent on chrome above and two on the tail; whatever
    // is left goes to the list. A confirmation that quietly drops names
    // is the one thing this screen must not do, so an elision counts what
    // it hid.
    let room = capacity.saturating_sub(5);
    let shown = if family.len() > room {
        room.saturating_sub(1)
    } else {
        family.len()
    };
    for member in family.iter().take(shown) {
        lines.push(confirm_delete_row(model, ctx, member, width));
    }
    if shown < family.len() {
        let mut more = new_line();
        push_span(
            &mut more,
            NAME_COL,
            format!("… and {} more", family.len() - shown),
            dim(),
        );
        lines.push(more);
    }

    lines.push(blank_line(width));
    let mut tail = new_line();
    let (text, style) = match working {
        0 => ("none of them is working".to_string(), dim()),
        1 => (
            "1 is working — deleting stops it".to_string(),
            Style::default().fg(Color::Yellow),
        ),
        n => (
            format!("{n} are working — deleting stops them"),
            Style::default().fg(Color::Yellow),
        ),
    };
    push_span(&mut tail, BADGE_COL, text, style);
    lines.push(tail);
    lines
}

/// One agent the cascade would take: indented to its generation like the
/// fleet indents an open family, flagged when it is working, and saying
/// what it says it is doing so the flag is actionable rather than
/// alarming.
fn confirm_delete_row(
    model: &Model,
    ctx: &FrameContext,
    member: &amux_ui::FamilyMember<'_>,
    width: usize,
) -> Line<'static> {
    let card = member.card;
    let attention = model.effective_attention(card);
    let indent = member.depth.saturating_sub(1) * FAMILY_INDENT;
    let mut line = new_line();
    if attention == Attention::Working {
        push_span(
            &mut line,
            BADGE_COL + indent,
            "●",
            Style::default().fg(Color::Yellow),
        );
    }
    push_span(&mut line, NAME_COL + indent, card.display_name(), plain());
    let mut detail = format!(
        " · {} · {}",
        card.agent.agent_type,
        model.status_label_for(card)
    );
    if let Some(claim) = card.working_on() {
        detail.push_str(&format!(
            " · {} {}",
            claim.text,
            format_relative_age(ctx.now, claim.updated_at)
        ));
    }
    let room = width.saturating_sub(2 + line_len(&line));
    line.spans.push(Span::styled(
        clip_to_width(&detail, room).to_string(),
        dim(),
    ));
    line
}

fn centered_lines(
    messages: &[(String, Style)],
    width: usize,
    capacity: usize,
) -> Vec<Line<'static>> {
    let top = capacity.saturating_sub(messages.len()) / 2;
    let mut lines = Vec::new();
    for _ in 0..top {
        lines.push(new_line());
    }
    for (text, style) in messages {
        let mut line = new_line();
        let col = (width.saturating_sub(text.chars().count())) / 2;
        push_span(&mut line, col.max(MARKER_COL), text.clone(), *style);
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use amux_ui::Model;
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use super::{FrameContext, INVARIANT_WARNING, Theme, build_lines};
    use crate::{ChatView, ViewState};

    fn model_with_invariant_warning() -> Model {
        // The runtime setter is deliberately crate-private: renderers get a
        // read-only fact. Serde supplies a focused fixture without widening
        // that production boundary.
        let mut value = serde_json::to_value(Model::default()).expect("serialize Model fixture");
        value["invariant_warning"] = serde_json::Value::Bool(true);
        serde_json::from_value(value).expect("deserialize warning Model fixture")
    }

    fn context() -> FrameContext {
        FrameContext {
            viewport: (80, 24),
            theme: Theme::default(),
            now: DateTime::<Utc>::from_timestamp(1_754_697_600, 0).expect("fixture time"),
        }
    }

    fn frame_contains_warning(model: &Model, view: &ViewState) -> bool {
        build_lines(model, view, &context()).iter().any(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .contains(INVARIANT_WARNING)
        })
    }

    #[test]
    fn invariant_warning_is_visible_in_fleet_and_both_native_chats() {
        let model = model_with_invariant_warning();
        assert!(frame_contains_warning(&model, &ViewState::default()));

        let claude = ViewState {
            chat: Some(ChatView::open_claude(Uuid::from_u128(1), 'a', false)),
            ..ViewState::default()
        };
        assert!(frame_contains_warning(&model, &claude));

        let codex = ViewState {
            chat: Some(ChatView::open_codex(Uuid::from_u128(2), 'a', false)),
            ..ViewState::default()
        };
        assert!(frame_contains_warning(&model, &codex));
    }
}
