//! The overview pane: the row above the composer, unfolded. The chat's
//! task list in full, each background job still running, the working
//! tree's changed files, and the tool servers that failed, each section
//! folding under its heading. Drawn as a fixed title over a body that
//! scrolls; the chat places it beside itself, or over its feed on a narrow
//! terminal.

use std::collections::HashSet;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ui_state::Key;
use ui_view::{JobView, Strip, TaskMark};

use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// Columns between the pane's edge and its words: a highlighted job's tint
/// spans them, as home's cards span the margin.
const INSET: usize = 2;

/// The pane's sections, each under a heading that folds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Tasks,
    Background,
    Changes,
    Servers,
}

impl Section {
    fn words(self) -> &'static str {
        match self {
            Section::Tasks => "Tasks",
            Section::Background => "Background",
            Section::Changes => "Changes",
            Section::Servers => "Tool servers",
        }
    }

    /// Its name in the client's saved layout.
    pub fn key(self) -> &'static str {
        match self {
            Section::Tasks => "tasks",
            Section::Background => "background",
            Section::Changes => "changes",
            Section::Servers => "servers",
        }
    }

    pub fn from_key(key: &str) -> Option<Section> {
        [
            Section::Tasks,
            Section::Background,
            Section::Changes,
            Section::Servers,
        ]
        .into_iter()
        .find(|section| section.key() == key)
    }
}

/// What a click on the pane reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneHit {
    Close,
    Item(PaneItem),
    /// "↑ N more" or "↓ N more": a page up, or down.
    Page {
        up: bool,
    },
}

/// One of the pane's items the keys move across: the section headings,
/// the jobs and the changed files, as one list in the pane's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneItem {
    /// A section's heading, which folds it.
    Heading(Section),
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

/// What the pane shows, read once a frame.
pub struct Contents<'a> {
    pub strip: &'a Strip,
    pub jobs: &'a [JobView],
    /// None until the working tree's diff is read.
    pub files: Option<&'a [FileLine]>,
    pub folded: &'a HashSet<Section>,
}

impl Contents<'_> {
    /// The sections with something in them, in the pane's order.
    fn sections(&self) -> Vec<Section> {
        let mut sections = Vec::new();
        if self.strip.tasks.is_some() {
            sections.push(Section::Tasks);
        }
        if !self.jobs.is_empty() {
            sections.push(Section::Background);
        }
        if self.files.is_some_and(|files| !files.is_empty()) {
            sections.push(Section::Changes);
        }
        if !self.strip.failed_servers.is_empty() {
            sections.push(Section::Servers);
        }
        sections
    }

    /// The items the keys move across, in order: each heading, then the
    /// jobs or files under it unless it is folded.
    pub fn items(&self) -> Vec<PaneItem> {
        let mut items = Vec::new();
        for section in self.sections() {
            items.push(PaneItem::Heading(section));
            if self.folded.contains(&section) {
                continue;
            }
            match section {
                Section::Background => {
                    items.extend(self.jobs.iter().map(|job| PaneItem::Job(job.key.clone())))
                }
                Section::Changes => items.extend(
                    grouped(self.files.unwrap_or_default())
                        .into_iter()
                        .flat_map(|(_, files)| files)
                        .map(|file| PaneItem::File(file.path.clone())),
                ),
                Section::Tasks | Section::Servers => {}
            }
        }
        items
    }
}

/// The pane drawn: its fixed title line and its body, which scrolls.
#[derive(Default)]
pub struct PaneOut {
    pub title: Line<'static>,
    pub title_hits: Vec<((usize, usize), PaneHit)>,
    pub body: Vec<Line<'static>>,
    pub hits: Vec<(usize, (usize, usize), PaneHit)>,
    /// For each body line, the task, job or file it draws, numbered in
    /// order, to count what scrolling hides.
    pub entries: Vec<Option<usize>>,
    /// Each item's body line, for keeping the focused one in view.
    pub item_lines: Vec<(PaneItem, usize)>,
    /// The Changes groups' directory lines, and where the section ends: a
    /// scrolled group keeps its directory pinned.
    pub dir_lines: Vec<usize>,
    pub changes_end: usize,
}

