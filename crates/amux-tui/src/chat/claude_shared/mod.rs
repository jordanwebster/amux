//! The surfaces both Claude chats draw: the ask panels, the ask-panel
//! stage machine, the fullscreen reader, the review draft and Claude's
//! document chrome.
//!
//! Claude is one provider behind two transports, so an `Edit` permission,
//! an `AskUserQuestion` form and an `ExitPlanMode` plan carry the same
//! facts however they arrive. Those facts live in `amux_ui::claude::facts`;
//! this module is their one presentation. Nothing here folds rows, holds
//! session state, or knows which transport produced an ask — each chat
//! builds a [`SharedAsk`] from its own layer and encodes the answer its
//! own way.

pub(crate) mod ask_ui;
pub(crate) mod diff;
pub(crate) mod draft;
pub(crate) mod panel;
pub(crate) mod reader;

use amux_ui::claude::{AskDocument, QuestionFact, SuggestionFact, ToolInvocation};
use ratatui::text::{Line, Span};

use crate::chat::blocks;
use crate::render::{Theme, push_span, str_width};
use crate::view::QuitGuard;

/// The armed quit guard's replacement hint row (`docs/CHAT.md`
/// §Keybindings): it replaces the hint line — wherever that line lives —
/// in warning color.
pub(crate) fn armed_quit_line(theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    push_span(&mut line, blocks::TEXT_COL, QuitGuard::HINT, theme.warn());
    line
}

/// One pending ask as the shared panels and reader read it: the provider
/// facts, borrowed from whichever layer folded them, plus the two things
/// only the chat around it knows — the handle its panel state is keyed by
/// and whether this transport can carry an answer at all.
#[derive(Clone, Debug)]
pub(crate) struct SharedAsk<'m> {
    /// The chat's own stable handle for the ask; panel state is keyed by
    /// it and a new head gets a fresh panel.
    pub id: u64,
    pub kind: SharedAskKind<'m>,
    /// The ask-time document the reader opens: an estimated diff, a
    /// proposed new file. Plans travel on the invocation instead.
    pub document: Option<&'m AskDocument>,
    pub state: SharedAskState<'m>,
    /// Why this ask cannot be answered from here, when it cannot be: the
    /// panel states the reason read-only rather than offering actions the
    /// transport beneath would refuse.
    pub refusal: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum SharedAskKind<'m> {
    /// Permission for one tool use. Plan review is this kind carrying a
    /// [`ToolInvocation::Plan`], because that is how Claude asks for it.
    Permission {
        tool_name: Option<&'m str>,
        invocation: &'m ToolInvocation,
        /// The request's own permission suggestions: Claude's menu is
        /// generated from them, so the scoped option's label is too.
        suggestions: &'m [SuggestionFact],
    },
    /// `AskUserQuestion`.
    Question { questions: &'m [QuestionFact] },
}

/// Where the ask stands, in the terms the panel renders.
#[derive(Clone, Debug)]
pub(crate) enum SharedAskState<'m> {
    Pending,
    /// An answer is in flight: the panel collapses to a dim marker
    /// summarizing what was chosen until the session confirms it.
    Answered {
        summary: AnswerSummary,
    },
    /// The answer never left. The ask is back with the failure stated.
    Failed {
        message: &'m str,
    },
}

/// What an in-flight answer was, in the words the collapsed marker uses.
/// Each chat maps its own answer type onto this; the panel only reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AnswerSummary {
    AllowedOnce,
    AllowedScoped,
    Denied,
    PlanApprovedAuto,
    PlanApprovedManual,
    ChangesRequested,
    QuestionAnswered,
}

impl AnswerSummary {
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            AnswerSummary::Denied | AnswerSummary::ChangesRequested => "✗",
            AnswerSummary::QuestionAnswered => "?",
            _ => "✔",
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            AnswerSummary::AllowedOnce => "allowed once",
            AnswerSummary::AllowedScoped => "allowed (scoped)",
            AnswerSummary::Denied => "denied",
            AnswerSummary::PlanApprovedAuto => "plan approved (auto)",
            AnswerSummary::PlanApprovedManual => "plan approved (manual)",
            AnswerSummary::ChangesRequested => "changes requested",
            AnswerSummary::QuestionAnswered => "answered",
        }
    }
}

