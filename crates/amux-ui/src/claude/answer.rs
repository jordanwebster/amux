//! The answer seam: typed answers in, refusals the client can state
//! without guessing (`docs/CHAT.md` §The keystroke seam).
//!
//! A Claude PTY input leaves this process as a semantic intent — a
//! prompt, an interrupt, a permission-mode cycle, or an answer to a named
//! ask. The keystrokes that carry it are chosen by the daemon from a
//! keymap resolved against the session's observed Claude version; no
//! client in this workspace authors a Claude key byte.
//!
//! What stays here is the part a client can decide alone: whether the
//! answer the user assembled fits the ask in front of them, and whether
//! its free text is safe to carry. Both are checked before dispatch so a
//! refusal reads as finished local state instead of a round trip, and so
//! a panel never offers an action the far side would reject.
//!
//! # The unverified menu shape
//!
//! Claude generates the permission menu from the hook payload's
//! `permission_suggestions`: `1. Yes`, one option per suggestion, then
//! `No` last. Only the exactly-one-suggestion shape has been confirmed
//! against a real Claude, so a menu with any other count is refused with
//! that fact stated rather than answered on a guess — the panel renders
//! read-only and the user attaches to the terminal instead.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

pub use amux::claude_io::{
    AskAnswer, PermissionAnswer, PlanAnswer, QuestionAnswer, QuestionResponse,
};

use super::{AskKind, QuestionFact, ToolInvocation};

/// Why an answer or prompt was not sent. Typed, and never serialized — a
/// refusal becomes the finished op's stated message, not Msg traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerRefusal {
    /// The answer kind does not fit the ask (or misses/doubles answers).
    AnswerMismatchesAsk { detail: String },
    /// Empty where content is required (prompt text, request-changes
    /// feedback, an unanswered question).
    EmptyText { what: &'static str },
    /// Menu text fields are single-line; a newline would submit early.
    MultilineUnsupported { what: &'static str },
    /// Control bytes in free text are refused whole: an escape byte
    /// inside injected text could terminate the wrapper the daemon types
    /// it in and run the remainder as live keystrokes in the remote
    /// session. Rejection over neutralization: stripping would claim
    /// reassembly knowledge no capture confirms.
    ControlBytesUnsupported { what: &'static str },
    /// A menu shape outside the live-verified shapes.
    UnverifiedMenuShape { detail: String },
}

impl std::fmt::Display for AnswerRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerRefusal::AnswerMismatchesAsk { detail } => {
                write!(f, "answer does not fit the ask: {detail}")
            }
            AnswerRefusal::EmptyText { what } => write!(f, "{what} must not be empty"),
            AnswerRefusal::MultilineUnsupported { what } => {
                write!(f, "{what} must be a single line")
            }
            AnswerRefusal::ControlBytesUnsupported { what } => {
                write!(
                    f,
                    "{what} must not contain control characters — an escape byte could break \
                     out of the injected input and run as live keystrokes"
                )
            }
            AnswerRefusal::UnverifiedMenuShape { detail } => {
                write!(
                    f,
                    "unverified menu shape — refusing to guess keystrokes: {detail}"
                )
            }
        }
    }
}

/// The one verified permission-menu shape: exactly one
/// `permission_suggestions` entry (`1. Yes` · the suggestion · `No`).
fn unverified_permission_menu(suggestion_count: usize) -> Option<AnswerRefusal> {
    (suggestion_count != 1).then(|| AnswerRefusal::UnverifiedMenuShape {
        detail: format!(
            "permission menu with {suggestion_count} suggestions (verified for exactly 1)"
        ),
    })
}

/// Whether an ask's remote menu shape is outside the live-verified shapes
/// — the panel's read-only gate (C2): a panel must not offer actions the
/// far side would refuse, so unverified shapes render read-only-style
/// with this typed refusal stated. `None` for every answerable shape
/// (plan menus are fixed three-way; question navigation is arrow-driven).
pub fn menu_shape_refusal(kind: &AskKind) -> Option<AnswerRefusal> {
    match kind {
        AskKind::Permission {
            invocation: ToolInvocation::Plan { .. },
            ..
        } => None,
        AskKind::Permission { suggestions, .. } => unverified_permission_menu(suggestions.len()),
        AskKind::Question { .. } => None,
    }
}