impl PaneOut {
    /// The directory line to pin over body line `row`, the first under
    /// the top edge: the one naming the group `row` sits in, when it has
    /// scrolled out above. The next group's own line takes over as it
    /// reaches `row`.
    pub fn pin(&self, row: usize) -> Option<usize> {
        let dir = self.dir_lines.iter().copied().rfind(|line| *line <= row)?;
        (dir < row && row < self.changes_end).then_some(dir)
    }
}

/// The pane, `width` columns wide with its words `INSET` in from each side.
/// `lit` is the item drawn highlighted: a job or file as a card, a heading
/// by brightening. `focused` says the pane has the keys, which only its
/// title shows.
pub fn pane_lines(
    contents: &Contents<'_>,
    lit: Option<&PaneItem>,
    focused: bool,
    now_ms: i64,
    width: usize,
    theme: Theme,
) -> PaneOut {
    let inner = width.saturating_sub(2 * INSET);
    let mut out = PaneOut::default();
    let title_ink = if focused {
        theme.bright()
    } else {
        theme.text()
    };
    let mut title = Line::from(Span::raw(" ".repeat(INSET)));
    push(&mut title, "Overview", title_ink, width);
    const CLOSE: &str = "[x]";
    let close_at = INSET + inner.saturating_sub(text::str_width(CLOSE));
    pad_to(&mut title, close_at);
    push(&mut title, CLOSE, theme.muted(), width);
    out.title = title;
    out.title_hits.push((
        (close_at, close_at + text::str_width(CLOSE)),
        PaneHit::Close,
    ));

    let mut entry = 0;
    let push_line =
        |out: &mut PaneOut, line: Line<'static>, of: Option<usize>, hit: Option<PaneHit>| {
            if let Some(hit) = hit {
                out.hits.push((out.body.len(), (0, width), hit));
            }
            out.entries.push(of);
            out.body.push(line);
        };
    let blank = |out: &mut PaneOut| {
        out.entries.push(None);
        out.body.push(Line::default());
    };
    let mut card = None;
    let sections = contents.sections();
    for (n, section) in sections.iter().copied().enumerate() {
        if n > 0 {
            blank(&mut out);
            blank(&mut out);
        }
        let folded = contents.folded.contains(&section);
        let count = match section {
            Section::Tasks => contents
                .strip
                .tasks
                .as_ref()
                .map(|tasks| format!("{} of {}", tasks.done, tasks.total))
                .unwrap_or_default(),
            Section::Background => contents.jobs.len().to_string(),
            Section::Changes => contents.files.unwrap_or_default().len().to_string(),
            Section::Servers => contents.strip.failed_servers.len().to_string(),
        };
        // Like home's headings: a fold marker, the words, the count and a
        // hairline to the edge. A control: lit, its marker and words
        // brighten, and only they answer the mouse.
        let item = PaneItem::Heading(section);
        let chosen = lit == Some(&item);
        let (marker_ink, words_ink) = if chosen {
            (theme.emphasis(), theme.emphasis())
        } else {
            (theme.faint(), theme.muted().add_modifier(Modifier::BOLD))
        };
        let mut line = Line::default();
        push(&mut line, if folded { "▸" } else { "▾" }, marker_ink, inner);
        pad_to(&mut line, 2);
        push(&mut line, section.words(), words_ink, inner);
        let words_end = INSET + text::line_width(&line);
        push(&mut line, format!(" {count} "), theme.faint(), inner);
        let rule = inner.saturating_sub(text::line_width(&line));
        push(&mut line, "─".repeat(rule), theme.hairline(), inner);
        out.item_lines.push((item.clone(), out.body.len()));
        out.hits
            .push((out.body.len(), (0, words_end), PaneHit::Item(item)));
        push_line(&mut out, line, None, None);
        if folded {
            continue;
        }
        blank(&mut out);
        match section {
            Section::Tasks => {
                let Some(tasks) = &contents.strip.tasks else {
                    continue;
                };
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
                        push_line(&mut out, line, Some(entry), None);
                    }
                    entry += 1;
                }
            }
            Section::Background => {
                for job in contents.jobs {
                    // Each job on one line: its command, and how long it
                    // has run. A click (or Enter on the focused one) opens
                    // the step that started it.
                    let age = ran_for(now_ms - job.started_at_ms);
                    let mut line = Line::default();
                    let room = inner.saturating_sub(text::str_width(&age) + 2);
                    let command = text::ellipsize(text::first_line(&job.command), room);
                    push(&mut line, command, theme.code(), inner);
                    pad_to(&mut line, inner.saturating_sub(text::str_width(&age)));
                    push(&mut line, age, theme.faint(), inner);
                    let item = PaneItem::Job(job.key.clone());
                    if lit == Some(&item) {
                        card = Some(out.body.len());
                    }
                    out.item_lines.push((item.clone(), out.body.len()));
                    push_line(&mut out, line, Some(entry), Some(PaneHit::Item(item)));
                    entry += 1;
                }
            }
            Section::Changes => {
                // Files under their directory: a faint line naming it,
                // shortened in the middle so both ends survive, then each
                // file's name two columns in with its lines added and
                // removed at the right. Only files take the keys; a click
                // (or Enter) opens the review page at one.
                for (n, (dir, files)) in grouped(contents.files.unwrap_or_default())
                    .into_iter()
                    .enumerate()
                {
                    // A blank line sets each directory's group apart.
                    if n > 0 {
                        blank(&mut out);
                    }
                    let indent = if dir.is_empty() {
                        0
                    } else {
                        let mut line = Line::default();
                        push(&mut line, shorten_middle(dir, inner), theme.faint(), inner);
                        out.dir_lines.push(out.body.len());
                        push_line(&mut out, line, None, None);
                        2
                    };
                    for file in files {
                        // A zero side is left out: "+1", or "−5" for a
                        // pure deletion.
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
                        let name = &file.path[dir.len()..];
                        let room = inner.saturating_sub(indent + text::str_width(&counts) + 2);
                        let mut line = Line::default();
                        pad_to(&mut line, indent);
                        push(&mut line, shorten_left(name, room), theme.text(), inner);
                        pad_to(&mut line, inner.saturating_sub(text::str_width(&counts)));
                        // Green and red from the terminal's palette, as
                        // the review page colours them.
                        push(&mut line, added, theme.ok(), inner);
                        push(&mut line, removed, theme.error(), inner);
                        let item = PaneItem::File(file.path.clone());
                        if lit == Some(&item) {
                            card = Some(out.body.len());
                        }
                        out.item_lines.push((item.clone(), out.body.len()));
                        push_line(&mut out, line, Some(entry), Some(PaneHit::Item(item)));
                        entry += 1;
                    }
                }
                out.changes_end = out.body.len();
            }
            Section::Servers => {
                for server in &contents.strip.failed_servers {
                    let what = if server.needs_auth {
                        "needs sign-in"
                    } else {
                        "failed to start"
                    };
                    let mut line = Line::default();
                    push(
                        &mut line,
                        format!("{} {what}", server.name),
                        theme.error(),
                        inner,
                    );
                    push_line(&mut out, line, Some(entry), None);
                    if !server.error.is_empty() {
                        let mut line = Line::default();
                        push(
                            &mut line,
                            text::ellipsize(text::first_line(&server.error), inner),
                            theme.faint(),
                            inner,
                        );
                        push_line(&mut out, line, Some(entry), None);
                    }
                    entry += 1;
                }
            }
        }
    }
    if sections.is_empty() {
        let mut line = Line::default();
        push(&mut line, "Nothing running", theme.faint(), inner);
        push_line(&mut out, line, None, None);
    }

    for line in &mut out.body {
        line.spans.insert(0, Span::raw(" ".repeat(INSET)));
    }
    if let Some(at) = card {
        card_highlight(&mut out.body, at..at + 1, 0, width, false, theme);
    }
    out
}

