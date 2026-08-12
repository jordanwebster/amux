//! The keystroke seam (C6, `docs/CHAT.md` §The keystroke seam): typed
//! intents in, byte programs out.
//!
//! Every answer, prompt submission, and interrupt becomes a keystroke
//! program — an encoded byte sequence injected into the session PTY under
//! the seq guard. ALL Claude keystroke encodings in the workspace live in
//! this module: views dispatch typed Commands, the reducer calls these
//! encoders, and the runtime shell maps the returned [`KeyStep`]s onto the
//! `claude_pty_transcript_v1` input actions mechanically. Nothing else may
//! author Claude key bytes. This module is also the seam for a future
//! native structured-input protocol: the typed surface stays, the PTY
//! byte tables below are what gets replaced.
//!
//! # Verification provenance
//!
//! Every table below was confirmed against a REAL claude by the capture
//! harness (`crates/amux/tests/capture/`), **claude 2.1.228, model haiku,
//! 2026-08-11/12**; the committed fixtures under
//! `crates/amux/tests/fixtures/chat-v1/` carry the runs (scenario names
//! cited per table; findings recorded in
//! `notes/chat-v1/transcript-semantics.md` §18a/§18d). Per the phase
//! rule, an intent whose byte sequence was NOT confirmed live returns
//! [`EncodingError::UnverifiedMenuShape`] — never a guessed sequence.
//!
//! | intent | program | verified by |
//! |---|---|---|
//! | prompt submit | bracketed paste (`ESC[200~ text ESC[201~`) + delay + CR | `prompt_multiline` (row content byte-identical, newline included); plain-text form in every Phase 0 scenario |
//! | permission allow once | `1` | `permission` (Phase 0), `mode_cycle` |
//! | permission allow + scope option | `2` — menus with exactly ONE `permission_suggestions` entry | `permission_session` |
//! | permission deny (+ optional feedback) | `3` (= 2 + suggestion count, count 1); feedback pasted as a follow-up prompt in the same program | `permission_deny_feedback` (one-program form) |
//! | question, single-select | digit SELECTS and auto-advances; single-question forms submit on selection, multi-question forms end on a review step where one CR submits | `question_single` (Phase 0), `question_tabs` |
//! | question, Other on single-select | digit(options+1) opens the editor; text + CR commits | `question_other_single` |
//! | question, multi-select | `↓`/Space toggles; Other: CR opens, text, CR saves, **Space checks the box**; Tab advances to the next tab | `question_multi` (Phase 0 + re-verify), `question_mixed` |
//! | question, submit via Tab arrival | CR (review list) + CR (confirm) | `question_multi` |
//! | plan approve — auto | `1` | `plan_auto` (H.5) |
//! | plan approve — manual | `2` | `plan_approve` (Phase 0) |
//! | plan request changes | `3` + feedback text + CR (feedback field; turn continues) | `plan_reject` (Phase 0) |
//! | interrupt | `ESC` | `interrupt` (Phase 0) |
//! | permission-mode cycle | `ESC[Z` (CSI Z); order default → acceptEdits → plan → default | `mode_cycle` |
//!
//! Menu timing: claude's TUI drops keys that race a dialog render, so
//! multi-step programs carry the inter-key delays the verified runs used
//! (the daemon clamps any single delay to 5 s). Programs assume the menu
//! is already on screen — true whenever a human answers a rendered panel.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

use serde::{Deserialize, Serialize};

use super::{AskKind, QuestionFact, ToolInvocation};

/// One step of a keystroke program. The shell maps these onto the
/// `claude_pty_transcript_v1` input actions verbatim (`Write` → write
/// bytes, `Delay` → server-side pause).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum KeyStep {
    Write { text: String },
    Delay { ms: u32 },
}

fn write(text: impl Into<String>) -> KeyStep {
    KeyStep::Write { text: text.into() }
}

fn delay(ms: u32) -> KeyStep {
    KeyStep::Delay { ms }
}

// --- the byte vocabulary (the ONLY place these bytes exist) -----------------

const ESC: &str = "\u{1b}";
const CR: &str = "\r";
const TAB: &str = "\t";
const SPACE: &str = " ";
const ARROW_DOWN: &str = "\u{1b}[B";
const SHIFT_TAB: &str = "\u{1b}[Z";
const PASTE_BEGIN: &str = "\u{1b}[200~";
const PASTE_END: &str = "\u{1b}[201~";

