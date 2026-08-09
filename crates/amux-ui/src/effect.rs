//! Effects: what the shell must do. Internal to the amux-ui runtime, never a
//! public contract between processes. `update` returns them as data; the
//! shell executes them against `amux::Client` and feeds results back as Msgs.
//! Replay folds Msgs but never executes Effects.

use amux::AgentId;
use serde::{Deserialize, Serialize};

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
    /// Deliver a `Msg::Tick` after this many milliseconds.
    ScheduleTick { after_ms: u64 },
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
