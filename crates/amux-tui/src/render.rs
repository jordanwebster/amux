//! The fleet renderer: a pure function of (Model, ViewState, FrameContext).
//!
//! The layout grid reproduces the aligned frames in the TUI V1 spec
//! verbatim; the tier-3 golden tests lock every screen. The chrome draws on
//! the alternate screen only and never writes terminal scrollback.

use amux_ui::{
    AgentPhase, Attention, Command, Connection, DisconnectReason, Model, Why, agent_type_label,
    format_relative_age,
};
use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::view::{Mode, ViewState, VisibleRow, visible_rows};

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
/// The status-word column collapses first on narrow terminals: shown only
/// when the full grid fits.
const STATUS_MIN_FRAME_WIDTH: usize = 68;
/// Header right block ("5 agents" / "1/5") is left-anchored here.
const RIGHT_INFO_FROM_EDGE: usize = 13;
/// Below this width the column grid cannot lay out (the right-info block
/// anchors at `width - RIGHT_INFO_FROM_EDGE`, which must not underflow):
/// the frame degrades to the too-small notice instead.
const MIN_FRAME_WIDTH: usize = RIGHT_INFO_FROM_EDGE;
/// Key hints in the status line.
const HINTS_COL: usize = 31;

/// Rows of chrome that are not list rows: two borders, filter line, spacer,
/// banner/spacer, status line.
const CHROME_ROWS: usize = 6;

/// Placeholder for future theming; styles are currently fixed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Theme;

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
/// nested widgets, so goldens control every cell).
pub fn build_lines(model: &Model, view: &ViewState, ctx: &FrameContext) -> Vec<Line<'static>> {
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
        ScreenState::Help => help_lines(view, width),
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

fn bottom_border(width: usize) -> Line<'static> {
    let mut line = String::from("└");
    while line.chars().count() < width - 1 {
        line.push('─');
    }
    line.push('┘');
    Line::from(Span::styled(line, dim()))
}

fn blank_line(width: usize) -> Line<'static> {
    let mut line = new_line();
    finish_line(&mut line, width);
    line
}

/// A content line: left border plus spans; `finish_line` pads and closes it.
fn new_line() -> Line<'static> {
    Line::from(vec![Span::styled("│", dim())])
}

fn line_len(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum()
}

fn pad_to(line: &mut Line<'static>, col: usize) {
    let len = line_len(line);
    if col > len {
        line.spans.push(Span::raw(" ".repeat(col - len)));
    }
}

fn push_span(line: &mut Line<'static>, col: usize, text: impl Into<String>, style: Style) {
    pad_to(line, col);
    line.spans.push(Span::styled(text.into(), style));
}

fn finish_line(line: &mut Line<'static>, width: usize) {
    pad_to(line, width - 1);
    // Drop overflow defensively: goldens keep us honest about fit.
    let mut len = 0usize;
    for span in line.spans.iter_mut() {
        let span_len = span.content.chars().count();
        if len + span_len > width - 1 {
            let keep = (width - 1).saturating_sub(len);
            span.content = span.content.chars().take(keep).collect::<String>().into();
        }
        len += span.content.chars().count();
    }
    line.spans.retain(|span| !span.content.is_empty());
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
    match model.effective_attention(card) {
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

fn fleet_row_line(
    model: &Model,
    view: &ViewState,
    ctx: &FrameContext,
    row: &VisibleRow<'_>,
    selected: bool,
) -> Line<'static> {
    let width = ctx.viewport.0 as usize;
    let show_status = width >= STATUS_MIN_FRAME_WIDTH;
    let renaming = matches!(
        (&view.mode, row),
        (Mode::Rename { agent, .. }, VisibleRow::Agent(card)) if *agent == card.agent.id
    );
    let mut line = new_line();
    if selected && !renaming {
        push_span(&mut line, MARKER_COL, "▸", plain());
    }
    match row {
        VisibleRow::Agent(card) => {
            let offline = !model.host_online(card.agent.host_id);
            let base = if offline { dim() } else { plain() };
            let (badge, badge_style) = badge_for(model, card);
            if badge != " " {
                push_span(&mut line, BADGE_COL, badge, badge_style);
            }
            let name = match &view.mode {
                Mode::Rename { agent, draft } if *agent == card.agent.id => {
                    clip(&format!("{draft}▌"), NAME_WIDTH)
                }
                _ => clip(&card.display_name(), NAME_WIDTH),
            };
            push_span(&mut line, NAME_COL, name, base);
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
                    model.status_label_for(card),
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
    let mut line = new_line();
    if model.cloud_auth_required() && model.is_connected() {
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
    }
}

fn status_line(model: &Model, view: &ViewState, width: usize) -> Line<'static> {
    let mut line = new_line();

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
        Mode::Normal => "n new  r rename  d delete  ? help",
        Mode::Filter => "esc nav-mode  enter attach",
        Mode::Rename { .. } => "enter apply  esc cancel",
        Mode::ConfirmDelete { .. } => "",
        Mode::Help => "any key to close",
    };
    if !hints.is_empty() && HINTS_COL + hints.chars().count() <= width - 2 {
        push_span(&mut line, HINTS_COL, hints, dim());
    }
    finish_line(&mut line, width);
    line
}

fn help_lines(view: &ViewState, _width: usize) -> Vec<Line<'static>> {
    let leader = view_leader(view);
    let rows = vec![
        ("j/k or ↑/↓".to_string(), "move".to_string()),
        ("gg / G".to_string(), "top / bottom".to_string()),
        ("/ or i".to_string(), "filter".to_string()),
        ("enter".to_string(), "attach".to_string()),
        ("n".to_string(), "new agent".to_string()),
        ("r".to_string(), "rename selected".to_string()),
        ("d".to_string(), "delete selected".to_string()),
        ("C-g".to_string(), "debug dump".to_string()),
        ("q".to_string(), "quit".to_string()),
        (String::new(), String::new()),
        (format!("{leader} d"), "detach from session".to_string()),
        (format!("{leader} s"), "back to fleet".to_string()),
    ];
    rows.into_iter()
        .map(|(key, label)| {
            let mut line = new_line();
            if !key.is_empty() {
                push_span(&mut line, BADGE_COL, format!("{key:<14}"), plain());
                push_span(&mut line, BADGE_COL + 14, label, dim());
            }
            line
        })
        .collect()
}

fn view_leader(view: &ViewState) -> String {
    view.leader_label.clone()
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
