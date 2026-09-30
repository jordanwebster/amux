//! The chat's feed, one turn at a time: your message as a tinted block, the
//! agent's text in the reading ink, and each stretch of its tool steps
//! folded to one faint line between them. A stretch opens to its steps, one
//! line each, and a step opens to its detail. A stretch still under way
//! shows its newest steps as they happen, so the person sees the agent
//! working, and folds once the agent speaks again.
//!
//! Two columns carry everything, as on home: markers and the rail that joins
//! a stretch's steps sit on the left edge, words on the column after it.
//! Rows that are neither text nor a step (asks, errors, boundaries) keep
//! their own drawing from [`super::rows`].

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ui_state::Key;
use ui_view::{
    FileChangeView, Row, RowKind, RunInfo, Segment, Stretch, StretchCounts, ToolStateView,
};

use super::rows::{
    RowFacts, call_verb, chip, detail, explore_verb, markdown, patch_lines, segment_lines,
    state_meta, tail, with_decision_after,
};
use crate::text::{self, first_line, pad_to, push, push_right};
use crate::theme::Theme;

/// A stretch under way shows at most this many of its newest steps.
pub const LIVE_STEPS: usize = 3;
/// The left edge: markers, and the rail joining a stretch's steps.
const EDGE: usize = 2;
/// Where words start.
const WORDS: usize = 4;
/// Where a step's detail starts, under its words.
const DETAIL: usize = 6;
/// Lines of a step's detail before it is cut with a count.
const DETAIL_LINES: usize = 12;

/// What a click on a feed line does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedHit {
    /// Fold or unfold the stretch whose oldest step this is.
    Stretch(Key),
    /// Open or close a step's detail.
    Step(Key),
    /// Open the working tree's diff.
    Diff,
}

/// Clickable places on one feed line: column ranges, `None` for the whole
/// line.
pub type LineHits = Vec<(Option<(usize, usize)>, FeedHit)>;

/// Where a row sits among the stretches.
#[derive(Clone, Debug)]
pub enum Placement {
    /// Not in a stretch.
    Plain,
    /// The one line a folded stretch draws, on its newest step.
    Folded {
        stretch: Stretch,
        unresolved: Vec<Row>,
    },
    /// A step drawn on its own line.
    Step {
        /// The stretch's line drawn above this, its first drawn step.
        header: Option<Header>,
        /// Another step of the stretch follows.
        joined: bool,
        /// The step under way right now.
        current: bool,
    },
}

/// The line above an open or live stretch's first drawn step.
#[derive(Clone, Debug)]
pub struct Header {
    pub stretch: Stretch,
    /// Open: it folds the stretch again. Otherwise the stretch is under way
    /// and `earlier` of its steps are not drawn.
    pub open: bool,
    pub earlier: usize,
}

/// One row's lines and, for each line, what clicking along it does.
#[derive(Clone, Debug, Default)]
pub struct Drawn {
    pub lines: Vec<Line<'static>>,
    pub hits: Vec<LineHits>,
}

impl Drawn {
    fn line(&mut self, line: Line<'static>) {
        self.lines.push(line);
        self.hits.push(Vec::new());
    }

    fn hit_line(&mut self, line: Line<'static>, hit: FeedHit) {
        self.lines.push(line);
        self.hits.push(vec![(None, hit)]);
    }

    fn blank(&mut self) {
        self.line(Line::default());
    }
}

/// "3 commands · 2 edits · 4 reads".
pub fn counts_words(counts: &StretchCounts, open_below: bool) -> String {
    let plus = if open_below { "+" } else { "" };
    let mut parts = Vec::new();
    let mut part = |n: u32, one: &str, many: &str| {
        if n > 0 {
            parts.push(format!("{n}{plus} {}", if n == 1 { one } else { many }));
        }
    };
    part(counts.commands, "command", "commands");
    part(counts.edits, "edit", "edits");
    part(counts.reads, "read", "reads");
    part(counts.searches, "search", "searches");
    part(counts.subagents, "subagent", "subagents");
    part(counts.other, "call", "calls");
    if parts.is_empty() {
        parts.push(format!("{}{plus} steps", counts.steps));
    }
    parts.join(" · ")
}

