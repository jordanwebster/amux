//! Chat rows as terminal lines. Each row is one line of glyph, verb, subject
//! and ` · ` meta; consecutive tool rows hang off one rail in the gutter,
//! and rows that open show their detail below when the reader expands
//! them. Wording lives here; the facts come from `ui_view::Row`.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ui_view::{
    AnswerView, AskRow, AttachmentView, Decision, DecisionView, ExploreVerb, FileChangeView,
    LineKind, PatchHead, PlanVerdict, QuestionView, Resolution, Row, RowKind, RunInfo, Segment,
    ToolStateView,
};
use wire::{BoundaryKind, EnvelopeKind, SendState};

use crate::markdown::markdown_rows;
use crate::text::{self, first_line, push, push_right};
use crate::theme::Theme;

/// Where body text starts.
const INDENT: usize = 4;
/// Lines of an opened body before it is cut with a count.
pub const OPEN_LINES: usize = 40;
/// Lines of a landed edit's patch shown under its row until it is opened.
pub const PATCH_HEAD_LINES: usize = 5;

/// How the reader has this row, and where it sits among its neighbours.
#[derive(Clone, Copy, Debug, Default)]
pub struct RowState {
    pub focused: bool,
    /// The row is open, or its run is expanded.
    pub expanded: bool,
    /// A tool row beside another: it hangs off the rail in the gutter.
    pub rail: bool,
    /// The next row continues the rail, so no blank line separates them.
    pub joined: bool,
}

/// What a row shows beyond its own view value, looked up by the layout.
#[derive(Clone, Debug)]
pub struct RowFacts {
    /// A collapsed run's newest subjects, newest first.
    pub run_subjects: Vec<String>,
    /// A landed file change's patch head.
    pub patch: Option<PatchHead>,
    /// The leader key, for the keys a row names.
    pub leader: char,
}

impl Default for RowFacts {
    fn default() -> Self {
        RowFacts {
            run_subjects: Vec::new(),
            patch: None,
            leader: 'a',
        }
    }
}

/// Whether a row is a tool row: one that joins its neighbours' rail.
pub fn on_rail(row: &Row) -> bool {
    matches!(
        row.kind,
        RowKind::Explore { .. }
            | RowKind::Command { .. }
            | RowKind::ToolCall { .. }
            | RowKind::FileChange { .. }
            | RowKind::Background { .. }
            | RowKind::Subagent { .. }
    )
}

/// The lines one row draws at `width`, its trailing blank separator
/// included unless the next row joins its rail. A row the view marks
/// collapsed draws nothing.
pub fn row_lines(
    row: &Row,
    state: RowState,
    facts: &RowFacts,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = body(row, state, facts, width, theme);
    if lines.is_empty() {
        return lines;
    }
    if state.rail {
        for line in &mut lines {
            if let Some(first) = line.spans.first_mut()
                && first.content.starts_with(' ')
            {
                let rest = first.content[1..].to_owned();
                *first = Span::styled(rest, first.style);
                line.spans.insert(0, Span::styled("│", theme.muted()));
            }
        }
    }
    let bar = if state.focused {
        Some(Span::styled("▌", theme.focus_bar()))
    } else if row.attention {
        Some(Span::styled("▌", theme.accent()))
    } else {
        None
    };
    if let Some(bar) = bar {
        // The bar takes the row's first cell, so the row keeps its width.
        for line in &mut lines {
            if let Some(first) = line.spans.first_mut() {
                let mut chars = first.content.chars();
                if matches!(chars.next(), Some(' ' | '▎' | '│')) {
                    *first = Span::styled(chars.as_str().to_owned(), first.style);
                }
            }
            line.spans.insert(0, bar.clone());
        }
    }
    if !state.joined {
        lines.push(Line::default());
    }
    lines
}

pub(crate) fn state_meta(state: ToolStateView) -> Option<&'static str> {
    match state {
        ToolStateView::Pending => Some("waiting"),
        ToolStateView::Running => Some("running"),
        ToolStateView::Succeeded => None,
        ToolStateView::Failed => Some("failed"),
        ToolStateView::Denied => Some("denied"),
        ToolStateView::Cancelled => Some("cancelled"),
    }
}

/// A call's verb says what happened to it: waiting for the person, under
/// way, refused, cancelled or done. The meta then never repeats it.
pub(crate) fn call_verb(
    state: ToolStateView,
    row: &Row,
    [wants, doing, done]: [&'static str; 3],
) -> &'static str {
    let denied = state == ToolStateView::Denied
        || row
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == DecisionView::Denied);
    match state {
        _ if denied => "Denied",
        ToolStateView::Pending => wants,
        // An open ask on the call: it runs only if the person allows it.
        ToolStateView::Running if row.attention && row.decision.is_none() => wants,
        ToolStateView::Running => doing,
        ToolStateView::Cancelled => "Cancelled",
        ToolStateView::Succeeded | ToolStateView::Failed | ToolStateView::Denied => done,
    }
}

