//! The per-kind interpreters: the only place provider facts are read and the
//! only code, besides the client's thin per-kind layer, that decodes an item
//! or snapshot body.
//!
//! An interpreter is pure. A provider fact, an input or a tick goes in; a
//! journal [`Step`] and the [`Effect`]s the agent process performs come out.
//! There is no clock (time arrives as [`Event::Tick`]), no id generation
//! (ids arrive on inputs and facts) and no I/O. The kind-neutral pieces every
//! interpreter shares live in [`Shared`]; the golden harness that replays
//! recorded and authored facts through an interpreter is [`run_golden`].

#![forbid(unsafe_code)]

mod claude_common;
pub mod claude_pty;
mod golden;
mod serde_pb;
mod shared;
pub mod unknown;

#[cfg(test)]
mod testkind;

pub use golden::{
    EndRules, FixtureInput, GoldenReport, UPDATE_GOLDENS_ENV, check_invariants, claude_pty_input,
    claude_sdk_input, codex_input, run_golden,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use serde_pb::{from_hex, to_hex};
pub use shared::{
    AMUX_TOOL_SERVER, Asks, Emit, ItemDraft, OpenAsk, OpenTurn, Queue, STATUS_TOOL, Shared,
    TurnCounter, agent_message_body, agent_message_key, human, is_status_tool, reason,
    status_working_on,
};
use wire::{AgentSpec, Envelope, Input, SendInputResponse, Step, StopMode};

/// One interpreter per agent kind.
pub trait Interpreter {
    /// Everything the interpreter holds; written as the checkpoint at facts
    /// ring rotation and resumed from on restart.
    type State: Checkpoint;

    /// The kind tag its items and snapshots carry.
    const KIND: &'static str;

    /// The state an agent starts in and the first journal frame: a Snapshot
    /// with phase starting and every field at its explicit unknown.
    fn initial(spec: &AgentSpec, producer_version: &str) -> (Self::State, Step);

    /// One event in; the step to journal and the effects to perform out.
    fn step(state: &mut Self::State, event: Event) -> Stepped;

    /// Removes secrets from a body, a facts-ring entry or a checkpoint while
    /// keeping its structure; the daemon calls it on a store slice at dump
    /// time and the agent process on its own dump part.
    fn redact(target: RedactTarget) -> RedactTarget;

    /// The snapshot body with every field at its explicit unknown.
    fn unknown_snapshot() -> Vec<u8>;

    /// Reads an item body for goldens and invariants.
    fn describe_item(body: &[u8]) -> ItemView;

    /// Reads a snapshot body for goldens and invariants.
    fn describe_snapshot(body: &[u8]) -> SnapshotView;

    /// The wire input an authored fixture's input stands for. None for an
    /// input this kind has no arm for.
    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input>;

    /// Turns a recorded provider corpus into events, for recorded fixtures.
    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        let _ = bytes;
        Err(format!(
            "{} has no reader for recordings in format {format:?}",
            Self::KIND
        ))
    }
}

/// What an interpreter consumes. One event is in hand at a time, so the
/// input variant's size costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Fact(Fact),
    Input(Input),
    /// The clock, as a fact: the agent process sends one before each event
    /// whose time matters and on a timer while a quiet period is watched.
    Tick {
        at_ms: i64,
    },
    ProviderExit {
        code: Option<i32>,
    },
    /// ctl.sock closed.
    DaemonLost,
    StopRequested(StopMode),
}

/// One provider fact exactly as it arrived; what the facts ring records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fact {
    pub channel: Channel,
    pub payload: Vec<u8>,
}

/// Where a fact came from. Each kind reads the channels its provider has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// A row of Claude's transcript file.
    Transcript,
    /// A hook payload on private/hooks.sock.
    Hook,
    /// A line of stream-JSON from headless Claude's stdout.
    Stream,
    /// A JSON-RPC message from the Codex app server.
    Rpc,
    /// A call on the agent's own tool server (tools.sock).
    Tools,
    /// What the agent process itself observed about the provider it
    /// launched: for Claude in a terminal, the version it found and the
    /// keymap it resolved for that version.
    Agent,
}

/// A step's output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stepped {
    pub step: Step,
    pub effects: Vec<Effect>,
}

/// What the agent process does on an interpreter's behalf, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Bytes to the provider's one input channel: the PTY, stdin or the
    /// app-server connection.
    ProviderWrite(Vec<u8>),
    /// The verdict on an input, relayed to the sender as the SendInput
    /// reply.
    Reply {
        input_id: Vec<u8>,
        verdict: SendInputResponse,
    },
    /// Hand an agent message to the provider's own injection channel.
    Inject { envelope: Envelope, via: Carrier },
    /// Start an empty turn so the provider consumes parked injected items.
    KickTurn,
    /// The agent process should exit with this cause.
    Exit { cause: String },
    /// Claude in a terminal: a semantic input the agent process types into
    /// the PTY through the keymap it resolved for the running Claude.
    Terminal(claude_pty::TerminalInput),
    /// Claude in a terminal: a session started on this transcript file;
    /// the agent process tails it from now on.
    FollowTranscript { path: String },
}

/// The provider's own injection channel for agent messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    /// Claude in a PTY: the messaging socket.
    MessagingSocket,
    /// Headless Claude: a user message on stdin.
    Stdin,
    /// Codex: inject_items on the app server.
    InjectItems,
}

/// What [`Interpreter::redact`] works on.
#[derive(Clone, Debug, PartialEq)]
pub enum RedactTarget {
    ItemBody(Vec<u8>),
    SnapshotBody(Vec<u8>),
    Fact(Fact),
    Checkpoint(Vec<u8>),
}

/// An item body as the harness and goldens read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemView {
    /// The body's oneof arm; a key never changes arm.
    pub arm: String,
    /// No append may follow.
    pub complete: bool,
    /// A one-line rendering for goldens.
    pub text: String,
}

/// A snapshot body as the harness and goldens read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotView {
    /// Open asks as (ask key, item key).
    pub asks: Vec<(String, String)>,
    pub text: String,
}

/// Interpreter state that can be written at ring rotation and resumed.
pub trait Checkpoint: Serialize + DeserializeOwned + Sized {
    /// The state to continue from and the step a resume starts with: every
    /// open item in full on its existing key, and the snapshot.
    fn resume(self) -> (Self, Step);
}

pub fn encode_checkpoint<S: Checkpoint>(state: &S) -> Vec<u8> {
    serde_json::to_vec(state).expect("interpreter state serializes")
}

pub fn decode_checkpoint<S: Checkpoint>(bytes: &[u8]) -> Result<S, serde_json::Error> {
    serde_json::from_slice(bytes)
}

/// SendInput verdicts.
pub mod reply {
    use wire::{Accepted, Rejected, SendInputResponse, send_input_response};

    pub fn accepted(queued: bool) -> SendInputResponse {
        SendInputResponse {
            of: Some(send_input_response::Of::Accepted(Accepted { queued })),
        }
    }

    pub fn rejected(reason: &str) -> SendInputResponse {
        SendInputResponse {
            of: Some(send_input_response::Of::Rejected(Rejected {
                reason: reason.to_owned(),
            })),
        }
    }
}
