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

pub mod artifact;
pub mod encoding;
mod fold;
pub(crate) mod update;

pub use artifact::{AskArtifact, DiffArtifact, DiffHunk, DiffMagnitude, DiffNumbering};

use std::collections::{BTreeSet, VecDeque};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::claude::encoding::AskAnswer;
use crate::model::{AgentPhase, Attention, Model, StreamPhase, StreamState, Violation, Why};
use crate::msg::OpId;

/// The native structured protocol owned by this layer.
pub const PROTOCOL: &str = "claude_pty_transcript_v1";

/// Claude-native client writes. This vocabulary stays deliberately
/// asymmetric with every other agent layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "claude_command", rename_all = "snake_case")]
pub enum ClaudeCommand {
    SendPrompt {
        agent: amux::AgentId,
        text: String,
    },
    AnswerAsk {
        agent: amux::AgentId,
        ask: u64,
        answer: AskAnswer,
    },
    Interrupt {
        agent: amux::AgentId,
    },
    CyclePermissionMode {
        agent: amux::AgentId,
    },
}

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

/// Pending-ask retention (C): asks queue outside the feed window — evicting
/// content never evicts asks (B9) — under their own explicit bound.
/// Realistically a handful pend at once (parallel tool use enqueues a few);
/// overflow drops the oldest, honestly bounded like everything else.
pub(crate) const ASKS_RETAINED: usize = 32;

/// Optimistic prompt echoes awaiting their transcript row (B1). The
/// send-in-flight gate keeps one in flight; the bound is defensive.
pub(crate) const ECHOES_RETAINED: usize = 4;

/// Staleness cap on the INFERRED working phase (E1): a crashed claude
/// leaves "working" stuck, so a turn with no observed delivery for this
/// long degrades to Unknown — never to a wrong badge. Generous because
/// long silent stretches are legal (a 10-minute Bash timeout, a long
/// generation burst-written only at completion).
pub(crate) const WORKING_STALENESS_CAP_SECS: i64 = 600;

/// How long the idle-from-authority phase stays tagged FACT (E1: "FACT at
/// the signal, decays to INFERRED" — an external session may already be
/// typing).
pub(crate) const IDLE_FACT_DECAY_SECS: i64 = 60;

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

/// An agent-initiated blocking request (`docs/CHAT.md` §Asks) — the
/// chat-layer surface of a live obligation. Queued in arrival order; the
/// head renders with an honest `(1 of N)` count.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ask {
    /// Monotonic per window — the stable handle renderers and Phase 3
    /// commands address the ask by.
    pub id: u64,
    /// Stream seq of the creating row (provenance).
    pub seq: u64,
    /// The `toolu_*` id once the transcript row is correlated — the
    /// resolution key. Hook payloads carry NO tool_use id
    /// (fixture-verified), so a hook-born ask starts uncorrelated and gains
    /// the id when the transcript tail catches up.
    pub tool_use_id: Option<String>,
    pub kind: AskKind,
    pub state: AskState,
    /// The typed panel/reader body (C2): the ask-time mini-diff for Edit,
    /// the proposed content for Write, `None` otherwise — computed once in
    /// the fold at ask creation and retained with the ask (evict bytes,
    /// never obligations). Plan asks carry their markdown on the
    /// invocation instead.
    pub artifact: Option<AskArtifact>,
    /// Hook-side identity: hash of (tool_name, canonical tool_input). The
    /// hook's `tool_input` equals the transcript `tool_use.input`
    /// byte-for-byte (fixture-verified), so one key both dedupes
    /// at-least-once hook delivery and correlates the transcript row.
    hook_key: Option<u64>,
}

impl Ask {
    pub fn why(&self) -> AskWhy {
        match self.kind {
            AskKind::Permission { .. } => AskWhy::Permission,
            AskKind::Question { .. } => AskWhy::Question,
        }
    }
}