/// Normalized prompt text: CRLF and lone CR become LF, so the bytes the
/// daemon injects equal the transcript row's content (B1's reconciliation
/// key).
pub fn normalize_prompt(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Free text carried inside an injected program may contain printable
/// characters and (where the caller allows multiline) `\n` — nothing
/// else. Every other control byte is refused: an escape could terminate
/// the paste wrapper mid-text (injection into the remote session), and no
/// capture verifies how Claude's input handling treats any of them (a
/// normalized byte would desync the content-equality reconciliation).
fn reject_control(text: &str, what: &'static str) -> Result<(), AnswerRefusal> {
    if text.chars().any(|c| c.is_control() && c != '\n') {
        return Err(AnswerRefusal::ControlBytesUnsupported { what });
    }
    Ok(())
}

/// Free text destined for a menu field (Other editor, plan feedback):
/// single-line, and control-byte-free — the daemon types it raw, so a
/// newline would submit the field early and an escape would navigate the
/// menu.
fn menu_text<'t>(text: &'t str, what: &'static str) -> Result<&'t str, AnswerRefusal> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AnswerRefusal::EmptyText { what });
    }
    if text.contains('\n') || text.contains('\r') {
        return Err(AnswerRefusal::MultilineUnsupported { what });
    }
    reject_control(text, what)?;
    Ok(text)
}

/// A prompt submission is sendable when it carries content and nothing
/// that could break out of the paste the daemon wraps it in.
pub fn check_prompt(text: &str) -> Result<(), AnswerRefusal> {
    let text = normalize_prompt(text);
    if text.trim().is_empty() {
        return Err(AnswerRefusal::EmptyText { what: "prompt" });
    }
    reject_control(&text, "prompt")
}

/// Whether one answer fits one ask: routes on the ask's typed kind, so an
/// answer that does not fit the ask is a stated refusal, not a request
/// the daemon has to reject.
pub fn check_answer(kind: &AskKind, answer: &AskAnswer) -> Result<(), AnswerRefusal> {
    match (kind, answer) {
        (
            AskKind::Permission {
                invocation: ToolInvocation::Plan { .. },
                ..
            },
            AskAnswer::Plan(plan),
        ) => check_plan(plan),
        (
            AskKind::Permission {
                invocation: ToolInvocation::Plan { .. },
                ..
            },
            _,
        ) => Err(AnswerRefusal::AnswerMismatchesAsk {
            detail: "a plan-review ask takes a plan answer".to_string(),
        }),
        (AskKind::Permission { suggestions, .. }, AskAnswer::Permission(permission)) => {
            check_permission(suggestions.len(), permission)
        }
        (AskKind::Question { questions }, AskAnswer::Question(response)) => {
            check_question(questions, &response.answers)
        }
        _ => Err(AnswerRefusal::AnswerMismatchesAsk {
            detail: "answer kind does not match the ask kind".to_string(),
        }),
    }
}

fn check_permission(
    suggestion_count: usize,
    answer: &PermissionAnswer,
) -> Result<(), AnswerRefusal> {
    if let Some(refusal) = unverified_permission_menu(suggestion_count) {
        return Err(refusal);
    }
    match answer {
        PermissionAnswer::AllowOnce => Ok(()),
        PermissionAnswer::AllowScoped { suggestion } => {
            if *suggestion >= suggestion_count {
                return Err(AnswerRefusal::AnswerMismatchesAsk {
                    detail: format!(
                        "suggestion {suggestion} chosen on a menu with {suggestion_count} \
                         suggestions"
                    ),
                });
            }
            Ok(())
        }
        PermissionAnswer::Deny { feedback } => {
            // Deny takes optional feedback, which rides as a follow-up
            // prompt: empty is a plain deny, text is paste-wrapped.
            let feedback = feedback.as_deref().map(str::trim).unwrap_or_default();
            if feedback.is_empty() {
                return Ok(());
            }
            reject_control(&normalize_prompt(feedback), "deny feedback")
        }
    }
}

fn check_plan(answer: &PlanAnswer) -> Result<(), AnswerRefusal> {
    match answer {
        PlanAnswer::ApproveAuto | PlanAnswer::ApproveManual => Ok(()),
        // C3: request-changes will not submit empty.
        PlanAnswer::RequestChanges { feedback } => {
            menu_text(feedback, "request-changes feedback").map(|_| ())
        }
    }
}

/// Every question of the form must be answered, and each answer must fit
/// the question it answers: a single-select takes exactly one option or
/// an Other, a multi-select takes at least one of either, and no
/// selection may point past the question's options.
fn check_question(
    questions: &[QuestionFact],
    answers: &[QuestionAnswer],
) -> Result<(), AnswerRefusal> {
    if questions.is_empty() {
        return Err(AnswerRefusal::AnswerMismatchesAsk {
            detail: "the ask carries no questions".to_string(),
        });
    }
    if questions.len() != answers.len() {
        return Err(AnswerRefusal::AnswerMismatchesAsk {
            detail: format!(
                "{} questions, {} responses — every question must be answered",
                questions.len(),
                answers.len()
            ),
        });
    }
    for (question, answer) in questions.iter().zip(answers) {
        if question.multi_select {
            check_multi_select(question, answer)?;
        } else {
            check_single_select(question, answer)?;
        }
    }
    Ok(())
}

