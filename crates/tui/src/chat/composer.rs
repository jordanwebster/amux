//! The bottom of the chat: the activity line, the session strip, foot
//! cards, the tray of queued and unconfirmed prompts, and the composer.

use ratatui::text::{Line, Span};
use ui_state::{Activity, ActivityKind, Composer, Waiting};
use ui_view::{Away, OutboxRow, OutboxState, QueuedRow, Segment, Strip, composer_tokens};
use wire::SignInState;

use super::rows::chip;
use crate::editor::Editor;
use crate::text::{self, push, push_right};
use crate::theme::Theme;

/// Most lines the composer grows to before it scrolls.
pub const COMPOSER_LINES: usize = 6;

/// "Running cargo test · 12s", with the moving glyph.
pub fn activity_line(
    activity: &Activity,
    running: Option<&str>,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let frames = ['◐', '◓', '◑', '◒'];
    let glyph = frames[((activity.elapsed_ms / 1_000) % 4) as usize];
    let elapsed = text::duration(activity.elapsed_ms - activity.elapsed_ms % 1_000);
    let words = match &activity.kind {
        ActivityKind::Working => format!("Working · {elapsed}"),
        ActivityKind::Thinking => format!("Thinking · {elapsed}"),
        ActivityKind::Running { .. } => match running {
            Some(subject) if !subject.is_empty() => format!("Running {subject} · {elapsed}"),
            _ => format!("Running · {elapsed}"),
        },
        ActivityKind::Subagents { count } => format!(
            "{count} subagent{} working · {elapsed}",
            if *count == 1 { "" } else { "s" }
        ),
        ActivityKind::Compacting => format!("Compacting · {elapsed}"),
        ActivityKind::Retrying {
            attempt,
            max_attempts,
            ..
        } if *max_attempts > 0 => format!("Retrying · attempt {attempt} of {max_attempts}"),
        ActivityKind::Retrying { attempt, .. } => format!("Retrying · attempt {attempt}"),
    };
    let mut line = Line::from(Span::styled(format!("  {glyph} "), theme.accent()));
    push(&mut line, words, theme.muted(), width);
    push_right(&mut line, "ctrl+x stop", theme.muted(), width);
    line
}

/// The strip's facts on one line; None when there is nothing to say.
pub fn strip_line(strip: &Strip, width: usize, theme: Theme) -> Option<Line<'static>> {
    let mut parts = Vec::new();
    if let Some(tasks) = &strip.tasks {
        let mut words = format!("Tasks {}/{}", tasks.done, tasks.total);
        if !tasks.current.is_empty() {
            words.push_str(&format!(" · {}", tasks.current));
        }
        parts.push((words, theme.muted()));
    }
    if let Some(context) = strip.context.as_ref().filter(|c| c.in_strip)
        && let Some(percent) = context.percent
    {
        parts.push((format!("{percent}% context"), theme.warn()));
    }
    if let Some(count) = strip.background {
        parts.push((format!("{count} in background"), theme.muted()));
    }
    if let Some(usage) = strip.usage.as_ref().filter(|u| !u.blocked) {
        let near = usage
            .windows
            .iter()
            .map(|(name, used, _)| format!("{name} {used:.0}%"))
            .collect::<Vec<_>>()
            .join(" · ");
        parts.push((format!("Near the usage limit · {near}"), theme.warn()));
    }
    for server in &strip.failed_servers {
        let words = if server.needs_auth {
            format!("{} needs sign-in", server.name)
        } else {
            format!("{} failed to start", server.name)
        };
        parts.push((words, theme.warn()));
    }
    if parts.is_empty() {
        return None;
    }
    let mut line = Line::from(Span::raw("    "));
    for (i, (words, style)) in parts.into_iter().enumerate() {
        if i > 0 {
            push(&mut line, " · ", theme.muted(), width);
        }
        push(&mut line, words, style, width);
    }
    Some(line)
}

/// Foot cards: a sign-in problem, or usage blocked.
pub fn foot_cards(strip: &Strip, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(sign_in) = &strip.sign_in {
        let what = match sign_in.state {
            SignInState::Expired => "Sign-in expired",
            SignInState::Failed => "Sign-in failed",
            _ => "Signed out",
        };
        let mut line = Line::from(Span::styled("  ! ", theme.error()));
        let mut words = what.to_owned();
        if !sign_in.account.is_empty() {
            words.push_str(&format!(" · {}", sign_in.account));
        }
        push(&mut line, words, theme.emphasis(), width);
        lines.push(line);
        if !sign_in.message.is_empty() {
            let mut line = Line::from(Span::raw("    "));
            push(&mut line, sign_in.message.clone(), theme.muted(), width);
            lines.push(line);
        }
    }
    if let Some(usage) = strip.usage.as_ref().filter(|u| u.blocked) {
        let mut line = Line::from(Span::styled("  ! ", theme.error()));
        push(&mut line, "Usage limit reached", theme.emphasis(), width);
        if let Some(credits) = &usage.credits {
            push(&mut line, format!(" · {credits}"), theme.muted(), width);
        }
        lines.push(line);
    }
    lines
}

fn first_text(segments: &[Segment]) -> String {
    let mut out = String::new();
    for segment in segments {
        match segment {
            Segment::Text(text) => out.push_str(text),
            Segment::Attachment(view) => out.push_str(&chip(view)),
        }
    }
    text::first_line(&out).to_owned()
}

