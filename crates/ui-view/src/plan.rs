//! Plans: agent text that proposes work and waits on the person's decision.
//!
//! Claude writes its plan to a plan file with its ordinary Write tool, then
//! calls ExitPlanMode, whose ask carries the plan. The plan file's Write is
//! the plan while it is being written (headless Claude streams it) and until
//! the ExitPlanMode call arrives; from then on that call's row is the plan
//! and the Write draws nothing. Edits to the plan file never draw.
//!
//! Codex has no plan item on the wire yet, nor any plan approval: a
//! Plan-mode turn's plan arrives as an ordinary message. Until the
//! interpreter tells plans apart, a Codex message wrapped in
//! `<proposed_plan>` tags (the form Codex's model writes them in) is read as
//! a plan, and the decision Codex's own terminal offers is composed here
//! from inputs that exist: see [`plan_inputs`].

use ui_state::{Held, ItemBody, ItemClass, SessionState};
use wire::Kind;

use crate::ask::{
    Answer, AskBody, AskCard, CardState, Choice, ChoiceOutcome, PlanStep, plan_choice,
};
use crate::rows::{AskRow, PlanVerdict, RowKind};
use crate::settings::{SettingChange, codex_preset, setting_input};

/// The words Codex sends as the next turn when its plan is approved.
pub const IMPLEMENT_PLAN: &str = "Implement the plan.";

/// What a fresh conversation is seeded with, before the plan itself.
pub const FRESH_PLAN_PREFIX: &str =
    "A plan was written and approved in an earlier conversation. Implement it:";

const OPEN_TAG: &str = "<proposed_plan>";
const CLOSE_TAG: &str = "</proposed_plan>";

/// A path under Claude's plans directory.
pub(crate) fn is_plan_file(path: &str) -> bool {
    path.contains("/.claude/plans/") || path.starts_with(".claude/plans/")
}

/// A Codex message's plan, its tags taken off; None when the message is not
/// a plan. A closing tag still streaming in is taken off too.
pub(crate) fn codex_plan(text: &str) -> Option<String> {
    let body = text.trim_start().strip_prefix(OPEN_TAG)?;
    let body = body.trim_end();
    let closing = |line: &str| line.starts_with("</") && CLOSE_TAG.starts_with(line.trim());
    let body = match body.rsplit_once('\n') {
        Some((rest, last)) if closing(last) => rest,
        None if closing(body) => "",
        _ => body,
    };
    Some(body.trim_matches('\n').trim_end().to_owned())
}

/// What happened after a plan: the next prompt's words, or a fresh session
/// begun before any prompt. Turn ends and hidden items between are passed
/// over; anything else means nothing was decided by what follows.
#[derive(Debug, PartialEq)]
pub(crate) enum After {
    Nothing,
    Prompt(String),
    Fresh,
}

pub(crate) fn after(state: &SessionState, order: u64) -> After {
    let transcript = state.transcript();
    let Some(head) = transcript.head() else {
        return After::Nothing;
    };
    if order >= head {
        return After::Nothing;
    }
    for held in transcript.range(order + 1..=head) {
        match &held.class {
            ItemClass::Turn | ItemClass::Thinking { .. } | ItemClass::Other => continue,
            ItemClass::Boundary => return After::Fresh,
            ItemClass::Prompt | ItemClass::Steer => return After::Prompt(held.item.text.clone()),
            _ => return After::Nothing,
        }
    }
    After::Nothing
}

/// Whether an ExitPlanMode call comes after the plan file's Write at
/// `order`, before the next prompt.
fn exit_follows(state: &SessionState, order: u64) -> bool {
    let transcript = state.transcript();
    let Some(head) = transcript.head() else {
        return false;
    };
    if order >= head {
        return false;
    }
    for held in transcript.range(order + 1..=head) {
        if matches!(held.class, ItemClass::Prompt) {
            return false;
        }
        if tool_name(held) == Some("ExitPlanMode") {
            return true;
        }
    }
    false
}

fn tool_name(held: &Held) -> Option<&str> {
    match &held.body {
        ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
        | ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool)) => Some(tool.name.as_str()),
        _ => None,
    }
}

/// The plan file's newest Write before `order`, for an ExitPlanMode call
/// that carries no plan of its own.
pub(crate) fn written_before(state: &SessionState, order: u64) -> String {
    let transcript = state.transcript();
    let Some(oldest) = transcript.oldest_held() else {
        return String::new();
    };
    if order <= oldest {
        return String::new();
    }
    for held in transcript.range(oldest..=order - 1).rev() {
        if matches!(held.class, ItemClass::Prompt) {
            break;
        }
        if let ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
        | ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool)) = &held.body
            && tool.name == "Write"
        {
            let input: serde_json::Value =
                serde_json::from_slice(&tool.input_json).unwrap_or_default();
            let path = input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if is_plan_file(path) {
                return input
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
            }
        }
    }
    String::new()
}

/// A Claude tool call on the plan file: the plan while it is written, or
/// nothing once ExitPlanMode carries it. None for any other call.
pub(crate) fn plan_file_row(
    state: &SessionState,
    held: &Held,
    name: &str,
    path: &str,
    content: String,
    writing: bool,
) -> Option<RowKind> {
    if !is_plan_file(path) {
        return None;
    }
    if name != "Write" || exit_follows(state, held.item.order) {
        return Some(RowKind::Hidden);
    }
    Some(RowKind::Ask(AskRow::Plan {
        plan: content,
        verdict: PlanVerdict::Open,
        edits_accepted: false,
        started_fresh: false,
        note: None,
        writing,
    }))
}

