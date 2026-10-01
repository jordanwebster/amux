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
    AnswerView, AskRow, DecisionView, FileChangeView, PlanVerdict, QuestionView, Resolution, Row,
    RowKind, RunInfo, Segment, Stretch, StretchCounts, ToolStateView,
};

use super::rows::{
    RowFacts, call_verb, chip, detail, explore_verb, markdown_linked, patch_lines, segment_lines,
    state_meta, tail,
};
use crate::text::{self, first_line, pad_to, push, push_right};
use crate::theme::Theme;

/// A stretch under way shows at most this many of its newest steps.
pub const LIVE_STEPS: usize = 3;
/// The left edge: markers, and the rail joining a stretch's steps.
const EDGE: usize = 2;
/// Where words start.
pub(crate) const WORDS: usize = 4;
/// The longest line of the agent's text, in columns, on a wide terminal.
const READING: usize = 100;
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
    /// Open a link in the agent's text.
    Link(String),
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
            RowKind::Ask(AskRow::Plan {
                plan,
                verdict,
                edits_accepted,
                note,
                writing,
            }) => plan_lines(
                &mut drawn,
                &row.id,
                &Plan {
                    text: plan,
                    verdict: *verdict,
                    edits_accepted: *edits_accepted,
                    note: note.as_deref(),
                    writing: *writing,
                },
                expanded,
                width,
                theme,
            ),
            RowKind::Ask(
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
                },
            ) => questions_step(
                &mut drawn,
                row,
                questions,
                answers,
                note.as_deref(),
                *resolution,
                width,
                theme,
            ),
            RowKind::Ask(AskRow::Form {
                server,
                fields,
                resolution,
                ..
            }) => {
                let words = match resolution {
                    Resolution::Open => format!("{server} needs details"),
                    Resolution::Answered => format!("Sent the form to {server}"),
                    Resolution::Declined => format!("Declined {server}'s form"),
                    Resolution::Cancelled | Resolution::Dismissed => {
                        format!("Dismissed {server}'s form")
                    }
                };
                let mut lines = vec![words];
                // The wire keeps which fields were sent, never their values.
                if *resolution == Resolution::Answered && !fields.is_empty() {
                    lines.push(fields.join(", "));
                }
                ask_step(&mut drawn, &lines, width, theme);
            }
            RowKind::Ask(AskRow::Link {
                server, resolution, ..
            }) => {
                let words = match resolution {
                    Resolution::Open => format!("{server} needs you to sign in"),
                    Resolution::Answered => format!("Signed in to {server}"),
                    Resolution::Declined => format!("Declined {server}'s sign-in"),
                    Resolution::Cancelled | Resolution::Dismissed => {
                        format!("Dismissed {server}'s sign-in")
                    }
                };
                ask_step(&mut drawn, &[words], width, theme);
            }
            RowKind::Ask(AskRow::Grant {
                read,
                write,
                network,
                hosts,
                granted,
                resolution,
                ..
            }) => {
                let asked = super::ask::access_words(read, write, *network, hosts);
                let words = match (resolution, granted) {
                    (Resolution::Open, _) => format!("Wants access to {asked}"),
                    (Resolution::Answered, Some(granted)) => format!(
                        "Granted {} · for this {}",
                        super::ask::access_words(
                            &granted.read,
                            &granted.write,
                            granted.network,
                            hosts
                        ),
                        if granted.for_session {
                            "session"
                        } else {
                            "turn"
                        }
                    ),
                    (Resolution::Answered, None) => format!("Granted {asked}"),
                    (Resolution::Declined, _) => format!("Refused {asked}"),
                    (Resolution::Cancelled | Resolution::Dismissed, _) => {
                        format!("Dismissed the ask for {asked}")
                    }
                };
                ask_step(&mut drawn, &[words], width, theme);
            }
            RowKind::Ask(AskRow::Unanswerable { resolution, .. }) => {
                let words = match resolution {
                    Resolution::Open => "Can't answer this here",
                    Resolution::Answered => "Answered in the agent's own terminal",
                    _ => "Dismissed what this client couldn't show",
                };
                ask_step(&mut drawn, &[words.to_owned()], width, theme);
            }
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

