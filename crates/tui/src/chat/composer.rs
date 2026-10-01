//! The bottom of the chat: the activity line, the session strip, foot
//! cards, the tray of queued and unconfirmed prompts, and the composer.

use ratatui::text::{Line, Span};
use ui_state::{Activity, ActivityKind, Composer, Waiting};
use ui_view::{Away, QueuedRow, Segment, Strip, composer_tokens};
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
    let mut line = Line::from(Span::styled(format!("  {glyph} "), theme.accent()));
    // A running call names its subject in the code face, as its row does.
    if let (ActivityKind::Running { .. }, Some(subject)) = (&activity.kind, running)
        && !subject.is_empty()
    {
        let tail = format!(" · {elapsed}");
        let room =
            width.saturating_sub(text::str_width(&tail) + text::str_width("ctrl+x stop") + 2);
        push(&mut line, "Running ", theme.muted(), room);
        push(&mut line, subject, theme.code(), room);
        push(&mut line, tail, theme.muted(), width);
        push_right(&mut line, "ctrl+x stop", theme.muted(), width);
        return line;
    }
    let words = match &activity.kind {
        ActivityKind::Working => format!("Working · {elapsed}"),
        ActivityKind::Thinking => format!("Thinking · {elapsed}"),
        ActivityKind::Running { .. } => format!("Running · {elapsed}"),
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
        parts.push((format!("{percent}% context"), theme.warning()));
    }
    if let Some(count) = strip.background {
        parts.push((format!("{count} in background"), theme.muted()));
    }
    if let Some(usage) = strip.usage.as_ref().filter(|u| !u.blocked) {
        let near = usage
            .windows
            .iter()
            .map(|window| format!("{} {:.0}%", window.name, window.used_percent))
            .collect::<Vec<_>>()
            .join(" · ");
        parts.push((format!("Near the usage limit · {near}"), theme.warning()));
    }
    for server in &strip.failed_servers {
        let words = if server.needs_auth {
            format!("{} needs sign-in", server.name)
        } else {
            format!("{} failed to start", server.name)
        };
        parts.push((words, theme.warning()));
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

/// Columns between the groups of the row above the composer.
const GROUP_GAP: usize = 4;

/// The row that sits on the composer's box, starting on its border's
/// column so it reads as the composer's and not the feed's: what is still
/// in flight in this chat at the left (the task in progress and how many
/// are done, failed tool servers, background jobs), set apart by space
/// rather than dots, and at the right the usage limit, only near it and
/// only while `typing`, since that is when it bears on a choice. None when
/// there is nothing to say.
pub fn edge_row(
    strip: &Strip,
    typing: bool,
    now_ms: i64,
    width: usize,
    theme: Theme,
) -> Option<Line<'static>> {
    const MARGIN: usize = 2;
    // The task's name is the one part that gives way: it shortens, then
    // drops, before anything else does.
    let mut name: Option<String> = None;
    let mut groups: Vec<Vec<(String, ratatui::style::Style)>> = Vec::new();
    if let Some(tasks) = strip.tasks.as_ref().filter(|t| t.done < t.total) {
        let count = format!("{} of {} tasks", tasks.done, tasks.total);
        if !tasks.current.is_empty() {
            name = Some(tasks.current.clone());
        }
        groups.push(vec![(count, theme.faint())]);
    }
    for server in &strip.failed_servers {
        let words = if server.needs_auth {
            format!("{} needs sign-in", server.name)
        } else {
            format!("{} failed to start", server.name)
        };
        groups.push(vec![(words, theme.error())]);
    }
    if let Some(count) = strip.background {
        let s = if count == 1 { "" } else { "s" };
        groups.push(vec![(format!("{count} background job{s}"), theme.faint())]);
    }
    // The key that unfolds the row into the pane, named last.
    if !groups.is_empty() {
        groups.push(vec![("ctrl+o".to_owned(), theme.faint())]);
    }
    let usage = strip
        .usage
        .as_ref()
        .filter(|usage| typing && !usage.blocked)
        .and_then(|usage| {
            usage
                .windows
                .iter()
                .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
        })
        .map(|window| usage_words(window, now_ms, theme));
    if groups.is_empty() && usage.is_none() {
        return None;
    }
    let end = width.saturating_sub(MARGIN);
    let right_width = usage
        .as_ref()
        .map(|spans| spans.iter().map(|(w, _)| text::str_width(w)).sum::<usize>())
        .unwrap_or(0);
    let room = if right_width > 0 {
        end.saturating_sub(right_width + GROUP_GAP)
    } else {
        end
    };
    let fixed: usize = MARGIN
        + groups
            .iter()
            .flatten()
            .map(|(words, _)| text::str_width(words))
            .sum::<usize>()
        + GROUP_GAP * groups.len().saturating_sub(1);
    const NAME_MIN: usize = 12;
    if let Some(name) = name {
        let name_room = room.saturating_sub(fixed + 2);
        if name_room >= NAME_MIN {
            let first = &mut groups[0];
            first[0].0 = format!("  {}", first[0].0);
            first.insert(0, (text::ellipsize(&name, name_room), theme.muted()));
        }
    }
    let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
    for (i, group) in groups.into_iter().enumerate() {
        if i > 0 {
            push(&mut line, " ".repeat(GROUP_GAP), theme.faint(), room);
        }
        for (words, style) in group {
            push(&mut line, words, style, room);
        }
    }
    if let Some(spans) = usage
        && text::line_width(&line) + GROUP_GAP + right_width <= end
    {
        text::pad_to(&mut line, end - right_width);
        for (words, style) in spans {
            line.spans.push(Span::styled(words, style));
        }
    }
    Some(line)
}