// Inter-key delays, in ms, matching the verified capture programs.
const AFTER_PASTE: u32 = 400;
const AFTER_TOGGLE: u32 = 400;
const AFTER_MOVE: u32 = 300;
const AFTER_TAB: u32 = 800;
const AFTER_EDITOR_OPEN: u32 = 800;
const BEFORE_SUBMIT: u32 = 400;
const AFTER_REVIEW_OPEN: u32 = 1000;
/// Deny flushes the denial + interrupt artifacts and refocuses the
/// composer before the feedback prompt can be typed
/// (`permission_deny_feedback`, one-program form).
const AFTER_DENY: u32 = 1500;
const AFTER_MENU_TEXT_OPEN: u32 = 800;

/// A typed answer to an ask — the payload of `Command::AnswerAsk`, and the
/// pending-marker data the optimistic ask state retains (C5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum AskAnswer {
    Permission(PermissionAnswer),
    Plan(PlanAnswer),
    Question { responses: Vec<QuestionResponse> },
}

/// Permission-menu outcomes (C2). The menu claude renders is generated
/// from the hook payload's `permission_suggestions`: `1. Yes`, one option
/// per suggestion, then `No` last (§18d) — so the scoped-allow and deny
/// digits depend on the suggestion count carried by the ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "permission", rename_all = "snake_case")]
pub enum PermissionAnswer {
    AllowOnce,
    /// Allow AND apply the menu's scope option (e.g. "always allow access
    /// to <dir> from this project" for an `addDirectories` suggestion).
    /// Verified for the one-suggestion menu only.
    AllowScoped,
    /// Deny. Claude's permission menu denies immediately (typed denial +
    /// interrupt artifacts — no feedback field, §18d); optional feedback
    /// rides the same program as a follow-up prompt.
    Deny {
        feedback: Option<String>,
    },
}

/// Plan-review outcomes (C3). The `ExitPlanMode` menu is fixed three-way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum PlanAnswer {
    ApproveAuto,
    ApproveManual,
    /// Feedback required (C3: will not submit empty). Lands in the
    /// rejection row's `userFeedback`; the turn continues.
    RequestChanges {
        feedback: String,
    },
}

/// One question's answer within a question form (C4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionResponse {
    /// Selected predefined options, 0-based into the ask's `options`.
    /// Single-select: exactly one entry (or none with `other`).
    pub selected: Vec<usize>,
    /// Free text for the appended `Other` option.
    pub other: Option<String>,
}

/// Why an intent could not be encoded. Typed, never a guessed byte
/// sequence: `UnverifiedMenuShape` is the honest refusal for menu shapes
/// no capture has confirmed. Never serialized — a refusal becomes the
/// finished op's stated message, not Msg traffic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodingError {
    /// The answer kind does not fit the ask (or misses/doubles answers).
    AnswerMismatchesAsk { detail: String },
    /// Empty where content is required (prompt text, request-changes
    /// feedback, an unanswered question).
    EmptyText { what: &'static str },
    /// Menu text fields are single-line; a newline would submit early.
    MultilineUnsupported { what: &'static str },
    /// Control bytes in free text are refused whole: a literal ESC could
    /// form the bracketed-paste terminator (`ESC[201~`) mid-text — the
    /// remainder would break out of the paste and run as live keystrokes
    /// in the remote session — and the transcript transparency of any
    /// control byte other than `\n` is unverified (a normalized byte
    /// would desync the echo). Rejection over neutralization: stripping
    /// would claim reassembly knowledge no capture confirms.
    ControlBytesUnsupported { what: &'static str },
    /// A menu shape outside the live-verified tables (§18d provenance).
    UnverifiedMenuShape { detail: String },
}

impl std::fmt::Display for EncodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodingError::AnswerMismatchesAsk { detail } => {
                write!(f, "answer does not fit the ask: {detail}")
            }
            EncodingError::EmptyText { what } => write!(f, "{what} must not be empty"),
            EncodingError::MultilineUnsupported { what } => {
                write!(f, "{what} must be a single line")
            }
            EncodingError::ControlBytesUnsupported { what } => {
                write!(
                    f,
                    "{what} must not contain control characters — an escape byte could break \
                     out of the injected input and run as live keystrokes"
                )
            }
            EncodingError::UnverifiedMenuShape { detail } => {
                write!(
                    f,
                    "unverified menu shape — refusing to guess keystrokes: {detail}"
                )
            }
        }
    }
}

