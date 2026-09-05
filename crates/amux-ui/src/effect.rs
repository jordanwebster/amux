//! Effects: what the shell must do. Internal to the amux-ui runtime, never a
//! public contract between processes. `update` returns them as data; the
//! shell executes them against `amux::Client` and feeds results back as Msgs.
//! Replay folds Msgs but never executes Effects.

use amux::{AgentId, ArtifactId, DiffBase, claude_io};
use serde::{Deserialize, Serialize};

use crate::codex::CodexInput;
use crate::model::StructuredProtocol;
use crate::msg::{Command, OpId};

/// Native input for one typed agent layer. Adding a layer adds an enum arm;
/// payloads are never normalized across agents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum InputPayload {
    Claude {
        /// The layer's stream cursor: the source refuses the write unless
        /// the client has seen its newest row (`SequenceNumberMismatch`).
        expected_seq: u64,
        intent: claude_io::Intent,
        /// Reducer policy for a stale-seq refusal: an interrupt's meaning
        /// does not depend on the session's position, so the shell may
        /// retry it mechanically with the seq the refusal reported;
        /// positional programs fail fast and resurface instead.
        retry_stale: bool,
    },
    Codex {
        payload: CodexInput,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// Dispatch the RPC for a command. The shell must always answer with a
    /// `Msg::OpResult` for `op` — success or error — so no spinner is left
    /// lying.
    Rpc { op: OpId, command: Command },
    /// Open the structured session stream for an agent, catching up over a
    /// bounded tail.
    OpenStream {
        agent: AgentId,
        /// The native protocol advertised by the selected agent layer.
        protocol: StructuredProtocol,
        tail: u64,
    },
    /// Close a previously opened stream.
    CloseStream { agent: AgentId },
    /// Send one layer-native input and MUST answer with a `Msg::OpResult`
    /// for `op`. Disconnected executions fail fast with an error outcome —
    /// explicitly held messages retain the failed delivery in queue state.
    SendInput {
        op: OpId,
        agent: AgentId,
        /// Correlates the server's `amux.input_result` row with this input.
        /// Derived deterministically from `op`; retries preserve it.
        input_id: Vec<u8>,
        payload: InputPayload,
    },
    /// Put draft bytes, then send one input carrying the complete pin list.
    PutThenSend {
        op: OpId,
        agent: AgentId,
        puts: Vec<crate::attachments::DraftAttachment>,
        input: InputPayload,
        pin: Vec<ArtifactId>,
    },
    /// Fetch a review patch for the bounded layer index.
    FetchDiff {
        op: OpId,
        agent: AgentId,
        id: ArtifactId,
    },
    /// Fetch and open an artifact through the viewing-host cache.
    OpenExternally {
        op: OpId,
        agent: AgentId,
        id: ArtifactId,
    },
    /// Ask the agent's host to freeze a repository diff.
    Diff {
        op: OpId,
        agent: AgentId,
        base: DiffBase,
    },
    /// A reducer tripwire observed an impossible state: report the recorder
    /// ring for diagnosis. The pure reducer never writes files — it requests.
    RequestDump { reason: DumpReason },
}

/// Why an automatic report was taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dump_reason", rename_all = "snake_case")]
pub enum DumpReason {
    /// `update` observed a state the protocol says cannot happen.
    Tripwire { detail: String },
    /// A lossless channel overflowed — a torn stream is a bug to fix.
    ChannelOverflow { detail: String },
    /// Best-effort report from the panic hook, after terminal restore.
    Panic { detail: String },
    /// Explicit user request from the legacy debug keybinding.
    UserRequested,
}

impl DumpReason {
    pub fn label(&self) -> &'static str {
        match self {
            DumpReason::Tripwire { .. } => "tripwire",
            DumpReason::ChannelOverflow { .. } => "channel-overflow",
            DumpReason::Panic { .. } => "panic",
            DumpReason::UserRequested => "user-requested",
        }
    }
}
