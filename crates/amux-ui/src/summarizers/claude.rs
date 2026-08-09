//! The Claude attention summarizer: a pure fold `(state, structured entry)
//! -> Attention` over the `claude_pty_transcript_v1` stream.
//!
//! The stream interleaves raw Claude Code transcript rows (`"type":
//! "user" | "assistant" | ...`) with the hook rows amux's claude session
//! emits (`"type": "hook.permission_request" | "hook.stop" |
//! "hook.notification"` — see `agents/claude/session/hooks.rs`; the full
//! `ClaudeHookKind` set is SessionStart, PermissionRequest, Stop,
//! SessionEnd, Notification, Unknown, of which only the middle three are
//! ever emitted as structured output — SessionStart/SessionEnd are
//! internal-only and Unknown is dropped at the core).
//!
//! Entry/clear table (last relevant row wins; entering states is easy,
//! clearing is the design work):
//!
//! | row                                        | state after              |
//! |--------------------------------------------|--------------------------|
//! | `hook.permission_request`                  | NeedsYou(Permission)     |
//! | `hook.notification` ("permission" in text) | NeedsYou(Permission)     |
//! | `hook.notification` (anything else)        | NeedsYou(Question)       |
//! | `hook.stop`                                | NeedsYou(Finished)       |
//! | `user` / `assistant` / `progress` row      | Working (clears NeedsYou)|
//! | any other row (`summary`, `system`, …)     | no change (weak)         |
//!
//! Clearing rationale: a permission answered through raw passthrough is
//! invisible as such — but a blocked agent emits nothing, so any subsequent
//! activity row *is* the evidence it was unblocked. Streaming output cannot
//! strobe `Working`: only hook rows leave it (hysteresis by construction).
//!
//! Honesty: with no strong evidence at all, a fold over a complete window
//! reports `Idle`, but over a truncated window (replay began past seq 1 —
//! history is bounded at the source) it reports `Unknown`, never a guess.

use serde::{Deserialize, Serialize};

use crate::model::{Attention, Why};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeSummarizer {
    /// The observation window began mid-stream; absence of evidence is not
    /// evidence of idleness.
    window_truncated: bool,
    state: FoldState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FoldState {
    #[default]
    NoEvidence,
    Working,
    NeedsPermission,
    NeedsAnswer,
    TurnComplete,
}

/// How one structured row bears on attention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowClass {
    Permission,
    Question,
    TurnComplete,
    Activity,
    Weak,
}

impl ClaudeSummarizer {
    /// Start (or restart) an observation window. `truncated` is the fact
    /// that replay began past the start of the stream.
    pub fn begin_window(&mut self, truncated: bool) {
        *self = Self {
            window_truncated: truncated,
            state: FoldState::NoEvidence,
        };
    }

    /// Fold one structured row.
    pub fn observe(&mut self, row: &serde_json::Value) {
        match classify(row) {
            RowClass::Permission => self.state = FoldState::NeedsPermission,
            RowClass::Question => self.state = FoldState::NeedsAnswer,
            RowClass::TurnComplete => self.state = FoldState::TurnComplete,
            RowClass::Activity => self.state = FoldState::Working,
            RowClass::Weak => {}
        }
    }

    /// The stream went away underneath us (transport loss, relink): whatever
    /// we knew is stale. Degrade to `Unknown`, never to a wrong badge.
    pub fn invalidate(&mut self) {
        self.begin_window(true);
    }

    /// The agent process ended in an orderly way; there is nothing left to
    /// need. (The card's phase carries the exit itself.)
    pub fn observe_exit(&mut self) {
        self.begin_window(false);
    }

    pub fn attention(&self) -> Attention {
        match self.state {
            FoldState::NoEvidence => {
                if self.window_truncated {
                    Attention::Unknown
                } else {
                    Attention::Idle
                }
            }
            FoldState::Working => Attention::Working,
            FoldState::NeedsPermission => Attention::NeedsYou {
                why: Why::Permission,
            },
            FoldState::NeedsAnswer => Attention::NeedsYou { why: Why::Question },
            FoldState::TurnComplete => Attention::NeedsYou { why: Why::Finished },
        }
    }
}

fn classify(row: &serde_json::Value) -> RowClass {
    let Some(kind) = row.get("type").and_then(|value| value.as_str()) else {
        return RowClass::Weak;
    };
    match kind {
        "hook.permission_request" => RowClass::Permission,
        "hook.stop" => RowClass::TurnComplete,
        // Hooks alone do not distinguish "question" from "permission":
        // Claude Code sends both as Notification, differing only in text
        // ("Claude needs your permission to use X" vs "Claude is waiting
        // for your input"). Interpretation belongs here, at observation
        // time — not in the core.
        "hook.notification" => {
            let message = row
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if message.to_lowercase().contains("permission") {
                RowClass::Permission
            } else {
                RowClass::Question
            }
        }
        // Transcript rows: the user answered / Claude is doing things.
        "user" | "assistant" | "progress" => RowClass::Activity,
        // Compaction summaries, system banners, and shapes we do not know
        // carry no attention signal either way.
        _ => RowClass::Weak,
    }
}