/// A row's lines in the feed. `expanded` opens a step's detail.
pub fn row_lines(
    row: &Row,
    placement: &Placement,
    expanded: bool,
    facts: &RowFacts,
    width: usize,
    theme: Theme,
) -> Option<Drawn> {
    let mut drawn = Drawn::default();
    match placement {
        Placement::Folded {
            stretch,
            unresolved,
        } => {
            folded(&mut drawn, stretch, unresolved, width, theme);
            drawn.blank();
        }
        Placement::Step {
            header,
            joined,
            current,
        } => {
            if let Some(header) = header {
                header_line(&mut drawn, header, width, theme);
            }
            step(&mut drawn, row, expanded, *current, facts, width, theme)?;
            if !joined {
                drawn.blank();
            }
        }
        Placement::Plain => match &row.kind {
            RowKind::Prompt { text, steered } => {
                prompt(&mut drawn, row, text, *steered, width, theme)
            }
            RowKind::Prose {
                text, streaming, ..
            } => prose(&mut drawn, text, *streaming, width, theme),
            RowKind::Thinking { .. } | RowKind::Hidden => return None,
            RowKind::TurnEnd {
                duration_ms,
                failed,
                ..
            } => {
                let mut line = Line::from(Span::raw(" ".repeat(WORDS)));
                let words = match duration_ms {
                    Some(ms) => format!("Worked {}", worked(*ms)),
                    None => "Turn ended".to_owned(),
                };
                push(&mut line, words, theme.faint(), width);
                if *failed {
                    push(&mut line, " · failed", theme.error(), width);
                }
                drawn.line(line);
                drawn.blank();
            }
            RowKind::Stopped => {
                let mut line = Line::from(Span::raw(" ".repeat(WORDS)));
                push(&mut line, "You stopped it", theme.faint(), width);
                drawn.line(line);
                drawn.blank();
            }
            // A step outside any stretch, as one whose stretch cannot be
            // read: drawn as a step on its own.
            _ if is_step(row) => {
                step(&mut drawn, row, expanded, false, facts, width, theme)?;
                drawn.blank();
            }
            _ => return None,
        },
    }
    Some(drawn)
}

/// Whether a row draws as a step.
pub fn is_step(row: &Row) -> bool {
    matches!(
        row.kind,
        RowKind::Explore { .. }
            | RowKind::Command { .. }
            | RowKind::ToolCall { .. }
            | RowKind::FileChange { .. }
            | RowKind::Background { .. }
            | RowKind::Subagent { .. }
            | RowKind::Image { .. }
    )
}

/// Your message: a tinted block with half a line of padding above and
/// below, the time at the right of its first line.
fn prompt(
    drawn: &mut Drawn,
    row: &Row,
    words: &[Segment],
    steered: bool,
    width: usize,
    theme: Theme,
) {
    let surface = theme.user_surface();
    let tint = surface.bg;
    let edge = |glyph: &str| match tint {
        Some(color) => Line::from(vec![
            Span::raw(" "),
            Span::styled(
                glyph.repeat(width.saturating_sub(2)),
                Style::default().fg(color),
            ),
        ]),
        None => Line::default(),
    };
    drawn.line(edge("▄"));
    let when = if steered {
        format!("steered · {}", clock(row.at_ms))
    } else {
        clock(row.at_ms)
    };
    let room = width.saturating_sub(WORDS + 4 + text::str_width(&when) + 2);
    for (i, words) in segment_lines(words, room, theme.text(), theme)
        .into_iter()
        .enumerate()
    {
        let mut line = Line::from(Span::raw(" "));
        push(&mut line, " ".repeat(WORDS - 1), Style::default(), width);
        line.spans.extend(words.spans);
        if i == 0 {
            push_right(&mut line, &format!("{when}  "), theme.faint(), width - 1);
        }
        let mut tinted = Line::from(Span::raw(" "));
        let mut first = true;
        for span in line.spans.into_iter() {
            if first {
                first = false;
                continue;
            }
            tinted
                .spans
                .push(Span::styled(span.content, span.style.patch(surface)));
        }
        text::fill(&mut tinted, surface, width - 1);
        drawn.line(tinted);
    }
    drawn.line(edge("▀"));
    drawn.blank();
}

/// The agent's text, in the reading ink.
fn prose(drawn: &mut Drawn, words: &[Segment], streaming: bool, width: usize, theme: Theme) {
    let source: String = words
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) => text.clone(),
            Segment::Attachment(view) => chip(view),
        })
        .collect();
    let mut lines = markdown(&source, width.saturating_sub(2), false, theme);
    if lines.is_empty() {
        return;
    }
    // Code and paths stand out by ink, not by a hue: colour is kept for
    // what needs the person and what failed.
    let code = theme.code().fg;
    for line in &mut lines {
        for span in &mut line.spans {
            if span.style.fg.is_some() && span.style.fg == code {
                span.style = span.style.patch(theme.bright());
            }
        }
    }
    if streaming && let Some(last) = lines.last_mut() {
        last.spans.push(Span::styled(" ▍", theme.faint()));
    }
    for line in lines {
        drawn.line(line);
    }
    drawn.blank();
}