fn out_of_range(selected: usize, question: &QuestionFact) -> AnswerRefusal {
    AnswerRefusal::AnswerMismatchesAsk {
        detail: format!(
            "option {} selected on a question with {} options",
            selected,
            question.options.len()
        ),
    }
}

fn check_single_select(
    question: &QuestionFact,
    answer: &QuestionAnswer,
) -> Result<(), AnswerRefusal> {
    match (answer.selected.as_slice(), answer.other.as_deref()) {
        ([selected], None) => {
            if *selected >= question.options.len() {
                return Err(out_of_range(*selected, question));
            }
            Ok(())
        }
        ([], Some(other)) => menu_text(other, "the Other answer").map(|_| ()),
        ([], None) => Err(AnswerRefusal::EmptyText {
            what: "a single-select answer",
        }),
        _ => Err(AnswerRefusal::AnswerMismatchesAsk {
            detail: "a single-select question takes exactly one selection or an Other".to_string(),
        }),
    }
}

fn check_multi_select(
    question: &QuestionFact,
    answer: &QuestionAnswer,
) -> Result<(), AnswerRefusal> {
    if answer.selected.is_empty() && answer.other.is_none() {
        return Err(AnswerRefusal::EmptyText {
            what: "a multi-select answer",
        });
    }
    if let Some(beyond) = answer
        .selected
        .iter()
        .copied()
        .find(|selected| *selected >= question.options.len())
    {
        return Err(out_of_range(beyond, question));
    }
    if let Some(other) = answer.other.as_deref() {
        menu_text(other, "the Other answer")?;
    }
    Ok(())
}