/// Your message: a tinted block with a whole tinted line above and below
/// its words, the time at the right of its first line. The block runs from
/// the outer margin to the outer margin; its words and time sit a margin
/// inside it.
fn prompt(
    drawn: &mut Drawn,
    row: &Row,
    words: &[Segment],
    steered: bool,
    width: usize,
    theme: Theme,
) {
    let when = if steered {
        format!("steered · {}", clock(row.at_ms))
    } else {
        clock(row.at_ms)
    };
    prompt_block(drawn, words, &when, width, theme);
}

/// Your message's block with `when` at the right of its first line.
fn prompt_block(drawn: &mut Drawn, words: &[Segment], when: &str, width: usize, theme: Theme) {
    let surface = theme.user_surface();
    drawn.line(tinted(Line::default(), surface, width));
    let inner = width.saturating_sub(2 * EDGE);
    let inset = WORDS - EDGE;
    let room = inner.saturating_sub(2 * inset + text::str_width(when) + 2);
    for (i, words) in segment_lines(words, room, theme.text(), theme)
        .into_iter()
        .enumerate()
    {
        let mut line = Line::from(Span::raw(" ".repeat(inset)));
        line.spans.extend(words.spans);
        if i == 0 {
            push_right(&mut line, when, theme.faint(), inner - inset);
        }
        drawn.line(tinted(line, surface, width));
    }
    drawn.line(tinted(Line::default(), surface, width));
    drawn.blank();
}

/// A line of the queue block, and where its controls are, by columns.
pub struct Queued {
    pub line: Line<'static>,
    /// Its controls in order ("[Send Now]" then "[Withdraw]", or
    /// "[Resend]" then "[Discard]"), in place of its state at the right.
    pub controls: Vec<(usize, usize)>,
}

/// A prompt waiting above the composer, one line like the pinned prompt:
/// faint on your message's surface, cut with "…", how it waits at the
/// right where the time would be ("queued", "queued from relay", "sending
/// into this turn", "waiting for laptop…", "may not have arrived").
/// Highlighted, or under the pointer, it reads in full ink and offers its
/// controls there.
pub fn queued_line(
    entry: &super::composer::QueueEntry,
    lit: bool,
    width: usize,
    theme: Theme,
) -> Queued {
    let surface = theme.user_surface();
    let ink = if lit { theme.text() } else { theme.faint() };
    let controls = entry.controls();
    let shows_controls = lit && !controls.is_empty();
    let right = if shows_controls {
        controls.join(" ")
    } else {
        entry.state_words()
    };
    let inner = width.saturating_sub(2 * EDGE);
    let inset = WORDS - EDGE;
    let room = inner.saturating_sub(2 * inset + text::str_width(&right) + 2);
    let mut line = Line::from(Span::raw(" ".repeat(inset)));
    push(
        &mut line,
        text::ellipsize(&one_line(entry.text()), room.max(1)),
        ink,
        inset + room,
    );
    push_right(
        &mut line,
        &right,
        if shows_controls {
            theme.muted()
        } else {
            theme.faint()
        },
        inner - inset,
    );
    let mut out = Queued {
        line: tinted(line, surface, width),
        controls: Vec::new(),
    };
    if shows_controls {
        let end = EDGE + inner - inset;
        let mut col = end.saturating_sub(text::str_width(&right));
        for control in controls {
            let to = col + text::str_width(control);
            out.controls.push((col, to));
            col = to + 1;
        }
    }
    out
}

/// A message's words on one line, its attachments as chips.
fn one_line(words: &[Segment]) -> String {
    let source: String = words
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) => text.clone(),
            Segment::Attachment(view) => chip(view),
        })
        .collect();
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Your message on its way, drawn at once at the feed's end as it will
/// stand once the agent has it; `when` is the time, or what it waits for
/// while the link is down.
pub fn pending_prompt(words: &[Segment], when: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut drawn = Drawn::default();
    prompt_block(&mut drawn, words, when, width, theme);
    drawn.lines
}