/// A folded stretch: its counts, then any failure it left unresolved.
fn folded(drawn: &mut Drawn, stretch: &Stretch, unresolved: &[Row], width: usize, theme: Theme) {
    let mut line = Line::default();
    pad_to(&mut line, EDGE);
    push(&mut line, "▸", theme.faint(), width);
    pad_to(&mut line, WORDS);
    push(
        &mut line,
        counts_words(&stretch.counts, stretch.open_below),
        theme.faint(),
        width,
    );
    drawn.hit_line(line, FeedHit::Stretch(stretch.oldest.clone()));
    for row in unresolved {
        let mut line = Line::default();
        pad_to(&mut line, EDGE);
        push(&mut line, "✗", theme.error(), width);
        pad_to(&mut line, WORDS);
        let (verb, subject, meta) = step_words(row, false);
        push(&mut line, verb, theme.error(), width);
        if !subject.is_empty() {
            push(&mut line, " ", theme.error(), width);
            push(
                &mut line,
                subject,
                theme.error(),
                width.saturating_sub(meta.len() + 3),
            );
        }
        if !meta.is_empty() {
            push(&mut line, format!(" · {meta}"), theme.faint(), width);
        }
        drawn.hit_line(line, FeedHit::Step(row.id.clone()));
    }
}

/// The line above an open or live stretch's steps.
fn header_line(drawn: &mut Drawn, header: &Header, width: usize, theme: Theme) {
    let mut line = Line::default();
    pad_to(&mut line, EDGE);
    if header.open {
        push(&mut line, "▾", theme.faint(), width);
        pad_to(&mut line, WORDS);
        push(
            &mut line,
            counts_words(&header.stretch.counts, header.stretch.open_below),
            theme.faint(),
            width,
        );
    } else {
        push(&mut line, "▸", theme.faint(), width);
        pad_to(&mut line, WORDS);
        push(
            &mut line,
            format!(
                "{} earlier step{}",
                header.earlier,
                if header.earlier == 1 { "" } else { "s" }
            ),
            theme.faint(),
            width,
        );
    }
    drawn.hit_line(line, FeedHit::Stretch(header.stretch.oldest.clone()));
}

/// A step's verb, subject and meta, in words. A closed run's summary
/// speaks for the run; `open`, it is one read among the others.
fn step_words(row: &Row, open: bool) -> (String, String, String) {
    match &row.kind {
        RowKind::Command {
            command,
            state,
            exit_code,
            duration_ms,
            ..
        } => {
            let verb = call_verb(*state, row, ["Wants to run", "Running", "Ran"]);
            let mut meta = Vec::new();
            match (exit_code, state) {
                (Some(code), _) if *code != 0 => meta.push(format!("exit {code}")),
                (None, ToolStateView::Failed) => meta.push("failed".into()),
                _ => {}
            }
            if let Some(ms) = duration_ms {
                meta.push(text::duration(*ms));
            }
            let meta = with_decision_after(meta.join(" · "), row, verb);
            (verb.to_owned(), first_line(command).to_owned(), meta)
        }
        RowKind::Explore {
            verb,
            subject,
            state,
        } => {
            if let Some(run) = row.run.as_ref().filter(|run| run.is_summary && !open) {
                return (run_words(run), String::new(), String::new());
            }
            let meta =
                with_decision_after(state_meta(*state).unwrap_or_default().to_owned(), row, "");
            (explore_verb(*verb).to_owned(), subject.clone(), meta)
        }
        RowKind::ToolCall {
            server,
            tool,
            fact,
            state,
            ..
        } => {
            let verb = call_verb(*state, row, ["Wants to use", "Using", "Used"]);
            let subject = if server.is_empty() {
                tool.clone()
            } else {
                format!("{server} · {tool}")
            };
            let mut meta = fact.clone();
            if *state == ToolStateView::Failed {
                meta = if meta.is_empty() {
                    "failed".into()
                } else {
                    format!("{meta} · failed")
                };
            }
            let meta = with_decision_after(meta, row, verb);
            (verb.to_owned(), subject, meta)
        }
        RowKind::FileChange { files, state } => {
            let file = files.first();
            let verb = match (state, file.map(|f| &f.change)) {
                (ToolStateView::Pending, _) => "Wants to edit",
                (ToolStateView::Running, _) => "Editing",
                (_, Some(FileChangeView::Created { .. })) => "Created",
                (_, Some(FileChangeView::Deleted)) => "Deleted",
                (_, Some(FileChangeView::Moved { .. })) => "Moved",
                _ => "Edited",
            };
            let subject = file.map(|f| f.path.clone()).unwrap_or_default();
            let meta = file
                .map(|f| match &f.change {
                    FileChangeView::Edited => format!("+{} −{}", f.added, f.removed),
                    FileChangeView::Created { lines } => format!("{lines} lines"),
                    FileChangeView::Moved { to } => format!("→ {to}"),
                    FileChangeView::Deleted => String::new(),
                })
                .unwrap_or_default();
            (
                verb.to_owned(),
                subject,
                with_decision_after(meta, row, verb),
            )
        }
        RowKind::Subagent {
            description,
            running,
            tool_count,
            duration_ms,
            ..
        } => {
            let mut meta = vec![format!(
                "{tool_count} tool{}",
                if *tool_count == 1 { "" } else { "s" }
            )];
            if *running {
                meta.push("running".into());
            } else if let Some(ms) = duration_ms {
                meta.push(text::duration(*ms));
            }
            (
                "Agent".to_owned(),
                first_line(description).to_owned(),
                meta.join(" · "),
            )
        }
        RowKind::Background { command, running } => (
            "In background".to_owned(),
            first_line(command).to_owned(),
            if *running { "running" } else { "finished" }.to_owned(),
        ),
        RowKind::Image {
            image,
            path,
            generated,
        } => {
            let subject = if path.is_empty() {
                image.as_ref().map(|b| b.name.clone()).unwrap_or_default()
            } else {
                path.clone()
            };
            let verb = if *generated {
                "Generated image"
            } else {
                "Image"
            };
            (verb.to_owned(), subject, String::new())
        }
        _ => (String::new(), String::new(), String::new()),
    }
}

