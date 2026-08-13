//! The docked ask panel renderer (C1–C4): the ask takes over the composer
//! area behind a dim rule; the feed above stays the context you decide
//! with. Pure formatting over Model facts and panel ViewState — option
//! labels derive from the hook's suggestion facts, magnitudes from the
//! ask's computed artifact, refusals from the encoder's typed gate; the
//! code here formats and never decides.
//!
//! Lines come back "open" (no padding, no right border); the frame
//! assembler finishes everything once.

use amux_ui::claude::encoding::{self, AskAnswer, PermissionAnswer, PlanAnswer};
use amux_ui::claude::{
    Ask, AskArtifact, AskKind, AskState, QuestionFact, SuggestionFact, ToolInvocation,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::chat::claude::ask_ui::{self, AskStage, AskUi, QuestionDraft, QuestionUi};
use crate::chat::claude::diff;
use crate::composer::Composer;
use crate::markdown;
use crate::render::{Theme, line_len, new_line, push_right, push_span, str_width};

/// Column grid shared with the feed: glyphs at 2, text at 4.
const GLYPH_COL: usize = 2;
const TEXT_COL: usize = 4;

/// The plan's docked preview length (C3: truncated plan; the reader has
/// the whole).
const PLAN_PREVIEW_LINES: usize = 6;

#[derive(Clone, Copy)]
struct PanelContext {
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
}

/// A dim interior rule — the takeover boundary above the panel (C1).
pub(crate) fn rule_line(width: usize, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    line.spans.push(Span::styled(
        "─".repeat(width.saturating_sub(2)),
        theme.muted(),
    ));
    line
}

fn blank() -> Line<'static> {
    new_line()
}

/// Wrapped dim hint rows at the text column (panel hint lines may wrap to
/// a second row — the C4 wireframe does).
fn hint_lines(
    text: &str,
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
) -> Vec<Line<'static>> {
    if quit_guard_armed {
        return vec![super::render::armed_quit_line(theme)];
    }
    markdown::plain_rows(
        text,
        width.saturating_sub(TEXT_COL + 1).max(1),
        theme.muted(),
    )
    .into_iter()
    .map(|spans| {
        let mut line = new_line();
        push_span(&mut line, TEXT_COL, "", theme.muted());
        line.spans.extend(spans);
        line
    })
    .collect()
}

/// A one-line text field: `› text▌` (the panel's deny/feedback/Other
/// stages).
fn field_line(field: &Composer, theme: Theme) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, "›", theme.text());
    push_span(
        &mut line,
        TEXT_COL,
        field.display_with_cursor(),
        theme.text(),
    );
    line
}

// --- identity and labels -----------------------------------------------------

/// The panel header's tool identity + magnitude: `Edit sync/config.rs
/// (+2 -1)`, `Write sync/retry.rs (12 lines)`, `Bash`, the tool name
/// otherwise. Magnitudes come from the ask's artifact (estimated at ask
/// time; `(replaces every occurrence)` under replace_all).
pub(crate) fn ask_identity(ask: &Ask) -> String {
    let AskKind::Permission {
        tool_name,
        invocation,
        ..
    } = &ask.kind
    else {
        return "question".to_string();
    };
    let name = tool_name.as_deref().unwrap_or("tool");
    let mut identity = match invocation {
        ToolInvocation::Edit {
            file_path: Some(path),
            ..
        }
        | ToolInvocation::Write {
            file_path: Some(path),
        }
        | ToolInvocation::Read {
            file_path: Some(path),
        } => format!("{name} {path}"),
        ToolInvocation::Plan { .. } => "plan review".to_string(),
        ToolInvocation::Query { text: Some(text) } => format!("{name} {text}"),
        ToolInvocation::Task {
            description: Some(description),
            ..
        } => format!("{name} {description}"),
        _ => name.to_string(),
    };
    match &ask.artifact {
        Some(AskArtifact::Diff(diff_artifact)) => {
            identity.push(' ');
            identity.push_str(&diff::magnitude_text(&diff_artifact.magnitude));
        }
        Some(AskArtifact::NewFile { content }) => {
            let lines = content.lines().count();
            identity.push_str(&format!(" ({lines} lines)"));
        }
        None => {}
    }
    identity
}

