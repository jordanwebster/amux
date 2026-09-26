//! The ask card: one anatomy for every kind. The head ask with its count,
//! the subject verbatim, the body variant, and choices stated as outcomes.

use serde_json::Value;
use ui_state::{InputState, OpenAsk, SessionState};
use wire::{ClaudeAnswer, CodexAnswer, Decision as CodexDecision, ask, claude_answer, codex_ask};

use crate::rows::patch_counts;

#[derive(Clone, Debug, PartialEq)]
pub struct AskCard {
    pub kind: wire::Kind,
    /// What an answer names.
    pub key: String,
    /// The item the ask points at, empty when the ask is itself the work.
    pub item_key: String,
    /// "1 of 3" when asks are queued.
    pub position: usize,
    pub count: usize,
    pub body: AskBody,
    /// The likely choice first. Only what this agent offers.
    pub choices: Vec<Choice>,
    pub state: CardState,
}

/// Where the card is after the person acts. Stop is always in the menu:
/// it is the interrupt, and the agent stays.
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptionView {
    /// "(Recommended)" lifted out into `recommended`.
    pub label: String,
    pub description: String,
    pub preview: String,
    pub recommended: bool,
}

/// One choice, stated as what happens, with the answer it sends.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub outcome: ChoiceOutcome,
    pub primary: bool,
    /// The person may add a note that goes back to the agent.
    pub takes_note: bool,
    pub answer: Answer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
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

/// The answer body a choice sends; the driver wraps it in an input.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Claude(ClaudeAnswer),
    CodexDecision(CodexDecision),
    Codex(CodexAnswer),
}

/// The head ask, or None when nothing is open or, before CaughtUp, when the
/// entry does not say needs_you.
pub fn ask_card(state: &SessionState) -> Option<AskCard> {
    let asks = state.open_asks();
    let head = asks.first()?;
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
            }
        }
        _ => AskBody::Tool {
            server: String::new(),
            tool: p.tool_name.clone(),
            arguments: pretty(&input),
        },
    }
}

fn pretty(value: &Value) -> String {
    if value.is_null() {
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

fn question(q: &wire::Question) -> QuestionView {
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

fn lifted(label: &str, description: &str, preview: &str, recommended: bool) -> OptionView {
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