/// Normalized prompt text: CRLF and lone CR become LF, so the injected
/// bytes equal the transcript row's content (B1's reconciliation key).
pub fn normalize_prompt(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Free text carried inside an injected program may contain printable
/// characters and (where the caller allows multiline) `\n` — nothing
/// else. Every other control byte is refused: ESC could form the
/// bracketed-paste terminator mid-text (injection into the remote
/// session), and no capture verifies how claude's input handling treats
/// any of them (a normalized byte would desync the content-equality
/// reconciliation). The verified transparency claim is printable + `\n`,
/// exactly (`prompt_multiline`).
fn reject_control(text: &str, what: &'static str) -> Result<(), EncodingError> {
    if text.chars().any(|c| c.is_control() && c != '\n') {
        return Err(EncodingError::ControlBytesUnsupported { what });
    }
    Ok(())
}

/// The paste-wrapped form of free text, validated so the wrapper cannot
/// be broken from inside (every paste in this module goes through here).
fn paste_block(text: &str, what: &'static str) -> Result<String, EncodingError> {
    reject_control(text, what)?;
    Ok(format!("{PASTE_BEGIN}{text}{PASTE_END}"))
}

/// A prompt submission: bracketed paste + CR. Paste keeps the text
/// literal — `/`, `!`, `@`, `#` prefixes and embedded newlines never
/// trigger claude's composer grammar — and the transcript user row's
/// string content lands byte-identical to the pasted text
/// (`prompt_multiline`).
pub fn prompt_program(text: &str) -> Result<Vec<KeyStep>, EncodingError> {
    let text = normalize_prompt(text);
    if text.trim().is_empty() {
        return Err(EncodingError::EmptyText { what: "prompt" });
    }
    Ok(vec![
        write(paste_block(&text, "prompt")?),
        delay(AFTER_PASTE),
        write(CR),
    ])
}

/// Interrupt: ESC. Cuts a generating message (null-`stop_reason` flush +
/// interrupt row) or rejects an open menu — either way the turn closes
/// with the §17 artifacts (`interrupt`, Phase 0).
pub fn interrupt_program() -> Vec<KeyStep> {
    vec![write(ESC)]
}

/// Permission-mode cycle: Shift+Tab (CSI Z). One press per step; the
/// cycle emits NO `permission-mode` row — the effective mode surfaces in
/// hook payloads (`mode_cycle`; the D4 verdict).
pub fn mode_cycle_program() -> Vec<KeyStep> {
    vec![write(SHIFT_TAB)]
}

/// The answer program for one ask: routes on the ask's typed kind, so an
/// answer that does not fit the ask is a typed error, not a byte guess.
pub fn answer_program(kind: &AskKind, answer: &AskAnswer) -> Result<Vec<KeyStep>, EncodingError> {
    match (kind, answer) {
        (
            AskKind::Permission {
                invocation: ToolInvocation::Plan { .. },
                ..
            },
            AskAnswer::Plan(plan),
        ) => plan_program(plan),
        (
            AskKind::Permission {
                invocation: ToolInvocation::Plan { .. },
                ..
            },
            _,
        ) => Err(EncodingError::AnswerMismatchesAsk {
            detail: "a plan-review ask takes a plan answer".to_string(),
        }),
        (AskKind::Permission { suggestions, .. }, AskAnswer::Permission(permission)) => {
            permission_program(suggestions.len(), permission)
        }
        (AskKind::Question { questions }, AskAnswer::Question { responses }) => {
            question_program(questions, responses)
        }
        _ => Err(EncodingError::AnswerMismatchesAsk {
            detail: "answer kind does not match the ask kind".to_string(),
        }),
    }
}

/// The permission menu (`permission_session` / `permission_deny_feedback`):
/// `1. Yes` · one option per suggestion · `No` last. Only the
/// one-suggestion shape has been observed live; other counts refuse.
fn permission_program(
    suggestion_count: usize,
    answer: &PermissionAnswer,
) -> Result<Vec<KeyStep>, EncodingError> {
    let verified = suggestion_count == 1;
    let unverified = |what: &str| EncodingError::UnverifiedMenuShape {
        detail: format!(
            "{what} on a permission menu with {suggestion_count} suggestions \
             (verified for exactly 1)"
        ),
    };
    match answer {
        PermissionAnswer::AllowOnce => {
            if !verified {
                return Err(unverified("allow once"));
            }
            Ok(vec![write("1")])
        }
        PermissionAnswer::AllowScoped => {
            if !verified {
                return Err(unverified("scoped allow"));
            }
            Ok(vec![write("2")])
        }
        PermissionAnswer::Deny { feedback } => {
            if !verified {
                return Err(unverified("deny"));
            }
            // Deny is the last digit: 1 (Yes) + suggestions + No.
            let mut program = vec![write("3")];
            let feedback = feedback.as_deref().map(str::trim).unwrap_or_default();
            if !feedback.is_empty() {
                // The denial flushes immediately; the feedback follows as
                // a prompt in the same program (one-program form verified).
                program.push(delay(AFTER_DENY));
                program.push(write(paste_block(
                    &normalize_prompt(feedback),
                    "deny feedback",
                )?));
                program.push(delay(AFTER_PASTE));
                program.push(write(CR));
            }
            Ok(program)
        }
    }
}

/// The plan-review menu (`plan_auto` / `plan_approve` / `plan_reject`):
/// `1. Yes, auto-accept edits` · `2. Yes, manually approve edits` ·
/// `3. Tell Claude what to change` (opens a feedback field).
fn plan_program(answer: &PlanAnswer) -> Result<Vec<KeyStep>, EncodingError> {
    match answer {
        PlanAnswer::ApproveAuto => Ok(vec![write("1")]),
        PlanAnswer::ApproveManual => Ok(vec![write("2")]),
        PlanAnswer::RequestChanges { feedback } => {
            let feedback = menu_text(feedback, "request-changes feedback")?;
            Ok(vec![
                write("3"),
                delay(AFTER_MENU_TEXT_OPEN),
                write(feedback),
                delay(BEFORE_SUBMIT),
                write(CR),
            ])
        }
    }
}

/// The question form (`question_tabs` / `question_other_single` /
/// `question_multi` / `question_mixed`). Claude's form appends an `Other`
/// ("Type something") row after the predefined options (and a "Chat about
/// this" row after that — unmodeled here). Navigation facts:
///
/// - single-select: a DIGIT selects the option and auto-advances to the
///   next tab; the Other digit opens an inline editor, text + CR commits.
/// - multi-select: ↓ moves, Space toggles; Other: CR opens, text, CR
///   saves, Space checks the box; Tab advances to the next tab.
/// - a single-question single-select form submits on selection; every
///   other shape ends on the review step — one CR when the last question
///   auto-advanced there, CR (review list) + CR (confirm) when it was
///   reached by Tab (C4's mandatory-submit rule, implemented by claude
///   itself).
fn question_program(
    questions: &[QuestionFact],
    responses: &[QuestionResponse],
) -> Result<Vec<KeyStep>, EncodingError> {
    if questions.is_empty() {
        return Err(EncodingError::AnswerMismatchesAsk {
            detail: "the ask carries no questions".to_string(),
        });
    }
    if questions.len() != responses.len() {
        return Err(EncodingError::AnswerMismatchesAsk {
            detail: format!(
                "{} questions, {} responses — every question must be answered",
                questions.len(),
                responses.len()
            ),
        });
    }

    let mut program = Vec::new();
    for (index, (question, response)) in questions.iter().zip(responses).enumerate() {
        if index > 0 {
            // The previous question advanced onto this tab (digit
            // auto-advance or Tab); give the form a beat to move.
            program.push(delay(AFTER_TAB));
        }
        if question.multi_select {
            encode_multi_select(&mut program, question, response)?;
        } else {
            encode_single_select(&mut program, question, response)?;
        }
    }

    let last_multi = questions
        .last()
        .is_some_and(|question| question.multi_select);
    let single_question_single_select = questions.len() == 1 && !last_multi;
    if single_question_single_select {
        // Selection submitted the form; no review step exists.
        return Ok(program);
    }
    if last_multi {
        // The trailing Tab of the multi-select block landed on the Submit
        // tab: CR opens the review list, CR confirms.
        program.push(delay(AFTER_TAB));
        program.push(write(CR));
        program.push(delay(AFTER_REVIEW_OPEN));
        program.push(write(CR));
    } else {
        // The last digit auto-advanced onto the review confirm.
        program.push(delay(AFTER_TAB));
        program.push(write(CR));
    }
    Ok(program)
}

/// The digit for a 1-based menu position; positions past 9 have no
/// verified selector.
fn digit(position: usize) -> Result<KeyStep, EncodingError> {
    if !(1..=9).contains(&position) {
        return Err(EncodingError::UnverifiedMenuShape {
            detail: format!("menu position {position} is beyond the digit row"),
        });
    }
    Ok(write(position.to_string()))
}

/// Free text typed RAW into a menu field (Other editor, plan feedback):
/// single-line, and control-byte-free — an ESC here would navigate the
/// menu instead of typing (the same in-band-injection class the paste
/// wrapper guards against).
fn menu_text<'t>(text: &'t str, what: &'static str) -> Result<&'t str, EncodingError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(EncodingError::EmptyText { what });
    }
    if text.contains('\n') || text.contains('\r') {
        return Err(EncodingError::MultilineUnsupported { what });
    }
    reject_control(text, what)?;
    Ok(text)
}