/// The refusals a client states on its own. Each case is the one a panel
/// must not offer or a text field must not send; the keystrokes that
/// would carry an accepted answer are the daemon's to choose.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::{QuestionOption, SuggestionFact};

    fn question(multi: bool, options: &[&str]) -> QuestionFact {
        QuestionFact {
            header: Some("H".to_string()),
            question: Some("Q?".to_string()),
            multi_select: multi,
            options: options
                .iter()
                .map(|label| QuestionOption {
                    label: label.to_string(),
                    description: None,
                })
                .collect(),
        }
    }

    fn permission_kind(suggestions: usize) -> AskKind {
        AskKind::Permission {
            tool_name: Some("Bash".to_string()),
            invocation: ToolInvocation::Bash {
                command: Some("echo hi".to_string()),
                description: None,
            },
            suggestions: (0..suggestions)
                .map(|_| SuggestionFact::default())
                .collect(),
        }
    }

    fn plan_kind() -> AskKind {
        AskKind::Permission {
            tool_name: Some("ExitPlanMode".to_string()),
            invocation: ToolInvocation::Plan {
                plan: Some("# plan".to_string()),
                plan_file_path: None,
            },
            suggestions: Vec::new(),
        }
    }

    fn question_answer(selected: &[usize], other: Option<&str>) -> AskAnswer {
        AskAnswer::Question(QuestionResponse {
            answers: vec![QuestionAnswer {
                selected: selected.to_vec(),
                other: other.map(str::to_string),
            }],
        })
    }

    #[test]
    fn a_prompt_needs_content() {
        assert!(check_prompt("hello\nworld").is_ok());
        assert!(matches!(
            check_prompt("   "),
            Err(AnswerRefusal::EmptyText { what: "prompt" })
        ));
    }

    /// The in-band-delimiter class: free text that could break out of the
    /// wrapper the session types it in refuses whole, on every path.
    ///
    /// The hostile byte here is a bell, not the escape that motivates the
    /// rule: a guard test forbids escape literals anywhere under this
    /// directory, so no reader can mistake one for a key table. The rule
    /// is a class test — `is_control` — so the two are the same case, and
    /// the escape-terminator scenario itself is driven end to end from the
    /// client spec suite.
    #[test]
    fn control_bytes_in_free_text_refuse_on_every_path() {
        const HOSTILE: &str = "\u{7}";
        assert!(matches!(
            check_prompt(&format!("evil{HOSTILE}rest")),
            Err(AnswerRefusal::ControlBytesUnsupported { what: "prompt" })
        ));
        assert!(matches!(
            check_answer(
                &permission_kind(1),
                &AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some(format!("no{HOSTILE}stop")),
                })
            ),
            Err(AnswerRefusal::ControlBytesUnsupported {
                what: "deny feedback"
            })
        ));
        let single = AskKind::Question {
            questions: vec![question(false, &["Red", "Blue"])],
        };
        assert!(matches!(
            check_answer(
                &single,
                &question_answer(&[], Some(&format!("ochre{HOSTILE}")))
            ),
            Err(AnswerRefusal::ControlBytesUnsupported { .. })
        ));
        let multi = AskKind::Question {
            questions: vec![question(true, &["Hammer", "Saw"])],
        };
        assert!(matches!(
            check_answer(
                &multi,
                &question_answer(&[0], Some(&format!("wrench{HOSTILE}")))
            ),
            Err(AnswerRefusal::ControlBytesUnsupported { .. })
        ));
        assert!(matches!(
            check_answer(
                &plan_kind(),
                &AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: format!("scope it{HOSTILE}down"),
                })
            ),
            Err(AnswerRefusal::ControlBytesUnsupported { .. })
        ));
    }

    /// Menu fields submit on Enter, so a newline in one would send half
    /// the text; the refusal names the field.
    #[test]
    fn menu_text_fields_stay_single_line_and_non_empty() {
        assert!(matches!(
            check_answer(
                &plan_kind(),
                &AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "first\nsecond".to_string(),
                })
            ),
            Err(AnswerRefusal::MultilineUnsupported {
                what: "request-changes feedback"
            })
        ));
        assert!(matches!(
            check_answer(
                &plan_kind(),
                &AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "  ".to_string(),
                })
            ),
            Err(AnswerRefusal::EmptyText {
                what: "request-changes feedback"
            })
        ));
    }

    /// Only the one-suggestion permission menu has been seen live; every
    /// other count refuses, and the panel gate reports the same fact.
    #[test]
    fn only_the_one_suggestion_permission_menu_is_answerable() {
        assert!(
            check_answer(
                &permission_kind(1),
                &AskAnswer::Permission(PermissionAnswer::AllowOnce)
            )
            .is_ok()
        );
        assert!(menu_shape_refusal(&permission_kind(1)).is_none());
        for count in [0, 2, 3] {
            assert!(matches!(
                menu_shape_refusal(&permission_kind(count)),
                Some(AnswerRefusal::UnverifiedMenuShape { .. })
            ));
            assert!(matches!(
                check_answer(
                    &permission_kind(count),
                    &AskAnswer::Permission(PermissionAnswer::AllowOnce)
                ),
                Err(AnswerRefusal::UnverifiedMenuShape { .. })
            ));
        }
        // A plan review is a fixed three-way menu, never suggestion-shaped.
        assert!(menu_shape_refusal(&plan_kind()).is_none());
    }

    #[test]
    fn an_answer_must_fit_the_ask_it_answers() {
        assert!(matches!(
            check_answer(
                &plan_kind(),
                &AskAnswer::Permission(PermissionAnswer::AllowOnce)
            ),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            check_answer(
                &permission_kind(1),
                &AskAnswer::Plan(PlanAnswer::ApproveAuto)
            ),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            check_answer(
                &permission_kind(1),
                &AskAnswer::Permission(PermissionAnswer::AllowScoped { suggestion: 4 })
            ),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
    }

    /// A question form is answered whole: one response per question, each
    /// pointing at options the question actually has.
    #[test]
    fn question_forms_are_answered_whole_and_in_range() {
        let form = AskKind::Question {
            questions: vec![
                question(false, &["Red", "Blue"]),
                question(true, &["A", "B"]),
            ],
        };
        assert!(matches!(
            check_answer(&form, &question_answer(&[0], None)),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
        let single = AskKind::Question {
            questions: vec![question(false, &["Red", "Blue"])],
        };
        assert!(check_answer(&single, &question_answer(&[1], None)).is_ok());
        assert!(check_answer(&single, &question_answer(&[], Some("ochre"))).is_ok());
        assert!(matches!(
            check_answer(&single, &question_answer(&[7], None)),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            check_answer(&single, &question_answer(&[0, 1], None)),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            check_answer(&single, &question_answer(&[], None)),
            Err(AnswerRefusal::EmptyText {
                what: "a single-select answer"
            })
        ));
        let multi = AskKind::Question {
            questions: vec![question(true, &["Hammer", "Saw"])],
        };
        assert!(check_answer(&multi, &question_answer(&[0, 1], None)).is_ok());
        assert!(check_answer(&multi, &question_answer(&[], Some("wrench"))).is_ok());
        assert!(matches!(
            check_answer(&multi, &question_answer(&[], None)),
            Err(AnswerRefusal::EmptyText {
                what: "a multi-select answer"
            })
        ));
        assert!(matches!(
            check_answer(&multi, &question_answer(&[0, 5], None)),
            Err(AnswerRefusal::AnswerMismatchesAsk { .. })
        ));
    }
}