/// Option 2's label derives from the hook's `permission_suggestions`
/// (Phase 3 fact: claude's menu is GENERATED from them — no fixed
/// phrase). The observed kind gets claude's own wording; unobserved kinds
/// state outcome + scope from the facts they carry.
fn scoped_label(suggestions: &[SuggestionFact]) -> String {
    let Some(suggestion) = suggestions.first() else {
        return "Allow — apply the suggested rule".to_string();
    };
    if suggestion.kind.as_deref() == Some("addDirectories") && !suggestion.directories.is_empty() {
        return format!(
            "Always allow access to {} from this project",
            suggestion.directories.join(", ")
        );
    }
    match suggestion.destination.as_deref() {
        Some("session") => "Allow for this session".to_string(),
        _ => "Allow — apply the suggested rule".to_string(),
    }
}

/// The optimistic pending marker's summary: what was answered, plainly.
fn answer_summary(answer: &AskAnswer, theme: Theme) -> (&'static str, Style, &'static str) {
    match answer {
        AskAnswer::Permission(PermissionAnswer::AllowOnce) => ("✔", theme.ok(), "allowed once"),
        AskAnswer::Permission(PermissionAnswer::AllowScoped) => {
            ("✔", theme.ok(), "allowed (scoped)")
        }
        AskAnswer::Permission(PermissionAnswer::Deny { .. }) => ("✗", theme.error(), "denied"),
        AskAnswer::Plan(PlanAnswer::ApproveAuto) => ("✔", theme.ok(), "plan approved (auto)"),
        AskAnswer::Plan(PlanAnswer::ApproveManual) => ("✔", theme.ok(), "plan approved (manual)"),
        AskAnswer::Plan(PlanAnswer::RequestChanges { .. }) => {
            ("✗", theme.error(), "changes requested")
        }
        AskAnswer::Question { .. } => ("?", theme.warn(), "answered"),
    }
}

// --- shared pieces -----------------------------------------------------------

/// `⚠ permission — <identity>` with the honest `(1 of N)` on the right
/// when asks queue (C1).
fn header_line(
    glyph: &str,
    glyph_style: Style,
    title: &str,
    ask_count: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, glyph.to_string(), glyph_style);
    push_span(&mut line, TEXT_COL, title.to_string(), theme.text());
    if ask_count > 1 {
        push_right(
            &mut line,
            format!("(1 of {ask_count})"),
            width,
            theme.muted(),
        );
    }
    line
}

/// The stated failure line (SendFailed resurfacing, or a synchronous
/// refusal the reducer reported).
fn failure_line(message: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    markdown::plain_rows(
        message,
        width.saturating_sub(TEXT_COL + 1).max(1),
        theme.text(),
    )
    .into_iter()
    .enumerate()
    .map(|(index, spans)| {
        let mut line = new_line();
        if index == 0 {
            push_span(&mut line, GLYPH_COL, "✗", theme.error());
        }
        push_span(&mut line, TEXT_COL, "", theme.text());
        line.spans.extend(spans);
        line
    })
    .collect()
}

/// The numbered action list — the one list idiom every ask uses (C2).
/// Descriptions render dim, aligned past the widest label.
fn action_lines(
    actions: &[(&str, Option<&str>)],
    cursor: Option<usize>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let label_col = TEXT_COL + 3; // "1. "
    let desc_col = label_col
        + actions
            .iter()
            .map(|(label, _)| str_width(label))
            .max()
            .unwrap_or(0)
        + 5;
    actions
        .iter()
        .enumerate()
        .map(|(index, (label, description))| {
            let mut line = new_line();
            if cursor == Some(index) {
                push_span(&mut line, GLYPH_COL, "›", theme.text());
            }
            push_span(&mut line, TEXT_COL, format!("{}.", index + 1), theme.text());
            push_span(&mut line, label_col, (*label).to_string(), theme.text());
            if let Some(description) = description
                && desc_col + str_width(description) < width.saturating_sub(1)
            {
                push_span(
                    &mut line,
                    desc_col,
                    (*description).to_string(),
                    theme.muted(),
                );
            }
            line
        })
        .collect()
}