/// Exactly two kinds (`docs/CHAT.md` §Vocabulary): plan review is a
/// permission whose invocation is [`ToolInvocation::Plan`] — the payload
/// carries the plan — not a third kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum AskKind {
    /// Permission for one tool use, with the same typed per-tool payload
    /// the feed's tool entries carry (one extraction, both consumers).
    Permission {
        tool_name: Option<String>,
        invocation: ToolInvocation,
        /// The hook payload's `permission_suggestions` facts: claude's
        /// menu is generated from them (`1. Yes` · one option per
        /// suggestion · `No` last — §18d), so the C6 encoder's digit
        /// table and the panel's scope label both read these.
        /// Transcript-only fallback asks carry none.
        suggestions: Vec<SuggestionFact>,
    },
    /// `AskUserQuestion` (C4 facts).
    Question { questions: Vec<QuestionFact> },
}

/// One `permission_suggestions` entry, extracted tolerantly (unknown
/// suggestion kinds keep their tag and render generically).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestionFact {
    /// The suggestion's `type` (observed: `addDirectories`).
    pub kind: Option<String>,
    /// The grant's scope (observed: `session`).
    pub destination: Option<String>,
    /// Directories for directory-grant suggestions.
    pub directories: Vec<String>,
}

/// Why an ask needs the user — the phase's needs-you discriminator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskWhy {
    Permission,
    Question,
}

/// C5 lifecycle state living in the Model. Resolution removes the ask from
/// the queue — the collapsed B5 fact renders from the tool entry — and
/// wins over any local state (remote resolution included).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AskState {
    Pending,
    /// An answer was submitted optimistically: the retained answer is the
    /// dim pending marker's data, `op` the in-flight operation whose
    /// failure would resurface the ask (C5). Confirmation is the
    /// transcript's resolution fact.
    AnsweredOptimistic {
        op: OpId,
        answer: AskAnswer,
    },
    /// The send failed (seq mismatch / transport / server rejection):
    /// resurfaced with the failure stated — never a stuck spinner. No
    /// transcript artifact exists for this; it is purely client-side
    /// state.
    SendFailed {
        message: String,
    },
}

/// An optimistic local prompt echo (B1): rendered as the sent prompt with
/// a dim `sending…` marker until the transcript's user row reconciles it
/// (content equality — the injected paste lands byte-identical, §18d) or
/// the send fails (the draft resurfaces from renderer ViewState; the
/// finished op carries the failure fact).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptEcho {
    pub op: OpId,
    /// The normalized prompt text — the reconciliation key.
    pub text: String,
    /// The Model's observed time at dispatch (`Model::now`), the display
    /// base; `None` before any Tick.
    pub at: Option<DateTime<Utc>>,
}

/// Fact-vs-inferred tag on derived phase values (E1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseTag {
    Fact,
    Inferred,
}

/// The derived session phase (`docs/CHAT.md` §Phase and attention — the E1
/// table). Variants whose epistemic status is fixed carry no tag field;
/// [`ChatPhase::tag`] states every variant's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ChatPhase {
    /// Before `amux.transcript_ready` (FACT at the amux layer).
    Replaying,
    /// Prompt seen, no turn-end signal (INFERRED — capped by the staleness
    /// timer into Unknown; see `phase(now)`).
    Working,
    /// Turn closed, nothing after. FACT at the authority signal, decaying
    /// to INFERRED (an external session may already be typing).
    Idle { tag: PhaseTag },
    /// A pending ask heads the queue (E1: the request FACT is newer than
    /// any resolving result).
    NeedsYou { why: AskWhy, tag: PhaseTag },
    /// `isApiErrorMessage:true` row (FACT); recovery is only visible as the
    /// next normal assistant message.
    Errored,
    /// Truncation/reset/staleness degradation — never a guess.
    Unknown,
}

/// The prompt-send gate (D2): the draft stays renderer-local and editable;
/// a refusal states why this layer cannot accept it now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendGate {
    Ready,
    Unavailable,
    Exited,
    Replaying,
    Working,
    NeedsYou,
    Unknown,
    SendInFlight,
}

impl SendGate {
    pub fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Unavailable => Some("chat input unavailable for this agent"),
            Self::Exited => Some("agent exited"),
            Self::Replaying => Some("send gated while replaying"),
            Self::Working => Some("send gated while working"),
            Self::NeedsYou => Some("send gated — answer the pending ask"),
            Self::Unknown => Some("send gated — session state unknown"),
            Self::SendInFlight => Some("send gated — a prompt is in flight"),
        }
    }
}

