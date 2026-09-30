//! The overview pane: the row above the composer, unfolded. The chat's
//! task list in full, each background job still running, the working
//! tree's changed files, and the tool servers that failed. Drawn as plain lines of a given width; the chat
//! places them beside itself, or over its feed on a narrow terminal.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ui_state::Key;
use ui_view::{JobView, Strip, TaskMark};

use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// Columns between the pane's edge and its words: a highlighted job's tint
/// spans them, as home's cards span the margin.
const INSET: usize = 2;

/// What a click on the pane reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneHit {
    Close,
    Item(PaneItem),
}

/// One of the pane's items the keys move across: the jobs, then the
/// changed files, as one list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneItem {
    /// A running background job, by the step that started it.
    Job(Key),
    /// A changed file of the working tree, by its path.
    File(String),
}

/// A changed file as the pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileLine {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}

/// The pane's items in the order the keys move across them.
pub fn items(jobs: &[JobView], files: Option<&[FileLine]>) -> Vec<PaneItem> {
    jobs.iter()
        .map(|job| PaneItem::Job(job.key.clone()))
        .chain(
            files
                .unwrap_or_default()
                .iter()
                .map(|file| PaneItem::File(file.path.clone())),
        )
        .collect()
}

/// The pane's lines and the columns of each line a click reaches.
#[derive(Default)]
pub struct PaneOut {
    pub lines: Vec<Line<'static>>,
    pub hits: Vec<(usize, (usize, usize), PaneHit)>,
}

