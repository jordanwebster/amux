//! The fullscreen reader (`docs/CHAT.md` §Diffs and the reader's
//! documents): ONE overlay over typed documents — Plan (the B2 markdown
//! renderer reused), Diff, NewFile — scrollable with a position
//! indicator, carrying an action row only while a writable ask is open.
//! Plan review opens here directly (C3: the full plan is the point); `f`
//! reaches it from ask panels and read-only fact panels; Ctrl+T reopens
//! accepted plans, ←/→ stepping between them when several exist (B6).

use std::borrow::Cow;

use amux_ui::attachments::AttachmentIndex;
use amux_ui::claude::{AcceptedPlan, AskDocument, DiffDocument};
use amux_ui::review::{ReviewComment, ReviewHeader};
use ratatui::text::{Line, Span};
use serde::{Deserialize, Serialize};

use crate::chat::claude_shared::ask_ui::{AskStage, AskUi};
use crate::chat::claude_shared::{SharedAsk, SharedAskKind, armed_quit_line, diff, panel};
use crate::markdown;
use crate::render::{Theme, push_right, push_span};

/// Everything the reader needs from the chat around it, gathered by that
/// chat from its own layer. The reader knows nothing about which
/// transport folded these facts.
pub(crate) struct ReaderContext<'m> {
    /// What is being read and where the viewport sits.
    pub reader: &'m ReaderView,
    /// The pending ask, when one heads the queue.
    pub ask: Option<SharedAsk<'m>>,
    /// Panel state for that ask, when the chat holds any.
    pub ask_ui: Option<&'m AskUi>,
    /// This client may answer at all (a read-only observer may not).
    pub can_answer: bool,
    /// Accepted plans, oldest first — what Ctrl+T steps through. Borrowed
    /// when the layer keeps them as a list and owned when the chat derives
    /// them from its feed, so neither transport pays for the other's shape.
    pub accepted_plans: Cow<'m, [AcceptedPlan]>,
    /// The layer's artifact index: a sent review's diff resolves here.
    pub attachments: &'m AttachmentIndex,
    pub quit_guard_armed: bool,
}

/// Fullscreen reader ViewState: what is being read and where the viewport
/// sits. The document itself is resolved from the Model at render — a
/// stale reader for a resolved ask stops resolving and the frame falls
/// back to the chat (reconcile also dismisses it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReaderView {
    pub source: ReaderSource,
    pub scroll: usize,
}