/// A turn's prompt pinned under the header while that turn owns the top of
/// the feed: one tinted line of the prompt, cut with "…" (its padding
/// lines are the in-feed block's, not the pin's).
/// Faint while the next turn's prompt is about to take the pin.
pub fn pinned_prompt(row: &Row, faint: bool, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let RowKind::Prompt { text: words, .. } = &row.kind else {
        return Vec::new();
    };
    let surface = theme.user_surface();
    let ink = if faint { theme.faint() } else { theme.text() };
    let when = clock(row.at_ms);
    let inner = width.saturating_sub(2 * EDGE);
    let inset = WORDS - EDGE;
    let room = inner.saturating_sub(2 * inset + text::str_width(&when) + 2);
    let source: String = words
        .iter()
        .map(|segment| match segment {
            Segment::Text(text) => text.clone(),
            Segment::Attachment(view) => chip(view),
        })
        .collect();
    let joined = source.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut line = Line::from(Span::raw(" ".repeat(inset)));
    push(
        &mut line,
        text::ellipsize(&joined, room.max(1)),
        ink,
        inset + room,
    );
    push_right(&mut line, &when, theme.faint(), inner - inset);
    // One line: the in-feed prompt keeps its padding, the pin gives the
    // feed every line it can.
    vec![tinted(line, surface, width)]
}

/// One line of a tinted block: its words on the surface, which runs from
/// the outer margin to the outer margin.
fn tinted(line: Line<'static>, surface: Style, width: usize) -> Line<'static> {
    // Only the ground: each span keeps its own ink.
    let surface = Style {
        bg: surface.bg,
        ..Style::default()
    };
    let mut out = Line::from(Span::raw(" ".repeat(EDGE)));
    for span in line.spans {
        // A span with its own ground (a chip) keeps it.
        let style = if span.style.bg.is_some() {
            span.style
        } else {
            span.style.patch(surface)
        };
        out.spans.push(Span::styled(span.content, style));
    }
    text::fill(&mut out, surface, width - EDGE);
    out
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
    // A comfortable measure on a wide terminal: lines run to at most
    // READING columns of text, however wide the screen.
    let wrap = width.saturating_sub(2).min(READING + WORDS);
    let source = if streaming {
        crate::markdown::streaming_source(&source)
    } else {
        source.as_str()
    };
    // Tables may run the chat's full width.
    let mut lines = markdown_linked(source, wrap, width.saturating_sub(2), theme);
    if lines.is_empty() {
        return;
    }
    if streaming && let Some((last, _)) = lines.last_mut() {
        last.spans.push(Span::styled(" ▍", theme.faint()));
    }
    for (line, links) in lines {
        drawn.lines.push(line);
        drawn.hits.push(
            links
                .into_iter()
                .map(|(from, to, url)| (Some((from, to)), FeedHit::Link(url)))
                .collect(),
        );
    }
    drawn.blank();
}

/// A plan, as the feed reads it.
struct Plan<'a> {
    text: &'a str,
    verdict: PlanVerdict,
    edits_accepted: bool,
    note: Option<&'a str>,
    writing: bool,
}

/// The plan's title and body: an opening heading of any level is lifted
/// out as the title. None while the first line is still being written and
/// may yet be a heading.
fn plan_title(text: &str, writing: bool) -> Option<(Option<String>, &str)> {
    let text = text.trim_start_matches(['\n', '\r', ' ']);
    let (first, rest) = match text.split_once('\n') {
        Some(split) => split,
        None if writing && (text.is_empty() || text.starts_with('#')) => return None,
        None => (text, ""),
    };
    let heading = first.trim_start();
    if heading.starts_with('#') {
        let title = heading.trim_start_matches('#').trim();
        Some(((!title.is_empty()).then(|| title.to_owned()), rest))
    } else {
        Some((None, text))
    }
}