/// "5-hour limit 81% used · resets 22:56": one usage window in words.
fn usage_words(
    window: &ui_view::UsageWindowView,
    now_ms: i64,
    theme: Theme,
) -> Vec<(String, ratatui::style::Style)> {
    let name = limit_name(&window.name);
    let mut spans = vec![
        (name, theme.muted()),
        (format!(" {:.0}% used", window.used_percent), theme.text()),
    ];
    if let Some(at) = window.resets_at_ms.filter(|at| *at > now_ms) {
        spans.push((format!(" · resets {}", resets(at, now_ms)), theme.faint()));
    }
    spans
}

/// A usage window by its name: "5h" is the 5-hour limit, "7d" the weekly
/// one; the provider's own name otherwise.
fn limit_name(name: &str) -> String {
    match name {
        "5h" => "5-hour limit".to_owned(),
        "7d" => "Weekly limit".to_owned(),
        other => format!("{other} limit"),
    }
}

/// "5-hour limit reached · resets 23:24": the fullest window of a usage
/// limit that has been reached, for the composer's edge. None when no limit
/// is reached.
pub fn limit_reached(strip: &Strip, now_ms: i64) -> Option<String> {
    let usage = strip.usage.as_ref().filter(|usage| usage.blocked)?;
    let Some(window) = usage
        .windows
        .iter()
        .max_by(|a, b| a.used_percent.total_cmp(&b.used_percent))
    else {
        return Some("Usage limit reached".to_owned());
    };
    let mut words = format!("{} reached", limit_name(&window.name));
    if let Some(at) = window.resets_at_ms.filter(|at| *at > now_ms) {
        words.push_str(&format!(" · resets {}", resets(at, now_ms)));
    }
    Some(words)
}

/// When a limit resets: the time today, else the weekday.
fn resets(at_ms: i64, now_ms: i64) -> String {
    use chrono::TimeZone;
    let Some(at) = chrono::Local.timestamp_millis_opt(at_ms).single() else {
        return String::new();
    };
    if at_ms - now_ms < 20 * 3_600_000 {
        at.format("%H:%M").to_string()
    } else {
        at.format("%a").to_string()
    }
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

/// One entry of the queue block above the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum QueueEntry {
    /// A prompt in the agent's queue.
    Queued(QueuedRow),
    /// This client's prompt on its way into the queue, drawn at once.
    /// `waiting` names the host while the link to it is down.
    Sending {
        input_id: Vec<u8>,
        text: Vec<Segment>,
        waiting: Option<String>,
    },
    /// This client's prompt whose connection dropped before a reply and
    /// that catching up found neither queued nor in the transcript: it may
    /// not have arrived, and only the person can say whether to send it
    /// again.
    Unconfirmed {
        input_id: Vec<u8>,
        text: Vec<Segment>,
    },
}