/// A Codex plan's row: decided by what the person sent next.
pub(crate) fn codex_plan_row(
    state: &SessionState,
    held: &Held,
    plan: String,
    complete: bool,
) -> RowKind {
    let (verdict, fresh) = match after(state, held.item.order) {
        After::Nothing => (PlanVerdict::Open, false),
        After::Fresh => (PlanVerdict::Approved, true),
        After::Prompt(text) if text.trim() == IMPLEMENT_PLAN => (PlanVerdict::Approved, false),
        After::Prompt(_) => (PlanVerdict::SentBack, false),
    };
    RowKind::Ask(AskRow::Plan {
        plan,
        verdict,
        edits_accepted: false,
        started_fresh: fresh,
        note: None,
        writing: !complete,
    })
}

/// The decision on a Codex plan, as an ask card: the newest plan of a turn
/// that has ended, while nothing after it has decided it.
pub(crate) fn codex_plan_card(state: &SessionState) -> Option<AskCard> {
    if state.kind() != Kind::Codex || state.agent_state().active_turn.is_some() {
        return None;
    }
    let transcript = state.transcript();
    let head = transcript.head()?;
    let oldest = transcript.oldest_held()?;
    for held in transcript.range(oldest..=head).rev() {
        match &held.body {
            ItemBody::Codex(wire::codex_item::Kind::Message(text)) => {
                let plan = codex_plan(&held.item.text)?;
                if !text.complete || after(state, held.item.order) != After::Nothing {
                    return None;
                }
                let item_key = held.item.key.clone();
                return Some(AskCard {
                    kind: Kind::Codex,
                    key: format!("plan-{item_key}"),
                    item_key,
                    position: 1,
                    count: 1,
                    body: AskBody::Plan { plan },
                    choices: vec![
                        Choice {
                            primary: true,
                            ..plan_choice(
                                ChoiceOutcome::ApprovePlan {
                                    auto_accept_edits: false,
                                },
                                PlanStep::Implement,
                            )
                        },
                        plan_choice(ChoiceOutcome::StartFresh, PlanStep::StartFresh),
                        plan_choice(ChoiceOutcome::SendBack, PlanStep::KeepPlanning),
                    ],
                    question_note: true,
                    state: CardState::Open,
                });
            }
            ItemBody::Codex(
                wire::codex_item::Kind::Prompt(_) | wire::codex_item::Kind::Steer(_),
            ) => return None,
            _ => {}
        }
    }
    None
}

/// The inputs a plan decision is made of, where the agent takes no answer
/// for it: approving a Codex plan leaves Plan mode and asks it to implement
/// the plan; keeping on planning sends the note, if any, as the next
/// message; starting fresh leaves Plan mode and begins a new conversation
/// seeded with the plan (Claude's is cleared first). Each input's id is the
/// sender's to fill.
pub fn plan_inputs(card: &AskCard, step: PlanStep, note: &str) -> Vec<wire::Input> {
    let AskBody::Plan { plan } = &card.body else {
        return Vec::new();
    };
    let prompt = |text: String| prompt_input(card.kind, text);
    let leave_plan = || {
        (card.kind == Kind::Codex)
            .then(|| codex_preset("auto"))
            .flatten()
            .and_then(|mode| setting_input(card.kind, &SettingChange::Mode(mode)))
    };
    let mut inputs = Vec::new();
    match step {
        PlanStep::Implement => {
            inputs.extend(leave_plan());
            inputs.extend(prompt(IMPLEMENT_PLAN.to_owned()));
        }
        PlanStep::KeepPlanning => {
            let note = note.trim();
            if !note.is_empty() {
                inputs.extend(prompt(note.to_owned()));
            }
        }
        PlanStep::StartFresh => {
            inputs.extend(leave_plan());
            inputs.extend(clear_input(card.kind));
            inputs.extend(prompt(format!("{FRESH_PLAN_PREFIX}\n\n{plan}")));
        }
    }
    inputs
}

fn prompt_input(kind: Kind, text: String) -> Option<wire::Input> {
    use wire::{claude_pty_input, claude_sdk_input, codex_input, input};
    let prompt = wire::PromptInput {
        text,
        attachments: Vec::new(),
    };
    let of = match kind {
        Kind::ClaudePty => input::Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(claude_pty_input::Of::Prompt(prompt)),
        }),
        Kind::ClaudeSdk => input::Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Prompt(prompt)),
        }),
        Kind::Codex => input::Of::Codex(wire::CodexInput {
            of: Some(codex_input::Of::Prompt(prompt)),
        }),
        Kind::Unspecified => return None,
    };
    Some(wire::Input {
        input_id: Vec::new(),
        of: Some(of),
    })
}

/// A new conversation for Claude; Codex has no such input yet (a new
/// thread), so its fresh start is only the seeded message.
fn clear_input(kind: Kind) -> Option<wire::Input> {
    use wire::{claude_pty_input, claude_sdk_input, input};
    let of = match kind {
        Kind::ClaudePty => input::Of::ClaudePty(wire::ClaudePtyInput {
            of: Some(claude_pty_input::Of::Clear(wire::Clear {})),
        }),
        Kind::ClaudeSdk => input::Of::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Clear(wire::Clear {})),
        }),
        _ => return None,
    };
    Some(wire::Input {
        input_id: Vec::new(),
        of: Some(of),
    })
}

/// Whether `answer` is composed here rather than answered by the agent.
pub fn composed(answer: &Answer) -> Option<PlanStep> {
    match answer {
        Answer::Plan(step) => Some(*step),
        _ => None,
    }
}