/// A plan the agent proposed, set apart by a landmark that folds like a
/// stretch: the marker in the gutter, "Plan", its title, how it was decided,
/// and a hairline to the margin. Waiting on the person it is open (the
/// composer's box asks); decided it folds to that line, with what the
/// person said when they sent it back. `toggled` is the reader's click,
/// which folds an open plan or opens a folded one.
fn plan_lines(
    drawn: &mut Drawn,
    id: &Key,
    plan: &Plan<'_>,
    toggled: bool,
    width: usize,
    theme: Theme,
) {
    let Some((title, body)) = plan_title(plan.text, plan.writing) else {
        return;
    };
    let outcome = match plan.verdict {
        PlanVerdict::Open => None,
        PlanVerdict::Approved if plan.edits_accepted => Some("approved · accepting edits"),
        PlanVerdict::Approved => Some("approved"),
        PlanVerdict::SentBack => Some("sent back"),
        PlanVerdict::Dismissed => Some("dismissed"),
    };
    let folded = outcome.is_some() != toggled;

    let end = width.saturating_sub(2);
    let mut head = Line::default();
    pad_to(&mut head, EDGE);
    push(
        &mut head,
        if folded { "▸" } else { "▾" },
        theme.faint(),
        end,
    );
    pad_to(&mut head, WORDS);
    push(&mut head, "Plan", theme.faint(), end);
    if let Some(title) = &title {
        push(&mut head, " · ", theme.faint(), end);
        let room = end
            .saturating_sub(text::line_width(&head))
            .saturating_sub(outcome.map_or(0, |words| text::str_width(words) + 3) + 4);
        push(&mut head, text::ellipsize(title, room), theme.bright(), end);
    }
    if let Some(words) = outcome {
        push(&mut head, format!(" · {words}"), theme.faint(), end);
    }
    push(&mut head, " ", theme.faint(), end);
    let used = text::line_width(&head);
    if used < end {
        head.spans
            .push(Span::styled("─".repeat(end - used), theme.hairline()));
    }
    drawn.hit_line(head, FeedHit::Step(id.clone()));
    // What the person said when sending it back, under the landmark.
    if plan.verdict == PlanVerdict::SentBack
        && let Some(note) = plan.note.filter(|note| !note.trim().is_empty())
    {
        let mut said = Line::default();
        pad_to(&mut said, WORDS);
        push(
            &mut said,
            format!("\u{201c}{}\u{201d}", first_line(note)),
            theme.faint(),
            end,
        );
        drawn.hit_line(said, FeedHit::Step(id.clone()));
    }
    drawn.blank();
    if !folded {
        prose(
            drawn,
            &[Segment::Text(body.to_owned())],
            plan.writing,
            width,
            theme,
        );
    }
}