/// The plan's three-way review actions (C3) — one list, reader and docked
/// panel alike.
pub(crate) fn plan_actions(
    cursor: Option<usize>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    action_lines(
        &[
            (
                "Approve — auto",
                Some("agent proceeds, edits apply without asking"),
            ),
            ("Approve — manual", Some("agent asks before each edit")),
            ("Request changes", Some("feedback required")),
        ],
        cursor,
        width,
        theme,
    )
}

/// The permission actions (C2): plain outcomes and scopes, option 2 from
/// the suggestion facts.
pub(crate) fn permission_actions(
    suggestions: &[SuggestionFact],
    cursor: Option<usize>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let scoped = scoped_label(suggestions);
    action_lines(
        &[
            ("Allow once", None),
            (scoped.as_str(), None),
            ("Deny — tell the agent why (optional)", None),
        ],
        cursor,
        width,
        theme,
    )
}

/// The ask's body under the permission header (C2): the mini-diff for
/// Edit, the `+` block for Write, `$ command` for Bash, the plan preview
/// for plan review, a compact typed line otherwise.
fn body_lines(ask: &Ask, width: usize, theme: Theme) -> Vec<Line<'static>> {
    match &ask.artifact {
        Some(AskArtifact::Diff(artifact)) => {
            return diff::diff_preview(artifact, width, theme, diff::PREVIEW_BUDGET, "f full diff");
        }
        Some(AskArtifact::NewFile { content }) => {
            return diff::new_file_preview(content, width, theme, diff::PREVIEW_BUDGET);
        }
        None => {}
    }
    let AskKind::Permission { invocation, .. } = &ask.kind else {
        return Vec::new();
    };
    match invocation {
        ToolInvocation::Bash { command, .. } => {
            let command = command.as_deref().unwrap_or_default();
            let mut lines = Vec::new();
            for (index, row) in markdown::plain_rows(
                command,
                width.saturating_sub(TEXT_COL + 2 + 1).max(1),
                theme.code(),
            )
            .into_iter()
            .enumerate()
            {
                let mut line = new_line();
                if index == 0 {
                    push_span(&mut line, TEXT_COL, "$", theme.muted());
                }
                push_span(&mut line, TEXT_COL + 2, "", theme.code());
                line.spans.extend(row);
                lines.push(line);
            }
            lines
        }
        ToolInvocation::Plan {
            plan: Some(plan), ..
        } => {
            let mut lines = Vec::new();
            let total = plan.lines().count();
            for text in plan.lines().take(PLAN_PREVIEW_LINES) {
                for spans in markdown::plain_rows(
                    text,
                    width.saturating_sub(TEXT_COL + 1).max(1),
                    theme.muted(),
                ) {
                    let mut line = new_line();
                    push_span(&mut line, TEXT_COL, "", theme.muted());
                    line.spans.extend(spans);
                    lines.push(line);
                }
            }
            if total > PLAN_PREVIEW_LINES {
                let mut line = new_line();
                push_span(
                    &mut line,
                    GLYPH_COL + 2,
                    format!(
                        "⋮  +{} more lines · f full plan",
                        total - PLAN_PREVIEW_LINES
                    ),
                    theme.muted(),
                );
                lines.push(line);
            }
            lines
        }
        // The compact typed fallback: the header already carries the
        // identity; nothing else is stated about a tool this build does
        // not know.
        _ => Vec::new(),
    }
}

// --- the panel ---------------------------------------------------------------