fn state_glyph(state: ToolStateView, done: &'static str, theme: Theme) -> (&'static str, Style) {
    match state {
        ToolStateView::Pending | ToolStateView::Running => ("▸", theme.accent()),
        ToolStateView::Succeeded => (done, theme.muted()),
        ToolStateView::Failed => ("✗", theme.error()),
        ToolStateView::Denied | ToolStateView::Cancelled => ("⊘", theme.muted()),
    }
}

/// "  ✔ Ran cargo test · exit 101 · 4.2s": the subject gives way first, so
/// the meta always shows.
fn head(
    glyph: (&str, Style),
    verb: &str,
    subject: &str,
    meta: &str,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, glyph.0, glyph.1, width);
    push(&mut line, " ", theme.text(), width);
    let meta = if meta.is_empty() {
        String::new()
    } else if subject.is_empty() && verb.is_empty() {
        meta.to_owned()
    } else {
        format!(" · {meta}")
    };
    let room = width.saturating_sub(text::str_width(&meta));
    if !verb.is_empty() {
        push(&mut line, verb, theme.emphasis(), room);
        if !subject.is_empty() {
            push(&mut line, " ", theme.text(), room);
        }
    }
    push(&mut line, subject, theme.code(), room);
    push(&mut line, meta, theme.muted(), width);
    line
}

/// Detail hung under a tool row: "└ " before its first line.
fn hung(text: &str, width: usize, style: Style, limit: usize, theme: Theme) -> Vec<Line<'static>> {
    hang(detail(text, width.saturating_sub(2), style, limit, theme))
}

/// Puts the hook before the first of these indented lines and aligns the
/// rest under its text.
fn hang(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut line)| {
            let hook = if i == 0 { "└ " } else { "  " };
            line.spans.insert(1, Span::raw(hook));
            line
        })
        .collect()
}

/// Indented body lines, cut with a count past `limit`.
pub(crate) fn detail(
    text: &str,
    width: usize,
    style: Style,
    limit: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let wrapped = text::wrap(text, width.saturating_sub(INDENT));
    let total = wrapped.len();
    let mut lines: Vec<Line<'static>> = wrapped
        .into_iter()
        .take(limit)
        .map(|line| {
            Line::from(vec![
                Span::raw(" ".repeat(INDENT)),
                Span::styled(line, style),
            ])
        })
        .collect();
    if total > limit {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(INDENT)),
            Span::styled(format!("··· {} more lines", total - limit), theme.muted()),
        ]));
    }
    lines
}

pub(crate) fn markdown(
    source: &str,
    width: usize,
    style_note: bool,
    theme: Theme,
) -> Vec<Line<'static>> {
    markdown_rows(source, width.saturating_sub(INDENT).max(1), theme)
        .into_iter()
        .map(|mut spans| {
            if style_note {
                for span in &mut spans {
                    span.style = theme.muted();
                }
            }
            spans.insert(0, Span::raw(" ".repeat(INDENT)));
            Line::from(spans)
        })
        .collect()
}

/// A link on a drawn line: its columns and where it goes.
pub(crate) type Link = (usize, usize, String);

/// Markdown drawn at the text's indent, each line with the links on it by
/// column, for a feed that opens them on a click.
pub(crate) fn markdown_linked(
    source: &str,
    width: usize,
    wide: usize,
    theme: Theme,
) -> Vec<(Line<'static>, Vec<Link>)> {
    crate::markdown::markdown_lines_wide(
        source,
        width.saturating_sub(INDENT).max(1),
        wide.saturating_sub(INDENT).max(1),
        theme,
    )
    .iter()
    .map(|line| {
        let mut spans = vec![Span::raw(" ".repeat(INDENT))];
        spans.extend(line.spans.iter().cloned());
        let links = line
            .links
            .iter()
            .map(|(from, to, url)| (INDENT + from, INDENT + to, url.clone()))
            .collect();
        (Line::from(spans), links)
    })
    .collect()
}