/// Questions the agent asked, once the box is done with them: a step that
/// says what happened ("Answered 2 questions"), a faint line per question
/// with its answer, and a reply sent instead as the person's own message.
#[allow(clippy::too_many_arguments)]
fn questions_step(
    drawn: &mut Drawn,
    row: &Row,
    questions: &[QuestionView],
    answers: &[AnswerView],
    note: Option<&str>,
    resolution: Resolution,
    width: usize,
    theme: Theme,
) {
    let reply = note.and_then(ui_view::reply_words);
    let count = if questions.len() == 1 {
        "a question".to_owned()
    } else {
        format!("{} questions", questions.len())
    };
    let answered = answers
        .iter()
        .filter(|answer| !answer_words(answer).is_empty())
        .count();
    let words = match (resolution, reply) {
        (_, Some(_)) => format!("Replied instead of answering {count}"),
        (Resolution::Open, _) => format!("Asking {count}"),
        (Resolution::Answered, _) if answered == 0 => format!("Skipped {count}"),
        (Resolution::Answered, _) if answered < questions.len() => {
            format!("Answered {answered} of {} questions", questions.len())
        }
        (Resolution::Answered, _) => format!("Answered {count}"),
        (Resolution::Declined, _) => format!("Declined {count}"),
        (Resolution::Cancelled | Resolution::Dismissed, _) => format!("Dismissed {count}"),
    };
    let railed = |words: String, style: Style| {
        let mut line = Line::default();
        pad_to(&mut line, EDGE);
        push(&mut line, "│", theme.hairline(), width);
        pad_to(&mut line, WORDS);
        push(&mut line, words, style, width.saturating_sub(2));
        line
    };
    drawn.line(railed(words, theme.muted()));
    // A reply carries the answers so far to the agent in its own words, so
    // the step lists answers only when they were sent as answers.
    if matches!(resolution, Resolution::Answered) && reply.is_none() {
        // Each question as asked, faint, then "→ answer" under it.
        let room = width.saturating_sub(WORDS + 2).max(1);
        for (at, question) in questions.iter().enumerate() {
            for part in text::wrap(&question.question, room) {
                drawn.line(railed(part, theme.faint()));
            }
            let answer = answers.get(at).map(answer_words).unwrap_or_default();
            let (answer, ink) = if answer.is_empty() {
                ("not answered".to_owned(), theme.faint())
            } else {
                (answer, theme.text())
            };
            for (i, part) in text::wrap(&answer, room.saturating_sub(4).max(1))
                .into_iter()
                .enumerate()
            {
                let lead = if i == 0 { "  → " } else { "    " };
                let mut line = railed(lead.to_owned(), theme.faint());
                push(&mut line, part, ink, width.saturating_sub(2));
                drawn.line(line);
            }
        }
    }
    drawn.blank();
    if let Some(words) = reply {
        prompt(
            drawn,
            row,
            &[Segment::Text(words.to_owned())],
            false,
            width,
            theme,
        );
    }
}

/// A finished ask's step: its words in the step ink on the rail, then any
/// further lines faint under them.
fn ask_step(drawn: &mut Drawn, lines: &[String], width: usize, theme: Theme) {
    let room = width.saturating_sub(WORDS + 2).max(1);
    for (i, words) in lines.iter().enumerate() {
        for part in text::wrap(words, room) {
            let mut line = Line::default();
            pad_to(&mut line, EDGE);
            push(&mut line, "│", theme.hairline(), width);
            pad_to(&mut line, WORDS);
            push(
                &mut line,
                part,
                if i == 0 { theme.muted() } else { theme.faint() },
                width.saturating_sub(2),
            );
            drawn.line(line);
        }
    }
    drawn.blank();
}

/// An answer in words: the picks, a typed answer in quotes, or "answered
/// (hidden)" for a secret; empty when the question was skipped.
fn answer_words(answer: &AnswerView) -> String {
    if answer.hidden {
        return "answered (hidden)".into();
    }
    let mut picks: Vec<String> = answer
        .picked
        .iter()
        .filter(|pick| !pick.is_empty())
        .cloned()
        .collect();
    if let Some(other) = answer.other.as_ref().filter(|other| !other.is_empty()) {
        picks.push(other.clone());
    }
    picks.join(", ")
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
        push(&mut line, verb.clone(), theme.error(), width);
        if !subject.is_empty() {
            if !verb.is_empty() {
                push(&mut line, " ", theme.error(), width);
            }
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
            // A denied call never ran, so it took no time.
            if let Some(ms) = duration_ms.filter(|_| verb != "Denied") {
                meta.push(text::duration(ms));
            }
            let meta = decided(meta.join(" · "), row, verb);
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
            let meta = decided(state_meta(*state).unwrap_or_default().to_owned(), row, "");
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
            // "Used tracker sign_in": the server, then the tool, which
            // `step` draws in the reading ink.
            let subject = if server.is_empty() {
                tool.clone()
            } else {
                format!("{server} {tool}")
            };
            let mut meta = fact.clone();
            if *state == ToolStateView::Failed {
                meta = if meta.is_empty() {
                    "failed".into()
                } else {
                    format!("{meta} · failed")
                };
            }
            let meta = decided(meta, row, verb);
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
            (verb.to_owned(), subject, decided(meta, row, verb))
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
            if !*running && let Some(ms) = duration_ms {
                meta.push(text::duration(*ms));
            }
            let verb = if *running {
                "Running subagent"
            } else {
                "Ran subagent"
            };
            (
                verb.to_owned(),
                first_line(description).to_owned(),
                meta.join(" · "),
            )
        }
        // A command like any other, where it runs said after it.
        RowKind::Background {
            command,
            running,
            duration_ms,
        } => {
            let mut meta = vec!["in background".to_owned()];
            if !*running && let Some(ms) = duration_ms {
                meta.push(text::duration(*ms));
            }
            (
                if *running { "Running" } else { "Ran" }.to_owned(),
                first_line(command).to_owned(),
                meta.join(" · "),
            )
        }
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
                "Viewed image"
            };
            (verb.to_owned(), subject, String::new())
        }
        _ => (String::new(), String::new(), String::new()),
    }
}

