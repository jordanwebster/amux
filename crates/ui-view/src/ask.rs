//! The ask card: one anatomy for every kind. The head ask with its count,
//! the subject verbatim, the body variant, and choices stated as outcomes.

use prost::Message as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ui_state::{InputState, OpenAsk, SessionState};
use wire::{ClaudeAnswer, CodexAnswer, Decision as CodexDecision, ask, claude_answer, codex_ask};

use crate::rows::{AnswerView, patch_counts};

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct AskCard {
    pub kind: wire::Kind,
    /// What an answer names.
    pub key: String,
    /// The item the ask points at: the call it would let run, or the row of
    /// an ask that is the work.
    pub item_key: String,
    /// "1 of 3" when asks are queued.
    pub position: usize,
    pub count: usize,
    pub body: AskBody,
    /// The likely choice first. Only what this agent offers.
    pub choices: Vec<Choice>,
    /// A note may go out with a question's answers. Terminal Claude has no
    /// place to type one for most questions, so it takes none.
    pub question_note: bool,
    pub state: CardState,
}

/// Where the card is after the person acts. Stop is always in the menu:
/// it is the interrupt, and the agent stays.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum CardState {
    Open,
    /// Shrunk to one line until the agent confirms.
    Sending,
    /// The card is back, with the reason.
    Rejected(String),
    /// The connection dropped before a reply: resend or discard.
    NotConfirmed,
    /// The agent exited with the ask open.
    Dismissed,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub enum AskBody {
    Command {
        command: String,
        cwd: String,
        reason: String,
        description: String,
    },
    Edit {
        path: String,
        files: u32,
        added: u32,
        removed: u32,
        diff: String,
        reason: String,
        /// The file does not exist yet: "Wants to create".
        created: bool,
    },
    Tool {
        server: String,
        tool: String,
        arguments: String,
    },
    Question(Vec<QuestionView>),
    Plan {
        plan: String,
    },
    Form {
        server: String,
        message: String,
        schema_json: String,
    },
    Link {
        server: String,
        message: String,
        url: String,
    },
    Access {
        reason: String,
        read: Vec<String>,
        write: Vec<String>,
        network: bool,
        hosts: Vec<String>,
    },
    /// The provider asks something this build cannot read: the only ways out
    /// are Stop and attaching a terminal.
    Unanswerable {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct QuestionView {
    pub header: String,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<OptionView>,
    /// "Something else…" is always last when offered.
    pub allow_other: bool,
    /// Typed answers are hidden, and the row later says "answered (hidden)".
    pub secret: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OptionView {
    /// "(Recommended)" lifted out into `recommended`.
    pub label: String,
    pub description: String,
    pub preview: String,
    pub recommended: bool,
}

/// One choice, stated as what happens, with the answer it sends.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct Choice {
    pub outcome: ChoiceOutcome,
    pub primary: bool,
    /// The person may add a note that goes back to the agent.
    pub takes_note: bool,
    #[serde(skip)]
    pub answer: Answer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ChoiceOutcome {
    AllowOnce,
    /// "Always allow cargo test in this project": what, and for how long.
    AllowAlways {
        subjects: Vec<String>,
        directories: Vec<String>,
        mode: String,
        scope: Scope,
        label: String,
    },
    AllowForSession,
    /// Codex: the exact command prefix it would allow.
    AllowSimilar {
        prefix: Vec<String>,
    },
    AllowNetwork {
        hosts: Vec<String>,
    },
    /// Deny and let it carry on; `stops` when denying always ends the turn.
    Deny {
        stops: bool,
    },
    DenyAndStop,
    ApprovePlan {
        auto_accept_edits: bool,
    },
    SendBack,
    Submit,
    Decline,
    OpenLink,
    GrantForTurn,
    GrantForSession,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum Scope {
    Session,
    /// This project, for this person.
    Project,
    /// This project, shared with everyone who works on it.
    ProjectShared,
    /// Every project.
    User,
    Other(String),
}

/// The answer body a choice sends; [`answer_input`] puts it in an input.
/// It stays on this side of the phone bridge: the phone names a choice by
/// its position and the answer is rebuilt here.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Claude(ClaudeAnswer),
    CodexDecision(CodexDecision),
    Codex(CodexAnswer),
    /// Not an answer the agent takes: a plan decision made of other inputs,
    /// which [`crate::plan_inputs`] builds.
    Plan(PlanStep),
}

/// A plan decision the client makes of inputs the agent already takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum PlanStep {
    Implement,
    KeepPlanning,
}

/// A plan choice made of inputs; keeping on planning takes a note.
pub(crate) fn plan_choice(outcome: ChoiceOutcome, step: PlanStep) -> Choice {
    Choice {
        outcome,
        primary: false,
        takes_note: step == PlanStep::KeepPlanning,
        answer: Answer::Plan(step),
    }
}

/// The head ask, or None when nothing is open or, before CaughtUp, when the
/// entry does not say needs_you.
pub fn ask_card(state: &SessionState) -> Option<AskCard> {
    let asks = state.open_asks();
    let Some(head) = asks.first() else {
        return crate::plan::codex_plan_card(state);
    };
    let (body, choices) = match head {
        OpenAsk::Claude(ask) => claude(ask),
        OpenAsk::Codex(ask) => codex(ask),
    };
    let card_state = if state.asks_dismissed() {
        CardState::Dismissed
    } else {
        match state.answering(head.key()).map(|sent| &sent.state) {
            Some(InputState::Sent | InputState::Settled | InputState::Queued) => CardState::Sending,
            Some(InputState::Rejected(reason)) => CardState::Rejected(reason.clone()),
            Some(InputState::Uncertain) => CardState::NotConfirmed,
            None => CardState::Open,
        }
    };
    Some(AskCard {
        kind: state.kind(),
        key: head.key().to_owned(),
        item_key: head.item_key().to_owned(),
        position: 1,
        count: asks.len(),
        body,
        choices,
        question_note: state.kind() != wire::Kind::ClaudePty,
        state: card_state,
    })
}

fn choice(outcome: ChoiceOutcome, answer: Answer) -> Choice {
    // Only Claude's deny and plan send-back carry a note back to the agent.
    let takes_note = matches!(
        &answer,
        Answer::Claude(ClaudeAnswer {
            of: Some(
                claude_answer::Of::Permission(wire::PermissionAnswer {
                    of: Some(wire::permission_answer::Of::Deny(_))
                }) | claude_answer::Of::Plan(wire::PlanAnswer {
                    of: Some(wire::plan_answer::Of::SendBack(_))
                })
            )
        })
    );
    Choice {
        outcome,
        primary: false,
        takes_note,
        answer,
    }
}

fn claude_answer(of: claude_answer::Of) -> Answer {
    Answer::Claude(ClaudeAnswer { of: Some(of) })
}

fn permission(of: wire::permission_answer::Of) -> Answer {
    claude_answer(claude_answer::Of::Permission(wire::PermissionAnswer {
        of: Some(of),
    }))
}

fn claude(ask: &wire::Ask) -> (AskBody, Vec<Choice>) {
    use wire::permission_answer::Of as P;
    let (body, mut choices) = match &ask.body {
        Some(ask::Body::Permission(p)) => {
            let body = permission_body(p);
            let mut choices = vec![choice(
                ChoiceOutcome::AllowOnce,
                permission(P::Allow(wire::PermissionAllow { scope: None })),
            )];
            for scope in &p.scopes {
                choices.push(choice(
                    ChoiceOutcome::AllowAlways {
                        subjects: scope.rules.iter().map(|rule| lift_rule(rule)).collect(),
                        directories: scope.directories.clone(),
                        mode: scope.mode.clone(),
                        scope: scope_of(&scope.destination),
                        label: scope.label.clone(),
                    },
                    permission(P::Allow(wire::PermissionAllow {
                        scope: Some(scope.index),
                    })),
                ));
            }
            let deny = |stop| {
                permission(P::Deny(wire::PermissionDeny {
                    note: String::new(),
                    stop,
                }))
            };
            if p.deny_stops {
                choices.push(choice(ChoiceOutcome::Deny { stops: true }, deny(true)));
            } else {
                choices.push(choice(ChoiceOutcome::Deny { stops: false }, deny(false)));
                if p.deny_can_stop {
                    choices.push(choice(ChoiceOutcome::DenyAndStop, deny(true)));
                }
            }
            (body, choices)
        }
        Some(ask::Body::Question(q)) => (
            AskBody::Question(q.questions.iter().map(question).collect()),
            vec![],
        ),
        Some(ask::Body::Plan(plan)) => {
            let approve = |auto| {
                claude_answer(claude_answer::Of::Plan(wire::PlanAnswer {
                    of: Some(wire::plan_answer::Of::Approve(wire::PlanApprove {
                        auto_accept_edits: auto,
                    })),
                }))
            };
            let mut choices = vec![choice(
                ChoiceOutcome::ApprovePlan {
                    auto_accept_edits: false,
                },
                approve(false),
            )];
            if plan.offers_auto_accept {
                choices.push(choice(
                    ChoiceOutcome::ApprovePlan {
                        auto_accept_edits: true,
                    },
                    approve(true),
                ));
            }
            choices.push(choice(
                ChoiceOutcome::SendBack,
                claude_answer(claude_answer::Of::Plan(wire::PlanAnswer {
                    of: Some(wire::plan_answer::Of::SendBack(wire::PlanSendBack {
                        note: String::new(),
                    })),
                })),
            ));
            (
                AskBody::Plan {
                    plan: plan.plan.clone(),
                },
                choices,
            )
        }
        Some(ask::Body::Form(form)) => {
            let action = |action: wire::FormAction| {
                claude_answer(claude_answer::Of::Form(wire::FormAnswer {
                    action: action as i32,
                    content_json: vec![],
                }))
            };
            (
                form_body(form),
                vec![
                    choice(ChoiceOutcome::Submit, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(ask::Body::Link(link)) => {
            let action = |action: wire::FormAction| {
                claude_answer(claude_answer::Of::Link(wire::LinkAnswer {
                    action: action as i32,
                }))
            };
            (
                link_body(link),
                vec![
                    choice(ChoiceOutcome::OpenLink, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(ask::Body::Unanswerable(u)) => (
            AskBody::Unanswerable {
                reason: u.reason.clone(),
            },
            vec![],
        ),
        None => (
            AskBody::Unanswerable {
                reason: String::new(),
            },
            vec![],
        ),
    };
    if let Some(first) = choices.first_mut() {
        first.primary = true;
    }
    (body, choices)
}

fn permission_body(p: &wire::PermissionAsk) -> AskBody {
    let input: Value = serde_json::from_slice(&p.input_json).unwrap_or(Value::Null);
    let text = |name: &str| {
        input
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    if !p.server.is_empty() {
        return AskBody::Tool {
            server: p.server.clone(),
            tool: p.tool_name.clone(),
            arguments: pretty(&input),
        };
    }
    match p.tool_name.as_str() {
        "Bash" => AskBody::Command {
            command: text("command"),
            cwd: String::new(),
            reason: p.reason.clone(),
            description: if p.description.is_empty() {
                text("description")
            } else {
                p.description.clone()
            },
        },
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let path = [text("file_path"), text("notebook_path")]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default();
            let edits = input
                .get("edits")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| vec![input.clone()]);
            let mut diff = String::new();
            for edit in &edits {
                let field = |name: &str| {
                    edit.get(name)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                for line in field("old_string").lines() {
                    diff.push_str(&format!("-{line}\n"));
                }
                for line in field("new_string").lines().chain(field("content").lines()) {
                    diff.push_str(&format!("+{line}\n"));
                }
            }
            let (added, removed) = patch_counts(&diff);
            AskBody::Edit {
                path,
                files: 1,
                added,
                removed,
                diff,
                reason: p.reason.clone(),
                created: p.tool_name == "Write",
            }
        }
        _ => AskBody::Tool {
            server: String::new(),
            tool: p.tool_name.clone(),
            arguments: pretty(&input),
        },
    }
}

/// A tool called with no arguments shows no arguments block: null, `{}`
/// and `[]` all read as nothing.
fn pretty(value: &Value) -> String {
    let empty = match value {
        Value::Null => true,
        Value::Object(fields) => fields.is_empty(),
        Value::Array(items) => items.is_empty(),
        _ => false,
    };
    if empty {
        String::new()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }
}

fn form_body(form: &wire::FormAsk) -> AskBody {
    AskBody::Form {
        server: form.server.clone(),
        message: form.message.clone(),
        schema_json: String::from_utf8_lossy(&form.schema_json).into_owned(),
    }
}

fn link_body(link: &wire::LinkAsk) -> AskBody {
    AskBody::Link {
        server: link.server.clone(),
        message: link.message.clone(),
        url: link.url.clone(),
    }
}

/// "Bash(cargo test:*)" reads "cargo test"; a bare tool name stays.
fn lift_rule(rule: &str) -> String {
    let Some((_, inner)) = rule.split_once('(') else {
        return rule.to_owned();
    };
    let inner = inner.strip_suffix(')').unwrap_or(inner);
    let inner = inner
        .strip_suffix(":*")
        .or_else(|| inner.strip_suffix(" *"))
        .unwrap_or(inner);
    let inner = inner.strip_prefix("domain:").unwrap_or(inner);
    inner.to_owned()
}

fn scope_of(destination: &str) -> Scope {
    match destination {
        "session" => Scope::Session,
        "localSettings" => Scope::Project,
        "projectSettings" => Scope::ProjectShared,
        "userSettings" => Scope::User,
        other => Scope::Other(other.to_owned()),
    }
}

pub(crate) fn question(q: &wire::Question) -> QuestionView {
    QuestionView {
        header: q.header.clone(),
        question: q.question.clone(),
        multi_select: q.multi_select,
        options: q
            .options
            .iter()
            .map(|option| {
                lifted(
                    &option.label,
                    &option.description,
                    &option.preview,
                    option.recommended,
                )
            })
            .collect(),
        allow_other: q.allow_other,
        secret: q.secret,
    }
}

pub(crate) fn lifted(
    label: &str,
    description: &str,
    preview: &str,
    recommended: bool,
) -> OptionView {
    let trimmed = label.trim_end();
    let (label, tagged) = match trimmed.strip_suffix("(Recommended)") {
        Some(rest) => (rest.trim_end().to_owned(), true),
        None => (trimmed.to_owned(), false),
    };
    OptionView {
        label,
        description: description.to_owned(),
        preview: preview.to_owned(),
        recommended: recommended || tagged,
    }
}

/// What a Claude AskUserQuestion call's recorded result says was answered,
/// one per question in the ask's order. Both Claude kinds record `answers`
/// as each question's text to one string: the picked labels joined by ", "
/// (a label may carry the "(Recommended)" tag), then anything typed.
pub(crate) fn recorded_answers(questions: &[QuestionView], result: &[u8]) -> Vec<AnswerView> {
    let Ok(result) = serde_json::from_slice::<Value>(result) else {
        return Vec::new();
    };
    let Some(answers) = result.get("answers").and_then(Value::as_object) else {
        return Vec::new();
    };
    questions
        .iter()
        .map(|q| {
            let text = answers
                .get(&q.question)
                .and_then(Value::as_str)
                .unwrap_or_default();
            recorded_answer(q, text)
        })
        .collect()
}

fn recorded_answer(q: &QuestionView, text: &str) -> AnswerView {
    // Longest first, so a label that begins with another label wins.
    let mut labels: Vec<&str> = q
        .options
        .iter()
        .map(|option| option.label.as_str())
        .filter(|label| !label.is_empty())
        .collect();
    labels.sort_by_key(|label| std::cmp::Reverse(label.len()));
    let mut picked = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && (q.multi_select || picked.is_empty()) {
        let found = labels.iter().find_map(|label| {
            let after = rest.strip_prefix(*label)?;
            let after = after
                .strip_prefix(" (Recommended)")
                .or_else(|| after.strip_prefix("(Recommended)"))
                .unwrap_or(after);
            match after.strip_prefix(",") {
                Some(more) if q.multi_select => Some((*label, more.trim_start())),
                _ if after.is_empty() => Some((*label, after)),
                _ => None,
            }
        });
        let Some((label, after)) = found else {
            break;
        };
        picked.push(label.to_owned());
        rest = after;
    }
    AnswerView {
        picked,
        other: (!rest.is_empty()).then(|| rest.to_owned()),
        hidden: false,
    }
}

/// The questions of a Claude AskUserQuestion call's input, for its row.
pub(crate) fn question_view(input: &Value) -> Vec<QuestionView> {
    let text = |value: &Value, name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|q| QuestionView {
            header: text(q, "header"),
            question: text(q, "question"),
            multi_select: q
                .get("multiSelect")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            options: q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|o| {
                    lifted(
                        &text(o, "label"),
                        &text(o, "description"),
                        &text(o, "preview"),
                        false,
                    )
                })
                .collect(),
            allow_other: true,
            secret: false,
        })
        .collect()
}

fn codex(ask: &wire::CodexAsk) -> (AskBody, Vec<Choice>) {
    let decisions: Vec<CodexDecision> = ask
        .decisions
        .iter()
        .filter_map(|d| CodexDecision::try_from(*d).ok())
        .collect();
    let decision_choices = |similar: Vec<String>, hosts: Vec<String>| {
        decisions
            .iter()
            .filter_map(|decision| {
                let outcome = match decision {
                    CodexDecision::Approve => ChoiceOutcome::AllowOnce,
                    CodexDecision::ApproveSession => ChoiceOutcome::AllowForSession,
                    CodexDecision::ApproveSimilar => ChoiceOutcome::AllowSimilar {
                        prefix: similar.clone(),
                    },
                    CodexDecision::ApproveNetwork => ChoiceOutcome::AllowNetwork {
                        hosts: hosts.clone(),
                    },
                    CodexDecision::Deny => ChoiceOutcome::Deny { stops: false },
                    CodexDecision::Abort => ChoiceOutcome::DenyAndStop,
                    CodexDecision::Unspecified => return None,
                };
                Some(choice(outcome, Answer::CodexDecision(*decision)))
            })
            .collect::<Vec<_>>()
    };
    let codex_answer = |of: wire::codex_answer::Of| Answer::Codex(CodexAnswer { of: Some(of) });
    let (body, mut choices) = match &ask.body {
        Some(codex_ask::Body::Command(c)) => (
            AskBody::Command {
                command: c.command.clone(),
                cwd: c.cwd.clone(),
                reason: c.reason.clone(),
                description: String::new(),
            },
            decision_choices(c.allow_prefix.clone(), c.network_hosts.clone()),
        ),
        Some(codex_ask::Body::FileChange(f)) => {
            let diff: String = f
                .changes
                .iter()
                .map(|change| change.patch.as_str())
                .collect();
            let (added, removed) = patch_counts(&diff);
            (
                AskBody::Edit {
                    path: f
                        .changes
                        .first()
                        .map(|change| change.path.clone())
                        .unwrap_or_else(|| f.grant_root.clone()),
                    files: f.changes.len() as u32,
                    added,
                    removed,
                    diff,
                    reason: f.reason.clone(),
                    created: f.changes.len() == 1
                        && f.changes[0].kind() == wire::FileChangeKind::Add,
                },
                decision_choices(vec![], vec![]),
            )
        }
        Some(codex_ask::Body::McpTool(t)) => (
            AskBody::Tool {
                server: t.server.clone(),
                tool: t.tool.clone(),
                arguments: pretty(
                    &serde_json::from_slice(&t.arguments_json).unwrap_or(Value::Null),
                ),
            },
            decision_choices(vec![], vec![]),
        ),
        Some(codex_ask::Body::McpForm(form)) => {
            let action = |action: wire::FormAction| {
                codex_answer(wire::codex_answer::Of::Form(wire::FormAnswer {
                    action: action as i32,
                    content_json: vec![],
                }))
            };
            (
                form_body(form),
                vec![
                    choice(ChoiceOutcome::Submit, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(codex_ask::Body::McpLink(link)) => {
            let action = |action: wire::FormAction| {
                codex_answer(wire::codex_answer::Of::Link(wire::LinkAnswer {
                    action: action as i32,
                }))
            };
            (
                link_body(link),
                vec![
                    choice(ChoiceOutcome::OpenLink, action(wire::FormAction::Accept)),
                    choice(ChoiceOutcome::Decline, action(wire::FormAction::Decline)),
                ],
            )
        }
        Some(codex_ask::Body::Access(grant)) => {
            let answer = |for_session: bool, all: bool| {
                codex_answer(wire::codex_answer::Of::Grant(wire::GrantAnswer {
                    read: if all { grant.read.clone() } else { vec![] },
                    write: if all { grant.write.clone() } else { vec![] },
                    network: all && grant.network,
                    for_session,
                }))
            };
            (
                AskBody::Access {
                    reason: grant.reason.clone(),
                    read: grant.read.clone(),
                    write: grant.write.clone(),
                    network: grant.network,
                    hosts: grant.network_hosts.clone(),
                },
                vec![
                    choice(ChoiceOutcome::GrantForTurn, answer(false, true)),
                    choice(ChoiceOutcome::GrantForSession, answer(true, true)),
                    choice(ChoiceOutcome::Deny { stops: false }, answer(false, false)),
                ],
            )
        }
        Some(codex_ask::Body::Question(q)) => (
            AskBody::Question(q.questions.iter().map(question).collect()),
            vec![],
        ),
        None => (
            AskBody::Unanswerable {
                reason: String::new(),
            },
            vec![],
        ),
    };
    if let Some(first) = choices.first_mut() {
        first.primary = true;
    }
    (body, choices)
}

/// One question's answer as the person gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Pick {
    Options(Vec<u32>),
    Other(String),
}

/// The answer to a question ask: one pick per question, in order, and the
/// optional note.
pub fn question_answer(card: &AskCard, picks: &[Pick], note: &str) -> Answer {
    let answers = wire::QuestionAnswer {
        answers: picks
            .iter()
            .map(|pick| match pick {
                Pick::Options(selected) => wire::QuestionResponse {
                    selected: selected.clone(),
                    other: None,
                },
                Pick::Other(text) => wire::QuestionResponse {
                    selected: vec![],
                    other: Some(text.clone()),
                },
            })
            .collect(),
        note: note.to_owned(),
    };
    match card.kind {
        wire::Kind::Codex => Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Question(answers)),
        }),
        _ => Answer::Claude(ClaudeAnswer {
            of: Some(claude_answer::Of::Question(answers)),
        }),
    }
}

/// Where the person's own words end in a reply sent instead of answering
/// questions; what follows tells the agent what it had asked and what was
/// answered so far.
pub const REPLY_CONTEXT: &str = "\n\n— Sent instead of answering your questions.";

/// A reply sent instead of answering a question ask: the person's words,
/// then each question with its answer so far. Claude can only take it as
/// the ask's refusal and Codex as a note with the answers, until either
/// can decline a question with a message: stand-ins the lab understands.
pub fn question_reply(card: &AskCard, picks: &[Pick], words: &str) -> Option<wire::Input> {
    let AskBody::Question(questions) = &card.body else {
        return None;
    };
    let mut message = format!("{}{REPLY_CONTEXT} Answers so far:", words.trim());
    for (question, pick) in questions.iter().zip(picks) {
        let answer = match pick {
            // A secret is never repeated, not even to the agent that asked.
            Pick::Other(_) if question.secret => "answered (hidden)".to_owned(),
            Pick::Options(selected) if selected.is_empty() => "not answered".to_owned(),
            Pick::Options(selected) => selected
                .iter()
                .filter_map(|i| question.options.get(*i as usize))
                .map(|option| option.label.clone())
                .collect::<Vec<_>>()
                .join(", "),
            Pick::Other(text) => format!("\"{text}\""),
        };
        message.push_str(&format!("\n- {}: {answer}", question.question));
    }
    let answer = match card.kind {
        wire::Kind::Codex => question_answer(card, picks, &message),
        _ => Answer::Claude(ClaudeAnswer {
            of: Some(claude_answer::Of::Permission(wire::PermissionAnswer {
                of: Some(wire::permission_answer::Of::Deny(wire::PermissionDeny {
                    note: message.clone(),
                    stop: false,
                })),
            })),
        }),
    };
    answer_input(card, &answer, &message)
}

/// The person's own words in a reply sent instead of answering questions,
/// or None when `note` is not such a reply.
pub fn reply_words(note: &str) -> Option<&str> {
    note.split_once(REPLY_CONTEXT).map(|(words, _)| words)
}

/// A form's Submit carrying the person's field values, as the JSON object
/// the tool server's schema describes. Any other answer comes back as it
/// was.
pub fn with_form_content(answer: &Answer, content_json: Vec<u8>) -> Answer {
    let mut answer = answer.clone();
    match &mut answer {
        Answer::Claude(ClaudeAnswer {
            of: Some(claude_answer::Of::Form(form)),
        })
        | Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Form(form)),
        }) => form.content_json = content_json,
        _ => {}
    }
    answer
}

/// The input that sends `answer` to the card's ask, in the card's kind's
/// arm: an answer body under the ask's key, or for a Codex approval the
/// decision on the request the card names. `note` goes back to the agent
/// with a choice that takes one and with a question answer; other choices
/// ignore it. The input has no id: the session gives it a fresh one when it
/// sends it. None when the kind does not take this answer.
pub fn answer_input(card: &AskCard, answer: &Answer, note: &str) -> Option<wire::Input> {
    use wire::{claude_pty_input, claude_sdk_input, codex_input, input};
    let body = |kind: &str, body: Vec<u8>| wire::AnswerInput {
        ask_key: card.key.clone(),
        kind: kind.to_owned(),
        body,
    };
    let of = match (card.kind, with_note(answer, note)) {
        (wire::Kind::ClaudePty, Answer::Claude(answer)) => {
            input::Of::ClaudePty(wire::ClaudePtyInput {
                of: Some(claude_pty_input::Of::Answer(body(
                    "claude_pty",
                    answer.encode_to_vec(),
                ))),
            })
        }
        (wire::Kind::ClaudeSdk, Answer::Claude(answer)) => {
            input::Of::ClaudeSdk(wire::ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Answer(body(
                    "claude_sdk",
                    answer.encode_to_vec(),
                ))),
            })
        }
        (wire::Kind::Codex, Answer::Codex(answer)) => input::Of::Codex(wire::CodexInput {
            of: Some(codex_input::Of::Answer(body(
                "codex",
                answer.encode_to_vec(),
            ))),
        }),
        (wire::Kind::Codex, Answer::CodexDecision(decision)) => {
            input::Of::Codex(wire::CodexInput {
                of: Some(codex_input::Of::Approve(wire::Approve {
                    request_id: card.key.clone(),
                    decision: decision as i32,
                })),
            })
        }
        _ => return None,
    };
    Some(wire::Input {
        input_id: Vec::new(),
        of: Some(of),
    })
}

/// The answer with the person's note in the place its kind carries one.
fn with_note(answer: &Answer, note: &str) -> Answer {
    let mut answer = answer.clone();
    if note.is_empty() {
        return answer;
    }
    match &mut answer {
        Answer::Claude(ClaudeAnswer { of: Some(of) }) => match of {
            claude_answer::Of::Permission(wire::PermissionAnswer {
                of: Some(wire::permission_answer::Of::Deny(deny)),
            }) => deny.note = note.to_owned(),
            claude_answer::Of::Plan(wire::PlanAnswer {
                of: Some(wire::plan_answer::Of::SendBack(send_back)),
            }) => send_back.note = note.to_owned(),
            claude_answer::Of::Question(question) => question.note = note.to_owned(),
            _ => {}
        },
        Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Question(question)),
        }) => question.note = note.to_owned(),
        _ => {}
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_called_with_no_arguments_shows_no_arguments_block() {
        assert_eq!(pretty(&Value::Null), "");
        assert_eq!(pretty(&serde_json::json!({})), "");
        assert_eq!(pretty(&serde_json::json!([])), "");
        assert_eq!(
            pretty(&serde_json::json!({"path": "a.txt"})),
            "{\n  \"path\": \"a.txt\"\n}"
        );
    }

    fn asked(multi_select: bool, labels: &[&str]) -> QuestionView {
        QuestionView {
            header: String::new(),
            question: "Which?".into(),
            multi_select,
            options: labels
                .iter()
                .map(|label| lifted(label, "", "", false))
                .collect(),
            allow_other: true,
            secret: false,
        }
    }

    fn answer(picked: &[&str], other: Option<&str>) -> AnswerView {
        AnswerView {
            picked: picked.iter().map(|label| (*label).to_owned()).collect(),
            other: other.map(str::to_owned),
            hidden: false,
        }
    }

    #[test]
    fn recorded_answers_split_into_picks_and_what_was_typed() {
        let single = asked(false, &["Red", "Blue (Recommended)"]);
        assert_eq!(recorded_answer(&single, "Red"), answer(&["Red"], None));
        assert_eq!(
            recorded_answer(&single, "Blue (Recommended)"),
            answer(&["Blue"], None)
        );
        assert_eq!(
            recorded_answer(&single, "a warm ochre"),
            answer(&[], Some("a warm ochre"))
        );
        assert_eq!(
            recorded_answer(&single, "Redder"),
            answer(&[], Some("Redder"))
        );
        let multi = asked(true, &["Hammer", "Saw", "Drill", "Saw blade"]);
        assert_eq!(
            recorded_answer(&multi, "Hammer, Drill"),
            answer(&["Hammer", "Drill"], None)
        );
        assert_eq!(
            recorded_answer(&multi, "Hammer, Saw, Torque wrench "),
            answer(&["Hammer", "Saw"], Some("Torque wrench"))
        );
        assert_eq!(
            recorded_answer(&multi, "Saw blade"),
            answer(&["Saw blade"], None)
        );
    }

    #[test]
    fn answers_follow_the_questions_and_a_result_without_them_has_none() {
        let questions = vec![asked(false, &["Red"]), {
            let mut second = asked(false, &["Large"]);
            second.question = "Which size?".into();
            second
        }];
        let result = br#"{"answers":{"Which size?":"Large","Which?":"Red"},"annotations":{}}"#;
        assert_eq!(
            recorded_answers(&questions, result),
            vec![answer(&["Red"], None), answer(&["Large"], None)]
        );
        assert!(recorded_answers(&questions, b"\"User rejected tool use\"").is_empty());
    }
}
