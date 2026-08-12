//! The fullscreen reader (`docs/CHAT.md` §Diffs and the reader's
//! artifacts): ONE overlay over typed artifacts — Plan (the B2 markdown
//! renderer reused), Diff, NewFile — scrollable with a position
//! indicator, carrying an action row only while a writable ask is open.
//! Plan review opens here directly (C3: the full plan is the point); `f`
//! reaches it from ask panels and read-only fact panels; Ctrl+T reopens
//! accepted plans, ←/→ stepping between them when several exist (B6).

use amux_ui::Model;
use amux_ui::claude::{Ask, AskArtifact, AskKind, AskState, DiffArtifact, ToolInvocation};
use ratatui::text::{Line, Span};

use crate::chat::ask_ui::AskStage;
use crate::chat::{ChatView, diff, markdown, panel};
use crate::render::{Theme, blank_line, finish_line, line_len, new_line, push_span};

/// Fullscreen reader ViewState: what is being read and where the viewport
/// sits. The artifact itself is resolved from the Model at render — a
/// stale reader for a resolved ask stops resolving and the frame falls
/// back to the chat (reconcile also dismisses it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReaderView {
    pub source: ReaderSource,
    pub scroll: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReaderSource {
    /// The pending ask's artifact (plan review reader-first; `f` from
    /// panels and fact panels).
    Ask,
    /// An accepted plan (Ctrl+T), by index into `accepted_plans` (oldest
    /// first; opened at the newest).
    Plans { index: usize },
}

/// The typed artifact body, borrowed from the Model (§5: a match, not a
/// viewer framework — Text and Image are reserved kinds with no variant
/// until something produces them).
enum Body<'m> {
    Plan(&'m str),
    Diff(&'m DiffArtifact),
    NewFile(&'m str),
}

struct Resolved<'m> {
    title: String,
    body: Body<'m>,
    /// The pending ask whose action row is live (writable chats only).
    ask: Option<&'m Ask>,
    /// (index, count) when stepping between accepted plans.
    plans_nav: Option<(usize, usize)>,
}

/// Resolve what the reader shows, or `None` when its source no longer
/// exists (the frame then falls back to the chat).
fn resolve<'m>(model: &'m Model, chat: &ChatView) -> Option<Resolved<'m>> {
    let reader = chat.reader.as_ref()?;
    let layer = model.claude(chat.agent)?;
    match &reader.source {
        ReaderSource::Ask => {
            let ask = layer.ask_head()?;
            let resolved = match (&ask.artifact, &ask.kind) {
                (Some(AskArtifact::Diff(artifact)), kind) => Resolved {
                    title: format!(
                        "diff — {}  {}",
                        permission_path(kind).unwrap_or("(unknown file)"),
                        diff::magnitude_text(&artifact.magnitude)
                    ),
                    body: Body::Diff(artifact),
                    ask: Some(ask),
                    plans_nav: None,
                },
                (Some(AskArtifact::NewFile { content }), kind) => Resolved {
                    title: format!(
                        "new file — {}  ({} lines)",
                        permission_path(kind).unwrap_or("(unknown file)"),
                        content.lines().count()
                    ),
                    body: Body::NewFile(content),
                    ask: Some(ask),
                    plans_nav: None,
                },
                (
                    None,
                    AskKind::Permission {
                        invocation:
                            ToolInvocation::Plan {
                                plan: Some(plan), ..
                            },
                        ..
                    },
                ) => Resolved {
                    title: "plan".to_string(),
                    body: Body::Plan(plan),
                    ask: Some(ask),
                    plans_nav: None,
                },
                _ => return None,
            };
            Some(resolved)
        }
        ReaderSource::Plans { index } => {
            let plans = layer.accepted_plans();
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

fn permission_path(kind: &AskKind) -> Option<&str> {
    match kind {
        AskKind::Permission {
            invocation:
                ToolInvocation::Edit {
                    file_path: Some(path),
                    ..
                }
                | ToolInvocation::Write {
                    file_path: Some(path),
                },
            ..
        } => Some(path),
        _ => None,
    }
}

fn body_lines<'m>(body: &Body<'m>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    match body {
        Body::Plan(markdown_source) => {
            markdown::markdown_rows(markdown_source, width.saturating_sub(3).max(1), theme)
                .into_iter()
                .map(|spans| {
                    let mut line = new_line();
                    push_span(&mut line, 2, "", theme.text());
                    line.spans.extend(spans);
                    line
                })
                .collect()
        }
        Body::Diff(artifact) => diff::reader_rows(artifact, width, theme),
        Body::NewFile(content) => diff::new_file_rows(content, width, theme, true),
    }
}

/// The reader frame, replacing the whole chat frame while open; `None`
/// when the reader's source no longer resolves. `readonly` suppresses the
/// action row — read affordances only (F1).
pub(crate) fn reader_frame(
    model: &Model,
    chat: &ChatView,
    theme: Theme,
    width: usize,
    height: usize,
    readonly: bool,
) -> Option<Vec<Line<'static>>> {
    let resolved = resolve(model, chat)?;
    let body = body_lines(&resolved.body, width, theme);
    let total = body.len();
    let tail = reader_tail(&resolved, chat, readonly, width, theme);

    // Frame rows: top, title, rule, body, rule, tail, bottom.
    let body_h = height.saturating_sub(5 + tail.len()).max(1);
    let start = chat
        .reader
        .as_ref()
        .map(|reader| reader.scroll)
        .unwrap_or(0)
        .min(total.saturating_sub(body_h));
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
    content.push(panel::rule_line(width, theme));
    let mut window: Vec<Line<'static>> = body.into_iter().skip(start).take(body_h).collect();
    while window.len() < body_h {
        window.push(new_line());
    }
    content.extend(window);
    content.push(panel::rule_line(width, theme));
    content.extend(tail);
    for line in &mut content {
        finish_line(line, width);
    }

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    lines.push(top_border(width, theme));
    lines.extend(content);
    // Degenerate heights: the bottom border keeps the last row.
    lines.truncate(height.saturating_sub(1));
    while lines.len() + 1 < height {
        lines.push(blank_line(width));
    }
    lines.push(crate::render::bottom_border(width));
    Some(lines)
}

