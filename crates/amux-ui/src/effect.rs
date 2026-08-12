//! Effects: what the shell must do. Internal to the amux-ui runtime, never a
//! public contract between processes. `update` returns them as data; the
//! shell executes them against `amux::Client` and feeds results back as Msgs.
//! Replay folds Msgs but never executes Effects.

use amux::AgentId;
use serde::{Deserialize, Serialize};

use crate::claude::encoding::KeyStep;
use crate::msg::{Command, OpId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// Dispatch the RPC for a command. The shell must always answer with a
    /// `Msg::OpResult` for `op` — success or error — so no spinner is left
    /// lying.
    Rpc { op: OpId, command: Command },
    /// Open the structured session stream for an agent, catching up over a
    /// bounded tail.
    OpenStream { agent: AgentId, tail: u64 },
    /// Close a previously opened stream.
    CloseStream { agent: AgentId },
    /// Inject a keystroke program into the agent's session PTY under the
    /// seq guard (`docs/CHAT.md` C6): the shell maps the steps onto the
    /// `claude_pty_transcript_v1` input actions and MUST answer with a
    /// `Msg::OpResult` for `op`. Disconnected executions fail fast with an
    /// error outcome — no offline queue.
    SendInput {
        op: OpId,
        agent: AgentId,
        /// The layer's stream cursor: the source refuses the write unless
        /// the client has seen its newest row (`SequenceNumberMismatch`).
        expected_seq: u64,
        program: Vec<KeyStep>,
        /// Reducer policy for a stale-seq refusal: an interrupt's meaning
        /// does not depend on the session's position, so the shell may
        /// retry it mechanically with the seq the refusal reported;
        /// positional programs (menu answers, prompts) fail fast and
        /// resurface instead.
        retry_stale: bool,
    },
    /// A reducer tripwire observed an impossible state: dump the recorder
    /// ring for diagnosis. The pure reducer never writes files — it requests.
    RequestDump { reason: DumpReason },
}

/// Why a recorder dump was taken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "dump_reason", rename_all = "snake_case")]
pub enum DumpReason {
    /// `update` observed a state the protocol says cannot happen.
    Tripwire { detail: String },
    /// A lossless channel overflowed — a torn stream is a bug to fix.
    ChannelOverflow { detail: String },
    /// Best-effort dump from the panic hook, after terminal restore.
    Panic { detail: String },
    /// Explicit user request (keybinding or `amux debug ui-dump`).
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