/// An attachment's chip, from its reference alone.
pub fn chip(view: &AttachmentView) -> String {
    match view {
        // The name says what it is.
        AttachmentView::Image(blob) | AttachmentView::File(blob) => {
            format!("[{} · {}]", blob.name, text::bytes(blob.size))
        }
        AttachmentView::Text { name, lines, .. } => format!("[{name} · {lines} lines]"),
        AttachmentView::Review { comments, .. } => match comments {
            1 => "[review · 1 comment]".into(),
            n => format!("[review · {n} comments]"),
        },
        AttachmentView::Empty => "[attachment]".into(),
    }
}

/// Text with chips at their places, wrapped at `width` in `style`.
pub fn segment_lines(
    segments: &[Segment],
    width: usize,
    style: Style,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![Line::default()];
    let width = width.max(1);
    for segment in segments {
        match segment {
            Segment::Text(text) => {
                for (i, part) in text.split('\n').enumerate() {
                    if i > 0 {
                        lines.push(Line::default());
                    }
                    let used = lines.last().map_or(0, text::line_width);
                    let wrapped = text::wrap(part, width.saturating_sub(used).max(1));
                    for (j, piece) in wrapped.into_iter().enumerate() {
                        if j > 0 {
                            lines.push(Line::default());
                        }
                        if let Some(last) = lines.last_mut() {
                            last.spans.push(Span::styled(piece, style));
                        }
                    }
                }
            }
            Segment::Attachment(view) => {
                let chip = chip(view);
                let used = lines.last().map_or(0, text::line_width);
                if used > 0 && used + text::str_width(&chip) + 1 > width {
                    lines.push(Line::default());
                } else if used > 0
                    && let Some(last) = lines.last_mut()
                {
                    last.spans.push(Span::styled(" ", style));
                }
                if let Some(last) = lines.last_mut() {
                    last.spans.push(Span::styled(
                        text::ellipsize(&chip, width),
                        theme.chip_raised(),
                    ));
                }
            }
        }
    }
    lines
}

/// A decision's meta; without its outcome word when the row's verb says
/// it.
fn decision_meta(decision: &Decision, said: bool) -> String {
    let mut parts = Vec::new();
    if !said {
        parts.push(
            match decision.outcome {
                DecisionView::Allowed => "allowed",
                DecisionView::Denied => "denied",
                DecisionView::AutoApproved => "auto-approved",
                DecisionView::Dismissed => "dismissed",
            }
            .to_owned(),
        );
    }
    if let Some(scope) = &decision.scope {
        parts.push(scope.clone());
    }
    if let Some(note) = &decision.note {
        parts.push(format!("\"{}\"", first_line(note)));
    }
    if decision.elsewhere {
        parts.push("in the terminal".into());
    }
    parts.join(" · ")
}

/// The row's meta with its permission decision after it. The decision
/// already says allowed or denied, so the call's own state word for the
/// same thing is dropped.
fn with_decision(meta: String, row: &Row) -> String {
    with_decision_after(meta, row, "")
}

/// [`with_decision`] on a row whose verb may already say the outcome, as
/// "Denied" does.
pub(crate) fn with_decision_after(meta: String, row: &Row, verb: &str) -> String {
    let Some(decision) = &row.decision else {
        return meta;
    };
    let kept: Vec<&str> = meta
        .split(" · ")
        .filter(|part| !part.is_empty() && !matches!(*part, "denied" | "cancelled" | "waiting"))
        .collect();
    let decided = decision_meta(
        decision,
        verb == "Denied" && decision.outcome == DecisionView::Denied,
    );
    if decided.is_empty() || kept.is_empty() {
        [kept.join(" · "), decided].concat()
    } else {
        format!("{} · {decided}", kept.join(" · "))
    }
}

fn run_summary(run: &RunInfo) -> String {
    let mut parts = Vec::new();
    let plus = if run.open_below { "+" } else { "" };
    if run.reads > 0 {
        parts.push(format!(
            "{}{plus} read{}",
            run.reads,
            if run.reads == 1 { "" } else { "s" }
        ));
    }
    if run.searches > 0 {
        parts.push(format!(
            "{}{plus} search{}",
            run.searches,
            if run.searches == 1 { "" } else { "es" }
        ));
    }
    let other = run.len.saturating_sub(run.reads + run.searches);
    if other > 0 || parts.is_empty() {
        parts.push(format!("{other}{plus} more"));
    }
    parts.join(" · ")
}

pub(crate) fn explore_verb(verb: ExploreVerb) -> &'static str {
    match verb {
        ExploreVerb::Read => "Read",
        ExploreVerb::Search => "Searched",
        ExploreVerb::List => "Listed",
        ExploreVerb::Fetch => "Fetched",
        ExploreVerb::WebSearch => "Searched the web",
    }
}