/// What a window over the body, `height` lines from line `top`, hides past
/// each edge: how many tasks, jobs and files lie wholly above and below
/// what it shows, once an edge line gives way to its "↑ N more" or "↓ N
/// more". None where nothing is hidden that way, and none either where
/// only a heading or blank lines are: the edge line then shows as itself.
/// `cover` more lines under the top edge are hidden by a pinned line.
pub fn hidden(
    out: &PaneOut,
    top: usize,
    height: usize,
    cover: usize,
) -> (Option<usize>, Option<usize>) {
    let len = out.body.len();
    let count = |range: std::ops::Range<usize>, shown: std::ops::Range<usize>| {
        let mut seen: Vec<usize> = out.entries[range]
            .iter()
            .flatten()
            .copied()
            .filter(|entry| {
                !out.entries[shown.clone()]
                    .iter()
                    .flatten()
                    .any(|each| each == entry)
            })
            .collect();
        seen.dedup();
        seen.len()
    };
    let bottom = (top + height).min(len);
    let mut above_edge = top > 0;
    let mut below_edge = top + height < len;
    let window = |above_edge: bool, below_edge: bool| {
        (top + usize::from(above_edge) + cover).min(bottom)
            ..bottom.saturating_sub(usize::from(below_edge))
    };
    let above = above_edge.then(|| count(0..top + 1 + cover, window(true, below_edge)));
    if above == Some(0) {
        above_edge = false;
    }
    let shown = window(above_edge, below_edge);
    let below = below_edge.then(|| count(shown.end..len, shown.clone()));
    if below == Some(0) {
        below_edge = false;
    }
    (above.filter(|_| above_edge), below.filter(|_| below_edge))
}