/// Structural invariant classes owned by the Claude layer. The outer
/// kernel violation is namespaced by layer; these keys remain stable for
/// dump throttling.
#[derive(Clone, Debug, PartialEq)]
pub enum ClaudeViolation {
    RetentionOverflow {
        agent: amux::AgentId,
        store: &'static str,
        len: usize,
        cap: usize,
    },
    FeedOrder {
        agent: amux::AgentId,
    },
    IndexAhead {
        agent: amux::AgentId,
        index: &'static str,
        entry: u64,
        next: u64,
    },
    DedupeIncoherent {
        agent: amux::AgentId,
        rows: usize,
        set: usize,
    },
    AskOrder {
        agent: amux::AgentId,
    },
    EchoDuplicate {
        agent: amux::AgentId,
    },
}

impl ClaudeViolation {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::RetentionOverflow { .. } => "claude-retention-overflow",
            Self::FeedOrder { .. } => "claude-feed-order",
            Self::IndexAhead { .. } => "claude-index-ahead",
            Self::DedupeIncoherent { .. } => "claude-dedupe-incoherent",
            Self::AskOrder { .. } => "claude-ask-order",
            Self::EchoDuplicate { .. } => "claude-echo-duplicate",
        }
    }
}

impl std::fmt::Display for ClaudeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RetentionOverflow {
                agent,
                store,
                len,
                cap,
            } => write!(
                f,
                "agent {agent} claude {store} holds {len} entries over the bound of {cap}"
            ),
            Self::FeedOrder { agent } => {
                write!(f, "agent {agent} claude feed id arithmetic is incoherent")
            }
            Self::IndexAhead {
                agent,
                index,
                entry,
                next,
            } => write!(
                f,
                "agent {agent} claude {index} index references entry {entry} past next id {next}"
            ),
            Self::DedupeIncoherent { agent, rows, set } => write!(
                f,
                "agent {agent} claude dedupe queue ({rows}) and set ({set}) disagree"
            ),
            Self::AskOrder { agent } => {
                write!(f, "agent {agent} claude ask id arithmetic is incoherent")
            }
            Self::EchoDuplicate { agent } => {
                write!(f, "agent {agent} claude prompt echoes share an op id")
            }
        }
    }
}

impl ChatPhase {
    /// The E1 fact-vs-inferred tag; `None` for Unknown (degradation has no
    /// epistemic claim to tag).
    pub fn tag(&self) -> Option<PhaseTag> {
        match self {
            ChatPhase::Replaying | ChatPhase::Errored => Some(PhaseTag::Fact),
            ChatPhase::Working => Some(PhaseTag::Inferred),
            ChatPhase::Idle { tag } | ChatPhase::NeedsYou { tag, .. } => Some(*tag),
            ChatPhase::Unknown => None,
        }
    }
}

/// How the last turn closed (idle provenance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnCloseSource {
    /// The in-transcript `turn_duration` row (FACT).
    Authority,
    /// The §17 interrupt artifacts (FACT — the user closed it).
    Interrupt,
}