fn encode_single_select(
    program: &mut Vec<KeyStep>,
    question: &QuestionFact,
    response: &QuestionResponse,
) -> Result<(), EncodingError> {
    match (response.selected.as_slice(), response.other.as_deref()) {
        ([selected], None) => {
            if *selected >= question.options.len() {
                return Err(EncodingError::AnswerMismatchesAsk {
                    detail: format!(
                        "option {} selected on a question with {} options",
                        selected,
                        question.options.len()
                    ),
                });
            }
            program.push(digit(selected + 1)?);
        }
        ([], Some(other)) => {
            let other = menu_text(other, "the Other answer")?;
            // The appended Other row sits right after the predefined
            // options; its digit opens the inline editor.
            program.push(digit(question.options.len() + 1)?);
            program.push(delay(AFTER_EDITOR_OPEN));
            program.push(write(other));
            program.push(delay(BEFORE_SUBMIT));
            program.push(write(CR));
        }
        ([], None) => {
            return Err(EncodingError::EmptyText {
                what: "a single-select answer",
            });
        }
        _ => {
            return Err(EncodingError::AnswerMismatchesAsk {
                detail: "a single-select question takes exactly one selection or an Other"
                    .to_string(),
            });
        }
    }
    Ok(())
}

fn encode_multi_select(
    program: &mut Vec<KeyStep>,
    question: &QuestionFact,
    response: &QuestionResponse,
) -> Result<(), EncodingError> {
    if response.selected.is_empty() && response.other.is_none() {
        return Err(EncodingError::EmptyText {
            what: "a multi-select answer",
        });
    }
    let mut selected: Vec<usize> = response.selected.clone();
    selected.sort_unstable();
    selected.dedup();
    if selected
        .last()
        .is_some_and(|last| *last >= question.options.len())
    {
        return Err(EncodingError::AnswerMismatchesAsk {
            detail: format!(
                "option {} selected on a question with {} options",
                selected.last().expect("non-empty"),
                question.options.len()
            ),
        });
    }

    // The cursor starts on the first option; ↓ moves it, Space toggles.
    let mut cursor = 0usize;
    let mut move_to = |program: &mut Vec<KeyStep>, row: usize| {
        while cursor < row {
            program.push(write(ARROW_DOWN));
            program.push(delay(AFTER_MOVE));
            cursor += 1;
        }
    };
    for row in selected {
        move_to(program, row);
        program.push(write(SPACE));
        program.push(delay(AFTER_TOGGLE));
    }
    if let Some(other) = response.other.as_deref() {
        let other = menu_text(other, "the Other answer")?;
        // The Other row sits after the predefined options: CR opens the
        // editor, CR saves the text, and the trailing Space CHECKS the
        // box — without it the value is silently dropped (§18a).
        move_to(program, question.options.len());
        program.push(write(CR));
        program.push(delay(AFTER_EDITOR_OPEN));
        program.push(write(other));
        program.push(delay(BEFORE_SUBMIT));
        program.push(write(CR));
        program.push(delay(AFTER_TOGGLE + 200));
        program.push(write(SPACE));
        program.push(delay(AFTER_TOGGLE));
    }
    // Advance off the multi-select tab (Space never advances): Tab lands
    // on the next question tab, or the Submit tab when this was the last.
    program.push(write(TAB));
    Ok(())
}