impl SharedAsk<'_> {
    /// Is this the plan-review variant? Claude asks for plan approval
    /// through `ExitPlanMode`, so the invocation is the discriminator.
    pub(crate) fn is_plan(&self) -> bool {
        matches!(
            &self.kind,
            SharedAskKind::Permission {
                invocation: ToolInvocation::Plan { .. },
                ..
            }
        )
    }

    /// The plan markdown, when this ask carries one.
    pub(crate) fn plan(&self) -> Option<&str> {
        match &self.kind {
            SharedAskKind::Permission {
                invocation: ToolInvocation::Plan { plan, .. },
                ..
            } => plan.as_deref(),
            _ => None,
        }
    }

    /// Whether the ask carries anything the reader can show (`f`'s
    /// liveness: hints never advertise dead keys).
    pub(crate) fn has_readable(&self) -> bool {
        self.document.is_some() || self.plan().is_some()
    }

    pub(crate) fn is_pending(&self) -> bool {
        matches!(self.state, SharedAskState::Pending)
    }

    /// The file this ask is about, when it is about one.
    pub(crate) fn path(&self) -> Option<&str> {
        match &self.kind {
            SharedAskKind::Permission {
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
}

/// The `?` overlay: the chat's full effective key list with tier
/// annotations, from the one binding table (`crate::bindings`) — kitty
/// rows appear only when probed, ext rows are marked terminal-dependent.
/// Fullscreen like the reader; any key closes. On short viewports the
/// tail gives way and a `⋮` row states the cut honestly.
pub(crate) fn help_overlay(
    sections: Vec<crate::bindings::Section>,
    quit_guard_armed: bool,
    theme: Theme,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    // One aligned action column across every section.
    let key_col = blocks::TEXT_COL
        + 2
        + sections
            .iter()
            .flat_map(|section| &section.bindings)
            .map(|binding| str_width(&binding.keys))
            .max()
            .unwrap_or(0)
        + 3;

    let mut rows: Vec<Line<'static>> = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            rows.push(Line::default());
        }
        let mut title = Line::default();
        push_span(&mut title, blocks::GLYPH_COL, section.title, theme.muted());
        rows.push(title);
        for binding in &section.bindings {
            let mut line = Line::default();
            push_span(
                &mut line,
                blocks::TEXT_COL + 2,
                binding.keys.clone(),
                theme.text(),
            );
            push_span(&mut line, key_col, binding.action.clone(), theme.muted());
            if let Some(mark) = crate::render::tier_mark(binding.tier) {
                line.spans
                    .push(Span::styled(format!(" · {mark}"), theme.muted()));
            }
            rows.push(line);
        }
    }

    // Fixed chrome is five rows: the title, the gap under it, two rules
    // and the hint. The body consumes every remaining viewport row.
    let body_h = height.saturating_sub(5).max(1);
    if rows.len() > body_h {
        rows.truncate(body_h.saturating_sub(1));
        let mut more = Line::default();
        push_span(
            &mut more,
            blocks::GLYPH_COL,
            "⋮ more — a taller terminal shows the full list",
            theme.muted(),
        );
        rows.push(more);
    }
    while rows.len() < body_h {
        rows.push(Line::default());
    }

    let mut title = Line::default();
    push_span(&mut title, blocks::GLYPH_COL, "keys", theme.emphasis());
    let hint = if quit_guard_armed {
        armed_quit_line(theme)
    } else {
        let mut line = Line::default();
        push_span(
            &mut line,
            blocks::TEXT_COL,
            "any key to close",
            theme.muted(),
        );
        line
    };

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    lines.push(title);
    lines.push(Line::default());
    lines.push(reader::rule_line(width, theme));
    lines.extend(rows);
    lines.push(reader::rule_line(width, theme));
    lines.push(hint);
    lines.truncate(height);
    lines
}