/// Turn-scoped fold state: the pre-signal reconciliation and timestamp
/// chains duration inferences ride (B3), plus the activity signals the
/// phase derivation reads (E1).
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
    /// A human prompt opened a turn and no turn-end signal closed it (the
    /// phase's working input). Distinct from `prompt_at`, which duration
    /// inference rides and a compaction boundary clears — an auto-compact
    /// mid-turn ends duration chains, not the turn.
    open: bool,
    /// How the last turn closed, with the closing signal's arrival time
    /// (idle FACT decays to INFERRED).
    closed_by: Option<TurnCloseSource>,
    closed_at: Option<DateTime<Utc>>,
    /// An `isApiErrorMessage` row is the newest signal; cleared by the next
    /// normal assistant row, prompt, or interrupt (§20: recovery is only
    /// visible as the next normal message).
    error_live: bool,
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
    /// Kernel subscription catch-up finished (`StreamMsg::ReplayComplete`):
    /// everything arriving after is live even when the in-band ready marker
    /// never came — a long-running session writes past the bounded source
    /// tail, so a late attacher's truncated window no longer CONTAINS the
    /// marker. Liveness is never suppressed by truncation; the honest
    /// missing-history boundary (B9) is `truncated_start`, not this.
    replay_complete: bool,
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
    /// Pending asks in arrival order (C). Outside the feed window: feed
    /// eviction never touches this queue (B9).
    asks: VecDeque<Ask>,
    next_ask_id: u64,
    /// Optimistic prompt echoes awaiting their transcript row (B1).
    echoes: Vec<PromptEcho>,
    /// Newest observed stream seq — the `expected_seq` the write path's
    /// seq guard sends (the source refuses input unless the client has
    /// seen its newest row). Advanced by every delivered row, deduped or
    /// not: the guard tracks the SOURCE, not the fold.
    cursor: u64,
    /// The stream died underneath us (transport loss): whatever the fold
    /// knew is stale. Phase and attention degrade to Unknown until a fresh
    /// window replays; obligations are kept (the replay refolds them).
    stale: bool,
    /// The agent process ended in an orderly way (stream closed
    /// `AgentExited`): a definitive FACT that overrides truncation and
    /// staleness — nothing is running, nothing can need you.
    exited: bool,
    /// Any transcript row folded this window — distinguishes a fresh
    /// session whose file does not exist yet (empty chat) from a replay in
    /// progress (loading), B10.
    transcript_rows_seen: bool,
    /// Arrival time of the newest folded delivery (the batch's observed
    /// `at`): the staleness clock's base.
    last_arrival: Option<DateTime<Utc>>,
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
    /// `arrived` is the shell's observed arrival time from the batch Msg —
    /// the staleness clock's base (time enters through Msgs).
    pub(crate) fn observe(&mut self, seq: u64, arrived: DateTime<Utc>, row: &serde_json::Value) {
        fold::observe(self, seq, arrived, row);
    }

    /// The stream died underneath us (transport loss / relink of the
    /// subscription): whatever the fold knew is stale. Degrade to Unknown,
    /// never to a wrong badge; keep the obligations — the reopened window
    /// replays and refolds them.
    pub(crate) fn invalidate(&mut self) {
        self.stale = true;
    }

    /// The agent process ended in an orderly way: nothing is left to need.
    /// The feed stays readable; the obligations and activity signals do not
    /// outlive the process that owned them. The exit closure is recorded as
    /// a FACT that settles the phase — it overrides truncation and
    /// staleness (a truncated window must not report an exited agent as
    /// Unknown forever).
    pub(crate) fn observe_exit(&mut self) {
        self.asks.clear();
        self.echoes.clear();
        self.turn.open = false;
        self.turn.stop_presignal = false;
        self.turn.closed_by = None;
        self.turn.closed_at = None;
        self.turn.error_live = false;
        self.exited = true;
    }

    /// Kernel subscription catch-up finished: everything after is live.
    /// This is the out-of-band unlock for truncated windows whose tail no
    /// longer contains the in-band `amux.transcript_ready` marker (the most
    /// common real-world attach — a long-running session). A relink resets
    /// it with the window; the relink replay arrives on the live stream and
    /// its fresh in-band marker unlocks that window instead.
    pub(crate) fn observe_replay_complete(&mut self) {
        self.replay_complete = true;
    }

    /// The window is live: the in-band ready marker was seen, or kernel
    /// replay completed without it (truncated-tail attach). Before either,
    /// everything is replay (B10).
    fn live(&self) -> bool {
        self.transcript_ready || self.replay_complete
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

    /// Count of unpaired tool uses (the pairing index ask correlation
    /// reads).
    pub fn open_tool_count(&self) -> usize {
        self.open_tools.len()
    }

    /// The ask queue in arrival order; the head is the docked panel, the
    /// length the honest `(1 of N)` count (C).
    pub fn asks(&self) -> impl Iterator<Item = &Ask> {
        self.asks.iter()
    }

    pub fn ask_head(&self) -> Option<&Ask> {
        self.asks.front()
    }

    pub fn ask_count(&self) -> usize {
        self.asks.len()
    }

    /// Optimistic prompt echoes, oldest first (B1). Rendered after the
    /// feed entries with the dim `sending…` marker.
    pub fn pending_echoes(&self) -> &[PromptEcho] {
        &self.echoes
    }

    /// The newest observed stream seq — the write path's `expected_seq`.
    /// Public for diagnostics and the real-daemon H.7 race assertion; views
    /// have no reason to render or derive from this transport fact.
    #[doc(hidden)]
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// A prompt send was dispatched: the optimistic echo pends until the
    /// transcript's user row reconciles it (C5/B1).
    pub(crate) fn note_prompt_sent(&mut self, op: OpId, text: String, at: Option<DateTime<Utc>>) {
        self.echoes.push(PromptEcho { op, text, at });
        if self.echoes.len() > ECHOES_RETAINED {
            self.echoes.remove(0);
        }
    }

    /// The prompt send failed: the echo leaves; the finished op carries
    /// the failure fact and the draft resurfaces from ViewState (B1).
    pub(crate) fn note_prompt_send_failed(&mut self, op: OpId) {
        self.echoes.retain(|echo| echo.op != op);
    }

    /// An answer was dispatched for an ask: flip it to the optimistic
    /// state carrying the pending-marker data (C5). `false` when the ask
    /// is gone (remote resolution won before dispatch folded).
    pub(crate) fn note_ask_answered(&mut self, ask: u64, op: OpId, answer: AskAnswer) -> bool {
        match self.asks.iter_mut().find(|entry| entry.id == ask) {
            Some(entry) => {
                entry.state = AskState::AnsweredOptimistic { op, answer };
                true
            }
            None => false,
        }
    }

    /// The answer send failed: resurface the ask with the failure stated
    /// (C5). Only the op that flipped it may fail it — a late failure for
    /// a superseded answer must not clobber a newer one — and an ask that
    /// already left the queue stays gone (remote resolution wins).
    pub(crate) fn note_ask_send_failed(&mut self, ask: u64, op: OpId, message: String) {
        if let Some(entry) = self.asks.iter_mut().find(|entry| entry.id == ask)
            && matches!(&entry.state, AskState::AnsweredOptimistic { op: in_flight, .. } if *in_flight == op)
        {
            entry.state = AskState::SendFailed { message };
        }
    }

    /// The derived session phase (the E1 table), including the kernel's
    /// subscription lifecycle. `now` enters via `Msg::Tick` and caps
    /// inferred states.
    pub fn phase(&self, stream: Option<&StreamState>, now: Option<DateTime<Utc>>) -> ChatPhase {
        match stream.map(|stream| &stream.phase) {
            Some(StreamPhase::Opening | StreamPhase::Replaying) => ChatPhase::Replaying,
            Some(StreamPhase::Live)
            | Some(StreamPhase::Closed {
                reason:
                    crate::msg::StreamCloseReason::AgentExited { .. }
                    | crate::msg::StreamCloseReason::AgentDeleted,
            }) => self.folded_phase(now),
            _ => ChatPhase::Unknown,
        }
    }

    fn folded_phase(&self, now: Option<DateTime<Utc>>) -> ChatPhase {
        if self.exited {
            // Orderly process termination is a FACT that overrides
            // truncation and staleness: nothing is running, nothing can
            // need you. The card's own `phase` carries the exit itself.
            return ChatPhase::Idle {
                tag: PhaseTag::Fact,
            };
        }
        if self.stale {
            return ChatPhase::Unknown;
        }
        if !self.live() {
            // Mid-replay of an existing transcript is Replaying; a window
            // with no transcript rows at all and no truncation is a fresh
            // session whose file does not exist yet (B10) — an idle empty
            // chat, inferred.
            return if self.transcript_rows_seen || self.truncated_start {
                ChatPhase::Replaying
            } else {
                ChatPhase::Idle {
                    tag: PhaseTag::Inferred,
                }
            };
        }
        if let Some(ask) = self.asks.front() {
            // The request FACT is newer than any resolving result (E1):
            // resolution facts remove asks from the queue as they land.
            return ChatPhase::NeedsYou {
                why: ask.why(),
                tag: PhaseTag::Fact,
            };
        }
        if self.turn.error_live {
            return ChatPhase::Errored;
        }
        if self.turn.open {
            if self.turn.stop_presignal {
                // Arrival-ordered pre-signal: the turn ended, the
                // transcript tail has not caught up (INFERRED until the
                // authority lands).
                return ChatPhase::Idle {
                    tag: PhaseTag::Inferred,
                };
            }
            if self.working_is_stale(now) {
                return ChatPhase::Unknown;
            }
            return ChatPhase::Working;
        }
        if self.turn.closed_by.is_some() {
            let fresh = match (now, self.turn.closed_at) {
                (Some(now), Some(at)) => now - at <= TimeDelta::seconds(IDLE_FACT_DECAY_SECS),
                _ => true,
            };
            return ChatPhase::Idle {
                tag: if fresh {
                    PhaseTag::Fact
                } else {
                    PhaseTag::Inferred
                },
            };
        }
        // No evidence at all: over a truncated window that is Unknown —
        // absence of evidence in a bounded history is not evidence of
        // idleness.
        if self.truncated_start {
            ChatPhase::Unknown
        } else {
            ChatPhase::Idle {
                tag: PhaseTag::Inferred,
            }
        }
    }

    /// D2's single send decision, shared by reducer and renderer.
    pub fn send_gate(
        &self,
        agent_phase: &AgentPhase,
        stream: Option<&StreamState>,
        now: Option<DateTime<Utc>>,
    ) -> SendGate {
        if matches!(agent_phase, AgentPhase::Exited { .. }) {
            return SendGate::Exited;
        }
        if !self.pending_echoes().is_empty() {
            return SendGate::SendInFlight;
        }
        match self.phase(stream, now) {
            ChatPhase::Idle { .. } | ChatPhase::Errored => SendGate::Ready,
            ChatPhase::Replaying => SendGate::Replaying,
            ChatPhase::Working => SendGate::Working,
            ChatPhase::NeedsYou { .. } => SendGate::NeedsYou,
            ChatPhase::Unknown => SendGate::Unknown,
        }
    }

    /// D4's permission-mode cycle gate. It is allowed exactly when the
    /// injected Shift+Tab would reach Claude's composer.
    pub fn mode_cycle_gate(
        &self,
        agent_phase: &AgentPhase,
        stream: Option<&StreamState>,
        now: Option<DateTime<Utc>>,
    ) -> Option<&'static str> {
        if matches!(agent_phase, AgentPhase::Exited { .. }) {
            return Some("agent exited");
        }
        match self.phase(stream, now) {
            ChatPhase::Idle { .. } | ChatPhase::Working | ChatPhase::Errored => None,
            ChatPhase::Replaying => Some("mode cycle gated while replaying"),
            ChatPhase::NeedsYou { .. } => Some("mode cycle gated while an ask is pending"),
            ChatPhase::Unknown => Some("mode cycle gated — session state unknown"),
        }
    }

    /// Kernel attention derived from the SAME interpretation as the phase
    /// (E2: one fold, not two). Time-free so the cached card value stays
    /// coherent between Ticks; the read-time staleness degrade lives in
    /// `Model::effective_attention`, keeping fleet and chat in agreement
    /// (E3).
    pub fn attention(&self) -> Attention {
        if self.exited {
            // Definitive termination: nothing left to need.
            return Attention::Idle;
        }
        if self.stale {
            return Attention::Unknown;
        }
        if !self.live() {
            return if self.transcript_rows_seen || self.truncated_start {
                Attention::Unknown
            } else {
                Attention::Idle
            };
        }
        if let Some(ask) = self.asks.front() {
            return Attention::NeedsYou {
                why: match ask.why() {
                    AskWhy::Permission => Why::Permission,
                    AskWhy::Question => Why::Question,
                },
            };
        }
        if self.turn.error_live {
            // The kernel vocabulary cannot say "errored"; Unknown is the
            // honest word — never a wrong badge (retries may already be
            // running invisibly).
            return Attention::Unknown;
        }
        if self.turn.open {
            return if self.turn.stop_presignal {
                Attention::NeedsYou { why: Why::Finished }
            } else {
                Attention::Working
            };
        }
        match self.turn.closed_by {
            // The turn finished and nothing came after: come look.
            Some(TurnCloseSource::Authority) => Attention::NeedsYou { why: Why::Finished },
            // The user closed it deliberately; nothing to come look at.
            Some(TurnCloseSource::Interrupt) => Attention::Idle,
            None => {
                if self.truncated_start {
                    Attention::Unknown
                } else {
                    Attention::Idle
                }
            }
        }
    }

    /// The E1 staleness cap: the working inference is stale once no
    /// delivery has been observed for [`WORKING_STALENESS_CAP_SECS`].
    pub(crate) fn working_is_stale(&self, now: Option<DateTime<Utc>>) -> bool {
        match (now, self.last_arrival) {
            (Some(now), Some(at)) => now - at > TimeDelta::seconds(WORKING_STALENESS_CAP_SECS),
            _ => false,
        }
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
            ("asks", self.asks.len(), ASKS_RETAINED),
            ("echoes", self.echoes.len(), ECHOES_RETAINED),
        ] {
            if len > cap {
                out.push(Violation::Claude(ClaudeViolation::RetentionOverflow {
                    agent,
                    store,
                    len,
                    cap,
                }));
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
            out.push(Violation::Claude(ClaudeViolation::FeedOrder { agent }));
        }

        if self.seen_rows.len() != self.seen_set.len() {
            out.push(Violation::Claude(ClaudeViolation::DedupeIncoherent {
                agent,
                rows: self.seen_rows.len(),
                set: self.seen_set.len(),
            }));
        }

        // Ask arithmetic: ids are assigned monotonically and the queue is
        // append-only at the back, so retained ids are strictly increasing
        // and below the next id.
        let ask_ids_coherent = self
            .asks
            .iter()
            .zip(self.asks.iter().skip(1))
            .all(|(a, b)| a.id < b.id)
            && self.asks.back().is_none_or(|ask| ask.id < self.next_ask_id);
        if !ask_ids_coherent {
            out.push(Violation::Claude(ClaudeViolation::AskOrder { agent }));
        }

        // Echo identity: each dispatched send mints one op, so two echoes
        // sharing an op would mean a double-recorded dispatch.
        let echo_ops_distinct = self
            .echoes
            .iter()
            .enumerate()
            .all(|(i, a)| self.echoes[i + 1..].iter().all(|b| a.op != b.op));
        if !echo_ops_distinct {
            out.push(Violation::Claude(ClaudeViolation::EchoDuplicate { agent }));
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
                out.push(Violation::Claude(ClaudeViolation::IndexAhead {
                    agent,
                    index,
                    entry,
                    next: self.next_entry_id,
                }));
            }
        }
    }
}