/// The docked panel for the current ask head (writable chats): rule,
/// header, body, actions, hints — replacing the composer block. The
/// caller guarantees an ask heads the queue.
pub(crate) fn panel_lines(
    ask: &Ask,
    ask_count: usize,
    ui: Option<&AskUi>,
    ask_failure: Option<&str>,
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
) -> Vec<Line<'static>> {
    let ctx = PanelContext {
        width,
        theme,
        quit_guard_armed,
    };
    // The optimistic collapse (C5): a dim pending marker holds the
    // collapsed entry until the transcript confirms.
    if let AskState::AnsweredOptimistic { answer, .. } = &ask.state {
        let (glyph, glyph_style, summary) = answer_summary(answer, theme);
        let mut lines = vec![rule_line(width, theme)];
        let mut line = new_line();
        push_span(&mut line, GLYPH_COL, glyph.to_string(), glyph_style);
        push_span(
            &mut line,
            TEXT_COL,
            format!("{summary} — {}", ask_identity(ask)),
            theme.text(),
        );
        line.spans.push(Span::styled(" · sending…", theme.muted()));
        lines.push(line);
        lines.push(blank());
        lines.extend(hint_lines(
            "answer sent — awaiting confirmation",
            width,
            theme,
            quit_guard_armed,
        ));
        return lines;
    }

    // A stale panel state for a different ask renders as a fresh one
    // (ViewState staleness is tolerance territory, clamped at render).
    let fresh = AskUi::for_ask(ask);
    let ui = match ui {
        Some(ui) if ui.ask_id == ask.id => ui,
        _ => &fresh,
    };

    // The failure to state, if any: a failed send resurfaces the ask with
    // the failure verbatim (C5); a synchronous refusal reports the same
    // way.
    let failed = match &ask.state {
        AskState::SendFailed { message } => Some(message.as_str()),
        _ => ask_failure,
    };

    match &ask.kind {
        AskKind::Question { questions } => question_panel(questions, ui, failed, ask_count, ctx),
        AskKind::Permission { suggestions, .. } => {
            if ask_ui::is_plan(ask) {
                return plan_panel(ask, ui, failed, ask_count, ctx);
            }
            if let Some(refusal) = encoding::menu_shape_refusal(&ask.kind) {
                return refusal_panel(ask, &refusal.to_string(), ask_count, ctx);
            }
            permission_panel(ask, suggestions, ui, failed, ask_count, ctx)
        }
    }
}

fn permission_panel(
    ask: &Ask,
    suggestions: &[SuggestionFact],
    ui: &AskUi,
    failed: Option<&str>,
    ask_count: usize,
    ctx: PanelContext,
) -> Vec<Line<'static>> {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut lines = vec![
        rule_line(width, theme),
        header_line(
            "⚠",
            theme.warn(),
            &format!("permission — {}", ask_identity(ask)),
            ask_count,
            width,
            theme,
        ),
    ];
    if let Some(message) = failed {
        lines.extend(failure_line(message, width, theme));
    }
    lines.push(blank());
    match &ui.stage {
        AskStage::DenyFeedback => {
            let mut label = new_line();
            push_span(
                &mut label,
                TEXT_COL,
                "Deny — tell the agent why (optional)",
                theme.text(),
            );
            lines.push(label);
            lines.push(blank());
            lines.push(field_line(&ui.deny_feedback, theme));
            lines.push(blank());
            lines.extend(hint_lines(
                "enter deny (empty = plain deny) · esc back (never answers)",
                width,
                theme,
                quit_guard_armed,
            ));
        }
        _ => {
            let cursor = ui.menu_cursor();
            let body = body_lines(ask, width, theme);
            let has_body = !body.is_empty();
            lines.extend(body);
            if has_body {
                lines.push(blank());
            }
            lines.extend(permission_actions(suggestions, Some(cursor), width, theme));
            lines.push(blank());
            let f_hint = if ask_ui::has_readable(ask) {
                match &ask.artifact {
                    Some(AskArtifact::NewFile { .. }) => " · f full view",
                    _ => " · f full diff",
                }
            } else {
                ""
            };
            lines.extend(hint_lines(
                &format!("1-3/↑↓ select · enter confirm{f_hint} · esc back (never answers)"),
                width,
                theme,
                quit_guard_armed,
            ));
        }
    }
    lines
}

