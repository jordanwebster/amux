//! The Claude chat layer: a typed child model folding native
//! `claude_pty_transcript_v1` rows into feed facts (`docs/CHAT.md` §The
//! feed; `docs/UI.md` "Kernel and per-agent layers").
//!
//! This is a per-agent layer, not a projection: it consumes the agent's
//! native rows directly — transcript rows interleaved with amux hook rows —
//! and derives typed feed entries. There is no intermediate representation
//! and no capability flags; unknown rows become explicit unrecognized
//! entries, never silent drops (G1). Interpretation happens only here, in
//! the fold; renderers format these facts and never re-derive them.
//!
//! Grounding: `notes/chat-v1/transcript-semantics.md` (the row survey) and
//! the Phase 0 fixtures at `crates/amux/tests/fixtures/chat-v1/`. Every
//! derived value keeps the survey's FACT vs INFERRED discipline; comments
//! below tag the rule they implement.
//!
//! This module is part of the pure reducer core: no IO, no clocks, no
//! randomness may be imported here.

mod fold;

use std::collections::{BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Violation;

/// Feed retention bound (B9): matches the source's bounded tail, so the fold
/// never retains more than one window of history. Eviction is from the
/// front, counted, and honest (`history_truncated`).
pub(crate) const FEED_RETAINED: usize = 1000;

/// Row-uuid dedupe memory (B10): a source-shrink re-replay must fold
/// idempotently. Sized with headroom over the feed window; a re-replay
/// reaching past this memory would re-append only rows already evicted from
/// the feed — degradation is bounded and recorded, never unbounded growth.
pub(crate) const SEEN_ROWS_RETAINED: usize = 4096;

/// Message upsert index bound (B2). Main-session files burst-write whole
/// messages, so only recent message ids ever receive late rows.
pub(crate) const MESSAGES_RETAINED: usize = 64;

/// Unpaired `tool_use` index bound (B4). This index outlives feed eviction —
/// it is the structure Phase 2's obligations-outlive-eviction rule builds
/// on — so it is bounded separately from the feed.
pub(crate) const OPEN_TOOLS_RETAINED: usize = 256;

/// Accepted plan payload retention (B6): session state keyed by tool_use
/// id, outside feed windowing, bounded by count.
pub(crate) const PLANS_RETAINED: usize = 8;

/// Bounded head of tool output retained for the compact one-liner (B4).
/// The full text stays on disk behind the Effect seam.
const OUTPUT_HEAD_MAX: usize = 400;

/// One feed entry: a single rendered unit (`docs/CHAT.md` §Vocabulary).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedEntry {
    /// Monotonic within a transcript epoch; the canonical feed order.
    pub id: u64,
    /// Stream seq of the row that created the entry (provenance).
    pub seq: u64,
    pub kind: FeedEntryKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum FeedEntryKind {
    /// A user prompt (B1).
    Prompt(PromptEntry),
    /// An assistant message's text, upserted by `message.id` (B2).
    Message(MessageEntry),
    /// Retroactive `~ thought for Ns` marker (B3, INFERRED from FACT
    /// timestamps).
    Thinking(ThinkingEntry),
    /// Turn closure rule (B3).
    Turn(TurnEntry),
    /// Compaction boundary (B3, FACT).
    Compaction(CompactionEntry),
    /// The post-compaction summary row, flagged transcript-only in the
    /// source (semantics §16).
    CompactSummary(CompactSummaryEntry),
    /// One tool use, paired by `tool_use.id` (B4).
    Tool(ToolEntry),
    /// A background-subagent completion notice (B7, FACT it finished;
    /// content is prose).
    TaskNotification(TaskNotificationEntry),
    /// Interruption artifact (B8, FACT rows).
    Interruption(InterruptionEntry),
    /// `isApiErrorMessage:true` row (B8, FACT).
    ApiError(ApiErrorEntry),
    /// A row shape this build does not know. Retained and rendered
    /// explicitly, never silently dropped (G1).
    Unrecognized(UnrecognizedEntry),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptEntry {
    pub text: String,
    pub source: PromptSource,
    /// Groups the turn's rows; Phase 3's optimistic-echo reconciliation key.
    pub prompt_id: Option<String>,
    pub at: Option<DateTime<Utc>>,
}

/// How the prompt reached the session, from the row's own discriminators
/// (`origin.kind` / `promptSource`, ≥2.1.22x — FACT; absent on older rows
/// and on bare local-command records).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum PromptSource {
    Typed,
    Queued,
    SuggestionAccepted,
    /// `origin.kind:"human"` without a known `promptSource`.
    Human,
    /// A `promptSource` value this build does not know.
    Other {
        label: String,
    },
    /// No discriminator on the row (older versions; bare local-command
    /// records like `/compact`). Rendered as a prompt, but never treated as
    /// a turn start.
    Unstated,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageEntry {
    /// The API message id (`msg_*`) — the upsert key (B2).
    pub message_id: String,
    /// Markdown source segments, one per `text` block, in file order.
    pub segments: Vec<String>,
    pub finality: MessageFinality,
    /// Timestamp of the first block row.
    pub at: Option<DateTime<Utc>>,
}

/// "Streaming" is not a state (B2): a message is Open only until a closing
/// fact or closing inference arrives, and is never rendered as streaming.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "finality", rename_all = "snake_case")]
pub enum MessageFinality {
    /// Newest row still carries a null `stop_reason`.
    Open,
    /// Some row carried a non-null `stop_reason` (FACT).
    Final { stop_reason: String },
    /// Closed by an interrupt row (§17 — FACT-paired via
    /// `interruptedMessageId` where present).
    Interrupted,
    /// Closed because a new message, prompt, or user row arrived while the
    /// `stop_reason` was still null (INFERRED; upgraded to `Final` if the
    /// fact lands later).
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingEntry {
    /// `thinking_row.ts − previous_row.ts`, clamped at zero (INFERRED from
    /// FACT timestamps; includes API latency). `None` when the chain is
    /// broken — never computed across an interrupt or compaction (B3).
    pub duration_ms: Option<i64>,
    /// `redacted_thinking` renders the same marker flagged redacted.
    pub redacted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEntry {
    pub duration: TurnDuration,
    /// Cumulative conversation messages (`turn_duration.messageCount`).
    pub message_count: Option<u64>,
    /// FACT count of still-running background subagents at turn end (B7).
    pub pending_background_agents: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "duration", rename_all = "snake_case")]
pub enum TurnDuration {
    /// `system/turn_duration.durationMs` — the authority (FACT,
    /// wall-time-verified).
    Measured { ms: u64 },
    /// Interrupt-ended turns have no `turn_duration`; elapsed from the
    /// prompt row's timestamp (INFERRED). Reconciled in place to `Measured`
    /// if the authority lands after all (observed on tool-use denials).
    SincePrompt { ms: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionEntry {
    /// `"manual"` or `"auto"` (FACT).
    pub trigger: Option<String>,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactSummaryEntry {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolEntry {
    pub tool_use_id: String,
    /// `None` only for an orphan `tool_result` whose `tool_use` fell
    /// outside the window (truncated history).
    pub name: Option<String>,
    pub invocation: ToolInvocation,
    pub outcome: ToolOutcome,
    /// The carrying message reached a non-null `stop_reason`: an unpaired
    /// tool in a final message renders as running (INFERRED-pending, B4).
    pub message_final: bool,
    /// Grouping fact (B4): this and the immediately preceding entry are
    /// both read/search one-liners. Computed here, never by renderer
    /// layout introspection.
    pub group_with_previous: bool,
    pub message_id: Option<String>,
}

/// Typed invocation facts per tool family, extracted tolerantly from
/// `tool_use.input` — absent fields are `None`, never an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolInvocation {
    Edit {
        file_path: Option<String>,
        replace_all: bool,
    },
    Write {
        file_path: Option<String>,
    },
    Bash {
        command: Option<String>,
        description: Option<String>,
    },
    Read {
        file_path: Option<String>,
    },
    /// The read/search family beyond `Read`: Grep, Glob, WebSearch,
    /// WebFetch, ToolSearch — one line, one query-ish string.
    Query {
        text: Option<String>,
    },
    /// Subagent spawn (`Task` / `Agent`), B7.
    Task {
        description: Option<String>,
        subagent_type: Option<String>,
        background: bool,
    },
    /// `AskUserQuestion` (B5/C4 facts; options are `{label, description}`
    /// objects — Phase 0 capture).
    Question {
        questions: Vec<QuestionFact>,
    },
    /// `ExitPlanMode` (B6): the plan payload rides `input.plan`.
    Plan {
        plan: Option<String>,
        plan_file_path: Option<String>,
    },
    /// A tool this build does not know: name-only rendering.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionFact {
    pub header: Option<String>,
    pub question: Option<String>,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolOutcome {
    /// No paired `tool_result` yet. With `message_final`, renders as
    /// running (INFERRED-pending; FACT once the result lands).
    Pending,
    /// Non-error `tool_result` (FACT the tool ran; B5's allow source).
    Success { facts: SuccessFacts },
    /// `is_error:true` with a `toolDenialKind` — a typed denial fact, never
    /// an error-string sniff (B5).
    Denied { kind: Option<String> },
    /// `is_error:true` without a denial kind.
    Failed { message: Option<String> },
}

/// Typed result facts per family, from the `toolUseResult` sidecar where
/// the semantics spec names its shape (§12), generic output head otherwise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "facts", rename_all = "snake_case")]
pub enum SuccessFacts {
    /// File change magnitude from `filePath` + `structuredPatch` (FACT) —
    /// Edit and Write sidecars both carry it.
    Edit {
        file_path: String,
        added: u64,
        removed: u64,
    },
    /// AskUserQuestion answers, keyed by the question TEXT (Phase 0
    /// capture correction), multi-select joined into one string.
    Answers { answers: Vec<QuestionAnswer> },
    /// Synchronous subagent completion (B7, FACT).
    TaskCompleted {
        agent_id: Option<String>,
        duration_ms: Option<u64>,
        tool_count: Option<u64>,
    },
    /// Background subagent launch acknowledged (B7, FACT it launched).
    TaskLaunched { agent_id: Option<String> },
    /// ExitPlanMode approval (B6): non-error result with the plan sidecar.
    PlanApproved { plan_file_path: Option<String> },
    /// Generic bounded head of the result content; the full text stays on
    /// disk behind the Effect seam (B4).
    Output { head: String, truncated: bool },
    /// A result with no retainable content.
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub question: String,
    pub answer: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotificationEntry {
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptionEntry {
    pub kind: InterruptionKind,
    /// `interruptedMessageId` — FACT pairing to the message it cut off.
    pub interrupted_message_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionKind {
    /// `[Request interrupted by user]` — cut a generating message.
    Turn,
    /// `[Request interrupted by user for tool use]` — a tool approval was
    /// rejected by interrupt.
    ToolUse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorEntry {
    /// The row's typed `error` string (e.g. `"server_error"`).
    pub error: Option<String>,
    /// The synthetic message's text content.
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnrecognizedEntry {
    /// The row's `type`, when it had one.
    pub row_type: Option<String>,
    /// The unknown discriminant below the type (subtype, block type, …).
    pub detail: Option<String>,
}

/// Latest-wins session-state facts folded from the no-uuid rows
/// (`docs/CHAT.md`: not feed entries; they feed phase, composer, and header
/// state).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFacts {
    /// D4's source of truth (FACT at emission).
    pub permission_mode: Option<String>,
    pub ai_title: Option<String>,
    pub agent_name: Option<String>,
}

/// An accepted plan retained as session state (B6): keyed by tool_use id,
/// outside feed windowing, bounded by count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedPlan {
    pub tool_use_id: String,
    pub plan: String,
    pub plan_file_path: Option<String>,
}

/// Turn-scoped fold state (B3): the pre-signal reconciliation and the
/// timestamp chains that duration inferences ride.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TurnState {
    /// The current turn's prompt row timestamp (turn start, §14 FACT).
    prompt_at: Option<DateTime<Utc>>,
    /// An arrival-ordered `hook.stop` said the turn ended; awaiting the
    /// in-transcript `turn_duration` authority.
    stop_presignal: bool,
    /// Entry id of an inferred (elapsed) turn marker awaiting
    /// reconciliation should the authority land after an interrupt.
    inferred_turn_entry: Option<u64>,
    /// Timestamp of the previous uuid row in file order — the thinking
    /// duration chain. Cleared across interrupts and compaction.
    last_row_at: Option<DateTime<Utc>>,
}

/// Message upsert slot (B2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct MessageSlot {
    id: String,
    /// The text entry for this message, if any text block arrived.
    entry: Option<u64>,
    state: SlotState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SlotState {
    Open,
    /// Non-null `stop_reason` seen (FACT — wins over any inference).
    FinalFact,
    /// Closed as abandoned/interrupted (INFERRED).
    ClosedInferred,
}

/// One unpaired `tool_use`, indexed for pairing. Lives outside the feed
/// window: evicting content never evicts this (the ask-obligation seam).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OpenTool {
    tool_use_id: String,
    entry: u64,
    message_id: Option<String>,
}

/// The Claude layer state for one agent. Everything a chat renderer may
/// read; all interpretation happens in the fold that builds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeLayer {
    /// Replay began past the start of the source buffer (subscription
    /// fact): the feed's first state is the honest boundary (B9).
    truncated_start: bool,
    /// Entries evicted from the front of the bounded feed this epoch.
    evicted: u64,
    /// The `amux.transcript_ready` marker was seen for the current link:
    /// everything before it was replay (B10).
    transcript_ready: bool,
    /// Identity of the tailed transcript file (`sessionId` on rows, which
    /// always equals the file basename). A row from a different session is
    /// the relink fact and opens a fresh epoch (§16: the only reliable
    /// `/clear` signal).
    session_id: Option<String>,
    session: SessionFacts,
    entries: VecDeque<FeedEntry>,
    next_entry_id: u64,
    turn: TurnState,
    messages: VecDeque<MessageSlot>,
    open_tools: VecDeque<OpenTool>,
    plans: Vec<AcceptedPlan>,
    /// Row-uuid dedupe memory, FIFO-bounded; `seen_set` mirrors it for
    /// lookup.
    seen_rows: VecDeque<Uuid>,
    seen_set: BTreeSet<Uuid>,
}

impl ClaudeLayer {
    /// Start (or restart) an observation window: a fresh subscription
    /// replays the source tail from scratch, so the layer folds from
    /// scratch — non-uuid rows (hooks, markers) have no dedupe key and
    /// must fold exactly once per window.
    pub(crate) fn begin_window(&mut self, truncated: bool) {
        *self = Self {
            truncated_start: truncated,
            ..Self::default()
        };
    }

    /// Fold one structured row (transcript row, hook row, or amux marker).
    pub(crate) fn observe(&mut self, seq: u64, row: &serde_json::Value) {
        fold::observe(self, seq, row);
    }

    /// The feed, in file order.
    pub fn entries(&self) -> impl Iterator<Item = &FeedEntry> {
        self.entries.iter()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// The feed does not start at the beginning of history: replay began
    /// past the source start, or bounded retention has evicted entries.
    /// Renderers state it (`─ earlier history unavailable ─`), the Model
    /// decides it (B9).
    pub fn history_truncated(&self) -> bool {
        self.truncated_start || self.evicted > 0
    }

    pub fn evicted_entries(&self) -> u64 {
        self.evicted
    }

    /// Everything before this is replay; a fresh session has no transcript
    /// file until its first turn, so an empty feed without this marker is
    /// an empty chat, not a loading one (B10).
    pub fn transcript_ready(&self) -> bool {
        self.transcript_ready
    }

    pub fn session(&self) -> &SessionFacts {
        &self.session
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Accepted plans, oldest first (B6).
    pub fn accepted_plans(&self) -> &[AcceptedPlan] {
        &self.plans
    }

    /// An arrival-ordered `hook.stop` reported the turn ended before the
    /// transcript tail caught up (B3's low-latency pre-signal; cleared when
    /// the `turn_duration` authority lands or a new turn starts).
    pub fn turn_end_presignal(&self) -> bool {
        self.turn.stop_presignal
    }

    /// The current turn's prompt timestamp (elapsed-time base, D5).
    pub fn prompt_at(&self) -> Option<DateTime<Utc>> {
        self.turn.prompt_at
    }

    /// Count of unpaired tool uses (the pairing index Phase 2's ask
    /// extraction reads).
    pub fn open_tool_count(&self) -> usize {
        self.open_tools.len()
    }

    /// Structural coherence (`Model::check_invariants` extension): ids,
    /// counts, and arithmetic — never content.
    pub(crate) fn check_invariants(&self, agent: amux::AgentId, out: &mut Vec<Violation>) {
        for (store, len, cap) in [
            ("feed", self.entries.len(), FEED_RETAINED),
            ("seen-rows", self.seen_rows.len(), SEEN_ROWS_RETAINED),
            ("messages", self.messages.len(), MESSAGES_RETAINED),
            ("open-tools", self.open_tools.len(), OPEN_TOOLS_RETAINED),
            ("plans", self.plans.len(), PLANS_RETAINED),
        ] {
            if len > cap {
                out.push(Violation::ClaudeRetentionOverflow {
                    agent,
                    store,
                    len,
                    cap,
                });
            }
        }

        // Feed arithmetic: ids are assigned sequentially from 0 and evicted
        // only from the front, so evicted + retained == next id, and the
        // retained ids are exactly the contiguous tail.
        let coherent = self.evicted + self.entries.len() as u64 == self.next_entry_id
            && self
                .entries
                .front()
                .is_none_or(|front| front.id == self.evicted)
            && self
                .entries
                .back()
                .is_none_or(|back| back.id + 1 == self.next_entry_id);
        if !coherent {
            out.push(Violation::ClaudeFeedOrder { agent });
        }

        if self.seen_rows.len() != self.seen_set.len() {
            out.push(Violation::ClaudeDedupeIncoherent {
                agent,
                rows: self.seen_rows.len(),
                set: self.seen_set.len(),
            });
        }

        let index_refs = self
            .messages
            .iter()
            .filter_map(|slot| slot.entry.map(|entry| ("messages", entry)))
            .chain(
                self.open_tools
                    .iter()
                    .map(|tool| ("open-tools", tool.entry)),
            )
            .chain(
                self.turn
                    .inferred_turn_entry
                    .map(|entry| ("inferred-turn", entry)),
            );
        for (index, entry) in index_refs {
            if entry >= self.next_entry_id {
                out.push(Violation::ClaudeIndexAhead {
                    agent,
                    index,
                    entry,
                    next: self.next_entry_id,
                });
            }
        }
    }
}

/// The invariant classes must actually FIRE: a coherent layer is built
/// through public folds, then one structural field is corrupted per class.
/// (The wire_free differential spec proves no public fold sequence ever
/// trips them.)
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::Model;
    use crate::msg::{Msg, ServerMsg, StreamEntry, StreamMsg};
    use crate::update::update;

    fn agent_id() -> amux::AgentId {
        Uuid::from_u128(7)
    }

    fn a_model_with_a_folded_layer() -> Model {
        let agent = amux::Agent {
            id: agent_id(),
            host_id: Uuid::from_u128(1),
            name: Some("fix-auth-bug".to_string()),
            command: "claude".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            agent_type: "claude".to_string(),
            io_protocols: vec![
                "claude_raw_v1".to_string(),
                "claude_pty_transcript_v1".to_string(),
            ],
            readonly: false,
            args: Vec::new(),
            created_at: chrono::DateTime::from_timestamp(1_754_697_600, 0).expect("epoch"),
        };
        let host = amux::HostEntry {
            id: Uuid::from_u128(1),
            name: "nova".to_string(),
            online: true,
            version: None,
            capabilities: None,
            trust_status: amux::HostTrustStatus::Trusted,
            last_dial_error: None,
        };
        let mut model = Model::default();
        for msg in [
            Msg::Server(ServerMsg::Connected {
                local_host_id: Some(Uuid::from_u128(1)),
            }),
            Msg::Server(ServerMsg::HostUpserted { host }),
            Msg::Server(ServerMsg::AgentUpserted { agent }),
            Msg::Server(ServerMsg::HostsSynchronized),
            Msg::Server(ServerMsg::AgentsSynchronized),
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Opened { truncated: false },
            },
            Msg::Stream {
                agent: agent_id(),
                event: StreamMsg::Batch {
                    at: chrono::DateTime::from_timestamp(1_754_697_601, 0).expect("epoch"),
                    entries: vec![StreamEntry {
                        seq: 1,
                        payload: json!({
                            "type": "user",
                            "uuid": "11111111-1111-4111-8111-111111111111",
                            "sessionId": "22222222-2222-4222-8222-222222222222",
                            "timestamp": "2026-08-11T22:00:00.000Z",
                            "message": {"role": "user", "content": "hello"},
                            "origin": {"kind": "human"},
                            "promptSource": "typed"
                        }),
                    }],
                },
            },
        ] {
            update(&mut model, msg);
        }
        let violations = model.check_invariants();
        assert!(
            violations.is_empty(),
            "fixture must start coherent: {violations:?}"
        );
        assert!(
            model
                .claude(agent_id())
                .is_some_and(|layer| layer.entry_count() == 1),
            "fixture must carry a folded claude layer"
        );
        model
    }

    fn layer_mut(model: &mut Model) -> &mut ClaudeLayer {
        model
            .agents
            .get_mut(&agent_id())
            .expect("agent card")
            .claude
            .as_mut()
            .expect("claude layer")
    }

    fn fires(model: &Model, kind: &str) -> bool {
        model
            .check_invariants()
            .iter()
            .any(|violation| violation.kind() == kind)
    }

    #[test]
    fn detects_retention_overflow() {
        let mut model = a_model_with_a_folded_layer();
        let layer = layer_mut(&mut model);
        for _ in 0..=FEED_RETAINED {
            let id = layer.next_entry_id;
            layer.entries.push_back(FeedEntry {
                id,
                seq: id,
                kind: FeedEntryKind::CompactSummary(CompactSummaryEntry {
                    text: String::new(),
                }),
            });
            layer.next_entry_id += 1;
        }
        assert!(fires(&model, "claude-retention-overflow"));
    }

    #[test]
    fn detects_broken_feed_arithmetic() {
        let mut model = a_model_with_a_folded_layer();
        layer_mut(&mut model).next_entry_id += 1;
        assert!(fires(&model, "claude-feed-order"));
    }

    #[test]
    fn detects_an_index_pointing_past_the_feed() {
        let mut model = a_model_with_a_folded_layer();
        let layer = layer_mut(&mut model);
        let ahead = layer.next_entry_id + 5;
        layer.open_tools.push_back(OpenTool {
            tool_use_id: "toolu_ghost".to_string(),
            entry: ahead,
            message_id: None,
        });
        assert!(fires(&model, "claude-index-ahead"));
    }

    #[test]
    fn detects_dedupe_incoherence() {
        let mut model = a_model_with_a_folded_layer();
        layer_mut(&mut model)
            .seen_rows
            .push_back(Uuid::from_u128(99));
        assert!(fires(&model, "claude-dedupe-incoherent"));
    }
}