fn answer_text(answer: &AnswerView) -> String {
    if answer.hidden {
        return "answered (hidden)".into();
    }
    let mut picks = answer.picked.clone();
    if let Some(other) = &answer.other {
        picks.push(format!("\"{other}\""));
    }
    picks.join(", ")
}

fn resolution_verb(resolution: Resolution, answered: &'static str) -> &'static str {
    match resolution {
        Resolution::Open => "Asking",
        Resolution::Answered => answered,
        Resolution::Declined => "Declined",
        Resolution::Cancelled => "Cancelled",
        Resolution::Dismissed => "Dismissed",
    }
}

fn resolution_glyph(resolution: Resolution, theme: Theme) -> (&'static str, Style) {
    match resolution {
        Resolution::Open => ("?", theme.accent()),
        Resolution::Answered => ("✔", theme.ok()),
        _ => ("⊘", theme.muted()),
    }
}

fn body(
    row: &Row,
    state: RowState,
    facts: &RowFacts,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    if row.collapsed {
        return Vec::new();
    }
    let open = state.expanded;
    if let Some(run) = row.run.as_ref().filter(|run| run.is_summary && !open) {
        // "⌄ 2 reads · 1 search · sync/config.rs, sync/client.rs · C-a o expand"
        let mut line = Line::from(Span::raw("  "));
        push(&mut line, "⌄ ", theme.muted(), width);
        push(&mut line, run_summary(run), theme.text(), width);
        let hint = format!(" · ctrl+{} o expand", facts.leader);
        let room = width.saturating_sub(text::str_width(&hint));
        let mut subjects: Vec<String> = facts
            .run_subjects
            .iter()
            .rev()
            .map(|subject| tail(subject, 40))
            .collect();
        if subjects.is_empty() && !run.anchor.is_empty() {
            subjects.push(tail(&run.anchor, 40));
        }
        if !subjects.is_empty() {
            push(&mut line, " · ", theme.muted(), room);
            push(&mut line, subjects.join(", "), theme.code(), room);
        }
        push(&mut line, hint, theme.muted(), width);
        return vec![line];
    }
    match &row.kind {
        RowKind::Prompt { text, steered } => {
            let mut lines = Vec::new();
            let surface = theme.user_surface();
            for (i, words) in segment_lines(text, width.saturating_sub(INDENT), theme.text(), theme)
                .into_iter()
                .enumerate()
            {
                let mut line = Line::from(Span::styled("▎   ", theme.accent_bar()));
                line.spans.extend(
                    words
                        .spans
                        .into_iter()
                        .map(|span| Span::styled(span.content, span.style.patch(surface))),
                );
                if i == 0 && *steered {
                    push_right(&mut line, "steered", theme.muted().patch(surface), width);
                }
                text::fill(&mut line, surface, width);
                lines.push(line);
            }
            lines
        }
        RowKind::Prose {
            text,
            streaming,
            working_note,
        } => {
            let source: String = text
                .iter()
                .map(|segment| match segment {
                    Segment::Text(text) => text.clone(),
                    Segment::Attachment(view) => chip(view),
                })
                .collect();
            let mut lines = markdown(&source, width, *working_note, theme);
            if *streaming && let Some(last) = lines.last_mut() {
                last.spans.push(Span::styled(" ▍", theme.muted()));
            }
            lines
        }
        RowKind::Thinking {
            text,
            open: thinking,
            duration_ms,
        } => {
            let verb = match (thinking, duration_ms) {
                (true, _) => "Thinking".to_owned(),
                (false, Some(ms)) => format!("Thought for {}", text::duration(*ms)),
                (false, None) => "Thought".to_owned(),
            };
            let mut line = Line::from(Span::raw("  "));
            push(&mut line, "~ ", theme.muted(), width);
            push(&mut line, verb, theme.italic(), width);
            let mut lines = vec![line];
            if open && !text.is_empty() {
                lines.extend(detail(text, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
        RowKind::ToolCall {
            server,
            tool,
            fact,
            state: tool_state,
            result,
        } => {
            let subject = if server.is_empty() {
                tool.clone()
            } else {
                format!("{server} · {tool}")
            };
            let verb = call_verb(*tool_state, row, ["Wants to use", "Using", "Used"]);
            let mut meta = fact.clone();
            if *tool_state == ToolStateView::Failed {
                meta = if meta.is_empty() {
                    "failed".into()
                } else {
                    format!("{meta} · failed")
                };
            }
            let meta = with_decision_after(meta, row, verb);
            let mut lines = vec![head(
                state_glyph(*tool_state, "✔", theme),
                verb,
                &subject,
                &meta,
                width,
                theme,
            )];
            if open && !result.is_empty() {
                lines.extend(hung(result, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
        RowKind::FileChange {
            files,
            state: tool_state,
        } => {
            let mut lines = Vec::new();
            for (i, file) in files.iter().enumerate() {
                let (verb, subject, mut meta) = match &file.change {
                    FileChangeView::Edited => (
                        "Edited",
                        file.path.clone(),
                        format!("+{} −{}", file.added, file.removed),
                    ),
                    FileChangeView::Created { lines } => {
                        ("Created", file.path.clone(), format!("{lines} lines"))
                    }
                    FileChangeView::Deleted => ("Deleted", file.path.clone(), String::new()),
                    FileChangeView::Moved { to } => {
                        ("Moved", format!("{} → {to}", file.path), String::new())
                    }
                };
                if i == 0 {
                    if let Some(word) = state_meta(*tool_state) {
                        meta = if meta.is_empty() {
                            word.into()
                        } else {
                            format!("{meta} · {word}")
                        };
                    }
                    meta = with_decision(meta, row);
                }
                lines.push(head(
                    state_glyph(*tool_state, "✎", theme),
                    verb,
                    &subject,
                    &meta,
                    width,
                    theme,
                ));
                // The landed patch is the first file's, under its line.
                if i == 0
                    && let Some(patch) = &facts.patch
                {
                    lines.extend(patch_lines(patch, open, facts.leader, width, theme));
                }
            }
            if lines.is_empty() {
                lines.push(head(
                    state_glyph(*tool_state, "✎", theme),
                    "Editing",
                    "",
                    &with_decision(String::new(), row),
                    width,
                    theme,
                ));
            }
            lines
        }
        RowKind::Command {
            command,
            state: tool_state,
            exit_code,
            output_head,
            more_lines,
            duration_ms,
            ..
        } => {
            let verb = call_verb(*tool_state, row, ["Wants to run", "Running", "Ran"]);
            let mut meta = Vec::new();
            match (exit_code, tool_state) {
                (Some(code), _) if *code != 0 => meta.push(format!("exit {code}")),
                (None, ToolStateView::Failed) => meta.push("failed".into()),
                _ => {}
            }
            if let Some(ms) = duration_ms {
                meta.push(text::duration(*ms));
            }
            let meta = with_decision_after(meta.join(" · "), row, verb);
            let mut lines = vec![head(
                state_glyph(*tool_state, "✔", theme),
                verb,
                first_line(command),
                &meta,
                width,
                theme,
            )];
            let style = if *tool_state == ToolStateView::Failed {
                theme.error()
            } else {
                theme.muted()
            };
            let mut out: Vec<Line<'static>> = output_head
                .iter()
                .map(|out| {
                    let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
                    push(&mut line, out, style, width.saturating_sub(2));
                    line
                })
                .collect();
            if *more_lines > 0 {
                let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
                push(
                    &mut line,
                    format!("··· {more_lines} more lines"),
                    theme.muted(),
                    width.saturating_sub(2),
                );
                out.push(line);
            }
            lines.extend(hang(out));
            lines
        }
        RowKind::Explore {
            verb,
            subject,
            state: tool_state,
        } => {
            let meta = with_decision(state_meta(*tool_state).unwrap_or_default().to_owned(), row);
            let mut lines = vec![head(
                state_glyph(*tool_state, "·", theme),
                explore_verb(*verb),
                subject,
                &meta,
                width,
                theme,
            )];
            if let Some(run) = row.run.as_ref().filter(|run| run.is_summary && open) {
                let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
                push(
                    &mut line,
                    format!("⌃ {}", run_summary(run)),
                    theme.muted(),
                    width,
                );
                lines.push(line);
            }
            lines
        }
        RowKind::Subagent {
            description,
            running,
            tool_count,
            last_tool,
            answer,
            duration_ms,
        } => {
            let mut meta = vec![format!(
                "{tool_count} tool{}",
                if *tool_count == 1 { "" } else { "s" }
            )];
            if let Some(ms) = duration_ms {
                meta.push(text::duration(*ms));
            }
            let glyph = if *running {
                ("◇", theme.accent())
            } else {
                ("◆", theme.muted())
            };
            let mut lines = vec![head(
                glyph,
                "Agent",
                first_line(description),
                &meta.join(" · "),
                width,
                theme,
            )];
            if *running && !last_tool.is_empty() {
                let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
                push(&mut line, format!("└ {last_tool}"), theme.muted(), width);
                lines.push(line);
            } else if !answer.is_empty() {
                let limit = if open { OPEN_LINES } else { 2 };
                lines.extend(hung(answer, width, theme.muted(), limit, theme));
            }
            lines
        }
        RowKind::Background {
            command, running, ..
        } => vec![head(
            ("◷", theme.muted()),
            "In background",
            first_line(command),
            if *running { "running" } else { "finished" },
            width,
            theme,
        )],
        RowKind::Image {
            image,
            path,
            generated,
        } => {
            let subject = if path.is_empty() {
                image
                    .as_ref()
                    .map(|blob| blob.name.clone())
                    .unwrap_or_default()
            } else {
                path.clone()
            };
            let meta = image
                .as_ref()
                .map(|blob| text::bytes(blob.size))
                .unwrap_or_default();
            vec![head(
                ("▣", theme.muted()),
                if *generated {
                    "Generated image"
                } else {
                    "Image"
                },
                &subject,
                &meta,
                width,
                theme,
            )]
        }
        RowKind::SlashOutput {
            command,
            args,
            output,
        } => {
            let subject = if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {args}")
            };
            let mut lines = vec![head(("/", theme.muted()), "", &subject, "", width, theme)];
            let limit = if open { OPEN_LINES } else { 6 };
            lines.extend(detail(output, width, theme.muted(), limit, theme));
            lines
        }
        RowKind::Ask(ask) => ask_row(ask, open, width, theme),
        RowKind::TurnEnd {
            duration_ms,
            cost_usd,
            failed,
        } => {
            let mut words = Vec::new();
            match duration_ms {
                Some(ms) => words.push(format!("Worked {}", text::duration(*ms))),
                None => words.push("Turn ended".into()),
            }
            if let Some(cost) = cost_usd {
                words.push(format!("${cost:.2}"));
            }
            if *failed {
                words.push("failed".into());
            }
            let style = if *failed {
                theme.error()
            } else {
                theme.muted()
            };
            let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
            push(&mut line, words.join(" · "), style, width);
            vec![line]
        }
        RowKind::Stopped => {
            let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
            push(&mut line, "You stopped it", theme.muted(), width);
            vec![line]
        }
        RowKind::Compaction {
            tokens_before,
            tokens_after,
            automatic,
        } => {
            let mut words = String::from("Compacted");
            if let (Some(before), Some(after)) = (tokens_before, tokens_after) {
                words.push_str(&format!(
                    " · {} → {}",
                    text::tokens(*before),
                    text::tokens(*after)
                ));
            }
            if *automatic {
                words.push_str(" · automatic");
            }
            vec![rule(&words, width, theme)]
        }
        RowKind::Error {
            error_kind,
            message,
            attempts,
            gave_up,
        } => {
            let meta = if *gave_up && *attempts > 1 {
                format!("gave up after {attempts} tries")
            } else {
                String::new()
            };
            let subject = if error_kind.is_empty() {
                first_line(message).to_owned()
            } else {
                error_kind.clone()
            };
            let mut lines = vec![head(
                ("✗", theme.error()),
                "",
                &subject,
                &meta,
                width,
                theme,
            )];
            if !error_kind.is_empty() && !message.is_empty() {
                lines.extend(detail(
                    message,
                    width,
                    theme.muted(),
                    if open { OPEN_LINES } else { 3 },
                    theme,
                ));
            }
            lines
        }
        RowKind::ModelSwitch { from, to, reason } => {
            let _ = from;
            vec![head(
                ("⇄", theme.muted()),
                "Switched to",
                to,
                reason,
                width,
                theme,
            )]
        }
        RowKind::Boundary { kind, cause } => {
            let word = match kind {
                BoundaryKind::Started => "started",
                BoundaryKind::Cleared => "cleared",
                BoundaryKind::Compacted => "compacted",
                BoundaryKind::Resumed => "resumed",
                BoundaryKind::Forked => "forked",
                BoundaryKind::Restarted => "restarted",
                BoundaryKind::Exited => "exited",
                BoundaryKind::DaemonLost => "lost its daemon",
                BoundaryKind::Unspecified => "session",
            };
            let words = if cause.is_empty() {
                word.to_owned()
            } else {
                format!("{word} · {cause}")
            };
            vec![rule(&words, width, theme)]
        }
        RowKind::AgentMessage {
            from,
            kind,
            text: body,
            to,
            sent,
            rejection,
        } => {
            let (verb, who) = if to.is_empty() {
                ("From", from.as_str())
            } else {
                ("To", to.as_str())
            };
            let meta = match (kind, sent) {
                (EnvelopeKind::Finished, _) => "finished".to_owned(),
                (EnvelopeKind::Failed, _) => "failed".to_owned(),
                (_, SendState::Sending) => "sending".to_owned(),
                (_, SendState::Rejected) => format!("not delivered · {rejection}"),
                _ => String::new(),
            };
            let mut lines = vec![head(("✉", theme.muted()), verb, who, &meta, width, theme)];
            let limit = if open { OPEN_LINES } else { 1 };
            lines.extend(detail(body.trim(), width, theme.text(), limit, theme));
            lines
        }
        RowKind::AutoReview {
            decision,
            risk,
            rationale,
            ..
        } => {
            let meta = if risk.is_empty() {
                String::new()
            } else {
                format!("{risk} risk")
            };
            let mut lines = vec![head(
                ("⚖", theme.muted()),
                "Auto-reviewed",
                decision,
                &meta,
                width,
                theme,
            )];
            if open {
                lines.extend(detail(rationale, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
        RowKind::Unrecognized { what, summary } => {
            vec![head(
                ("?", theme.muted()),
                "Unreadable",
                what,
                first_line(summary),
                width,
                theme,
            )]
        }
        RowKind::Hidden => Vec::new(),
    }
}

/// A landed edit's patch under its row: numbered lines in the diff
/// colours, then how much more there is and the key that opens it.
pub(crate) fn patch_lines(
    patch: &PatchHead,
    open: bool,
    leader: char,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let digits = patch
        .lines
        .iter()
        .filter_map(|line| line.number)
        .max()
        .map_or(0, |n| n.to_string().len());
    let mut lines = Vec::new();
    for line in &patch.lines {
        let (sign, style) = match line.kind {
            LineKind::Added => ("+", theme.diff_added()),
            LineKind::Removed => ("-", theme.diff_removed()),
            LineKind::Context => (" ", theme.diff_context()),
        };
        let number = line
            .number
            .map(|n| format!("{n:>digits$}"))
            .unwrap_or_else(|| " ".repeat(digits));
        let mut out = Line::from(Span::raw(" ".repeat(INDENT)));
        if digits > 0 {
            push(&mut out, format!("{number} │ "), theme.muted(), width);
        }
        push(&mut out, format!("{sign}{}", line.text), style, width);
        lines.push(out);
    }
    if patch.more > 0 {
        let mut out = Line::from(Span::raw(" ".repeat(INDENT)));
        let words = if open {
            format!("··· {} more lines", patch.more)
        } else {
            format!("··· {} more lines · C-{leader} o open", patch.more)
        };
        push(&mut out, words, theme.muted(), width);
        lines.push(out);
    }
    lines
}

/// The end of `text` in at most `max` characters, "…" marking a cut.
pub(crate) fn tail(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let kept: String = chars[chars.len() - max.saturating_sub(1)..]
        .iter()
        .collect();
    format!("…{kept}")
}

/// A session boundary, drawn like home's section headings: the words in
/// the faint ink, then a hairline to the right margin.
pub(crate) fn rule(words: &str, width: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, format!("{words} "), theme.faint(), width);
    let end = width.saturating_sub(2);
    let used = text::line_width(&line);
    if used < end {
        line.spans
            .push(Span::styled("─".repeat(end - used), theme.hairline()));
    }
    line
}

/// Questions asked as the work: one reads "Answered <question> · <answer>",
/// several list each header with its answer.
fn questions_lines(
    questions: &[QuestionView],
    answers: &[AnswerView],
    note: Option<&str>,
    resolution: Resolution,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let glyph = resolution_glyph(resolution, theme);
    let verb = resolution_verb(resolution, "Answered");
    let mut lines = Vec::new();
    if questions.len() == 1 {
        let question = &questions[0];
        let subject = match answers.first().map(answer_text) {
            Some(answer) if !answer.is_empty() => format!("{} · {answer}", question.question),
            _ => question.question.clone(),
        };
        lines.push(head(glyph, verb, &subject, "", width, theme));
    } else {
        let subject = format!("{} questions", questions.len());
        lines.push(head(glyph, verb, &subject, "", width, theme));
        for (question, answer) in questions.iter().zip(answers) {
            let label = if question.header.is_empty() {
                &question.question
            } else {
                &question.header
            };
            let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
            push(&mut line, format!("{label}: "), theme.muted(), width);
            push(&mut line, answer_text(answer), theme.text(), width);
            lines.push(line);
        }
    }
    if let Some(note) = note {
        let mut line = Line::from(Span::raw(" ".repeat(INDENT)));
        push(
            &mut line,
            format!("Note: {}", first_line(note)),
            theme.muted(),
            width,
        );
        lines.push(line);
    }
    lines
}

fn ask_row(ask: &AskRow, open: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    match ask {
        AskRow::Question {
            questions,
            answers,
            note,
            resolution,
        }
        | AskRow::Questions {
            questions,
            answers,
            note,
            resolution,
        } => questions_lines(
            questions,
            answers,
            note.as_deref(),
            *resolution,
            width,
            theme,
        ),
        AskRow::Plan {
            plan,
            verdict,
            note,
            ..
        } => {
            let (glyph, verb) = match verdict {
                PlanVerdict::Open => (("?", theme.accent()), "Plan proposed"),
                PlanVerdict::Approved => (("✔", theme.ok()), "Plan approved"),
                PlanVerdict::SentBack => (("↩", theme.muted()), "Plan sent back"),
                PlanVerdict::Dismissed => (("⊘", theme.muted()), "Plan dismissed"),
            };
            // Sent back, the row says why; otherwise it names the plan by
            // its first line, without the heading's markup.
            let subject = match (verdict, note) {
                (PlanVerdict::SentBack, Some(note)) => format!("\"{}\"", first_line(note)),
                _ => first_line(plan)
                    .trim_start_matches('#')
                    .trim_start()
                    .to_owned(),
            };
            let mut lines = vec![head(glyph, verb, &subject, "", width, theme)];
            if open {
                lines.extend(markdown(plan, width, false, theme));
            }
            lines
        }
        AskRow::Form {
            server,
            message,
            fields,
            resolution,
        } => {
            let (verb, subject) = match resolution {
                Resolution::Answered => (
                    format!(
                        "Sent {} field{} to",
                        fields.len(),
                        if fields.len() == 1 { "" } else { "s" }
                    ),
                    server.clone(),
                ),
                Resolution::Open => (format!("{server} needs details"), String::new()),
                other => (
                    format!("{} {server}'s form", resolution_verb(*other, "")),
                    String::new(),
                ),
            };
            let mut lines = vec![head(
                resolution_glyph(*resolution, theme),
                &verb,
                &subject,
                "",
                width,
                theme,
            )];
            if open && !message.is_empty() {
                lines.extend(detail(message, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
        AskRow::Link {
            server,
            message,
            url,
            resolution,
        } => {
            let verb = match resolution {
                Resolution::Answered => "Opened link from".to_owned(),
                Resolution::Open => "Link from".to_owned(),
                other => format!("{} link from", resolution_verb(*other, "")),
            };
            let mut lines = vec![head(
                resolution_glyph(*resolution, theme),
                &verb,
                server,
                "",
                width,
                theme,
            )];
            if open {
                lines.extend(detail(
                    &format!("{message}\n{url}"),
                    width,
                    theme.muted(),
                    OPEN_LINES,
                    theme,
                ));
            }
            lines
        }
        AskRow::Unanswerable { reason, resolution } => {
            let verb = match resolution {
                Resolution::Open => "Can't answer this here".to_owned(),
                other => format!(
                    "{} a dialog this build can't read",
                    resolution_verb(*other, "Answered")
                ),
            };
            let mut lines = vec![head(
                resolution_glyph(*resolution, theme),
                &verb,
                "",
                "",
                width,
                theme,
            )];
            if open {
                lines.extend(detail(reason, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
        AskRow::Grant {
            reason,
            read,
            write,
            network,
            hosts,
            granted,
            resolution,
        } => {
            let describe = |read: &[String], write: &[String], network: bool| {
                let mut parts = Vec::new();
                if !write.is_empty() {
                    parts.push(format!("write {}", write.join(", ")));
                }
                if !read.is_empty() {
                    parts.push(format!("read {}", read.join(", ")));
                }
                if network {
                    parts.push(if hosts.is_empty() {
                        "network".to_owned()
                    } else {
                        format!("network {}", hosts.join(", "))
                    });
                }
                parts.join(" · ")
            };
            let (verb, subject, meta) = match (resolution, granted) {
                (Resolution::Answered, Some(granted)) => (
                    "Granted",
                    describe(&granted.read, &granted.write, granted.network),
                    if granted.for_session {
                        "this session"
                    } else {
                        "this turn"
                    }
                    .to_owned(),
                ),
                (Resolution::Open, _) => (
                    "Wants access",
                    describe(read, write, *network),
                    String::new(),
                ),
                (other, _) => (
                    resolution_verb(*other, "Denied"),
                    describe(read, write, *network),
                    String::new(),
                ),
            };
            let mut lines = vec![head(
                resolution_glyph(*resolution, theme),
                verb,
                &subject,
                &meta,
                width,
                theme,
            )];
            if open && !reason.is_empty() {
                lines.extend(detail(reason, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
    }
}