/// The unverified-shape panel (C2): the menu claude renders is generated
/// from the suggestions, and shapes no capture confirmed have no digit
/// table — the panel states the typed refusal read-only-style instead of
/// offering actions the encoder would refuse.
fn refusal_panel(
    ask: &Ask,
    refusal: &str,
    ask_count: usize,
    ctx: PanelContext,
) -> Vec<Line<'static>> {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut lines = vec![
        rule_line(width, theme),
        header_line(
            "⚠",
            theme.warn(),
            &format!("permission — {}", ask_identity(ask)),
            ask_count,
            width,
            theme,
        ),
        blank(),
    ];
    lines.extend(body_lines(ask, width, theme));
    lines.push(blank());
    lines.extend(failure_line(refusal, width, theme));
    lines.push(blank());
    let f_hint = if ask_ui::has_readable(ask) {
        "f full diff · "
    } else {
        ""
    };
    lines.extend(hint_lines(
        &format!("answer from the raw attach · {f_hint}ctrl+x interrupt"),
        width,
        theme,
        quit_guard_armed,
    ));
    lines
}

fn plan_panel(
    ask: &Ask,
    ui: &AskUi,
    failed: Option<&str>,
    ask_count: usize,
    ctx: PanelContext,
) -> Vec<Line<'static>> {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut lines = vec![
        rule_line(width, theme),
        header_line("⚠", theme.warn(), "plan review", ask_count, width, theme),
    ];
    if let Some(message) = failed {
        lines.extend(failure_line(message, width, theme));
    }
    lines.push(blank());
    match &ui.stage {
        AskStage::PlanFeedback => {
            let mut label = new_line();
            push_span(
                &mut label,
                TEXT_COL,
                "Request changes — tell the agent what to change (required)",
                theme.text(),
            );
            lines.push(label);
            lines.push(blank());
            lines.push(field_line(&ui.plan_feedback, theme));
            lines.push(blank());
            lines.extend(hint_lines(
                "enter request changes · esc back (keeps text)",
                width,
                theme,
                quit_guard_armed,
            ));
        }
        _ => {
            let cursor = ui.menu_cursor();
            lines.extend(body_lines(ask, width, theme));
            lines.push(blank());
            lines.extend(plan_actions(Some(cursor), width, theme));
            lines.push(blank());
            lines.extend(hint_lines(
                "1-3/↑↓ select · enter confirm · f full plan · esc back (never answers)",
                width,
                theme,
                quit_guard_armed,
            ));
        }
    }
    lines
}

// --- the question form (C4) --------------------------------------------------

fn question_panel(
    questions: &[QuestionFact],
    ui: &AskUi,
    failed: Option<&str>,
    ask_count: usize,
    ctx: PanelContext,
) -> Vec<Line<'static>> {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let fresh = QuestionUi::new(questions);
    let form = match &ui.stage {
        AskStage::Question(form) if form.drafts.len() == questions.len() => form,
        _ => &fresh,
    };
    let tabbed = ask_ui::tabbed(questions);

    let mut lines = vec![rule_line(width, theme)];
    lines.push(question_header(
        questions, form, tabbed, ask_count, width, theme,
    ));
    if let Some(message) = failed {
        lines.extend(failure_line(message, width, theme));
    }
    lines.push(blank());

    if form.on_submit_tab(questions) {
        lines.extend(review_lines(questions, form, width, theme));
        lines.push(blank());
        lines.extend(hint_lines(
            "enter submit · tab/←→ questions · esc back (never answers)",
            width,
            theme,
            quit_guard_armed,
        ));
        return lines;
    }

    let Some(question) = questions.get(form.tab) else {
        return lines;
    };
    let draft = &form.drafts[form.tab];

    // The question text, with the multi-select statement appended.
    let mut question_text = question
        .question
        .clone()
        .or_else(|| question.header.clone())
        .unwrap_or_default();
    if question.multi_select {
        question_text.push_str(" (select all that apply)");
    }
    for spans in markdown::plain_rows(
        &question_text,
        width.saturating_sub(TEXT_COL + 1).max(1),
        theme.text(),
    ) {
        let mut line = new_line();
        push_span(&mut line, TEXT_COL, "", theme.text());
        line.spans.extend(spans);
        lines.push(line);
    }
    lines.push(blank());
    lines.extend(option_lines(question, draft, form, width, theme));
    lines.push(blank());

    let count = question.options.len() + 1;
    let hint = if form.editing_other {
        "enter save · esc back (keeps text)".to_string()
    } else if question.multi_select {
        format!(
            "1-{count}/↑↓ select · space toggle · tab next question · enter advance · esc back (never answers)"
        )
    } else if tabbed {
        format!(
            "1-{count}/↑↓ select · tab next question · enter advance · esc back (never answers)"
        )
    } else {
        format!("1-{count}/↑↓ select · enter confirm · esc back (never answers)")
    };
    lines.extend(hint_lines(&hint, width, theme, quit_guard_armed));
    lines
}

