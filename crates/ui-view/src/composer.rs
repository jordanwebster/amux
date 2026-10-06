//! Around the composer: context use, a sign-in problem, the composer's mode
//! and tokens, the queued prompts and this client's unconfirmed inputs.

use schemars::JsonSchema;
use serde::Serialize;
use ui_state::{Activity, Composer, InputState, InputWhat, SessionState, Waiting};
use wire::{Attachment, SignInState};

use crate::segments::{Segment, segments};

/// Context use from here on is near full, which a client may set apart.
pub const CONTEXT_NEAR_FULL_PERCENT: u64 = 80;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ContextView {
    pub used_tokens: u64,
    pub window_tokens: Option<u64>,
    pub percent: Option<u64>,
    /// At or past [`CONTEXT_NEAR_FULL_PERCENT`] of the window.
    pub near_full: bool,
}

/// How much of its context window the agent uses, once it has said.
pub fn context(state: &SessionState) -> Option<ContextView> {
    let agent = state.agent_state();
    agent.context.known.then(|| {
        let percent = agent
            .context
            .window_tokens
            .filter(|window| *window > 0)
            .map(|window| agent.context.used_tokens * 100 / window);
        ContextView {
            used_tokens: agent.context.used_tokens,
            window_tokens: agent.context.window_tokens,
            percent,
            near_full: percent.is_some_and(|percent| percent >= CONTEXT_NEAR_FULL_PERCENT),
        }
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SignInView {
    pub state: SignInState,
    pub account: String,
    pub message: String,
}

/// The agent's account, only when it needs signing in: it replaces the
/// composer with a foot card.
pub fn sign_in(state: &SessionState) -> Option<SignInView> {
    let agent = state.agent_state();
    match agent.sign_in.state() {
        SignInState::SignedOut | SignInState::Expired | SignInState::Failed => Some(SignInView {
            state: agent.sign_in.state(),
            account: agent.sign_in.account.clone(),
            message: agent.sign_in.message.clone(),
        }),
        SignInState::Unknown | SignInState::SignedIn => None,
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