/// The pane's lines, `width` columns wide with its words `INSET` in from
/// each side. `lit` indexes [`items`]: the item drawn as a card, the one
/// under the mouse, else the one the keys are on. `focused` says the pane
/// has the keys, which only its title shows. Not `roomy`, the items pack
/// together and a card stays flat, so a short pane loses blank lines
/// before it loses items. `files` is None until the diff is read.
#[allow(clippy::too_many_arguments)]
pub fn pane_lines(
    strip: &Strip,
    jobs: &[JobView],
    files: Option<&[FileLine]>,
    lit: Option<usize>,
    focused: bool,
    roomy: bool,
    now_ms: i64,
    width: usize,
    theme: Theme,
) -> PaneOut {
    let inner = width.saturating_sub(2 * INSET);
    let mut out = PaneOut::default();
    let mut title = Line::default();
    let title_ink = if focused {
        theme.bright()
    } else {
        theme.text()
    };
    push(&mut title, "Overview", title_ink, inner);
    const CLOSE: &str = "[x]";
    let close_at = inner.saturating_sub(text::str_width(CLOSE));
    pad_to(&mut title, close_at);
    push(&mut title, CLOSE, theme.muted(), inner);
    out.hits.push((
        0,
        (INSET + close_at, INSET + close_at + text::str_width(CLOSE)),
        PaneHit::Close,
    ));
    out.lines.push(title);
    out.lines.push(Line::default());

    let mut sections = 0;
    let mut section = |out: &mut PaneOut, label: &str, count: String| {
        if sections > 0 {
            out.lines.push(Line::default());
            out.lines.push(Line::default());
        }
        sections += 1;
        let mut line = Line::default();
        push(
            &mut line,
            label,
            theme.muted().add_modifier(Modifier::BOLD),
            inner,
        );
        push(&mut line, format!(" {count} "), theme.faint(), inner);
        let rule = inner.saturating_sub(text::line_width(&line));
        push(&mut line, "─".repeat(rule), theme.hairline(), inner);
        out.lines.push(line);
        out.lines.push(Line::default());
    };

    if let Some(tasks) = &strip.tasks {
        section(
            &mut out,
            "Tasks",
            format!("{} of {}", tasks.done, tasks.total),
        );
        for task in &tasks.entries {
            let (mark, mark_style, words) = match task.mark {
                TaskMark::Done => ("✓", theme.faint(), theme.faint()),
                TaskMark::Current => ("●", theme.text(), theme.bright()),
                TaskMark::Todo => ("○", theme.faint(), theme.text()),
            };
            for (i, part) in text::wrap(&task.subject, inner.saturating_sub(2).max(1))
                .into_iter()
                .enumerate()
            {
                let mut line = Line::default();
                if i == 0 {
                    push(&mut line, mark, mark_style, inner);
                }
                pad_to(&mut line, 2);
                push(&mut line, part, words, inner);
                out.lines.push(line);
            }
        }
    }

    // Where the lit item's line lands, to draw it as a card afterwards.
    let mut card = None;
    if !jobs.is_empty() {
        section(&mut out, "Background", jobs.len().to_string());
        for (i, job) in jobs.iter().enumerate() {
            // Each job on one line, a blank line between them so a
            // highlighted one has room for its card: its command, and how
            // long it has run. A click (or Enter on the focused one) opens
            // the step that started it in the chat.
            if i > 0 && roomy {
                out.lines.push(Line::default());
            }
            let age = ran_for(now_ms - job.started_at_ms);
            let mut line = Line::default();
            let room = inner.saturating_sub(text::str_width(&age) + 2);
            let command = text::ellipsize(text::first_line(&job.command), room);
            push(&mut line, command, theme.code(), inner);
            pad_to(&mut line, inner.saturating_sub(text::str_width(&age)));
            push(&mut line, age, theme.faint(), inner);
            if lit == Some(i) {
                card = Some(out.lines.len());
            }
            out.hits.push((
                out.lines.len(),
                (0, width),
                PaneHit::Item(PaneItem::Job(job.key.clone())),
            ));
            out.lines.push(line);
        }
    }

    if let Some(files) = files.filter(|files| !files.is_empty()) {
        section(&mut out, "Changes", files.len().to_string());
        for (i, file) in files.iter().enumerate() {
            // Each file on one line: its path, the directory faint and the
            // name in the reading ink, shortened from the left so the name
            // survives; its lines added and removed at the right. A click
            // (or Enter) opens the review page at it.
            if i > 0 && roomy {
                out.lines.push(Line::default());
            }
            // A zero side is left out: "+1", or "−5" for a pure deletion.
            let added = if file.added > 0 {
                format!("+{}", file.added)
            } else {
                String::new()
            };
            let removed = match (file.removed, added.is_empty()) {
                (0, _) => String::new(),
                (n, true) => format!("\u{2212}{n}"),
                (n, false) => format!(" \u{2212}{n}"),
            };
            let counts = format!("{added}{removed}");
            let room = inner.saturating_sub(text::str_width(&counts) + 2);
            let shown = shorten_left(&file.path, room);
            let (dir, name) = match shown.rfind('/') {
                Some(at) => shown.split_at(at + 1),
                None => ("", shown.as_str()),
            };
            let mut line = Line::default();
            push(&mut line, dir, theme.faint(), inner);
            push(&mut line, name, theme.text(), inner);
            pad_to(&mut line, inner.saturating_sub(text::str_width(&counts)));
            // Green and red from the terminal's palette, as the review
            // page colours them.
            push(&mut line, added, theme.ok(), inner);
            push(&mut line, removed, theme.error(), inner);
            if lit == Some(jobs.len() + i) {
                card = Some(out.lines.len());
            }
            out.hits.push((
                out.lines.len(),
                (0, width),
                PaneHit::Item(PaneItem::File(file.path.clone())),
            ));
            out.lines.push(line);
        }
    }

    if !strip.failed_servers.is_empty() {
        section(
            &mut out,
            "Tool servers",
            strip.failed_servers.len().to_string(),
        );
        for server in &strip.failed_servers {
            let mut line = Line::default();
            let what = if server.needs_auth {
                "needs sign-in"
            } else {
                "failed to start"
            };
            push(
                &mut line,
                format!("{} {what}", server.name),
                theme.error(),
                inner,
            );
            out.lines.push(line);
            if !server.error.is_empty() {
                let mut line = Line::default();
                push(
                    &mut line,
                    text::ellipsize(text::first_line(&server.error), inner),
                    theme.faint(),
                    inner,
                );
                out.lines.push(line);
            }
        }
    }

    if sections == 0 {
        let mut line = Line::default();
        push(&mut line, "Nothing running", theme.faint(), inner);
        out.lines.push(line);
    }
    // Room under the last line for a highlighted job's padding.
    if roomy {
        out.lines.push(Line::default());
    }

    for line in &mut out.lines {
        line.spans.insert(0, Span::raw(" ".repeat(INSET)));
    }
    if let Some(at) = card {
        card_highlight(&mut out.lines, at..at + 1, 0, width, true, theme);
    }
    out
}