/// `? questions   [storage*] [rollout] [submit]` — the current tab is
/// starred, answered tabs brighten, unanswered ones stay dim (C4).
fn question_header(
    questions: &[QuestionFact],
    form: &QuestionUi,
    tabbed: bool,
    ask_count: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = new_line();
    push_span(&mut line, GLYPH_COL, "?", theme.warn());
    let title = if questions.len() > 1 {
        "questions"
    } else {
        "question"
    };
    push_span(&mut line, TEXT_COL, title, theme.text());
    if tabbed {
        let mut col = line_len(&line) + 2;
        for (index, question) in questions.iter().enumerate() {
            let label = question
                .header
                .clone()
                .unwrap_or_else(|| format!("q{}", index + 1));
            let current = form.tab == index;
            let star = if current { "*" } else { "" };
            let style = if current {
                theme.warn()
            } else if ask_ui::answered(&form.drafts[index]) {
                theme.text()
            } else {
                theme.muted()
            };
            push_span(&mut line, col + 1, format!("[{label}{star}]"), style);
            col = line_len(&line);
        }
        let submit_current = form.tab >= questions.len();
        let star = if submit_current { "*" } else { "" };
        let style = if submit_current {
            theme.warn()
        } else {
            theme.muted()
        };
        push_span(&mut line, col + 1, format!("[submit{star}]"), style);
    }
    if ask_count > 1 {
        push_right(
            &mut line,
            format!("(1 of {ask_count})"),
            width,
            theme.muted(),
        );
    }
    line
}

/// The numbered option rows: `› 1. [x] label   description`, the appended
/// `Other…` last with its inline editor when open.
fn option_lines(
    question: &QuestionFact,
    draft: &QuestionDraft,
    form: &QuestionUi,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let number_col = TEXT_COL;
    let label_col = if question.multi_select {
        number_col + 3 + 4 // "1. " + "[x] "
    } else {
        number_col + 3
    };
    let widest = question
        .options
        .iter()
        .map(|option| str_width(&option.label))
        .max()
        .unwrap_or(0)
        .max(str_width("Other…"));
    let desc_col = label_col + widest + 4;

    let mut lines = Vec::new();
    let total = question.options.len() + 1;
    for index in 0..total {
        let other = index == question.options.len();
        let label = if other {
            "Other…".to_string()
        } else {
            question.options[index].label.clone()
        };
        let chosen = if other {
            draft.other_chosen
        } else {
            draft.selected.contains(&index)
        };
        let mut line = new_line();
        if form.cursor == index {
            push_span(&mut line, GLYPH_COL, "›", theme.text());
        }
        push_span(
            &mut line,
            number_col,
            format!("{}.", index + 1),
            theme.text(),
        );
        if question.multi_select {
            let checkbox = if chosen { "[x]" } else { "[ ]" };
            push_span(&mut line, number_col + 3, checkbox, theme.text());
        }
        let label_style = if chosen && !question.multi_select {
            theme.emphasis()
        } else {
            theme.text()
        };
        push_span(&mut line, label_col, label, label_style);

        // The description column: dim per-option descriptions; the Other
        // row shows its inline editor when open, its committed text when
        // set, and the affordance otherwise.
        if other && form.editing_other && form.cursor == index {
            push_span(
                &mut line,
                desc_col,
                format!("› {}", draft.other.display_with_cursor()),
                theme.text(),
            );
        } else if other && ask_ui::other_present(draft) {
            push_span(&mut line, desc_col, draft.other.text(), theme.text());
        } else {
            let description = if other {
                Some("type your own answer".to_string())
            } else {
                question.options[index].description.clone()
            };
            if let Some(description) = description
                && desc_col + str_width(&description) < width.saturating_sub(1)
            {
                push_span(&mut line, desc_col, description, theme.muted());
            }
        }
        lines.push(line);
    }
    lines
}