/// "Read 4 files · searched 2".
fn run_words(run: &RunInfo) -> String {
    let plus = if run.open_below { "+" } else { "" };
    let mut parts = Vec::new();
    if run.reads > 0 {
        parts.push(format!(
            "Read {}{plus} file{}",
            run.reads,
            if run.reads == 1 { "" } else { "s" }
        ));
    }
    if run.searches > 0 {
        let lead = if parts.is_empty() {
            "Searched"
        } else {
            "searched"
        };
        parts.push(format!("{lead} {}{plus}", run.searches));
    }
    let other = run.len.saturating_sub(run.reads + run.searches);
    if other > 0 || parts.is_empty() {
        let lead = if parts.is_empty() {
            "Explored"
        } else {
            "explored"
        };
        parts.push(format!("{lead} {other}{plus} more"));
    }
    parts.join(" · ")
}

fn failed(row: &Row) -> bool {
    match &row.kind {
        RowKind::Command {
            state, exit_code, ..
        } => *state == ToolStateView::Failed || exit_code.is_some_and(|code| code != 0),
        RowKind::Explore { state, .. }
        | RowKind::ToolCall { state, .. }
        | RowKind::FileChange { state, .. } => *state == ToolStateView::Failed,
        _ => false,
    }
}

/// One step: a line of verb, subject and meta on the rail, then its detail
/// when opened.
#[allow(clippy::too_many_arguments)]
fn step(
    drawn: &mut Drawn,
    row: &Row,
    expanded: bool,
    current: bool,
    facts: &RowFacts,
    width: usize,
    theme: Theme,
) -> Option<()> {
    let (verb, subject, meta) = step_words(row, expanded);
    if verb.is_empty() && subject.is_empty() {
        return None;
    }
    // An open ask points at this step: it waits on the person, so it reads
    // as the step under way, marked in the accent, and nothing it has not
    // done yet is said about it.
    let asking = row.attention && verb.starts_with("Wants");
    let meta = if asking { String::new() } else { meta };
    let failed = !asking && failed(row);
    let current = current || asking;
    let (words, subject_style) = if failed {
        (theme.error(), theme.error())
    } else if current {
        (theme.bright(), theme.bright())
    } else {
        (theme.muted(), theme.muted())
    };
    let mut line = Line::default();
    pad_to(&mut line, EDGE);
    if failed {
        push(&mut line, "✗", theme.error(), width);
    } else if asking {
        push(&mut line, "●", theme.accent(), width);
    } else if current {
        push(&mut line, "●", theme.text(), width);
    } else {
        push(&mut line, "│", theme.hairline(), width);
    }
    pad_to(&mut line, WORDS);
    let diff = matches!(row.kind, RowKind::FileChange { .. })
        && !matches!(
            row.kind,
            RowKind::FileChange {
                state: ToolStateView::Pending | ToolStateView::Running,
                ..
            }
        );
    let control = if diff { "[diff]" } else { "" };
    let tail_width = if meta.is_empty() {
        0
    } else {
        text::str_width(&meta) + 3
    } + if diff {
        text::str_width(control) + 2
    } else {
        0
    };
    // Two blank columns at the right, as at the left.
    let width = width.saturating_sub(2);
    let room = width.saturating_sub(tail_width);
    push(&mut line, verb, words, room);
    if !subject.is_empty() {
        push(&mut line, " ", words, room);
        let left = room.saturating_sub(text::line_width(&line));
        let shown = if text::str_width(&subject) > left && left > 8 {
            tail(&subject, left)
        } else {
            subject
        };
        push(&mut line, shown, subject_style, room);
    }
    if !meta.is_empty() {
        push(&mut line, format!(" · {meta}"), theme.faint(), width);
    }
    let mut hits = vec![(None, FeedHit::Step(row.id.clone()))];
    if diff {
        push(&mut line, "  ", theme.faint(), width);
        let from = text::line_width(&line);
        push(&mut line, control, theme.faint(), width);
        hits.insert(0, (Some((from, text::line_width(&line))), FeedHit::Diff));
    }
    drawn.lines.push(line);
    drawn.hits.push(hits);
    // A multi-file change names each further file on its own line.
    if let RowKind::FileChange { files, .. } = &row.kind {
        for file in files.iter().skip(1) {
            let mut more = Line::default();
            pad_to(&mut more, EDGE);
            push(&mut more, "│", theme.hairline(), width);
            pad_to(&mut more, WORDS);
            push(&mut more, "and ", theme.muted(), width);
            push(&mut more, file.path.clone(), theme.muted(), width);
            if matches!(file.change, FileChangeView::Edited) {
                push(
                    &mut more,
                    format!(" · +{} −{}", file.added, file.removed),
                    theme.faint(),
                    width,
                );
            }
            drawn.hit_line(more, FeedHit::Step(row.id.clone()));
        }
    }
    if expanded {
        for line in step_detail(row, facts, width, theme) {
            let mut railed = Line::default();
            pad_to(&mut railed, EDGE);
            push(&mut railed, "│", theme.hairline(), width);
            pad_to(&mut railed, DETAIL);
            // The detail's own indent gives way to the rail and the column.
            railed.spans.extend(line.spans.into_iter().skip(1));
            drawn.hit_line(railed, FeedHit::Step(row.id.clone()));
        }
    }
    Some(())
}