impl ReaderView {
    /// Open on the pending ask's document, at the top.
    pub fn ask() -> Self {
        Self {
            source: ReaderSource::Ask,
            scroll: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ReaderSource {
    /// The pending ask's document (plan review reader-first; `f` from
    /// panels and fact panels).
    Ask,
    /// An accepted plan (Ctrl+T), by index into `accepted_plans` (oldest
    /// first; opened at the newest).
    Plans { index: usize },
    /// A text attachment from the feed. Its body is carried here rather
    /// than resolved from the Model: an inline attachment's words are
    /// part of the message that carried them and never change, so there
    /// is nothing to look up and nothing that can go stale.
    Text { name: String, body: String },
    /// A review someone sent. Its comments came with the message and are
    /// carried here; the diff they were written on is an artifact,
    /// resolved from the layer's index so it appears the moment the
    /// fetch lands.
    Review {
        header: Box<ReviewHeader>,
        comments: Vec<ReviewComment>,
    },
}

/// The typed document body, borrowed from the Model (§5: a match, not a
/// viewer framework — Text and Image are reserved kinds with no variant
/// until something produces them).
enum Body<'m> {
    Plan(&'m str),
    Diff(&'m DiffDocument),
    NewFile(&'m str),
    /// Words with no markup: a pasted attachment is read as it was
    /// pasted, not as markdown a renderer guessed at.
    Text(&'m str),
    /// A sent review: its comments, over the fetched patch when this host
    /// has it.
    Review {
        header: &'m ReviewHeader,
        comments: &'m [ReviewComment],
        diff: Option<&'m str>,
    },
}

struct Resolved<'m> {
    title: String,
    body: Body<'m>,
    /// The pending ask whose action row is live (writable chats only).
    ask: Option<&'m SharedAsk<'m>>,
    /// (index, count) when stepping between accepted plans.
    plans_nav: Option<(usize, usize)>,
}

/// Resolve what the reader shows, or `None` when its source no longer
/// exists (the frame then falls back to the chat).
fn resolve<'m>(ctx: &'m ReaderContext<'m>) -> Option<Resolved<'m>> {
    match &ctx.reader.source {
        ReaderSource::Ask => {
            let ask = ctx.ask.as_ref()?;
            let resolved = match (ask.document, ask.plan()) {
                (Some(AskDocument::Diff(document)), _) => Resolved {
                    title: format!(
                        "diff — {}  {}",
                        ask.path().unwrap_or("(unknown file)"),
                        diff::magnitude_text(&document.magnitude)
                    ),
                    body: Body::Diff(document),
                    ask: Some(ask),
                    plans_nav: None,
                },
                (Some(AskDocument::NewFile { content }), _) => Resolved {
                    title: format!(
                        "new file — {}  ({} lines)",
                        ask.path().unwrap_or("(unknown file)"),
                        content.lines().count()
                    ),
                    body: Body::NewFile(content),
                    ask: Some(ask),
                    plans_nav: None,
                },
                (None, Some(plan)) => Resolved {
                    title: "plan".to_string(),
                    body: Body::Plan(plan),
                    ask: Some(ask),
                    plans_nav: None,
                },
                _ => return None,
            };
            Some(resolved)
        }
        ReaderSource::Review { header, comments } => Some(Resolved {
            title: review_title(header, comments),
            body: Body::Review {
                header,
                comments,
                diff: ctx.attachments.diff(&header.diff),
            },
            ask: None,
            plans_nav: None,
        }),
        ReaderSource::Text { name, body } => Some(Resolved {
            title: format!("{name} \u{b7} {} lines", body.lines().count()),
            body: Body::Text(body),
            ask: None,
            plans_nav: None,
        }),
        ReaderSource::Plans { index } => {
            let plans = ctx.accepted_plans.as_ref();
            if plans.is_empty() {
                return None;
            }
            let index = (*index).min(plans.len() - 1);
            let title = if plans.len() > 1 {
                format!("plan ({} of {})", index + 1, plans.len())
            } else {
                "plan".to_string()
            };
            Some(Resolved {
                title,
                body: Body::Plan(&plans[index].plan),
                ask: None,
                plans_nav: Some((index, plans.len())),
            })
        }
    }
}

/// What a sent review is, in one line: what it was taken against, and how
/// much of it a person wrote on.
fn review_title(header: &ReviewHeader, comments: &[ReviewComment]) -> String {
    let mut paths: Vec<&str> = comments
        .iter()
        .map(|comment| comment.path.as_str())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let base = match header.base.strip_prefix("branch:") {
        Some(branch) => format!("against {branch}"),
        None => "working tree".to_string(),
    };
    format!(
        "review \u{2014} {base} @ {}  \u{b7}  {} {} in {} {}",
        header.head,
        comments.len(),
        if comments.len() == 1 {
            "comment"
        } else {
            "comments"
        },
        paths.len(),
        if paths.len() == 1 { "file" } else { "files" },
    )
}

fn body_lines<'m>(body: &Body<'m>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    match body {
        Body::Plan(markdown_source) => {
            markdown::markdown_rows(markdown_source, width.saturating_sub(3).max(1), theme)
                .into_iter()
                .map(|spans| {
                    let mut line = Line::default();
                    push_span(&mut line, 2, "", theme.text());
                    line.spans.extend(spans);
                    line
                })
                .collect()
        }
        Body::Diff(document) => crate::chat::diff::reader_rows(document, width, theme),
        Body::NewFile(content) => diff::new_file_rows(content, width, theme, true),
        Body::Review {
            header,
            comments,
            diff,
        } => crate::review::review_reader_rows(header, comments, *diff, width, theme),
        Body::Text(content) => {
            markdown::plain_rows(content, width.saturating_sub(3).max(1), theme.text())
                .into_iter()
                .map(|spans| {
                    let mut line = Line::default();
                    push_span(&mut line, 2, "", theme.text());
                    line.spans.extend(spans);
                    line
                })
                .collect()
        }
    }
}

/// A dim rule across the whole screen: the overlays' one boundary
/// between a title, a body and the keys that act on it.
pub(crate) fn rule_line(width: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    line.spans
        .push(Span::styled("─".repeat(width), theme.muted()));
    line
}

/// The reader frame, replacing the whole chat frame while open; `None`
/// when the reader's source no longer resolves. Non-actionable readers
/// suppress the action row and retain only read affordances.
pub(crate) fn reader_frame(
    ctx: &ReaderContext<'_>,
    theme: Theme,
    width: usize,
    height: usize,
) -> Option<Vec<Line<'static>>> {
    let resolved = resolve(ctx)?;
    let body = body_lines(&resolved.body, width, theme);
    let total = body.len();
    let tail = reader_tail(ctx, &resolved, width, theme);

    // Frame rows: title, the gap under it, two rules, and the tail.
    let body_h = height.saturating_sub(4 + tail.len()).max(1);
    let start = ctx.reader.scroll.min(total.saturating_sub(body_h));
    let shown = total.saturating_sub(start).min(body_h);

    let mut content: Vec<Line<'static>> = Vec::new();
    content.push(title_line(
        &resolved.title,
        start,
        shown,
        total,
        width,
        theme,
    ));
    content.push(Line::default());
    content.push(rule_line(width, theme));
    let mut window: Vec<Line<'static>> = body.into_iter().skip(start).take(body_h).collect();
    while window.len() < body_h {
        window.push(Line::default());
    }
    content.extend(window);
    content.push(rule_line(width, theme));
    content.extend(tail);
    content.truncate(height);
    Some(content)
}

/// The rows below the body: the writable ask's action rows / feedback
/// stage, or the read hints — ONE derivation, consumed by the frame and
/// by the scroll metrics so paging and rendering agree on the viewport.
fn reader_tail(
    ctx: &ReaderContext<'_>,
    resolved: &Resolved<'_>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    // The action row lives only while a writable ask is open.
    let acting = answer_actionable(ctx).then_some(resolved.ask).flatten();

    let mut tail: Vec<Line<'static>> = Vec::new();
    if let Some(ask) = acting {
        let plan_review = ask.is_plan();
        let ui = ctx.ask_ui.filter(|ui| ui.ask_id == ask.id);
        if plan_review
            && let Some(ui) = ui
            && ui.stage == AskStage::PlanFeedback
        {
            let mut label = Line::default();
            push_span(
                &mut label,
                4,
                "Request changes — tell the agent what to change (required)",
                theme.text(),
            );
            tail.push(label);
            let mut line = Line::default();
            push_span(&mut line, 2, "›", theme.text());
            push_span(
                &mut line,
                4,
                ui.plan_feedback.display_with_cursor(),
                theme.text(),
            );
            tail.push(line);
            tail.push(Line::default());
            tail.extend(hint("enter request changes · esc back (keeps text)", theme));
        } else {
            let cursor = ui.map(AskUi::menu_cursor).unwrap_or(0);
            if plan_review {
                tail.extend(indented(panel::plan_actions(Some(cursor), width, theme)));
                tail.push(Line::default());
                tail.extend(hint(
                    "↑↓/pgup scroll plan · 1-3 select · enter confirm · esc back (plan stays)",
                    theme,
                ));
            } else if let SharedAskKind::Permission { suggestions, .. } = &ask.kind {
                tail.extend(indented(panel::permission_actions(
                    suggestions,
                    Some(cursor),
                    width,
                    theme,
                )));
                tail.push(Line::default());
                tail.extend(hint(
                    "j/k scroll · g/G top/bottom · 1-3 select · enter confirm · esc back",
                    theme,
                ));
            }
        }
    } else {
        let mut text = String::from("j/k scroll · g/G top/bottom");
        if matches!(resolved.plans_nav, Some((_, count)) if count > 1) {
            text.push_str(" · ←/→ other plans");
        }
        text.push_str(" · q close");
        tail.extend(hint(&text, theme));
    }
    // The armed quit guard replaces the tail's hint row (the reader's
    // footer hint line) — same rule as every other bottom block. The row
    // count is unchanged, so scroll metrics agree.
    if ctx.quit_guard_armed
        && let Some(last) = tail.last_mut()
    {
        *last = armed_quit_line(theme);
    }
    tail
}

/// Reader scroll metrics for the key handler: (page height, max top) —
/// computed from the SAME tail derivation the frame renders, so End and
/// PgDn land exactly where render clamps and a following Up moves
/// immediately.
pub(crate) fn scroll_metrics(
    ctx: &ReaderContext<'_>,
    viewport: (u16, u16),
) -> Option<(usize, usize)> {
    let resolved = resolve(ctx)?;
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    // Layout is theme-independent (tokens change styles, never cells).
    let tail = reader_tail(ctx, &resolved, width, Theme::default());
    let total = body_lines(&resolved.body, width, Theme::default()).len();
    let body_h = height.saturating_sub(4 + tail.len()).max(1);
    Some((body_h, total.saturating_sub(body_h)))
}

/// Whether the current reader owns a writable answer surface. This is the
/// single TUI-side focus fact shared by rendering, keys, Ctrl+C, and paste;
/// observation-only policy is already carried by the classified query.
pub(crate) fn answer_actionable(ctx: &ReaderContext<'_>) -> bool {
    if !ctx.can_answer {
        return false;
    }
    resolve(ctx)
        .and_then(|resolved| resolved.ask)
        .is_some_and(|ask| ask.is_pending() && ask.refusal.is_none())
}

/// The ask actions are formatted for the inside of a panel, where the
/// painter supplies the indent; the reader has no panel, so it supplies
/// its own and the two lists line up with the body above them.
fn indented(rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::raw("  "));
            line
        })
        .collect()
}

fn hint(text: &str, theme: Theme) -> Vec<Line<'static>> {
    let mut line = Line::default();
    push_span(&mut line, 4, text.to_string(), theme.muted());
    vec![line]
}

fn title_line(
    title: &str,
    start: usize,
    shown: usize,
    total: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, 2, title.to_string(), theme.text());
    let position = if total == 0 {
        "lines 0-0/0".to_string()
    } else {
        format!("lines {}-{}/{}", start + 1, start + shown, total)
    };
    push_right(&mut line, position, width, theme.muted());
    line
}

/// Whether ←/→ can step and to which plan index (resolved-plans reader
/// only).
pub(crate) fn plans_step(ctx: &ReaderContext<'_>, delta: i64) -> Option<usize> {
    let count = ctx.accepted_plans.len();
    let ReaderSource::Plans { index } = &ctx.reader.source else {
        return None;
    };
    let index = (*index).min(count.checked_sub(1)?) as i64;
    let next = (index + delta).clamp(0, count as i64 - 1) as usize;
    Some(next)
}