/// The step's facts with how the person answered its ask after them:
/// "allowed", "always allowed" when the answer made a rule, "denied"
/// unless the verb already says so. A refusal's note goes on its own line,
/// not here.
fn decided(meta: String, row: &Row, verb: &str) -> String {
    let Some(decision) = &row.decision else {
        return meta;
    };
    let mut parts: Vec<String> = meta
        .split(" · ")
        .filter(|part| !part.is_empty() && !matches!(*part, "denied" | "cancelled" | "waiting"))
        .map(str::to_owned)
        .collect();
    match decision.outcome {
        DecisionView::Allowed if decision.scope.is_some() => parts.push("always allowed".into()),
        DecisionView::Allowed => parts.push("allowed".into()),
        DecisionView::AutoApproved => parts.push("auto-approved".into()),
        DecisionView::Denied if verb == "Denied" => {}
        DecisionView::Denied => parts.push("denied".into()),
        DecisionView::Dismissed => parts.push("dismissed".into()),
    }
    if decision.elsewhere {
        parts.push("in the terminal".into());
    }
    parts.join(" · ")
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
    // The diff has one place, the header's [Diff]; a step opens to its
    // own patch.
    let tail_width = if meta.is_empty() {
        0
    } else {
        text::str_width(&meta) + 3
    };
    // Two blank columns at the right, as at the left.
    let width = width.saturating_sub(2);
    let room = width.saturating_sub(tail_width);
    push(&mut line, verb.clone(), words, room);
    if !subject.is_empty() {
        if !verb.is_empty() {
            push(&mut line, " ", words, room);
        }
        let left = room.saturating_sub(text::line_width(&line));
        let shown = if text::str_width(&subject) > left && left > 8 {
            tail(&subject, left)
        } else {
            subject
        };
        // A tool-server call names its tool in the reading ink after the
        // server, unless the line has its own ink.
        match &row.kind {
            RowKind::ToolCall { tool, .. }
                if !failed && !current && shown.ends_with(tool.as_str()) && !tool.is_empty() =>
            {
                let (server, tool) = shown.split_at(shown.len() - tool.len());
                push(&mut line, server, subject_style, room);
                push(&mut line, tool, theme.text(), room);
            }
            _ => push(&mut line, shown, subject_style, room),
        }
    }
    if !meta.is_empty() {
        push(&mut line, format!(" · {meta}"), theme.faint(), width);
    }
    let hits = vec![(None, FeedHit::Step(row.id.clone()))];
    drawn.lines.push(line);
    drawn.hits.push(hits);
    // A refusal's note, what the person told the agent instead, under it.
    if let Some(note) = row
        .decision
        .as_ref()
        .filter(|decision| decision.outcome == DecisionView::Denied)
        .and_then(|decision| decision.note.as_ref())
    {
        let mut said = Line::default();
        pad_to(&mut said, EDGE);
        push(&mut said, "│", theme.hairline(), width);
        pad_to(&mut said, WORDS);
        push(
            &mut said,
            format!("\u{201c}{}\u{201d}", first_line(note)),
            theme.faint(),
            width,
        );
        drawn.hit_line(said, FeedHit::Step(row.id.clone()));
    }
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
pub fn clock(at_ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(at_ms)
        .single()
        .map(|at| at.format("%H:%M").to_string())
        .unwrap_or_default()
}
