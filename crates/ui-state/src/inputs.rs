//! What this client sent and what became of each input: the one thing only
//! the sender can know.

pub use model::{InputId, InputState};
use wire::{Attachment, Input, SendInputResponse};

/// What became of a SendInput call.
#[derive(Clone, Debug, PartialEq)]
pub enum InputOutcome {
    Reply(SendInputResponse),
    /// The transport failed before a reply.
    Lost,
}

/// What an input asked for, as far as the model needs it.
#[derive(Clone, Debug, PartialEq)]
pub enum InputWhat {
    Prompt {
        text: String,
        attachments: Vec<Attachment>,
    },
    Answer {
        ask_key: String,
    },
    Withdraw {
        target: InputId,
    },
    SendNow {
        target: InputId,
    },
    Interrupt,
    /// Keys, settings, clear.
    Other,
}

impl InputWhat {
    pub fn of(input: &Input) -> InputWhat {
        use wire::input::Of;
        use wire::{claude_pty_input as pty, claude_sdk_input as sdk, codex_input as codex};
        let prompt = |prompt: &wire::PromptInput| InputWhat::Prompt {
            text: prompt.text.clone(),
            attachments: prompt.attachments.clone(),
        };
        let answer = |answer: &wire::AnswerInput| InputWhat::Answer {
            ask_key: answer.ask_key.clone(),
        };
        let withdraw = |withdraw: &wire::WithdrawQueued| InputWhat::Withdraw {
            target: withdraw.queued_input_id.clone(),
        };
        let send_now = |now: &wire::SendQueuedNow| InputWhat::SendNow {
            target: now.queued_input_id.clone(),
        };
        match &input.of {
            Some(Of::ClaudePty(input)) => match &input.of {
                Some(pty::Of::Prompt(p)) => prompt(p),
                Some(pty::Of::Answer(a)) => answer(a),
                Some(pty::Of::Withdraw(w)) => withdraw(w),
                Some(pty::Of::SendNow(n)) => send_now(n),
                Some(pty::Of::Interrupt(_)) => InputWhat::Interrupt,
                Some(pty::Of::Key(_) | pty::Of::Clear(_)) | None => InputWhat::Other,
            },
            Some(Of::ClaudeSdk(input)) => match &input.of {
                Some(sdk::Of::Prompt(p)) => prompt(p),
                Some(sdk::Of::Answer(a)) => answer(a),
                Some(sdk::Of::Withdraw(w)) => withdraw(w),
                Some(sdk::Of::SendNow(n)) => send_now(n),
                Some(sdk::Of::Interrupt(_)) => InputWhat::Interrupt,
                Some(
                    sdk::Of::Mode(_) | sdk::Of::Model(_) | sdk::Of::Clear(_) | sdk::Of::Effort(_),
                )
                | None => InputWhat::Other,
            },
            Some(Of::Codex(input)) => match &input.of {
                Some(codex::Of::Prompt(p)) => prompt(p),
                Some(codex::Of::Answer(a)) => answer(a),
                Some(codex::Of::Approve(approve)) => InputWhat::Answer {
                    ask_key: approve.request_id.clone(),
                },
                Some(codex::Of::Withdraw(w)) => withdraw(w),
                Some(codex::Of::SendNow(n)) => send_now(n),
                Some(codex::Of::Interrupt(_)) => InputWhat::Interrupt,
                Some(
                    codex::Of::Approval(_)
                    | codex::Of::Model(_)
                    | codex::Of::Effort(_)
                    | codex::Of::Rename(_),
                )
                | None => InputWhat::Other,
            },
            Some(Of::AgentMessage(_) | Of::Dump(_)) | None => InputWhat::Other,
        }
    }
}

/// One input this client sent.
#[derive(Clone, Debug, PartialEq)]
pub struct SentInput {
    pub id: InputId,
    pub what: InputWhat,
    pub state: InputState,
    /// The input as sent, so a person can resend it under a new id.
    pub input: Input,
    /// A snapshot has listed it in the queue; leaving the queue after that
    /// means it was submitted or withdrawn.
    pub(crate) seen_queued: bool,
}

/// Every input this client sent during this open, in sending order. Nothing
/// is kept between opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inputs {
    sent: Vec<SentInput>,
}

impl Inputs {
    pub fn get(&self, id: &[u8]) -> Option<&SentInput> {
        self.sent.iter().find(|sent| sent.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: &[u8]) -> Option<&mut SentInput> {
        self.sent.iter_mut().find(|sent| sent.id == id)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &SentInput> {
        self.sent.iter()
    }

    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut SentInput> {
        self.sent.iter_mut()
    }

    pub(crate) fn push(&mut self, input: Input) -> bool {
        if self.get(&input.input_id).is_some() {
            return false;
        }
        self.sent.push(SentInput {
            id: input.input_id.clone(),
            what: InputWhat::of(&input),
            state: InputState::Sent,
            input,
            seen_queued: false,
        });
        true
    }

    pub(crate) fn remove(&mut self, id: &[u8]) -> bool {
        let before = self.sent.len();
        self.sent.retain(|sent| sent.id != id);
        before != self.sent.len()
    }
}