/// A step's detail: a command's output, an edit's patch, a call's result,
/// a subagent's answer. Each line starts with the indent `detail` and
/// `patch_lines` give as its own first span, which the rail replaces.
fn step_detail(row: &Row, facts: &RowFacts, width: usize, theme: Theme) -> Vec<Line<'static>> {
    // Their text starts four in; ours starts at the detail column.
    let width = width.saturating_sub(DETAIL - 4);
    match &row.kind {
        RowKind::Command {
            output_head,
            more_lines,
            state,
            ..
        } => {
            let style = if *state == ToolStateView::Failed {
                theme.error()
            } else {
                theme.muted()
            };
            let mut text = output_head.join("\n");
            if *more_lines > 0 {
                text.push_str(&format!("\n··· {more_lines} more lines"));
            }
            if text.is_empty() {
                return Vec::new();
            }
            detail(&text, width, style, DETAIL_LINES, theme)
        }
        RowKind::FileChange { .. } => facts
            .patch
            .as_ref()
            .map(|patch| patch_lines(patch, true, facts.leader, width, theme))
            .unwrap_or_default(),
        RowKind::ToolCall { result, .. } if !result.is_empty() => {
            detail(result, width, theme.muted(), DETAIL_LINES, theme)
        }
        RowKind::Subagent { answer, .. } if !answer.is_empty() => {
            detail(answer, width, theme.muted(), DETAIL_LINES, theme)
        }
        _ => Vec::new(),
    }
}

/// A turn's length at a glance: "25s", "6m", "1h 12m". Seconds stop
/// mattering past the first minute.
fn worked(ms: i64) -> String {
    let secs = ms.max(0) / 1_000;
    match secs {
        0..60 => format!("{}s", secs.max(1)),
        60..3_600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3_600, secs % 3_600 / 60),
    }
}

/// "14:07": the local time a row was written.
fn clock(at_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|at| at.format("%H:%M").to_string())
        .unwrap_or_default()
}