/// The tables asserted against the byte programs the live captures
/// verified (fixture provenance in the module docs). Renumbering a menu,
/// changing a delay, or touching the paste wrapper fails here first.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::{QuestionOption, SuggestionFact};

    fn writes(program: &[KeyStep]) -> Vec<&str> {
        program
            .iter()
            .filter_map(|step| match step {
                KeyStep::Write { text } => Some(text.as_str()),
                KeyStep::Delay { .. } => None,
            })
            .collect()
    }

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

    #[test]
    fn a_prompt_is_bracketed_paste_then_enter() {
        let program = prompt_program("hello\r\nworld").expect("encodes");
        assert_eq!(
            program,
            vec![
                write("\u{1b}[200~hello\nworld\u{1b}[201~"),
                delay(400),
                write("\r"),
            ],
            "paste keeps the text literal and CRLF normalizes to LF"
        );
        assert!(matches!(
            prompt_program("   "),
            Err(EncodingError::EmptyText { .. })
        ));
    }

    /// The in-band-delimiter class: free text that could break out of its
    /// wrapper refuses whole — a literal `ESC[201~` inside a paste would
    /// terminate it early and run the remainder as live keystrokes, and
    /// an ESC in a raw menu field would navigate the menu. Every
    /// free-text path takes the same refusal.
    #[test]
    fn control_bytes_in_free_text_refuse_on_every_path() {
        // The paste wrapper: prompt and deny-feedback.
        assert!(matches!(
            prompt_program("evil\u{1b}[201~\u{1b}[Zrest"),
            Err(EncodingError::ControlBytesUnsupported { what: "prompt" })
        ));
        assert!(matches!(
            answer_program(
                &permission_kind(1),
                &AskAnswer::Permission(PermissionAnswer::Deny {
                    feedback: Some("no\u{1b}[201~\u{1b}stop".to_string()),
                })
            ),
            Err(EncodingError::ControlBytesUnsupported {
                what: "deny feedback"
            })
        ));
        // Raw menu fields: Other text (single- and multi-select) and the
        // plan request-changes feedback.
        let single = AskKind::Question {
            questions: vec![question(false, &["Red", "Blue"])],
        };
        assert!(matches!(
            answer_program(
                &single,
                &AskAnswer::Question {
                    responses: vec![QuestionResponse {
                        selected: vec![],
                        other: Some("ochre\u{1b}".to_string()),
                    }]
                }
            ),
            Err(EncodingError::ControlBytesUnsupported { .. })
        ));
        let multi = AskKind::Question {
            questions: vec![question(true, &["Hammer", "Saw"])],
        };
        assert!(matches!(
            answer_program(
                &multi,
                &AskAnswer::Question {
                    responses: vec![QuestionResponse {
                        selected: vec![0],
                        other: Some("wrench\u{1b}".to_string()),
                    }]
                }
            ),
            Err(EncodingError::ControlBytesUnsupported { .. })
        ));
        assert!(matches!(
            answer_program(
                &plan_kind(),
                &AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "fix\u{1b}[201~it".to_string()
                })
            ),
            Err(EncodingError::ControlBytesUnsupported { .. })
        ));
        // Tabs are control bytes too: only printable + `\n` is the
        // verified transparency claim.
        assert!(matches!(
            prompt_program("a\tb"),
            Err(EncodingError::ControlBytesUnsupported { .. })
        ));
    }

    #[test]
    fn interrupt_and_mode_cycle_are_single_keys() {
        assert_eq!(interrupt_program(), vec![write("\u{1b}")]);
        assert_eq!(mode_cycle_program(), vec![write("\u{1b}[Z")]);
    }

    #[test]
    fn the_permission_menu_digits_follow_the_suggestion_count() {
        let kind = permission_kind(1);
        let allow = answer_program(&kind, &AskAnswer::Permission(PermissionAnswer::AllowOnce));
        assert_eq!(allow.expect("encodes"), vec![write("1")]);
        let scoped = answer_program(&kind, &AskAnswer::Permission(PermissionAnswer::AllowScoped));
        assert_eq!(scoped.expect("encodes"), vec![write("2")]);
        let deny = answer_program(
            &kind,
            &AskAnswer::Permission(PermissionAnswer::Deny { feedback: None }),
        );
        assert_eq!(deny.expect("encodes"), vec![write("3")]);
    }

    #[test]
    fn deny_feedback_rides_the_same_program_as_a_pasted_prompt() {
        let program = answer_program(
            &permission_kind(1),
            &AskAnswer::Permission(PermissionAnswer::Deny {
                feedback: Some("try the other file".to_string()),
            }),
        )
        .expect("encodes");
        assert_eq!(
            program,
            vec![
                write("3"),
                delay(AFTER_DENY),
                write("\u{1b}[200~try the other file\u{1b}[201~"),
                delay(AFTER_PASTE),
                write("\r"),
            ]
        );
        // Whitespace-only feedback is a plain deny (C2: empty = plain).
        let plain = answer_program(
            &permission_kind(1),
            &AskAnswer::Permission(PermissionAnswer::Deny {
                feedback: Some("  ".to_string()),
            }),
        );
        assert_eq!(plain.expect("encodes"), vec![write("3")]);
    }

    #[test]
    fn unobserved_permission_menu_shapes_refuse_rather_than_guess() {
        for suggestions in [0, 2] {
            let kind = permission_kind(suggestions);
            for answer in [
                PermissionAnswer::AllowOnce,
                PermissionAnswer::AllowScoped,
                PermissionAnswer::Deny { feedback: None },
            ] {
                assert!(
                    matches!(
                        answer_program(&kind, &AskAnswer::Permission(answer.clone())),
                        Err(EncodingError::UnverifiedMenuShape { .. })
                    ),
                    "{answer:?} on a {suggestions}-suggestion menu must refuse"
                );
            }
        }
    }

    #[test]
    fn the_plan_menu_is_fixed_three_way() {
        let kind = plan_kind();
        let auto = answer_program(&kind, &AskAnswer::Plan(PlanAnswer::ApproveAuto));
        assert_eq!(auto.expect("encodes"), vec![write("1")]);
        let manual = answer_program(&kind, &AskAnswer::Plan(PlanAnswer::ApproveManual));
        assert_eq!(manual.expect("encodes"), vec![write("2")]);
        let changes = answer_program(
            &kind,
            &AskAnswer::Plan(PlanAnswer::RequestChanges {
                feedback: "document VALUE too".to_string(),
            }),
        );
        assert_eq!(
            changes.expect("encodes"),
            vec![
                write("3"),
                delay(AFTER_MENU_TEXT_OPEN),
                write("document VALUE too"),
                delay(BEFORE_SUBMIT),
                write("\r"),
            ]
        );
        // C3: request-changes will not submit empty; the field is one line.
        assert!(matches!(
            answer_program(
                &kind,
                &AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: " ".to_string()
                })
            ),
            Err(EncodingError::EmptyText { .. })
        ));
        assert!(matches!(
            answer_program(
                &kind,
                &AskAnswer::Plan(PlanAnswer::RequestChanges {
                    feedback: "a\nb".to_string()
                })
            ),
            Err(EncodingError::MultilineUnsupported { .. })
        ));
    }

    #[test]
    fn a_single_question_single_select_submits_on_its_digit() {
        let kind = AskKind::Question {
            questions: vec![question(false, &["Red", "Blue"])],
        };
        let program = answer_program(
            &kind,
            &AskAnswer::Question {
                responses: vec![QuestionResponse {
                    selected: vec![0],
                    other: None,
                }],
            },
        );
        assert_eq!(program.expect("encodes"), vec![write("1")]);
    }

    #[test]
    fn the_single_select_other_types_into_the_inline_editor() {
        let kind = AskKind::Question {
            questions: vec![question(false, &["Red", "Blue"])],
        };
        let program = answer_program(
            &kind,
            &AskAnswer::Question {
                responses: vec![QuestionResponse {
                    selected: vec![],
                    other: Some("a warm ochre".to_string()),
                }],
            },
        )
        .expect("encodes");
        assert_eq!(
            writes(&program),
            vec!["3", "a warm ochre", "\r"],
            "digit(options+1) opens the editor; text + CR commits"
        );
    }

    #[test]
    fn a_two_question_form_ends_with_the_review_enter() {
        let kind = AskKind::Question {
            questions: vec![
                question(false, &["Red", "Blue"]),
                question(false, &["Small", "Large"]),
            ],
        };
        let program = answer_program(
            &kind,
            &AskAnswer::Question {
                responses: vec![
                    QuestionResponse {
                        selected: vec![0],
                        other: None,
                    },
                    QuestionResponse {
                        selected: vec![1],
                        other: None,
                    },
                ],
            },
        )
        .expect("encodes");
        assert_eq!(
            writes(&program),
            vec!["1", "2", "\r"],
            "digits auto-advance; one Enter confirms the review"
        );
    }

    #[test]
    fn a_multi_select_toggles_commits_the_other_and_tabs_to_submit() {
        let kind = AskKind::Question {
            questions: vec![question(true, &["Hammer", "Saw", "Drill"])],
        };
        let program = answer_program(
            &kind,
            &AskAnswer::Question {
                responses: vec![QuestionResponse {
                    selected: vec![0, 1],
                    other: Some("a torque wrench".to_string()),
                }],
            },
        )
        .expect("encodes");
        assert_eq!(
            writes(&program),
            vec![
                " ",        // toggle Hammer
                "\u{1b}[B", // down to Saw
                " ",        // toggle Saw
                "\u{1b}[B", // down to Drill
                "\u{1b}[B", // down to the Other row
                "\r",       // open the editor
                "a torque wrench",
                "\r", // save the text
                " ",  // the trailing Space CHECKS the box (§18a)
                "\t", // Tab to the Submit tab
                "\r", // review list
                "\r", // confirm
            ]
        );
    }

    #[test]
    fn a_mixed_form_tabs_off_the_multi_select_then_digit_advances() {
        let kind = AskKind::Question {
            questions: vec![
                question(true, &["Hammer", "Saw", "Drill"]),
                question(false, &["Small", "Large"]),
            ],
        };
        let program = answer_program(
            &kind,
            &AskAnswer::Question {
                responses: vec![
                    QuestionResponse {
                        selected: vec![0, 1],
                        other: None,
                    },
                    QuestionResponse {
                        selected: vec![1],
                        other: None,
                    },
                ],
            },
        )
        .expect("encodes");
        assert_eq!(
            writes(&program),
            vec![" ", "\u{1b}[B", " ", "\t", "2", "\r"],
            "the question_mixed capture's program exactly"
        );
    }

    #[test]
    fn misfitting_answers_are_typed_errors_never_bytes() {
        let question_kind = AskKind::Question {
            questions: vec![question(false, &["Red", "Blue"])],
        };
        assert!(matches!(
            answer_program(
                &question_kind,
                &AskAnswer::Permission(PermissionAnswer::AllowOnce)
            ),
            Err(EncodingError::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            answer_program(&question_kind, &AskAnswer::Question { responses: vec![] }),
            Err(EncodingError::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            answer_program(
                &question_kind,
                &AskAnswer::Question {
                    responses: vec![QuestionResponse {
                        selected: vec![5],
                        other: None
                    }]
                }
            ),
            Err(EncodingError::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            answer_program(
                &permission_kind(1),
                &AskAnswer::Plan(PlanAnswer::ApproveAuto)
            ),
            Err(EncodingError::AnswerMismatchesAsk { .. })
        ));
        assert!(matches!(
            answer_program(
                &plan_kind(),
                &AskAnswer::Permission(PermissionAnswer::AllowOnce)
            ),
            Err(EncodingError::AnswerMismatchesAsk { .. })
        ));
        // A menu position past the digit row has no verified selector.
        let wide = AskKind::Question {
            questions: vec![question(false, &["a"; 12])],
        };
        assert!(matches!(
            answer_program(
                &wide,
                &AskAnswer::Question {
                    responses: vec![QuestionResponse {
                        selected: vec![10],
                        other: None
                    }]
                }
            ),
            Err(EncodingError::UnverifiedMenuShape { .. })
        ));
    }
}