/// Claude chat phase for one card. This small typed accessor keeps kernel
/// lifecycle facts at the layer boundary without teaching `Model` a
/// Claude-specific phase.
pub fn phase(model: &Model, agent: amux::AgentId) -> ChatPhase {
    model.claude(agent).map_or(ChatPhase::Unknown, |layer| {
        layer.phase(model.stream(agent), model.now())
    })
}

/// D2's shared renderer/reducer gate for one Claude card.
pub fn send_gate(model: &Model, agent: amux::AgentId) -> SendGate {
    let Some(card) = model.agent(agent) else {
        return SendGate::Unavailable;
    };
    let Some(layer) = card.claude() else {
        return SendGate::Unavailable;
    };
    layer.send_gate(&card.phase, model.stream(agent), model.now())
}

/// D4's permission-mode cycle gate for one Claude card.
pub fn mode_cycle_gate(model: &Model, agent: amux::AgentId) -> Option<&'static str> {
    let Some(card) = model.agent(agent) else {
        return Some("chat input unavailable for this agent");
    };
    let Some(layer) = card.claude() else {
        return Some("chat input unavailable for this agent");
    };
    layer.mode_cycle_gate(&card.phase, model.stream(agent), model.now())
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
                "terminal_v1".to_string(),
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
            .layer
            .as_mut()
            .and_then(crate::model::AgentLayer::claude_mut)
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

    #[test]
    fn detects_duplicate_echo_ops() {
        let mut model = a_model_with_a_folded_layer();
        let layer = layer_mut(&mut model);
        for _ in 0..2 {
            layer.echoes.push(PromptEcho {
                op: OpId(Uuid::from_u128(41)),
                text: "hi".to_string(),
                at: None,
            });
        }
        assert!(fires(&model, "claude-echo-duplicate"));
    }

    #[test]
    fn detects_ask_id_incoherence() {
        let mut model = a_model_with_a_folded_layer();
        let layer = layer_mut(&mut model);
        // An ask id at or past the mint counter breaks the monotonic rule.
        layer.asks.push_back(Ask {
            id: layer.next_ask_id + 3,
            seq: 9,
            tool_use_id: None,
            kind: AskKind::Question {
                questions: Vec::new(),
            },
            state: AskState::Pending,
            artifact: None,
            hook_key: None,
        });
        assert!(fires(&model, "claude-ask-order"));
    }
}
