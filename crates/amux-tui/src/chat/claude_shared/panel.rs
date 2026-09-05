//! The docked ask panel renderer (C1–C4): the ask takes over the composer
//! area behind a dim rule; the feed above stays the context you decide
//! with. Pure formatting over Model facts and panel ViewState — option
//! labels derive from the hook's suggestion facts, magnitudes from the
//! ask's computed document, refusals from the encoder's typed gate; the
//! code here formats and never decides.
//!
//! Lines come back "open" (no padding, no right border); the frame
//! assembler finishes everything once.

use amux_ui::claude::{
    AskDocument, QuestionFact, SuggestionDestination, SuggestionFact, SuggestionKind,
    ToolInvocation,
};
use amux_ui::claude_sdk::{
    ElicitationField, ElicitationFieldKind, ElicitationForm, dialog_choices, dialog_payload_summary,
};
use ratatui::text::{Line, Span};
use serde_json::Value;

use crate::chat::blocks::{self, paint_ask_panel};
use crate::chat::claude_shared::ask_ui::{
    self, AskStage, AskUi, ElicitationUi, FormAction, QuestionDraft, QuestionUi, form_actions,
    form_fields,
};
use crate::chat::claude_shared::{SharedAsk, SharedAskKind, SharedAskState, diff};
use crate::chat::frame::BlockKey;
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
/// otherwise. Magnitudes come from the ask's document (estimated at ask
/// time; `(replaces every occurrence)` under replace_all).
pub(crate) fn ask_identity(ask: &SharedAsk<'_>) -> String {
    let SharedAskKind::Permission {
        tool_name,
        invocation,
        ..
    } = &ask.kind
    else {
        return match &ask.kind {
            SharedAskKind::Plan { .. } => "plan review".to_string(),
            SharedAskKind::Elicitation { server, .. } => match server {
                Some(server) => format!("{server} asks"),
                None => "external asks".to_string(),
            },
            SharedAskKind::Dialog { dialog_kind, .. } => (*dialog_kind).to_string(),
            _ => "question".to_string(),
        };
    };
    let name = tool_name.unwrap_or("tool");
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
    match ask.document() {
        Some(AskDocument::Diff(diff_document)) => {
            identity.push(' ');
            identity.push_str(&diff::magnitude_text(&diff_document.magnitude));
        }
        Some(AskDocument::NewFile { content }) => {
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
    if suggestion.kind.as_ref() == Some(&SuggestionKind::AddDirectories)
        && !suggestion.directories.is_empty()
    {
        return format!(
            "Always allow access to {} from this project",
            suggestion.directories.join(", ")
        );
    }
    match suggestion.destination.as_ref() {
        Some(SuggestionDestination::Session) => "Allow for this session".to_string(),
        _ => "Allow — apply the suggested rule".to_string(),
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
fn body_lines(ask: &SharedAsk<'_>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    // A diff document is not the panel's to place: `paint` puts its rows
    // above these, through the shared diff rows both chats use.
    if let Some(AskDocument::NewFile { content }) = ask.document() {
        return diff::new_file_preview(content, width, theme, diff::PREVIEW_BUDGET);
    }
    if let Some(plan) = ask.plan() {
        return plan_preview(plan, width, theme);
    }
    let SharedAskKind::Permission { invocation, .. } = &ask.kind else {
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
        // The compact typed fallback: the header already carries the
        // identity; nothing else is stated about a tool this build does
        // not know.
        _ => Vec::new(),
    }
}

/// The docked plan preview (C3): the first lines, and the arithmetic of
/// what the reader has that this does not.
fn plan_preview(plan: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
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

// --- the panel ---------------------------------------------------------------

/// The current ask head as panel parts (writable chats): what is being
/// asked, what it is about, the answers on offer and the keys that give
/// them. The caller guarantees an ask heads the queue.
pub(crate) fn ask_panel(
    ask: &SharedAsk<'_>,
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
    if let SharedAskState::Answered { summary } = &ask.state {
        let (glyph, summary) = (summary.glyph(), summary.text());
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
        SharedAskState::Failed { message } => Some(*message),
        _ => ask_failure,
    };

    match &ask.kind {
        SharedAskKind::Question { questions } => {
            question_panel(questions, ui, failed, ask_count, ctx)
        }
        SharedAskKind::Plan { .. } => plan_panel(ask, ui, failed, ask_count, ctx),
        SharedAskKind::Elicitation {
            server,
            message,
            form,
        } => elicitation_panel(*server, message, form, ui, failed, ask_count, ctx),
        SharedAskKind::Dialog {
            dialog_kind,
            payload,
        } => dialog_panel(dialog_kind, payload, ui, failed, ask_count, ctx),
        SharedAskKind::Permission { suggestions, .. } => {
            if ask.is_plan() {
                return plan_panel(ask, ui, failed, ask_count, ctx);
            }
            if let Some(refusal) = &ask.refusal {
                return refusal_panel(ask, refusal, ask_count, ctx);
            }
            permission_panel(ask, suggestions, ui, failed, ask_count, ctx)
        }
    }
}

fn permission_panel(
    ask: &SharedAsk<'_>,
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
            let f_hint = if ask.has_readable() {
                " · f open document"
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
fn refusal_panel(
    ask: &SharedAsk<'_>,
    refusal: &str,
    ask_count: usize,
    ctx: PanelContext,
) -> AskPanel {
    let PanelContext {
        width,
        theme,
        quit_guard_armed,
    } = ctx;
    let mut panel = AskPanel::titled(format!("permission — {}", ask_identity(ask)), ask_count);
    panel.body.extend(body_lines(ask, width, theme));
    panel.actions.extend(failure_line(refusal, width, theme));
    let f_hint = if ask.has_readable() {
        "f open document · "
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
    ask: &SharedAsk<'_>,
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

// --- the elicitation form ----------------------------------------------------

/// An MCP server's own question, as a form over the schema it sent: one
/// row per field in schema order, then the numbered answers. A schema
/// this build cannot express states why and offers only the two answers
/// that need no fields.
#[allow(clippy::too_many_arguments)]
fn elicitation_panel(
    server: Option<&str>,
    message: &str,
    form: &ElicitationForm,
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
    let title = match server {
        Some(server) => format!("{server} asks"),
        None => "external asks".to_string(),
    };
    let mut panel = AskPanel::titled(title, ask_count);
    if let Some(message) = failed {
        panel.body.extend(failure_line(message, width, theme));
    }
    for spans in markdown::plain_rows(message, width.saturating_sub(TEXT_COL).max(1), theme.text())
    {
        let mut line = Line::default();
        push_span(&mut line, TEXT_COL, "", theme.text());
        line.spans.extend(spans);
        panel.body.push(line);
    }

    let fresh = ElicitationUi::new(form);
    let state = match &ui.stage {
        AskStage::Elicitation(state) if state.drafts.len() == form_fields(form).len() => state,
        _ => &fresh,
    };
    let fields = form_fields(form);
    let actions = form_actions(form);

    if let ElicitationForm::Unsupported { reason } = form {
        panel.body.push(blank());
        panel.body.extend(failure_line(
            &format!("{reason} — this form cannot be filled in from the chat"),
            width,
            theme,
        ));
    }
    for (index, field) in fields.iter().enumerate() {
        panel
            .actions
            .extend(field_rows(field, index, state, width, theme));
    }
    if !fields.is_empty() {
        panel.actions.push(blank());
    }
    // The reason Send is not on offer belongs where Send is, not in a
    // footnote: a person reading the action list learns what is missing.
    let blocked = state.content(form).err();
    panel.actions.extend(action_lines(
        &actions
            .iter()
            .map(|action| match action {
                FormAction::Send => ("Send", blocked.as_deref()),
                FormAction::Decline => ("Decline", Some("the server is told you declined")),
                FormAction::Cancel => ("Cancel", None),
            })
            .collect::<Vec<_>>(),
        state.on_actions().then(|| state.action(form)),
        width,
        theme,
    ));

    let hint = if state.on_actions() {
        format!(
            "1-{}/↑↓ select · enter confirm · esc back (never answers)",
            actions.len()
        )
    } else {
        "tab/↑↓ move · enter next field · esc back (never answers)".to_string()
    };
    panel.hinted(&hint, quit_guard_armed, theme)
}

/// One schema field: its name and value on one row, what it is and
/// whether it must be filled on the dim row beneath.
fn field_rows(
    field: &ElicitationField,
    index: usize,
    state: &ElicitationUi,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let focused = !state.on_actions() && state.cursor == index;
    let label = field.title.clone().unwrap_or_else(|| field.name.clone());
    let value_col = TEXT_COL + str_width(&label).max(12) + 3;
    let mut line = Line::default();
    if focused {
        push_span(&mut line, GLYPH_COL, "›", theme.text());
    }
    push_span(&mut line, TEXT_COL, label, theme.text());
    let value = state
        .drafts
        .get(index)
        .map(|draft| draft.display(field, focused))
        .unwrap_or_default();
    push_span(&mut line, value_col, value, theme.text());
    let mut rows = vec![line];

    let mut meta = String::new();
    if field.required {
        meta.push_str("required · ");
    }
    meta.push_str(match &field.kind {
        ElicitationFieldKind::String => "text",
        ElicitationFieldKind::Number => "number",
        ElicitationFieldKind::Integer => "whole number",
        ElicitationFieldKind::Boolean => "space toggles",
        ElicitationFieldKind::Enum(_) => "←/→ choose",
    });
    if let Some(description) = &field.description {
        meta.push_str(" · ");
        meta.push_str(description);
    }
    // The same rule the option descriptions follow: a note that would
    // not fit is left off rather than wrapped under its own value.
    if value_col + str_width(&meta) < width {
        let mut note = Line::default();
        push_span(&mut note, value_col, meta, theme.muted());
        rows.push(note);
    }
    rows
}

// --- dialogs -----------------------------------------------------------------

/// A dialog the provider raised. A payload carrying a message and
/// labelled choices is answerable; anything else states the kind, what
/// the payload holds and offers only Cancel — which is labelled as what
/// it is, so nobody reads it as agreement.
fn dialog_panel(
    dialog_kind: &str,
    payload: &Value,
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
    let mut panel = AskPanel::titled(format!("dialog — {dialog_kind}"), ask_count);
    if let Some(message) = failed {
        panel.body.extend(failure_line(message, width, theme));
    }
    let cursor = Some(ui.menu_cursor());
    match dialog_choices(payload) {
        Some(choices) => {
            for spans in markdown::plain_rows(
                &choices.message,
                width.saturating_sub(TEXT_COL).max(1),
                theme.text(),
            ) {
                let mut line = Line::default();
                push_span(&mut line, TEXT_COL, "", theme.text());
                line.spans.extend(spans);
                panel.body.push(line);
            }
            let mut rows: Vec<(&str, Option<&str>)> = choices
                .options
                .iter()
                .map(|option| (option.label.as_str(), option.description.as_deref()))
                .collect();
            rows.push((CANCEL_LABEL, None));
            panel
                .actions
                .extend(action_lines(&rows, cursor, width, theme));
            panel.hinted(
                &format!(
                    "1-{}/↑↓ select · enter confirm · esc back (never answers)",
                    rows.len()
                ),
                quit_guard_armed,
                theme,
            )
        }
        None => {
            panel.body.extend(failure_line(
                "This request cannot be answered from the chat.",
                width,
                theme,
            ));
            let mut line = Line::default();
            push_span(
                &mut line,
                TEXT_COL,
                format!(
                    "kind {dialog_kind} · payload: {}",
                    dialog_payload_summary(payload)
                ),
                theme.muted(),
            );
            panel.body.push(line);
            panel
                .actions
                .extend(action_lines(&[(CANCEL_LABEL, None)], cursor, width, theme));
            panel.hinted(
                "enter confirm · esc back (never answers)",
                quit_guard_armed,
                theme,
            )
        }
    }
}

/// Cancel says what it does to the agent, wherever it appears.
const CANCEL_LABEL: &str = "Cancel — the agent is told the dialog was dismissed";

// --- read-only fact panels (F1) ----------------------------------------------

/// The read-only ask fact panel: what the agent is asking, the identical
/// preview, and the honest wait — read affordances only, no action row.
pub(crate) fn readonly_ask_panel(
    ask: &SharedAsk<'_>,
    ask_count: usize,
    width: usize,
    theme: Theme,
) -> AskPanel {
    let (title, read_hint) = match &ask.kind {
        SharedAskKind::Question { questions } => {
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
        SharedAskKind::Plan { .. } => (
            "the agent is asking for plan approval".to_string(),
            Some("f read document"),
        ),
        SharedAskKind::Permission { .. } if ask.is_plan() => (
            "the agent is asking for plan approval".to_string(),
            Some("f read document"),
        ),
        SharedAskKind::Elicitation {
            server, message, ..
        } => (
            match server {
                Some(server) => format!("{server} is asking — {message}"),
                None => format!("a server is asking — {message}"),
            },
            None,
        ),
        SharedAskKind::Dialog {
            dialog_kind,
            payload,
        } => (
            match dialog_choices(payload) {
                Some(choices) => format!("the agent is asking — {}", choices.message),
                None => format!("the agent raised a {dialog_kind} dialog"),
            },
            None,
        ),
        SharedAskKind::Permission { .. } => (
            format!("the agent is asking permission — {}", ask_identity(ask)),
            ask.document().map(|_| "f read document"),
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

// --- painting ----------------------------------------------------------------

/// One docked ask, painted: its diff document leads the body through the
/// shared diff rows, then the ask's own words, answers and keys.
///
/// The parts and the paint are separate so a chat can retitle a panel
/// before it lands — a child's ask docked in its parent's chat says whose
/// it is in the title row.
pub(crate) fn paint(
    ask: &SharedAsk<'_>,
    mut parts: AskPanel,
    theme: Theme,
    width: usize,
) -> Vec<Line<'static>> {
    if let Some(AskDocument::Diff(document)) = ask.document() {
        let body_width = blocks::panel_body_width(width);
        let preview =
            crate::chat::diff::paint_rows(&document.document.rows(), theme, body_width, 0, true)
                .into_preview(diff::PREVIEW_BUDGET);
        let mut body = preview.lines;
        if preview.hidden > 0 {
            body.push(remainder_row(preview.hidden, "f full document", theme));
        }
        body.append(&mut parts.body);
        parts.body = body;
    }
    paint_ask_panel(
        BlockKey(ask.id),
        &parts.title,
        parts.body,
        parts.actions,
        &parts.hints,
        theme,
        width,
    )
    .lines
}

/// `⋮ +K more lines · f full document` — a preview always states its own
/// arithmetic and names what shows the rest.
fn remainder_row(hidden: usize, affordance: &str, theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(
        &mut line,
        blocks::TEXT_COL - 2,
        format!("⋮ +{hidden} more lines · {affordance}"),
        theme.muted(),
    );
    line
}