/// The submit tab's review list: every question with its answer, the
/// unanswered ones in error color (C4).
fn review_lines(
    questions: &[QuestionFact],
    form: &QuestionUi,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, question) in questions.iter().enumerate() {
        let draft = &form.drafts[index];
        let name = question
            .header
            .clone()
            .or_else(|| question.question.clone())
            .unwrap_or_else(|| format!("q{}", index + 1));
        let mut line = new_line();
        if ask_ui::answered(draft) {
            let mut parts: Vec<String> = draft
                .selected
                .iter()
                .filter_map(|selected| question.options.get(*selected))
                .map(|option| option.label.clone())
                .collect();
            if draft.other_chosen && ask_ui::other_present(draft) {
                parts.push(draft.other.text());
            }
            push_span(&mut line, TEXT_COL, format!("{name} — "), theme.text());
            let answer = parts.join(", ");
            let room = width.saturating_sub(line_len(&line) + 1).max(1);
            let mut spans = markdown::plain_rows(&answer, room, theme.muted())
                .into_iter()
                .next()
                .unwrap_or_default();
            line.spans.append(&mut spans);
        } else {
            push_span(&mut line, TEXT_COL, format!("{name} — "), theme.text());
            line.spans.push(Span::styled("unanswered", theme.error()));
        }
        lines.push(line);
    }
    lines
}

// --- read-only fact panels (F1) ----------------------------------------------

/// The read-only ask fact panel: what the agent is asking, the identical
/// preview, and the honest wait — read affordances only, no action row.
pub(crate) fn readonly_panel_lines(
    ask: &Ask,
    ask_count: usize,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (glyph, glyph_style, title, read_hint) = match &ask.kind {
        AskKind::Question { questions } => {
            let text = questions
                .first()
                .and_then(|question| {
                    question
                        .question
                        .clone()
                        .or_else(|| question.header.clone())
                })
                .unwrap_or_default();
            (
                "?",
                theme.warn(),
                format!("the agent is asking a question — {text}"),
                None,
            )
        }
        AskKind::Permission { .. } if ask_ui::is_plan(ask) => (
            "⚠",
            theme.warn(),
            "the agent is asking for plan approval".to_string(),
            Some("f read the plan"),
        ),
        AskKind::Permission { .. } => (
            "⚠",
            theme.warn(),
            format!("the agent is asking permission — {}", ask_identity(ask)),
            ask.artifact.as_ref().map(|artifact| match artifact {
                AskArtifact::Diff(_) => "f read the diff",
                AskArtifact::NewFile { .. } => "f read the file",
            }),
        ),
    };
    let mut lines = vec![
        rule_line(width, theme),
        header_line(glyph, glyph_style, &title, ask_count, width, theme),
    ];
    lines.extend(body_lines(ask, width, theme));
    let mut wait = String::from("waiting for a writable client");
    if let Some(hint) = read_hint {
        wait.push_str(" · ");
        wait.push_str(hint);
    }
    lines.extend(hint_lines(&wait, width, theme, false));
    lines
}