/// One entry of the tray under the feed.
#[derive(Clone, Debug, PartialEq)]
pub enum TrayRow {
    Queued(QueuedRow),
    Outbox(OutboxRow),
}

impl TrayRow {
    pub fn hint(&self) -> &'static str {
        match self {
            TrayRow::Queued(row) if row.can_send_now => "enter send now · w withdraw · esc back",
            TrayRow::Queued(_) => "esc back",
            TrayRow::Outbox(row) => match row.state {
                OutboxState::NotConfirmed => "r resend · d discard · esc back",
                OutboxState::Rejected(_) => "e edit · d discard · esc back",
                OutboxState::Sending => "esc back",
            },
        }
    }

    pub fn line(&self, selected: bool, width: usize, theme: Theme) -> Line<'static> {
        let mut line = Line::from(Span::styled(
            if selected { "› " } else { "  " },
            theme.accent(),
        ));
        match self {
            TrayRow::Queued(row) => {
                let label = if row.steered {
                    "steered".to_owned()
                } else {
                    match &row.from_agent {
                        Some(agent) => format!("queued from {agent}"),
                        None => "queued".to_owned(),
                    }
                };
                push(&mut line, "⋯ ", theme.muted(), width);
                push(&mut line, format!("{label}  "), theme.muted(), width);
                push(&mut line, first_text(&row.text), theme.text(), width);
                if selected && row.can_send_now {
                    push_right(&mut line, "send now · withdraw", theme.muted(), width);
                }
            }
            TrayRow::Outbox(row) => {
                let (glyph, label, style) = match &row.state {
                    OutboxState::Sending => ("◌ ", "sending".to_owned(), theme.muted()),
                    OutboxState::NotConfirmed => ("! ", "not confirmed".to_owned(), theme.warn()),
                    OutboxState::Rejected(reason) => {
                        ("✗ ", format!("not sent: {reason}"), theme.error())
                    }
                };
                push(&mut line, glyph, style, width);
                push(&mut line, format!("{label}  "), style, width);
                push(&mut line, first_text(&row.text), theme.text(), width);
                if row.state == OutboxState::NotConfirmed {
                    push_right(&mut line, "resend · discard", theme.muted(), width);
                }
            }
        }
        line
    }
}

/// What the empty composer says, and whether it can send.
pub fn placeholder(mode: &Composer, name: &str, host: &str, away: Away) -> String {
    match mode {
        Composer::Send => format!("Message {name}"),
        Composer::Resume => format!("{name} has exited · type to resume it with a message"),
        Composer::Disabled(Waiting::CatchingUp) => "Catching up · your draft is kept".into(),
        Composer::Disabled(Waiting::Detached) => match away {
            Away::Plain => {
                format!("{host} is away · your draft is kept; sending waits until it is back")
            }
            Away::Revoked => format!(
                "{host} no longer trusts this machine · your draft is kept; sending waits until you pair again"
            ),
            Away::SignedOut => format!(
                "{host} is away · this machine is signed out · your draft is kept; sending waits until you sign in again"
            ),
        },
        Composer::Disabled(Waiting::Reconnecting) => {
            "Reconnecting to amux · your draft is kept".into()
        }
    }
}

/// The composer's lines at `width` and where its cursor is: text runs and
/// attachment chips at their places, a chip one cursor position.
pub fn editor_lines(
    editor: &Editor,
    placeholder: &str,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, (usize, usize)) {
    let lead = "▎ ";
    let room = width.saturating_sub(2).max(1);
    if editor.is_empty() {
        let mut line = Line::from(Span::styled(lead, theme.accent()));
        push(&mut line, placeholder, theme.muted(), width);
        return (vec![line], (0, 2));
    }
    let cursor = editor.cursor_chars();
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    let mut at = 0usize;
    let mut position = (0usize, 0usize);
    let place =
        |rows: &Vec<Vec<Span<'static>>>, used: usize, at: usize, position: &mut (usize, usize)| {
            if at == cursor {
                *position = (rows.len() - 1, used);
            }
        };
    for segment in composer_tokens(editor.text(), editor.attachments()) {
        match segment {
            Segment::Text(text) => {
                for c in text.chars() {
                    place(&rows, used, at, &mut position);
                    at += 1;
                    if c == '\n' {
                        rows.push(Vec::new());
                        used = 0;
                        continue;
                    }
                    let shown = if editor.secret { '•' } else { c };
                    let w = text::str_width(shown.encode_utf8(&mut [0; 4]));
                    if used + w > room {
                        rows.push(Vec::new());
                        used = 0;
                    }
                    if let Some(row) = rows.last_mut() {
                        row.push(Span::styled(shown.to_string(), theme.text()));
                    }
                    used += w;
                }
            }
            Segment::Attachment(view) => {
                let chip = text::ellipsize(&chip(&view), room);
                let w = text::str_width(&chip);
                if used > 0 && used + w > room {
                    rows.push(Vec::new());
                    used = 0;
                }
                place(&rows, used, at, &mut position);
                at += 1;
                if let Some(row) = rows.last_mut() {
                    row.push(Span::styled(chip, theme.code()));
                }
                used += w;
            }
        }
    }
    place(&rows, used, at, &mut position);
    let lines = rows
        .into_iter()
        .enumerate()
        .map(|(i, spans)| {
            let mut line = Line::from(Span::styled(
                if i == 0 { lead } else { "  " },
                theme.accent(),
            ));
            line.spans.extend(spans);
            line
        })
        .collect();
    (lines, (position.0, position.1 + 2))
}