/// The faint edge line that says how much scrolling hides that way.
pub fn more_line(up: bool, count: usize, theme: Theme) -> Line<'static> {
    let arrow = if up { "↑" } else { "↓" };
    Line::from(vec![
        Span::raw(" ".repeat(INSET)),
        Span::styled(format!("{arrow} {count} more"), theme.faint()),
    ])
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

/// The changed files by directory, sorted by path: files at the root first,
/// under no directory, then each directory with its files. A directory is
/// named with its trailing slash.
fn grouped(files: &[FileLine]) -> Vec<(&str, Vec<&FileLine>)> {
    let mut sorted: Vec<&FileLine> = files.iter().collect();
    sorted.sort_by(|a, b| {
        let dir = |f: &FileLine| f.path.rfind('/').map_or(0, |at| at + 1);
        f_key(a, dir(a)).cmp(&f_key(b, dir(b)))
    });
    let mut groups: Vec<(&str, Vec<&FileLine>)> = Vec::new();
    for file in sorted {
        let dir = &file.path[..file.path.rfind('/').map_or(0, |at| at + 1)];
        match groups.last_mut() {
            Some((last, members)) if *last == dir => members.push(file),
            _ => groups.push((dir, vec![file])),
        }
    }
    groups
}

/// Root files first, then by directory, then by name.
fn f_key(file: &FileLine, dir: usize) -> (bool, &str, &str) {
    (dir > 0, &file.path[..dir], &file.path[dir..])
}

/// A directory in at most `max` columns, cut in the middle at directories
/// so its first and last parts survive: "apps/…/Sources/Chat/".
fn shorten_middle(dir: &str, max: usize) -> String {
    if text::str_width(dir) <= max {
        return dir.to_owned();
    }
    let parts: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
    if let Some((first, rest)) = parts.split_first() {
        for keep in (1..rest.len()).rev() {
            let tail = rest[rest.len() - keep..].join("/");
            let shown = format!("{first}/…/{tail}/");
            if text::str_width(&shown) <= max {
                return shown;
            }
        }
    }
    shorten_left(dir, max)
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