/// The rows below the body: the writable ask's action rows / feedback
/// stage, or the read hints — ONE derivation, consumed by the frame and
/// by the scroll metrics so paging and rendering agree on the viewport.
fn reader_tail(
    resolved: &Resolved<'_>,
    chat: &ChatView,
    readonly: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    // The action row lives only while a writable ask is open.
    let acting = resolved
        .ask
        .filter(|ask| !readonly && matches!(ask.state, AskState::Pending));
    let plan_review = acting.is_some_and(super::ask_ui::is_plan);

    let mut tail: Vec<Line<'static>> = Vec::new();
    let ui = chat.ask_ui.as_ref();
    if let Some(ask) = acting {
        let feedback_stage = plan_review
            && ui.is_some_and(|ui| ui.ask_id == ask.id && ui.stage == AskStage::PlanFeedback);
        if feedback_stage {
            let field = &ui.expect("checked above").plan_feedback;
            let mut label = new_line();
            push_span(
                &mut label,
                4,
                "Request changes — tell the agent what to change (required)",
                theme.text(),
            );
            tail.push(label);
            let mut line = new_line();
            push_span(&mut line, 2, "›", theme.text());
            push_span(&mut line, 4, field.display_with_cursor(), theme.text());
            tail.push(line);
            tail.push(new_line());
            tail.extend(hint("enter request changes · esc back (keeps text)", theme));
        } else {
            let cursor = ui
                .filter(|ui| ui.ask_id == ask.id)
                .and_then(|ui| match &ui.stage {
                    AskStage::Menu { cursor } => Some(*cursor),
                    _ => None,
                })
                .unwrap_or(0);
            if plan_review {
                tail.extend(panel::plan_actions(Some(cursor), width, theme));
                tail.push(new_line());
                tail.extend(hint(
                    "↑↓/pgup scroll plan · 1-3 select · enter confirm · esc back (plan stays)",
                    theme,
                ));
            } else if let AskKind::Permission { suggestions, .. } = &ask.kind {
                tail.extend(panel::permission_actions(
                    suggestions,
                    Some(cursor),
                    width,
                    theme,
                ));
                tail.push(new_line());
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
    tail
}

/// Reader scroll metrics for the key handler: (page height, max top) —
/// computed from the SAME tail derivation the frame renders, so End and
/// PgDn land exactly where render clamps and a following Up moves
/// immediately.
pub(crate) fn scroll_metrics(
    model: &Model,
    chat: &ChatView,
    viewport: (u16, u16),
) -> Option<(usize, usize)> {
    let resolved = resolve(model, chat)?;
    let width = viewport.0 as usize;
    let height = viewport.1 as usize;
    // Layout is theme-independent (tokens change styles, never cells).
    let readonly = chat.read_only(model);
    let tail = reader_tail(&resolved, chat, readonly, width, Theme::default());
    let total = body_lines(&resolved.body, width, Theme::default()).len();
    let body_h = height.saturating_sub(5 + tail.len()).max(1);
    Some((body_h, total.saturating_sub(body_h)))
}

fn hint(text: &str, theme: Theme) -> Vec<Line<'static>> {
    let mut line = new_line();
    push_span(&mut line, 4, text.to_string(), theme.muted());
    vec![line]
}

fn top_border(width: usize, theme: Theme) -> Line<'static> {
    let mut text = String::from("┌");
    while text.chars().count() < width - 1 {
        text.push('─');
    }
    text.push('┐');
    Line::from(Span::styled(text, theme.muted()))
}

fn title_line(
    title: &str,
    start: usize,
    shown: usize,
    total: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, 2, title.to_string(), theme.text());
    let position = if total == 0 {
        "lines 0-0/0".to_string()
    } else {
        format!("lines {}-{}/{}", start + 1, start + shown, total)
    };
    let col = width.saturating_sub(2 + position.chars().count());
    if col > line_len(&line) {
        push_span(&mut line, col, position, theme.muted());
    }
    line
}

/// Whether ←/→ can step and to which plan index (resolved-plans reader
/// only).
pub(crate) fn plans_step(model: &Model, chat: &ChatView, delta: i64) -> Option<usize> {
    let layer = model.claude(chat.agent)?;
    let count = layer.accepted_plans().len();
    let ReaderSource::Plans { index } = &chat.reader.as_ref()?.source else {
        return None;
    };
    let index = (*index).min(count.checked_sub(1)?) as i64;
    let next = (index + delta).clamp(0, count as i64 - 1) as usize;
    Some(next)
}
