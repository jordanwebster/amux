//! Inputs in each kind's own arm. A person's acts on a chat are the same on
//! every kind; which message carries them is the kind's.

use wire::{
    AnswerInput, Attachment, ClaudePtyInput, ClaudeSdkInput, CodexInput, Input, Interrupt, Kind,
    PromptInput, SendQueuedNow, WithdrawQueued, claude_pty_input, claude_sdk_input, codex_input,
    input,
};

/// A fresh client-generated input id.
pub fn input_id() -> Vec<u8> {
    uuid::Uuid::new_v4().as_bytes().to_vec()
}

/// One act, before it is put in a kind's arm.
enum Act {
    Prompt(PromptInput),
    Answer(AnswerInput),
    Withdraw(WithdrawQueued),
    SendNow(SendQueuedNow),
    Interrupt,
}

fn wrap(kind: Kind, act: Act) -> Option<Input> {
    let of = match kind {
        Kind::ClaudePty => input::Of::ClaudePty(ClaudePtyInput {
            of: Some(match act {
                Act::Prompt(prompt) => claude_pty_input::Of::Prompt(prompt),
                Act::Answer(answer) => claude_pty_input::Of::Answer(answer),
                Act::Withdraw(withdraw) => claude_pty_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => claude_pty_input::Of::SendNow(now),
                Act::Interrupt => claude_pty_input::Of::Interrupt(Interrupt {}),
            }),
        }),
        Kind::ClaudeSdk => input::Of::ClaudeSdk(ClaudeSdkInput {
            of: Some(match act {
                Act::Prompt(prompt) => claude_sdk_input::Of::Prompt(prompt),
                Act::Answer(answer) => claude_sdk_input::Of::Answer(answer),
                Act::Withdraw(withdraw) => claude_sdk_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => claude_sdk_input::Of::SendNow(now),
                Act::Interrupt => claude_sdk_input::Of::Interrupt(Interrupt {}),
            }),
        }),
        Kind::Codex => input::Of::Codex(CodexInput {
            of: Some(match act {
                Act::Prompt(prompt) => codex_input::Of::Prompt(prompt),
                Act::Answer(answer) => codex_input::Of::Answer(answer),
                Act::Withdraw(withdraw) => codex_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => codex_input::Of::SendNow(now),
                Act::Interrupt => codex_input::Of::Interrupt(Interrupt {}),
            }),
        }),
        Kind::Unspecified => return None,
    };
    Some(Input {
        input_id: input_id(),
        of: Some(of),
    })
}

/// A prompt in the kind's arm, under a fresh id; `None` for an unknown kind.
pub fn prompt(kind: Kind, text: &str, attachments: Vec<Attachment>) -> Option<Input> {
    wrap(
        kind,
        Act::Prompt(PromptInput {
            text: text.to_owned(),
            attachments,
        }),
    )
}

pub fn answer(kind: Kind, answer: AnswerInput) -> Option<Input> {
    wrap(kind, Act::Answer(answer))
}

pub fn withdraw(kind: Kind, queued_input_id: &[u8]) -> Option<Input> {
    wrap(
        kind,
        Act::Withdraw(WithdrawQueued {
            queued_input_id: queued_input_id.to_vec(),
        }),
    )
}

/// Steers a queued prompt into the running turn.
pub fn send_now(kind: Kind, queued_input_id: &[u8]) -> Option<Input> {
    wrap(
        kind,
        Act::SendNow(SendQueuedNow {
            queued_input_id: queued_input_id.to_vec(),
        }),
    )
}

pub fn interrupt(kind: Kind) -> Option<Input> {
    wrap(kind, Act::Interrupt)
}
