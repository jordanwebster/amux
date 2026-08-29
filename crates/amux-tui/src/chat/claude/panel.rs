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
use crate::render::{Theme, line_len, push_span, str_width};
use crate::view::QuitGuard;

/// Column grid inside the panel, measured from the column the panel's
/// tint starts its text at: a row's own glyph on the left, its text two
/// cells in. The painter adds the frame indent, so nothing here knows
/// where the panel sits on the screen.
const GLYPH_COL: usize = 0;
const TEXT_COL: usize = 2;

/// The plan's docked preview length (C3: truncated plan; the reader has
/// the whole).
const PLAN_PREVIEW_LINES: usize = 6;

/// What an ask asks, formatted but not yet painted: the painter owns the
/// surface, the gaps and the frame indent; this owns the words.
pub(crate) struct AskPanel {
    pub(crate) title: String,
    pub(crate) body: Vec<Line<'static>>,
    pub(crate) actions: Vec<Line<'static>>,
    pub(crate) hints: String,
}

impl AskPanel {
    /// The queue is stated in the title because the panel has one title
    /// row and no right margin of its own: an ask that is one of several
    /// says so where its name is.
    fn titled(title: String, ask_count: usize) -> Self {
        Self {
            title: match ask_count {
                0 | 1 => title,
                count => format!("{title} · 1 of {count}"),
            },
            body: Vec::new(),
            actions: Vec::new(),
            hints: String::new(),
        }
    }

    /// The armed guard replaces the hints entirely, in warning colour, so
    /// it lands as the last action row rather than as hint text.
    fn hinted(mut self, hints: &str, armed: bool, theme: Theme) -> Self {
        if armed {
            self.actions.push(blank());
            self.actions.push(armed_quit_row(theme));
        } else {
            self.hints = hints.to_string();
        }
        self
    }
}

#[derive(Clone, Copy)]
struct PanelContext {
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
}

fn blank() -> Line<'static> {
    Line::default()
}

/// The armed quit guard's hint, as a panel row: it replaces the hints
/// wherever they live, and it has to keep the warning colour a plain
/// hint string could not carry.
fn armed_quit_row(theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, TEXT_COL, QuitGuard::HINT, theme.warn());
    line
}