/// Draws `range` of `lines` as home draws a highlighted card: tinted from
/// column `from` to `to`, and when `pad`, with half a line of the tint
/// above and below if the lines on both sides are blank. Where lines are
/// packed, the tint stays flat rather than lopsided.
pub fn card_highlight(
    lines: &mut [Line<'static>],
    range: std::ops::Range<usize>,
    from: usize,
    to: usize,
    pad: bool,
    theme: Theme,
) {
    let Some(surface) = theme.row_surface() else {
        return;
    };
    let Some(bg) = surface.bg else {
        return;
    };
    for line in lines.get_mut(range.clone()).into_iter().flatten() {
        *line = tint(std::mem::take(line), from, to, surface);
    }
    let edge = |glyph: &str| {
        Line::from(vec![
            Span::raw(" ".repeat(from)),
            Span::styled(
                glyph.repeat(to.saturating_sub(from)),
                Style::default().fg(bg),
            ),
        ])
    };
    let blank = |line: &Line<'_>| text::line_width(line) == 0 || is_spaces(line);
    let above = range.start.checked_sub(1);
    let padded = pad
        && above.and_then(|at| lines.get(at)).is_some_and(blank)
        && lines.get(range.end).is_some_and(blank);
    if let Some(above) = above.filter(|_| padded) {
        lines[above] = edge("▄");
        lines[range.end] = edge("▀");
    }
}

fn is_spaces(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.chars().all(|c| c == ' '))
}

/// `line` with its cells from `from` to `to` on `surface`, each span
/// keeping its own ink.
fn tint(line: Line<'static>, from: usize, to: usize, surface: Style) -> Line<'static> {
    let mut out = Line::default();
    let mut col = 0;
    for span in line.spans {
        for c in span.content.chars() {
            let cell = text::str_width(c.encode_utf8(&mut [0; 4]));
            let style = if col >= from && col < to {
                span.style.patch(surface)
            } else {
                span.style
            };
            out.spans.push(Span::styled(c.to_string(), style));
            col += cell;
        }
    }
    if col < from {
        out.spans.push(Span::raw(" ".repeat(from - col)));
        col = from;
    }
    if col < to {
        out.spans.push(Span::styled(" ".repeat(to - col), surface));
    }
    out
}

/// `path` in at most `max` columns, cut from the left at a directory so the
/// file's name survives: "…/src/session.rs".
fn shorten_left(path: &str, max: usize) -> String {
    if text::str_width(path) <= max {
        return path.to_owned();
    }
    let mut at = 0;
    while let Some(slash) = path[at..].find('/') {
        at += slash + 1;
        let tail = &path[at..];
        if text::str_width(tail) + 2 <= max {
            return format!("…/{tail}");
        }
    }
    let name = &path[at..];
    let keep = max.saturating_sub(1);
    let skip = name.chars().count().saturating_sub(keep);
    format!("…{}", name.chars().skip(skip).collect::<String>())
}

/// How long a job has run, coarse enough not to tick every second once it
/// has run a minute: "12s", "4m", "1h 5m".
fn ran_for(ms: i64) -> String {
    let secs = ms.max(0) / 1_000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3_600, secs % 3_600 / 60),
    }
}
