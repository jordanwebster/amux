//! Around the composer: the session strip, the composer's mode and tokens,
//! the queued prompts and this client's unconfirmed inputs.

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Activity, Composer, InputState, InputWhat, SessionState, Waiting};
use wire::{Attachment, SignInState, TaskListStatus, UsageState};

use crate::segments::{Segment, segments};

/// Context use shows in the strip only from here; below it lives in
/// settings.
pub const CONTEXT_STRIP_PERCENT: u64 = 80;

/// The facts strip: each field is None when there is nothing to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
pub struct Strip {
    pub tasks: Option<TasksView>,
    pub context: Option<ContextView>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    /// Only near or at a limit.
    pub usage: Option<UsageView>,
    /// Only tool servers that failed.
    pub failed_servers: Vec<ServerView>,
    /// Only a problem; it replaces the composer with a foot card.
    pub sign_in: Option<SignInView>,
    /// Running background processes, when known and any.
    pub background: Option<u32>,
    pub working_on: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TasksView {
    pub done: u32,
    pub total: u32,
    /// The task in progress, in its active form.
    pub current: String,
    /// Every task in the agent's order.
    pub entries: Vec<TaskLine>,
}

/// One task of the list, by its subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TaskLine {
    pub subject: String,
    pub mark: TaskMark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum TaskMark {
    Done,
    Current,
    Todo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ContextView {
    pub used_tokens: u64,
    pub window_tokens: Option<u64>,
    pub percent: Option<u64>,
    /// High enough to show in the strip rather than only in settings.
    pub in_strip: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct UsageView {
    pub blocked: bool,
    pub windows: Vec<UsageWindowView>,
    pub credits: Option<String>,
}

/// One rate-limit window: its name, how much of it is used, and when it
/// resets.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct UsageWindowView {
    pub name: String,
    pub used_percent: f64,
    pub resets_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ServerView {
    pub name: String,
    pub error: String,
    pub needs_auth: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SignInView {
    pub state: SignInState,
    pub account: String,
    pub message: String,
}

pub fn session_strip(state: &SessionState) -> Strip {
    let agent = state.agent_state();
    let tasks = agent.tasks.known.then(|| {
        let total = agent.tasks.entries.len() as u32;
        let done = agent
            .tasks
            .entries
            .iter()
            .filter(|task| task.status() == TaskListStatus::Completed)
            .count() as u32;
        let current = agent
            .tasks
            .entries
            .iter()
            .find(|task| task.status() == TaskListStatus::InProgress)
            .map(|task| {
                if task.active_form.is_empty() {
                    task.subject.clone()
                } else {
                    task.active_form.clone()
                }
            })
            .unwrap_or_default();
        let entries = agent
            .tasks
            .entries
            .iter()
            .map(|task| TaskLine {
                subject: task.subject.clone(),
                mark: match task.status() {
                    TaskListStatus::Completed => TaskMark::Done,
                    TaskListStatus::InProgress => TaskMark::Current,
                    _ => TaskMark::Todo,
                },
            })
            .collect();
        TasksView {
            done,
            total,
            current,
            entries,
        }
    });
    let tasks = tasks.filter(|tasks| tasks.total > 0);
    let context = agent.context.known.then(|| {
        let percent = agent
            .context
            .window_tokens
            .filter(|window| *window > 0)
            .map(|window| agent.context.used_tokens * 100 / window);
        ContextView {
            used_tokens: agent.context.used_tokens,
            window_tokens: agent.context.window_tokens,
            percent,
            in_strip: percent.is_some_and(|percent| percent >= CONTEXT_STRIP_PERCENT),
        }
    });
    let usage = match agent.usage.state() {
        UsageState::NearLimit | UsageState::Blocked => Some(UsageView {
            blocked: agent.usage.state() == UsageState::Blocked,
            windows: agent
                .usage
                .windows
                .iter()
                .map(|window| UsageWindowView {
                    name: window.name.clone(),
                    used_percent: window.used_percent,
                    resets_at_ms: window.resets_at_ms,
                })
                .collect(),
            credits: agent.usage.credits.clone(),
        }),
        UsageState::Unknown | UsageState::Ok => None,
    };
    let failed_servers = agent
        .servers
        .servers
        .iter()
        .filter(|server| {
            matches!(
                server.status(),
                wire::ToolServerStatus::Failed | wire::ToolServerStatus::NeedsAuth
            )
        })
        .map(|server| ServerView {
            name: server.name.clone(),
            error: server.error.clone(),
            needs_auth: server.status() == wire::ToolServerStatus::NeedsAuth,
        })
        .collect();
    let sign_in = match agent.sign_in.state() {
        SignInState::SignedOut | SignInState::Expired | SignInState::Failed => Some(SignInView {
            state: agent.sign_in.state(),
            account: agent.sign_in.account.clone(),
            message: agent.sign_in.message.clone(),
        }),
        SignInState::Unknown | SignInState::SignedIn => None,
    };
    Strip {
        tasks,
        context,
        model: agent.model.clone(),
        effort: agent.effort.clone(),
        mode: agent.mode.clone(),
        usage,
        failed_servers,
        sign_in,
        background: (agent.background.known && agent.background.running > 0)
            .then_some(agent.background.running),
        working_on: agent.working_on.clone(),
    }
}

/// The composer as the chat draws it: Send, Resume for an exited agent,
/// or waiting while drafting continues; with the activity line inside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ComposerView {
    pub mode: Composer,
    pub activity: Option<Activity>,
}

pub fn composer(state: &SessionState, now_ms: i64) -> ComposerView {
    ComposerView {
        mode: state.composer(),
        activity: state.activity(now_ms),
    }
}

/// The waiting reason, for a client that words the disabled composer.
pub fn waiting(state: &SessionState) -> Option<Waiting> {
    match state.composer() {
        Composer::Disabled(why) => Some(why),
        Composer::Send | Composer::Resume => None,
    }
}

/// The draft as tokens: text runs and attachment chips at their places.
pub fn composer_tokens(draft: &str, attachments: &[Attachment]) -> Vec<Segment> {
    segments(draft, attachments)
}

/// A queued prompt under the composer.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct QueuedRow {
    pub input_id: Vec<u8>,
    pub text: Vec<Segment>,
    /// Who queued it: None for a person, the agent's name for an agent.
    pub from_agent: Option<String>,
    pub mine: bool,
    /// Reads "steered" until its reflection lands.
    pub steered: bool,
    pub can_withdraw: bool,
    /// Send now steers it into the running turn.
    pub can_send_now: bool,
}

pub fn queue_rows(state: &SessionState) -> Vec<QueuedRow> {
    let live = state.can_send();
    state
        .queue()
        .into_iter()
        .map(|row| {
            let from_agent = match row
                .entry
                .sender
                .as_ref()
                .and_then(|sender| sender.value.as_ref())
            {
                Some(wire::sender::Value::Agent(agent)) => Some(agent.name.clone()),
                _ => None,
            };
            QueuedRow {
                input_id: row.entry.input_id.clone(),
                text: segments(&row.entry.text, &row.entry.attachments),
                from_agent,
                mine: row.mine,
                steered: row.steered,
                can_withdraw: live && !row.steered,
                can_send_now: live && !row.steered,
            }
        })
        .collect()
}

/// This client's prompts not yet in the transcript or the queue: sending,
/// not confirmed (resend or discard), or rejected with the reason.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct OutboxRow {
    pub input_id: Vec<u8>,
    pub text: Vec<Segment>,
    pub state: OutboxState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum OutboxState {
    Sending,
    NotConfirmed,
    Rejected(String),
}

pub fn outbox_rows(state: &SessionState) -> Vec<OutboxRow> {
    // A sent prompt the agent's queue lists is drawn by its queue row: a
    // resume's first prompt waits there when the new incarnation cannot
    // take it yet.
    let queued = state
        .queue()
        .iter()
        .map(|row| row.entry.input_id.clone())
        .collect::<Vec<_>>();
    state
        .inputs()
        .iter()
        .filter_map(|sent| {
            let InputWhat::Prompt { text, attachments } = &sent.what else {
                return None;
            };
            let state = match &sent.state {
                InputState::Sent if queued.contains(&sent.id) => return None,
                InputState::Sent => OutboxState::Sending,
                InputState::Uncertain => OutboxState::NotConfirmed,
                InputState::Rejected(reason) => OutboxState::Rejected(reason.clone()),
                InputState::Queued | InputState::Settled => return None,
            };
            Some(OutboxRow {
                input_id: sent.id.clone(),
                text: segments(text, attachments),
                state,
            })
        })
        .collect()
}

/// A background job the agent started and that is still running, found
/// from its step in the transcript: the agent's own state counts them but
/// does not name them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JobView {
    /// The step that started it, to open from the list.
    pub key: ui_state::Key,
    pub command: String,
    pub started_at_ms: i64,
}

/// The running background jobs among the held rows, oldest first.
pub fn background_jobs(state: &SessionState) -> Vec<JobView> {
    state
        .transcript()
        .iter()
        .filter_map(|held| match crate::rows::kind_of(state, held).0 {
            crate::rows::RowKind::Background {
                command,
                running: true,
                ..
            } => Some(JobView {
                key: held.item.key.clone(),
                command,
                started_at_ms: held.item.at_ms,
            }),
            _ => None,
        })
        .collect()
}