impl QueueEntry {
    pub fn hint(&self) -> &'static str {
        match self {
            QueueEntry::Queued(row) if row.can_send_now && row.can_withdraw => {
                "enter send now · backspace withdraw · ↑/↓ queued · esc back"
            }
            QueueEntry::Queued(row) if row.can_withdraw => {
                "backspace withdraw · ↑/↓ queued · esc back"
            }
            QueueEntry::Unconfirmed { .. } => {
                "enter resend · backspace discard · ↑/↓ queued · esc back"
            }
            QueueEntry::Queued(_) | QueueEntry::Sending { .. } => "↑/↓ queued · esc back",
        }
    }

    pub fn text(&self) -> &[Segment] {
        match self {
            QueueEntry::Queued(row) => &row.text,
            QueueEntry::Sending { text, .. } | QueueEntry::Unconfirmed { text, .. } => text,
        }
    }

    /// What it says at its right: how it waits.
    pub fn state_words(&self) -> String {
        match self {
            QueueEntry::Queued(row) if row.steered => "sending into this turn".to_owned(),
            QueueEntry::Queued(row) => match &row.from_agent {
                Some(agent) => format!("queued from {agent}"),
                None => "queued".to_owned(),
            },
            QueueEntry::Sending {
                waiting: Some(host),
                ..
            } => format!("waiting for {host}…"),
            QueueEntry::Sending { .. } => "queued".to_owned(),
            QueueEntry::Unconfirmed { .. } => "may not have arrived".to_owned(),
        }
    }

    /// Its controls, in order: what Enter does first, what Backspace does
    /// second.
    pub fn controls(&self) -> Vec<&'static str> {
        match self {
            QueueEntry::Queued(row) => {
                let mut out = Vec::new();
                if row.can_send_now {
                    out.push("[Send Now]");
                }
                if row.can_withdraw {
                    out.push("[Withdraw]");
                }
                out
            }
            QueueEntry::Sending { .. } => Vec::new(),
            QueueEntry::Unconfirmed { .. } => vec!["[Resend]", "[Discard]"],
        }
    }

    /// The old design's one line for it.
    pub fn line(&self, selected: bool, width: usize, theme: Theme) -> Line<'static> {
        let mut line = Line::from(Span::styled(
            if selected { "› " } else { "  " },
            theme.accent(),
        ));
        push(&mut line, "⋯ ", theme.muted(), width);
        push(
            &mut line,
            format!("{}  ", self.state_words()),
            theme.muted(),
            width,
        );
        push(&mut line, first_text(self.text()), theme.text(), width);
        line
    }
}

/// What the empty composer says, and whether it can send.
pub fn placeholder(mode: &Composer, name: &str, host: &str, away: Away) -> String {
    match mode {
        Composer::Send => format!("Message {name}"),
        Composer::Resume => format!("{name} has exited · type to resume it with a message"),
        // The hint under the composer says the draft is kept and what sending waits
        // for, so the placeholder only names the cause.
        Composer::Disabled(Waiting::CatchingUp) => "Catching up".into(),
        Composer::Disabled(Waiting::Detached) => match away {
            Away::Plain => format!("{host} is away"),
            Away::Revoked => format!("{host} no longer trusts this machine"),
            Away::SignedOut => format!("{host} is away · this machine is signed out"),
        },
        Composer::Disabled(Waiting::Reconnecting) => "Reconnecting to amux".into(),
    }
}

/// The composer's lines at `width` and where its cursor is: text runs and
/// attachment chips at their places, a chip one cursor position.
/// The draft position under a click in the composer: `row` and `col`
/// count from the first wrapped line and the first column after the
/// prompt mark, wrapped at `width` as [`editor_lines`] wraps. The answer is
/// a count of characters, an attachment's placeholder one.
pub fn cursor_at(editor: &Editor, width: usize, target_row: usize, target_col: usize) -> usize {
    let room = width.saturating_sub(2).max(1);
    let mut row = 0usize;
    let mut used = 0usize;
    let mut at = 0usize;
    for segment in composer_tokens(editor.text(), editor.attachments()) {
        match segment {
            Segment::Text(text) => {
                for c in text.chars() {
                    if c == '\n' {
                        if row >= target_row {
                            return at;
                        }
                        row += 1;
                        used = 0;
                        at += 1;
                        continue;
                    }
                    let shown = if editor.secret { '•' } else { c };
                    let w = text::str_width(shown.encode_utf8(&mut [0; 4]));
                    if used + w > room {
                        if row >= target_row {
                            return at;
                        }
                        row += 1;
                        used = 0;
                    }
                    if row > target_row || (row == target_row && used + w > target_col) {
                        return at;
                    }
                    used += w;
                    at += 1;
                }
            }
            Segment::Attachment(view) => {
                let w = text::str_width(&text::ellipsize(&chip(&view), room));
                if used > 0 && used + w > room {
                    if row >= target_row {
                        return at;
                    }
                    row += 1;
                    used = 0;
                }
                if row > target_row || (row == target_row && used + w > target_col) {
                    return at;
                }
                used += w;
                at += 1;
            }
        }
    }
    at
}

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
                    row.push(Span::styled(chip, theme.chip()));
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