/// A one-line text field: `› text▌` (the panel's deny/feedback/Other
/// stages).
fn field_line(field: &Composer, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
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

/// The stated failure line (SendFailed resurfacing, or a synchronous
/// refusal the reducer reported).
fn failure_line(message: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    markdown::plain_rows(message, width.saturating_sub(TEXT_COL).max(1), theme.text())
        .into_iter()
        .enumerate()
        .map(|(index, spans)| {
            let mut line = Line::default();
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
            let mut line = Line::default();
            if cursor == Some(index) {
                push_span(&mut line, GLYPH_COL, "›", theme.text());
            }
            push_span(&mut line, TEXT_COL, format!("{}.", index + 1), theme.text());
            push_span(&mut line, label_col, (*label).to_string(), theme.text());
            if let Some(description) = description
                && desc_col + str_width(description) < width
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
    // A diff artifact is not the panel's to place: the adapter puts its
    // rows above these, through the shared diff rows both chats use.
    if let Some(AskArtifact::NewFile { content }) = &ask.artifact {
        return diff::new_file_preview(content, width, theme, diff::PREVIEW_BUDGET);
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
                width.saturating_sub(TEXT_COL + 2).max(1),
                theme.code(),
            )
            .into_iter()
            .enumerate()
            {
                let mut line = Line::default();
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
                for spans in
                    markdown::plain_rows(text, width.saturating_sub(TEXT_COL).max(1), theme.muted())
                {
                    let mut line = Line::default();
                    push_span(&mut line, TEXT_COL, "", theme.muted());
                    line.spans.extend(spans);
                    lines.push(line);
                }
            }
            if total > PLAN_PREVIEW_LINES {
                let mut line = Line::default();
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

/// The current ask head as panel parts (writable chats): what is being
/// asked, what it is about, the answers on offer and the keys that give
/// them. The caller guarantees an ask heads the queue.
pub(crate) fn ask_panel(
    ask: &Ask,
    ask_count: usize,
    ui: Option<&AskUi>,
    ask_failure: Option<&str>,
    width: usize,
    theme: Theme,
    quit_guard_armed: bool,
) -> AskPanel {
    let ctx = PanelContext {
        width,
        theme,
        quit_guard_armed,
    };
    // The optimistic collapse (C5): a dim pending marker holds the
    // collapsed entry until the transcript confirms.
    if let AskState::AnsweredOptimistic { answer, .. } = &ask.state {
        let (glyph, _, summary) = answer_summary(answer, theme);
        return AskPanel::titled(
            format!("{glyph} {summary} — {} · sending…", ask_identity(ask)),
            ask_count,
        )
        .hinted(
            "answer sent — awaiting confirmation",
            quit_guard_armed,
            theme,
        );
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
) -> AskPanel {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut panel = AskPanel::titled(format!("permission — {}", ask_identity(ask)), ask_count);
    if let Some(message) = failed {
        panel.body.extend(failure_line(message, width, theme));
    }
    match &ui.stage {
        AskStage::DenyFeedback => {
            let mut label = Line::default();
            push_span(
                &mut label,
                TEXT_COL,
                "Deny — tell the agent why (optional)",
                theme.text(),
            );
            panel.body.push(label);
            panel.actions.push(field_line(&ui.deny_feedback, theme));
            panel.hinted(
                "enter deny (empty = plain deny) · esc back (never answers)",
                quit_guard_armed,
                theme,
            )
        }
        _ => {
            panel.body.extend(body_lines(ask, width, theme));
            panel.actions.extend(permission_actions(
                suggestions,
                Some(ui.menu_cursor()),
                width,
                theme,
            ));
            let f_hint = if ask_ui::has_readable(ask) {
                match &ask.artifact {
                    Some(AskArtifact::NewFile { .. }) => " · f full view",
                    _ => " · f full diff",
                }
            } else {
                ""
            };
            panel.hinted(
                &format!("1-3/↑↓ select · enter confirm{f_hint} · esc back (never answers)"),
                quit_guard_armed,
                theme,
            )
        }
    }
}

/// The unverified-shape panel (C2): the menu claude renders is generated
/// from the suggestions, and shapes no capture confirmed have no digit
/// table — the panel states the typed refusal read-only-style instead of
/// offering actions the encoder would refuse.
fn refusal_panel(ask: &Ask, refusal: &str, ask_count: usize, ctx: PanelContext) -> AskPanel {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut panel = AskPanel::titled(format!("permission — {}", ask_identity(ask)), ask_count);
    panel.body.extend(body_lines(ask, width, theme));
    panel.actions.extend(failure_line(refusal, width, theme));
    let f_hint = if ask_ui::has_readable(ask) {
        "f full diff · "
    } else {
        ""
    };
    panel.hinted(
        &format!("answer from the raw attach · {f_hint}ctrl+x interrupt"),
        quit_guard_armed,
        theme,
    )
}

fn plan_panel(
    ask: &Ask,
    ui: &AskUi,
    failed: Option<&str>,
    ask_count: usize,
    ctx: PanelContext,
) -> AskPanel {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut panel = AskPanel::titled("plan review".to_string(), ask_count);
    if let Some(message) = failed {
        panel.body.extend(failure_line(message, width, theme));
    }
    match &ui.stage {
        AskStage::PlanFeedback => {
            let mut label = Line::default();
            push_span(
                &mut label,
                TEXT_COL,
                "Request changes — tell the agent what to change (required)",
                theme.text(),
            );
            panel.body.push(label);
            panel.actions.push(field_line(&ui.plan_feedback, theme));
            panel.hinted(
                "enter request changes · esc back (keeps text)",
                quit_guard_armed,
                theme,
            )
        }
        _ => {
            panel.body.extend(body_lines(ask, width, theme));
            panel
                .actions
                .extend(plan_actions(Some(ui.menu_cursor()), width, theme));
            panel.hinted(
                "1-3/↑↓ select · enter confirm · f full plan · esc back (never answers)",
                quit_guard_armed,
                theme,
            )
        }
    }
}

// --- the question form (C4) --------------------------------------------------

fn question_panel(
    questions: &[QuestionFact],
    ui: &AskUi,
    failed: Option<&str>,
    ask_count: usize,
    ctx: PanelContext,
) -> AskPanel {
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

    let title = if questions.len() > 1 {
        "questions"
    } else {
        "question"
    };
    let mut panel = AskPanel::titled(title.to_string(), ask_count);
    // The tab strip is a body row, not part of the title: each tab
    // carries its own colour — current, answered, unanswered — and a
    // title is one word in one style.
    if tabbed {
        panel.body.push(tab_strip(questions, form, theme));
    }
    if let Some(message) = failed {
        panel.body.extend(failure_line(message, width, theme));
    }

    if form.on_submit_tab(questions) {
        panel
            .actions
            .extend(review_lines(questions, form, width, theme));
        return panel.hinted(
            "enter submit · tab/←→ questions · esc back (never answers)",
            quit_guard_armed,
            theme,
        );
    }

    let Some(question) = questions.get(form.tab) else {
        return panel;
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
        width.saturating_sub(TEXT_COL).max(1),
        theme.text(),
    ) {
        let mut line = Line::default();
        push_span(&mut line, TEXT_COL, "", theme.text());
        line.spans.extend(spans);
        panel.body.push(line);
    }
    panel
        .actions
        .extend(option_lines(question, draft, form, width, theme));

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
    panel.hinted(&hint, quit_guard_armed, theme)
}

/// `[storage*] [rollout] [submit]` — the current tab is starred,
/// answered tabs brighten, unanswered ones stay dim (C4).
fn tab_strip(questions: &[QuestionFact], form: &QuestionUi, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    let mut col = TEXT_COL;
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
        push_span(&mut line, col, format!("[{label}{star}]"), style);
        col = line_len(&line) + 1;
    }
    let submit_current = form.tab >= questions.len();
    let star = if submit_current { "*" } else { "" };
    let style = if submit_current {
        theme.warn()
    } else {
        theme.muted()
    };
    push_span(&mut line, col, format!("[submit{star}]"), style);
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
        let mut line = Line::default();
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
                && desc_col + str_width(&description) < width
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
        let mut line = Line::default();
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
            let room = width.saturating_sub(line_len(&line)).max(1);
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
pub(crate) fn readonly_ask_panel(
    ask: &Ask,
    ask_count: usize,
    width: usize,
    theme: Theme,
) -> AskPanel {
    let (title, read_hint) = match &ask.kind {
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
            (format!("the agent is asking a question — {text}"), None)
        }
        AskKind::Permission { .. } if ask_ui::is_plan(ask) => (
            "the agent is asking for plan approval".to_string(),
            Some("f read the plan"),
        ),
        AskKind::Permission { .. } => (
            format!("the agent is asking permission — {}", ask_identity(ask)),
            ask.artifact.as_ref().map(|artifact| match artifact {
                AskArtifact::Diff(_) => "f read the diff",
                AskArtifact::NewFile { .. } => "f read the file",
            }),
        ),
    };
    let mut panel = AskPanel::titled(title, ask_count);
    panel.body.extend(body_lines(ask, width, theme));
    let mut wait = String::from("waiting for a writable client");
    if let Some(hint) = read_hint {
        wait.push_str(" · ");
        wait.push_str(hint);
    }
    panel.hints = wait;
    panel
}
