//! The Claude feed fold: one native row in, typed feed facts out.
//!
//! Tolerate-unknown runs in both directions (G1): typed extraction where
//! `notes/chat-v1/transcript-semantics.md` names a shape, retained-as-
//! unknown otherwise. No rule gates on key-set equality or crashes on an
//! absent field — the format is internal to Claude Code and drifts by
//! version. Unknown rows become explicit unrecognized entries, never
//! silent drops.
//!
//! Render order is file order; `parentUuid` is used for nothing here —
//! pairing rides `message.id`, `tool_use.id`, and `interruptedMessageId`,
//! which the survey verified are redundant with the DAG edges.

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::artifact::{self, AskArtifact};
use super::{
    ASKS_RETAINED, AcceptedPlan, AgentMessageEntry, ApiErrorEntry, Ask, AskKind, AskState,
    ClaudeLayer, CompactSummaryEntry, CompactionEntry, FEED_RETAINED, FeedEntry, FeedEntryKind,
    InterruptionEntry, InterruptionKind, MESSAGES_RETAINED, MessageEntry, MessageFinality,
    MessageSlot, OPEN_TOOLS_RETAINED, OUTPUT_HEAD_MAX, OpenTool, PLANS_RETAINED, PromptEntry,
    PromptSource, QuestionAnswer, QuestionFact, QuestionOption, SEEN_ROWS_RETAINED, SlotState,
    SuccessFacts, SuggestionFact, TaskNotificationEntry, ThinkingEntry, ToolEntry, ToolInvocation,
    ToolOutcome, TurnCloseSource, TurnDuration, TurnEntry, UnrecognizedEntry, envelope,
};

// --- tolerant readers -------------------------------------------------------

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn u64_of(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn bool_of(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

fn string_of(value: &Value, key: &str) -> Option<String> {
    str_of(value, key).map(str::to_string)
}

/// The row's capture timestamp (envelope FACT), when present and parseable.
fn timestamp_of(row: &Value) -> Option<DateTime<Utc>> {
    str_of(row, "timestamp")
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// Bounded head of a text, char-safe; `true` when it was cut.
fn head_of(text: &str) -> (String, bool) {
    match text.char_indices().nth(OUTPUT_HEAD_MAX) {
        Some((byte, _)) => (text[..byte].to_string(), true),
        None => (text.to_string(), false),
    }
}

/// The interrupt artifacts are canonical strings (§17, FACT).
const INTERRUPT_TURN: &str = "[Request interrupted by user]";
const INTERRUPT_TOOL: &str = "[Request interrupted by user for tool use]";

// --- the fold ---------------------------------------------------------------

pub(super) fn observe(layer: &mut ClaudeLayer, seq: u64, arrived: DateTime<Utc>, row: &Value) {
    // Every delivery is liveness evidence — the base of the E1 staleness
    // clock (a crashed claude delivers nothing).
    layer.last_arrival = Some(arrived);
    // The seq-guard cursor tracks the SOURCE: every delivered row advances
    // it, deduped or not (the guard asks "has the client seen the newest
    // row", not "did the fold keep it").
    layer.cursor = layer.cursor.max(seq);

    let kind = str_of(row, "type");

    // amux-layer rows first: they carry no transcript identity.
    match kind {
        Some("amux.transcript_ready") => {
            layer.transcript_ready = true;
            return;
        }
        Some("hook.stop") | Some("hook.permission_request") => {
            // Hook rows carry no uuid, historical streams carry duplicate
            // deliveries (§18b — two hook registrations each delivered
            // every event), and a shrink re-replay repeats them all: the
            // bounded content-hash dedupe makes hook folds idempotent per
            // window, so a re-replay can never resurrect a resolved ask as
            // pending. Trade-off (bounded, recorded): a byte-identical
            // genuine re-request within the dedupe window folds once —
            // payloads carry `prompt_id` and `tool_input`, so distinct
            // events collide only when truly identical.
            if !remember(layer, content_key(row)) {
                return;
            }
            // The effective permission mode rides hook payloads — the D4
            // live source: a mid-session Shift+Tab cycle emits NO
            // `permission-mode` row (§18d), so the row alone goes stale.
            // Latest-wins across both sources by arrival.
            if let Some(mode) = str_of(row, "permission_mode") {
                layer.session.permission_mode = Some(mode.to_string());
            }
            if kind == Some("hook.stop") {
                // Arrival-ordered turn-end pre-signal (B3);
                // `stop_hook_active` means a stop hook forced continuation
                // — not an end.
                if !bool_of(row, "stop_hook_active") {
                    layer.turn.stop_presignal = true;
                    // An ended turn is incompatible with a blocking ask:
                    // the pre-signal is also the catch-up closer when the
                    // resolving transcript rows lag behind (remote
                    // resolution, C5).
                    layer.asks.clear();
                }
            } else {
                // The pending-ask FACT (C): fires for tool permissions AND
                // for AskUserQuestion/ExitPlanMode — extraction routes on
                // `tool_name`.
                fold_permission_request(layer, seq, row);
            }
            return;
        }
        // Notification wording is forbidden interpretation ground (E2): the
        // plan-approval notification says "needs your approval" with no
        // "permission" substring (fixture-verified). Every signal it could
        // carry arrives better-typed elsewhere, so it folds to nothing.
        Some("hook.notification") => return,
        _ => {}
    }

    // Relink epoch guard: `sessionId` always equals the tailed file's
    // basename (§2, verified), so a differing id IS the relink — the
    // buffer was cleared and a new file is replaying (§16: the only
    // reliable `/clear` signal). Hook rows never reach here, so their
    // arrival-ordered `session_id` cannot false-trigger an epoch.
    if let Some(session_id) = str_of(row, "sessionId") {
        match layer.session_id.as_deref() {
            Some(current) if current != session_id => {
                let truncated = false;
                layer.begin_window(truncated);
                layer.session_id = Some(session_id.to_string());
                // The reset wiped the arrival clock and seq cursor the
                // triggering row set; this row is a delivery of the new
                // window (seqs keep increasing across relinks, §1).
                layer.last_arrival = Some(arrived);
                layer.cursor = seq;
            }
            Some(_) => {}
            None => layer.session_id = Some(session_id.to_string()),
        }
    }

    // Everything from here down is a transcript row: the replay-vs-fresh
    // discriminator (B10 — a fresh session has no transcript file yet).
    layer.transcript_rows_seen = true;

    // Row-uuid dedupe: a source-shrink re-replay repeats the file prefix;
    // the fold must be idempotent by row uuid (B10).
    if let Some(uuid) = str_of(row, "uuid").and_then(|text| Uuid::parse_str(text).ok())
        && !remember(layer, uuid)
    {
        return;
    }

    match kind {
        Some("user") => fold_user(layer, seq, arrived, row),
        Some("assistant") => fold_assistant(layer, seq, row),
        Some("system") => fold_system(layer, seq, arrived, row),
        // Attachments are system-injected context riding a turn — never
        // feed entries (`docs/CHAT.md` §The feed).
        Some("attachment") => touch_row_chain(layer, row),
        // Checkpointing bookkeeping (§11) and session-state facts (§10):
        // latest-wins state, never feed entries.
        Some("file-history-snapshot") | Some("file-history-delta") => {}
        Some("mode") => {}
        Some("permission-mode") => {
            layer.session.permission_mode = string_of(row, "permissionMode");
        }
        Some("ai-title") => layer.session.ai_title = string_of(row, "aiTitle"),
        Some("agent-name") => layer.session.agent_name = string_of(row, "agentName"),
        // Queue lifecycle and DAG-leaf bookkeeping: known, no feed target
        // in V1 (queueing is a reserved door).
        Some("last-prompt") | Some("queue-operation") => {}
        other => {
            // Unknown shapes may carry no uuid at all (the vanished
            // `progress`/`summary` generations) — B10's re-replay
            // idempotency still applies to the entries they produce, so
            // they dedupe by content hash within the same bounded window.
            if str_of(row, "uuid").is_none() && !remember(layer, content_key(row)) {
                return;
            }
            push(
                layer,
                seq,
                FeedEntryKind::Unrecognized(UnrecognizedEntry {
                    row_type: other.map(str::to_string),
                    detail: None,
                }),
            );
            // Unknown rows advance the timestamp chain like every other
            // row: a thinking marker after one must measure from it, not
            // from an older row (the chain rule below).
            touch_row_chain(layer, row);
        }
    }
}

/// Record one row identity in the bounded dedupe window; `false` when the
/// row was already folded this window (a re-replay).
fn remember(layer: &mut ClaudeLayer, key: Uuid) -> bool {
    if layer.seen_set.contains(&key) {
        return false;
    }
    layer.seen_set.insert(key);
    layer.seen_rows.push_back(key);
    if layer.seen_rows.len() > SEEN_ROWS_RETAINED
        && let Some(evicted) = layer.seen_rows.pop_front()
    {
        layer.seen_set.remove(&evicted);
    }
    true
}

/// FNV-1a over concatenated byte parts: the one content hash every no-uuid
/// identity in this fold rides. Explicit (not std's hasher) because the
/// values land in serialized model state and must be stable across builds.
fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in parts.iter().flat_map(|part| part.iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Deterministic identity for a row without a uuid: FNV-1a over its
/// canonical JSON (serde_json maps are key-sorted), domain-tagged so it
/// cannot collide with a genuine row uuid's random bits.
fn content_key(row: &Value) -> Uuid {
    let hash = fnv1a(&[row.to_string().as_bytes()]);
    Uuid::from_u128((0xc0deu128 << 64) | u128::from(hash))
}

/// Every uuid row advances the thinking-duration chain (§15: previous uuid
/// row in file order), even when it makes no entry. Timestamp-gated, so
/// rows without one leave the chain alone.
fn touch_row_chain(layer: &mut ClaudeLayer, row: &Value) {
    if let Some(at) = timestamp_of(row) {
        layer.turn.last_row_at = Some(at);
    }
}

// --- asks -------------------------------------------------------------------

/// Content identity of one ask request: FNV-1a over the tool name and the
/// canonical JSON of its input (serde_json objects iterate key-sorted). The
/// hook's `tool_input` equals the transcript `tool_use.input` byte-for-byte
/// (fixture-verified), so one key both dedupes the at-least-once hook
/// delivery and correlates the transcript row to the hook-born ask.
fn ask_key(tool_name: &str, input: &Value) -> u64 {
    fnv1a(&[tool_name.as_bytes(), &[0x1f], input.to_string().as_bytes()])
}

/// The ask kind for one request, from the same typed per-tool extraction
/// the feed's tool entries use — one interpretation, both consumers (E2).
/// `suggestions` come from the hook payload (the transcript-only fallback
/// has none — its permission menu shape is unknown, which the C6 encoder
/// states honestly).
fn ask_kind(tool_name: Option<&str>, input: &Value, suggestions: Vec<SuggestionFact>) -> AskKind {
    match extract_invocation(tool_name.unwrap_or_default(), input) {
        ToolInvocation::Question { questions } => AskKind::Question { questions },
        invocation => AskKind::Permission {
            tool_name: tool_name.map(str::to_string),
            invocation,
            suggestions,
        },
    }
}

/// `permission_suggestions` facts from the hook payload, tolerantly.
fn extract_suggestions(row: &Value) -> Vec<SuggestionFact> {
    row.get("permission_suggestions")
        .and_then(Value::as_array)
        .map(|suggestions| {
            suggestions
                .iter()
                .map(|suggestion| SuggestionFact {
                    kind: string_of(suggestion, "type"),
                    destination: string_of(suggestion, "destination"),
                    directories: suggestion
                        .get("directories")
                        .and_then(Value::as_array)
                        .map(|directories| {
                            directories
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ask's typed panel/reader body (C2), computed once at creation — the
/// one place a diff is ever computed (diff-rendering §1.4: the transcript
/// states no diff at ask time). Routed on the tool name like everything
/// else; tools without a body artifact carry `None`.
fn ask_artifact(tool_name: Option<&str>, input: &Value) -> Option<AskArtifact> {
    match tool_name {
        Some("Edit") => {
            let old = str_of(input, "old_string")?;
            let new = str_of(input, "new_string")?;
            Some(AskArtifact::Diff(artifact::ask_time_diff(
                old,
                new,
                bool_of(input, "replace_all"),
            )))
        }
        Some("Write") => Some(AskArtifact::NewFile {
            content: str_of(input, "content")?.to_string(),
        }),
        _ => None,
    }
}

fn push_ask(
    layer: &mut ClaudeLayer,
    seq: u64,
    tool_use_id: Option<String>,
    session_ask_id: String,
    hook_key: Option<u64>,
    kind: AskKind,
    artifact: Option<AskArtifact>,
) {
    let id = layer.next_ask_id;
    layer.next_ask_id += 1;
    layer.asks.push_back(Ask {
        id,
        seq,
        tool_use_id,
        session_ask_id,
        kind,
        state: AskState::Pending,
        artifact,
        hook_key,
    });
    if layer.asks.len() > ASKS_RETAINED {
        layer.asks.pop_front();
    }
}

/// The amux `hook.permission_request` row: the pending-ask FACT
/// (arrival-ordered — it normally precedes the transcript tail). Routing is
/// on `tool_name`, never notification wording: AskUserQuestion is a
/// question ask, ExitPlanMode a plan-review permission, anything else a
/// plain permission with its typed per-tool payload.
fn fold_permission_request(layer: &mut ClaudeLayer, seq: u64, row: &Value) {
    let tool_name = str_of(row, "tool_name");
    let input = row.get("tool_input").unwrap_or(&Value::Null);
    let key = ask_key(tool_name.unwrap_or_default(), input);
    // Hook delivery is at-least-once (§18b: every hook row observed twice —
    // multiple registrations each deliver the identical payload): a request
    // identical to one already queued is the same event re-delivered, not a
    // new ask. After resolution the ask has left the queue, so a genuine
    // re-ask with identical content becomes a new ask.
    if layer.asks.iter().any(|ask| ask.hook_key == Some(key)) {
        return;
    }
    let kind = ask_kind(tool_name, input, extract_suggestions(row));
    let artifact = ask_artifact(tool_name, input);
    push_ask(
        layer,
        seq,
        None,
        hook_ask_id(row, tool_name.unwrap_or_default()),
        Some(key),
        kind,
        artifact,
    );
}

/// The name the session gives a hook-announced ask, derived here exactly as
/// the session derives it: the tool_use id the permission hook carries when
/// it has one, else the prompt id, else a name built from the session and
/// the tool — the fallback for the permission hooks that carry neither
/// (fixture-verified). An ask no answer could address would be a menu the
/// user can see and not act on, so this never fails.
fn hook_ask_id(row: &Value, tool_name: &str) -> String {
    if let Some(id) = str_of(row, "tool_use_id").or_else(|| str_of(row, "prompt_id")) {
        return id.to_string();
    }
    let session = str_of(row, "session_id").unwrap_or_default();
    format!("permission:{session}:{tool_name}")
}

// --- user rows --------------------------------------------------------------

fn fold_user(layer: &mut ClaudeLayer, seq: u64, arrived: DateTime<Utc>, row: &Value) {
    // A message from another amux agent arrives INSIDE the recipient's own
    // text, by either carrier — so it is read before the meta filter (the
    // inbox carrier's row is meta) and before the prompt path (the paste
    // carrier's row wears human discriminators it did not earn).
    if let Some(text) = row.pointer("/message/content").and_then(Value::as_str)
        && let Some(message) = envelope::read(text)
    {
        fold_agent_message(layer, seq, row, message);
        return;
    }
    // Injected meta rows (caveats, hook feedback) are the session's own
    // bookkeeping in EITHER content form (§5) — filtered on the top-level
    // discriminator before any interrupt/tool/closure handling, so a meta
    // row can never abandon a message or masquerade as content.
    if bool_of(row, "isMeta") {
        touch_row_chain(layer, row);
        return;
    }
    let content = row.pointer("/message/content");
    match content {
        Some(Value::String(text)) => fold_user_text(layer, seq, row, text),
        Some(Value::Array(blocks)) => {
            // The interrupt artifacts are text blocks in array content
            // (§17); detect them before the generic closure so the closure
            // is tagged Interrupted, not Abandoned.
            let interrupt = blocks.iter().find_map(|block| match str_of(block, "text") {
                Some(INTERRUPT_TURN) => Some(InterruptionKind::Turn),
                Some(INTERRUPT_TOOL) => Some(InterruptionKind::ToolUse),
                _ => None,
            });
            if let Some(kind) = interrupt {
                fold_interrupt(layer, seq, arrived, row, kind);
                return;
            }
            // A user row closes any still-open message as abandoned (B2).
            close_messages(layer, MessageFinality::Abandoned, slot_open);
            for block in blocks {
                match str_of(block, "type") {
                    Some("tool_result") => fold_tool_result(layer, seq, row, block),
                    other => {
                        push(
                            layer,
                            seq,
                            FeedEntryKind::Unrecognized(UnrecognizedEntry {
                                row_type: Some("user".to_string()),
                                detail: other.map(str::to_string),
                            }),
                        );
                    }
                }
            }
            touch_row_chain(layer, row);
        }
        _ => {
            push(
                layer,
                seq,
                FeedEntryKind::Unrecognized(UnrecognizedEntry {
                    row_type: Some("user".to_string()),
                    detail: Some("no message content".to_string()),
                }),
            );
        }
    }
}

/// An inbound agent message. It renders as itself, never as a prompt: the
/// human did not say this, and a row that claimed otherwise would let a
/// peer borrow their voice. The turn bookkeeping still follows the row's
/// own discriminators — when the harness treated the delivery as the start
/// of a turn, the recipient IS working, and saying otherwise would leave a
/// wrong badge on the fleet.
fn fold_agent_message(
    layer: &mut ClaudeLayer,
    seq: u64,
    row: &Value,
    message: envelope::InboundMessage,
) {
    let starts_turn = row.pointer("/origin/kind").and_then(Value::as_str) == Some("human")
        || matches!(
            str_of(row, "promptSource"),
            Some("typed" | "queued" | "suggestion_accepted")
        );
    push(
        layer,
        seq,
        FeedEntryKind::AgentMessage(AgentMessageEntry {
            id: message.id,
            context: message.context,
            from: message.from,
            kind: message.kind,
            text: message.text,
        }),
    );
    if starts_turn {
        begin_turn(layer, timestamp_of(row));
    }
    touch_row_chain(layer, row);
}

fn fold_user_text(layer: &mut ClaudeLayer, seq: u64, row: &Value, text: &str) {
    close_messages(layer, MessageFinality::Abandoned, slot_open);

    // The post-compaction summary row (§16): renderable, flagged
    // transcript-only at the source.
    if bool_of(row, "isCompactSummary") {
        push(
            layer,
            seq,
            FeedEntryKind::CompactSummary(CompactSummaryEntry {
                text: text.to_string(),
            }),
        );
        touch_row_chain(layer, row);
        return;
    }

    // Local-command records (`<command-name>…`, `<local-command-stdout>…`)
    // are the session's own bookkeeping, not conversation (§5). Known and
    // deliberately not feed entries. (isMeta rows were filtered before the
    // content dispatch.)
    if text.starts_with("<command-") || text.starts_with("<local-command-") {
        touch_row_chain(layer, row);
        return;
    }

    let origin_kind = row.pointer("/origin/kind").and_then(Value::as_str);
    let prompt_source = str_of(row, "promptSource");

    // Background-subagent completion notice (B7): FACT that it finished,
    // content is prose — one line, no child-file tailing.
    if origin_kind == Some("task-notification") || prompt_source == Some("system") {
        push(
            layer,
            seq,
            FeedEntryKind::TaskNotification(TaskNotificationEntry {
                text: text.to_string(),
            }),
        );
        touch_row_chain(layer, row);
        return;
    }

    // An origin kind we do not know is not a prompt claim we can make.
    if let Some(kind) = origin_kind
        && kind != "human"
    {
        push(
            layer,
            seq,
            FeedEntryKind::Unrecognized(UnrecognizedEntry {
                row_type: Some("user".to_string()),
                detail: Some(format!("origin {kind}")),
            }),
        );
        touch_row_chain(layer, row);
        return;
    }

    let source = match prompt_source {
        Some("typed") => PromptSource::Typed,
        Some("queued") => PromptSource::Queued,
        Some("suggestion_accepted") => PromptSource::SuggestionAccepted,
        Some(other) => PromptSource::Other {
            label: other.to_string(),
        },
        None if origin_kind == Some("human") => PromptSource::Human,
        // No discriminator at all: older rows, or a bare local-command
        // record like `/compact` (Phase 1 fixture observation). Rendered,
        // never a turn start.
        None => PromptSource::Unstated,
    };

    let at = timestamp_of(row);
    // Turn start (§14): `origin.kind:"human"` is the definitive
    // discriminator — an unknown `promptSource` label is decoration and
    // must not silently alter turn semantics (tolerate-unknown degrades
    // gracefully). Without an origin, the known human source labels stand
    // in; a fully unstated row never starts a turn.
    let turn_start = origin_kind == Some("human")
        || matches!(
            source,
            PromptSource::Typed | PromptSource::Queued | PromptSource::SuggestionAccepted
        );
    // B1 echo reconciliation: an injected prompt lands as a human row
    // whose string content is byte-identical to the pasted text (§18d) —
    // the optimistic echo collapses into the real entry. Oldest match
    // only: two echoes with one landing must not both reconcile.
    if turn_start && let Some(reconciled) = layer.echoes.iter().position(|echo| echo.text == text) {
        layer.echoes.remove(reconciled);
    }
    push(
        layer,
        seq,
        FeedEntryKind::Prompt(PromptEntry {
            text: text.to_string(),
            source,
            prompt_id: string_of(row, "promptId"),
        }),
    );
    if turn_start {
        begin_turn(layer, at);
    }
    touch_row_chain(layer, row);
}

/// Turn start (§14 FACT): reset the turn-scoped signals.
fn begin_turn(layer: &mut ClaudeLayer, at: Option<DateTime<Utc>>) {
    layer.turn.prompt_at = at;
    layer.turn.stop_presignal = false;
    layer.turn.inferred_turn_entry = None;
    layer.turn.open = true;
    layer.turn.closed_by = None;
    layer.turn.closed_at = None;
    layer.turn.error_live = false;
    // A new turn implies nothing was blocking: any ask still queued is
    // stale (its closing rows fell outside the window).
    layer.asks.clear();
}

fn fold_interrupt(
    layer: &mut ClaudeLayer,
    seq: u64,
    arrived: DateTime<Utc>,
    row: &Value,
    kind: InterruptionKind,
) {
    let interrupted_message_id = string_of(row, "interruptedMessageId");

    // Interrupt closes every ask (C5): the user chose to cut the turn
    // instead of answering — the panel dismisses and the facts render.
    layer.asks.clear();

    // Close the message it cut off: FACT-paired by `interruptedMessageId`
    // when that slot is still indexed, otherwise any still-open message
    // (§17: a null-stop message followed by an interrupt row is
    // final-as-interrupted).
    match interrupted_message_id
        .as_deref()
        .filter(|id| layer.messages.iter().any(|slot| slot.id == *id))
    {
        Some(id) => close_messages(layer, MessageFinality::Interrupted, |slot| slot.id == id),
        None => close_messages(layer, MessageFinality::Interrupted, slot_open),
    }

    push(
        layer,
        seq,
        FeedEntryKind::Interruption(InterruptionEntry {
            kind,
            interrupted_message_id,
        }),
    );

    // Interrupt-ended turns have no `turn_duration`; the marker shows
    // elapsed-from-prompt, tagged inferred (B3) — reconciled in place if
    // the authority lands after all (observed on tool-use denials).
    if let (Some(prompt_at), Some(at)) = (layer.turn.prompt_at, timestamp_of(row)) {
        let ms = (at - prompt_at).num_milliseconds().max(0);
        let entry = push(
            layer,
            seq,
            FeedEntryKind::Turn(TurnEntry {
                duration: TurnDuration::SincePrompt { ms },
                message_count: None,
                pending_background_agents: None,
            }),
        );
        layer.turn.inferred_turn_entry = Some(entry);
    }

    // Durations are never computed across an interrupt (B3), and the turn
    // is over — closed by the user (FACT, §17 rows).
    layer.turn.last_row_at = None;
    layer.turn.prompt_at = None;
    layer.turn.stop_presignal = false;
    layer.turn.open = false;
    layer.turn.closed_by = Some(TurnCloseSource::Interrupt);
    layer.turn.closed_at = Some(arrived);
    layer.turn.error_live = false;
}

/// A still-open message slot — the target of every "this row ends the
/// request" closure (B2).
fn slot_open(slot: &MessageSlot) -> bool {
    slot.state == SlotState::Open
}

/// Close the message slots `selects` picks, as an inference: each moves to
/// `ClosedInferred`, and its entry — when still retained and still `Open`
/// — takes `closure`. A FACT-final slot is never touched, and a closing
/// FACT that lands later still upgrades (`fold_assistant`).
fn close_messages(
    layer: &mut ClaudeLayer,
    closure: MessageFinality,
    selects: impl Fn(&MessageSlot) -> bool,
) {
    let mut closed_entries: Vec<u64> = Vec::new();
    for slot in &mut layer.messages {
        if slot.state == SlotState::FinalFact || !selects(slot) {
            continue;
        }
        slot.state = SlotState::ClosedInferred;
        if let Some(entry) = slot.entry {
            closed_entries.push(entry);
        }
    }
    for entry in closed_entries {
        if let Some(FeedEntryKind::Message(message)) = entry_kind_mut(layer, entry)
            && message.finality == MessageFinality::Open
        {
            message.finality = closure.clone();
        }
    }
}

// --- tool results -----------------------------------------------------------

fn fold_tool_result(layer: &mut ClaudeLayer, seq: u64, row: &Value, block: &Value) {
    let Some(tool_use_id) = str_of(block, "tool_use_id") else {
        push(
            layer,
            seq,
            FeedEntryKind::Unrecognized(UnrecognizedEntry {
                row_type: Some("user".to_string()),
                detail: Some("tool_result without tool_use_id".to_string()),
            }),
        );
        return;
    };

    let outcome = tool_outcome(row, block);

    // The result resolves the correlated ask whatever its state — allow
    // and deny alike, local answer or remote (C5): the ask leaves the
    // queue and the collapsed B5 fact renders from the tool entry.
    layer
        .asks
        .retain(|ask| ask.tool_use_id.as_deref() != Some(tool_use_id));

    // B6: an approved plan is retained as session state keyed by tool_use
    // id, outside feed windowing — even if the feed entry was evicted.
    if matches!(
        outcome,
        ToolOutcome::Success {
            facts: SuccessFacts::PlanApproved { .. }
        }
    ) {
        retain_plan(layer, tool_use_id, row);
    }

    let open = layer
        .open_tools
        .iter()
        .position(|tool| tool.tool_use_id == tool_use_id);
    match open {
        Some(index) => {
            let open = layer
                .open_tools
                .remove(index)
                .expect("position came from the same deque");
            if let Some(FeedEntryKind::Tool(tool)) = entry_kind_mut(layer, open.entry) {
                tool.outcome = outcome;
                tool.message_final = true;
            }
            // else: the entry was evicted; the pairing fact resolved an
            // obligation that outlived the bytes, which is exactly the
            // retention rule (B9).
        }
        None => {
            // An orphan result: its tool_use fell outside the window.
            // Rendered honestly as a result-only line.
            push(
                layer,
                seq,
                FeedEntryKind::Tool(ToolEntry {
                    tool_use_id: tool_use_id.to_string(),
                    name: None,
                    invocation: ToolInvocation::Other,
                    outcome,
                    message_final: true,
                    group_with_previous: false,
                    message_id: None,
                }),
            );
        }
    }
}

fn tool_outcome(row: &Value, block: &Value) -> ToolOutcome {
    if bool_of(block, "is_error") {
        // Denial is a typed fact (`toolDenialKind`), never an error-string
        // sniff (B5/G2).
        return match string_of(row, "toolDenialKind") {
            Some(kind) => ToolOutcome::Denied { kind: Some(kind) },
            None => ToolOutcome::Failed {
                message: result_text(block).map(|(head, _)| head),
            },
        };
    }
    ToolOutcome::Success {
        facts: success_facts(row, block),
    }
}

/// Typed result facts where the sidecar shape is named (§12); bounded
/// generic output otherwise.
fn success_facts(row: &Value, block: &Value) -> SuccessFacts {
    let sidecar = row.get("toolUseResult");
    if let Some(sidecar) = sidecar.filter(|sidecar| sidecar.is_object()) {
        if let (Some(file_path), Some(patch)) = (
            str_of(sidecar, "filePath"),
            sidecar.get("structuredPatch").and_then(Value::as_array),
        ) {
            let (mut added, removed) = patch_magnitude(patch);
            // A Write that CREATED the file carries an empty patch (Phase 1
            // fixture observation); the honest magnitude is the created
            // content's line count.
            if added == 0 && removed == 0 && str_of(sidecar, "type") == Some("create") {
                added = str_of(sidecar, "content")
                    .map(|content| content.lines().count() as u64)
                    .unwrap_or(0);
            }
            return SuccessFacts::Edit {
                file_path: file_path.to_string(),
                added,
                removed,
            };
        }
        if let Some(answers) = sidecar.get("answers").and_then(Value::as_object) {
            return SuccessFacts::Answers {
                answers: answers
                    .iter()
                    .filter_map(|(question, answer)| {
                        answer.as_str().map(|answer| QuestionAnswer {
                            question: question.clone(),
                            answer: answer.to_string(),
                        })
                    })
                    .collect(),
            };
        }
        match str_of(sidecar, "status") {
            Some("completed") => {
                return SuccessFacts::TaskCompleted {
                    agent_id: string_of(sidecar, "agentId"),
                    duration_ms: u64_of(sidecar, "totalDurationMs"),
                    tool_count: u64_of(sidecar, "totalToolUseCount"),
                };
            }
            Some("async_launched") => {
                return SuccessFacts::TaskLaunched {
                    agent_id: string_of(sidecar, "agentId"),
                };
            }
            _ => {}
        }
        if sidecar.get("plan").is_some() {
            return SuccessFacts::PlanApproved {
                plan_file_path: string_of(sidecar, "filePath"),
            };
        }
    }
    match result_text(block) {
        Some((head, truncated)) => SuccessFacts::Output { head, truncated },
        None => SuccessFacts::None,
    }
}

/// The `tool_result` block's content as bounded text: a string, or the
/// first `text` block of an array (oversized outputs carry a notice plus a
/// `tool_reference` — the full text stays on disk).
fn result_text(block: &Value) -> Option<(String, bool)> {
    match block.get("content") {
        Some(Value::String(text)) => Some(head_of(text)),
        Some(Value::Array(parts)) => parts
            .iter()
            .find_map(|part| str_of(part, "text"))
            .map(head_of),
        _ => None,
    }
}

/// Change magnitude from `structuredPatch` hunks (FACT — the transcript
/// states every landed edit; the client never recomputes one).
fn patch_magnitude(patch: &[Value]) -> (u64, u64) {
    let mut added = 0;
    let mut removed = 0;
    for hunk in patch {
        let Some(lines) = hunk.get("lines").and_then(Value::as_array) else {
            continue;
        };
        for line in lines.iter().filter_map(Value::as_str) {
            match line.as_bytes().first() {
                Some(b'+') => added += 1,
                Some(b'-') => removed += 1,
                _ => {}
            }
        }
    }
    (added, removed)
}

fn retain_plan(layer: &mut ClaudeLayer, tool_use_id: &str, row: &Value) {
    if layer
        .plans
        .iter()
        .any(|plan| plan.tool_use_id == tool_use_id)
    {
        return;
    }
    let sidecar = row.get("toolUseResult");
    let plan = sidecar
        .and_then(|sidecar| str_of(sidecar, "plan"))
        .map(str::to_string)
        .or_else(|| {
            // Fall back to the tool_use input payload retained on the entry.
            let tool = layer
                .open_tools
                .iter()
                .find(|tool| tool.tool_use_id == tool_use_id)?;
            match entry_kind(layer, tool.entry) {
                Some(FeedEntryKind::Tool(ToolEntry {
                    invocation: ToolInvocation::Plan { plan, .. },
                    ..
                })) => plan.clone(),
                _ => None,
            }
        });
    let Some(plan) = plan else { return };
    layer.plans.push(AcceptedPlan {
        tool_use_id: tool_use_id.to_string(),
        plan,
    });
    if layer.plans.len() > PLANS_RETAINED {
        layer.plans.remove(0);
    }
}

// --- assistant rows ---------------------------------------------------------

fn fold_assistant(layer: &mut ClaudeLayer, seq: u64, row: &Value) {
    // API errors are synthetic rows (plain-uuid id, `<synthetic>` model) —
    // status entries, not messages (B8/§20). Their arrival still means the
    // request that carried any open message is over: B2's new-message-id
    // closure applies before the error is recorded, so no null-stop
    // message is left "streaming" behind an error.
    if bool_of(row, "isApiErrorMessage") {
        close_messages(layer, MessageFinality::Abandoned, slot_open);
        let text = row
            .pointer("/message/content")
            .and_then(Value::as_array)
            .and_then(|blocks| blocks.iter().find_map(|block| str_of(block, "text")))
            .map(str::to_string);
        push(
            layer,
            seq,
            FeedEntryKind::ApiError(ApiErrorEntry {
                error: string_of(row, "error"),
                text,
            }),
        );
        // Phase FACT (E1): errored until the next normal signal. The turn
        // stays open — retries run invisibly and may recover it.
        layer.turn.error_live = true;
        touch_row_chain(layer, row);
        return;
    }

    // Recovery from an API error is only visible as the next normal
    // assistant message (§20).
    layer.turn.error_live = false;

    let message = row.get("message");
    let message_id = message
        .and_then(|message| str_of(message, "id"))
        .or_else(|| str_of(row, "uuid"))
        .unwrap_or("unknown")
        .to_string();
    let stop_reason = message
        .and_then(|message| str_of(message, "stop_reason"))
        .map(str::to_string);

    // A new message id closes any OTHER still-open message as abandoned
    // (B2) — same-id rows are the normal multi-row upsert.
    close_messages(layer, MessageFinality::Abandoned, |slot| {
        slot.id != message_id && slot_open(slot)
    });

    let at = timestamp_of(row);
    let previous_row_at = layer.turn.last_row_at;

    if !layer.messages.iter().any(|slot| slot.id == message_id) {
        layer.messages.push_back(MessageSlot {
            id: message_id.clone(),
            entry: None,
            state: SlotState::Open,
        });
        if layer.messages.len() > MESSAGES_RETAINED {
            layer.messages.pop_front();
        }
    }

    let blocks: Vec<Value> = match message.and_then(|message| message.get("content")) {
        Some(Value::Array(blocks)) => blocks.clone(),
        // Tolerate string content as a single text segment.
        Some(Value::String(text)) => vec![serde_json::json!({"type": "text", "text": text})],
        _ => Vec::new(),
    };

    for block in &blocks {
        match str_of(block, "type") {
            Some("text") => {
                let text = str_of(block, "text").unwrap_or_default().to_string();
                append_message_text(layer, seq, &message_id, text);
            }
            kind @ (Some("thinking") | Some("redacted_thinking")) => {
                let redacted = kind == Some("redacted_thinking");
                // INFERRED from FACT timestamps; the chain is broken (None)
                // across interrupts and compaction (B3/§15).
                let duration_ms = match (previous_row_at, at) {
                    (Some(previous), Some(at)) => Some((at - previous).num_milliseconds().max(0)),
                    _ => None,
                };
                push(
                    layer,
                    seq,
                    FeedEntryKind::Thinking(ThinkingEntry {
                        duration_ms,
                        redacted,
                    }),
                );
            }
            Some("tool_use") => fold_tool_use(layer, seq, &message_id, &stop_reason, block),
            other => {
                push(
                    layer,
                    seq,
                    FeedEntryKind::Unrecognized(UnrecognizedEntry {
                        row_type: Some("assistant".to_string()),
                        detail: other.map(str::to_string),
                    }),
                );
            }
        }
    }

    // Finality is FACT on a non-null stop_reason and wins over any earlier
    // inferred closure (B2).
    if let Some(stop_reason) = stop_reason {
        finalize_message(layer, &message_id, stop_reason);
    } else {
        // A late null-stop row for an already-closed message must not
        // reopen a "streaming" display: re-tag the inferred closure over
        // whatever the block appends created.
        let closed = layer
            .messages
            .iter()
            .find(|slot| slot.id == message_id)
            .is_some_and(|slot| slot.state == SlotState::ClosedInferred);
        if closed {
            close_messages(layer, MessageFinality::Abandoned, |slot| {
                slot.id == message_id
            });
        }
    }
    touch_row_chain(layer, row);
}

fn append_message_text(layer: &mut ClaudeLayer, seq: u64, message_id: &str, text: String) {
    let existing = layer
        .messages
        .iter()
        .find(|slot| slot.id == message_id)
        .and_then(|slot| slot.entry);
    if let Some(entry) = existing
        && let Some(FeedEntryKind::Message(message)) = entry_kind_mut(layer, entry)
    {
        message.segments.push(text);
        return;
    }
    let entry = push(
        layer,
        seq,
        FeedEntryKind::Message(MessageEntry {
            message_id: message_id.to_string(),
            segments: vec![text],
            finality: MessageFinality::Open,
        }),
    );
    if let Some(slot) = layer.messages.iter_mut().find(|slot| slot.id == message_id) {
        slot.entry = Some(entry);
    }
}

fn fold_tool_use(
    layer: &mut ClaudeLayer,
    seq: u64,
    message_id: &str,
    stop_reason: &Option<String>,
    block: &Value,
) {
    let tool_use_id = str_of(block, "id").unwrap_or_default().to_string();
    let name = str_of(block, "name").unwrap_or_default().to_string();
    let input = block.get("input").unwrap_or(&Value::Null);
    let invocation = extract_invocation(&name, input);

    // Grouping fact (B4): strictly consecutive read/search one-liners.
    let group_with_previous = groupable(&invocation)
        && matches!(
            layer.entries.back().map(|entry| &entry.kind),
            Some(FeedEntryKind::Tool(previous)) if groupable(&previous.invocation)
        );

    let entry = push(
        layer,
        seq,
        FeedEntryKind::Tool(ToolEntry {
            tool_use_id: tool_use_id.clone(),
            name: Some(name.clone()),
            invocation,
            outcome: ToolOutcome::Pending,
            message_final: stop_reason.is_some(),
            group_with_previous,
            message_id: Some(message_id.to_string()),
        }),
    );

    if tool_use_id.is_empty() {
        return;
    }
    layer.open_tools.push_back(OpenTool {
        tool_use_id: tool_use_id.clone(),
        entry,
        message_id: Some(message_id.to_string()),
    });
    if layer.open_tools.len() > OPEN_TOOLS_RETAINED {
        layer.open_tools.pop_front();
    }

    correlate_ask(layer, seq, &tool_use_id, &name, input, stop_reason);
}

/// Attach the transcript identity to the ask this `tool_use` embodies (C).
///
/// The hook payload carries no tool_use id, so a hook-born ask is matched
/// by content key — the hook's `tool_input` equals this block's `input`
/// byte-for-byte (fixture-verified). Where no hook ask exists, the unpaired
/// AskUserQuestion/ExitPlanMode in a FINAL message is itself the ask
/// (FACT-grade pairing rule): the fallback where hook rows are absent, and
/// the kind-match fallback covers an input-drifted hook. A plain tool's
/// unpaired use is "running", never a permission inference — the
/// transcript-only INFERRED-and-laggy permission detection (E1's read-only
/// failure mode) is deliberately not implemented in V1.
fn correlate_ask(
    layer: &mut ClaudeLayer,
    seq: u64,
    tool_use_id: &str,
    name: &str,
    input: &Value,
    stop_reason: &Option<String>,
) {
    let key = ask_key(name, input);
    if let Some(ask) = layer
        .asks
        .iter_mut()
        .find(|ask| ask.tool_use_id.is_none() && ask.hook_key == Some(key))
    {
        ask.tool_use_id = Some(tool_use_id.to_string());
        return;
    }
    if stop_reason.is_none() || !matches!(name, "AskUserQuestion" | "ExitPlanMode") {
        return;
    }
    let uncorrelated_same_kind = layer.asks.iter_mut().find(|ask| {
        ask.tool_use_id.is_none()
            && matches!(
                (&ask.kind, name),
                (AskKind::Question { .. }, "AskUserQuestion")
                    | (
                        AskKind::Permission {
                            invocation: ToolInvocation::Plan { .. },
                            ..
                        },
                        "ExitPlanMode",
                    )
            )
    });
    match uncorrelated_same_kind {
        Some(ask) => ask.tool_use_id = Some(tool_use_id.to_string()),
        // The transcript-only fallback exists for question and plan asks
        // only; neither carries a body artifact (the plan markdown rides
        // the invocation).
        // A transcript-born ask is named by its tool_use id on both sides.
        None => push_ask(
            layer,
            seq,
            Some(tool_use_id.to_string()),
            tool_use_id.to_string(),
            Some(key),
            ask_kind(Some(name), input, Vec::new()),
            None,
        ),
    }
}

fn groupable(invocation: &ToolInvocation) -> bool {
    matches!(
        invocation,
        ToolInvocation::Read { .. } | ToolInvocation::Query { .. }
    )
}

fn extract_invocation(name: &str, input: &Value) -> ToolInvocation {
    match name {
        "Edit" => ToolInvocation::Edit {
            file_path: string_of(input, "file_path"),
            replace_all: bool_of(input, "replace_all"),
        },
        "Write" => ToolInvocation::Write {
            file_path: string_of(input, "file_path"),
        },
        "Bash" => ToolInvocation::Bash {
            command: string_of(input, "command"),
            description: string_of(input, "description"),
        },
        "Read" => ToolInvocation::Read {
            file_path: string_of(input, "file_path"),
        },
        "Grep" | "Glob" => ToolInvocation::Query {
            text: string_of(input, "pattern"),
        },
        "WebSearch" | "ToolSearch" => ToolInvocation::Query {
            text: string_of(input, "query"),
        },
        "WebFetch" => ToolInvocation::Query {
            text: string_of(input, "url"),
        },
        crate::claude::MCP_SEND_TOOL => ToolInvocation::AmuxSend {
            to: string_of(input, "to"),
            text: string_of(input, "text"),
        },
        "Task" | "Agent" => ToolInvocation::Task {
            description: string_of(input, "description"),
            subagent_type: string_of(input, "subagent_type"),
            background: bool_of(input, "run_in_background"),
        },
        "AskUserQuestion" => ToolInvocation::Question {
            questions: input
                .get("questions")
                .and_then(Value::as_array)
                .map(|questions| questions.iter().map(question_fact).collect())
                .unwrap_or_default(),
        },
        "ExitPlanMode" => ToolInvocation::Plan {
            plan: string_of(input, "plan"),
            plan_file_path: string_of(input, "planFilePath"),
        },
        _ => ToolInvocation::Other,
    }
}

fn question_fact(question: &Value) -> QuestionFact {
    QuestionFact {
        header: string_of(question, "header"),
        question: string_of(question, "question"),
        multi_select: bool_of(question, "multiSelect"),
        options: question
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        // Options are `{label, description}` objects (Phase
                        // 0 capture); tolerate the older plain-string form.
                        match option {
                            Value::String(label) => Some(QuestionOption {
                                label: label.clone(),
                                description: None,
                            }),
                            _ => str_of(option, "label").map(|label| QuestionOption {
                                label: label.to_string(),
                                description: string_of(option, "description"),
                            }),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn finalize_message(layer: &mut ClaudeLayer, message_id: &str, stop_reason: String) {
    let entry = layer.messages.iter_mut().find_map(|slot| {
        if slot.id != message_id {
            return None;
        }
        slot.state = SlotState::FinalFact;
        slot.entry
    });
    if let Some(entry) = entry
        && let Some(FeedEntryKind::Message(message)) = entry_kind_mut(layer, entry)
    {
        message.finality = MessageFinality::Final { stop_reason };
    }
    // Unpaired tools of a now-final message render as running (B4).
    let tool_entries: Vec<u64> = layer
        .open_tools
        .iter()
        .filter(|tool| tool.message_id.as_deref() == Some(message_id))
        .map(|tool| tool.entry)
        .collect();
    for entry in tool_entries {
        if let Some(FeedEntryKind::Tool(tool)) = entry_kind_mut(layer, entry) {
            tool.message_final = true;
        }
    }
}

// --- system rows ------------------------------------------------------------

fn fold_system(layer: &mut ClaudeLayer, seq: u64, arrived: DateTime<Utc>, row: &Value) {
    match str_of(row, "subtype") {
        Some("turn_duration") => {
            // Zero is not a fact: a row without a readable `durationMs`
            // cannot claim a measured duration (and must not overwrite a
            // better inferred marker) — it degrades to an unrecognized
            // entry instead, uninterpreted.
            let Some(duration_ms) = u64_of(row, "durationMs") else {
                push(
                    layer,
                    seq,
                    FeedEntryKind::Unrecognized(UnrecognizedEntry {
                        row_type: Some("system".to_string()),
                        detail: Some("turn_duration without durationMs".to_string()),
                    }),
                );
                touch_row_chain(layer, row);
                return;
            };
            let turn = TurnEntry {
                duration: TurnDuration::Measured { ms: duration_ms },
                message_count: u64_of(row, "messageCount"),
                pending_background_agents: u64_of(row, "pendingBackgroundAgentCount"),
            };
            // The authority reconciles an inferred marker in place when the
            // turn already closed by interrupt (observed: tool-use denials
            // still emit `turn_duration`).
            let reconciled = layer.turn.inferred_turn_entry.take().is_some_and(|entry| {
                match entry_kind_mut(layer, entry) {
                    Some(FeedEntryKind::Turn(existing)) => {
                        *existing = turn.clone();
                        true
                    }
                    _ => false,
                }
            });
            if !reconciled {
                push(layer, seq, FeedEntryKind::Turn(turn));
            }
            layer.turn.stop_presignal = false;
            layer.turn.prompt_at = None;
            // The turn-end authority (§14 FACT): the turn is over, and an
            // ended turn is incompatible with a blocking ask — any ask
            // still queued was resolved out-of-window or by the same
            // signals that ended the turn.
            layer.turn.open = false;
            layer.turn.closed_by = Some(TurnCloseSource::Authority);
            layer.turn.closed_at = Some(arrived);
            layer.turn.error_live = false;
            layer.asks.clear();
            touch_row_chain(layer, row);
        }
        // The stop-hook execution report rides ~2ms before its paired
        // `turn_duration` (§8): known, no separate feed entry.
        Some("stop_hook_summary") => touch_row_chain(layer, row),
        Some("compact_boundary") => {
            let metadata = row.get("compactMetadata").unwrap_or(&Value::Null);
            push(
                layer,
                seq,
                FeedEntryKind::Compaction(CompactionEntry {
                    trigger: string_of(metadata, "trigger"),
                    pre_tokens: u64_of(metadata, "preTokens"),
                    post_tokens: u64_of(metadata, "postTokens"),
                }),
            );
            // Durations are never computed across a compaction (B3): the
            // thinking chain, the elapsed prompt base, and any pending
            // marker reconciliation all end at the boundary.
            layer.turn.last_row_at = None;
            layer.turn.prompt_at = None;
            layer.turn.inferred_turn_entry = None;
        }
        // Idle-return summaries and local-command records (§8): known, not
        // feed entries in V1.
        Some("away_summary") | Some("local_command") => touch_row_chain(layer, row),
        other => {
            push(
                layer,
                seq,
                FeedEntryKind::Unrecognized(UnrecognizedEntry {
                    row_type: Some("system".to_string()),
                    detail: other.map(str::to_string),
                }),
            );
            touch_row_chain(layer, row);
        }
    }
}

// --- entry plumbing ---------------------------------------------------------

/// Append one entry, evicting from the front past the retention bound
/// (B9). Eviction never touches `open_tools` or `plans` — bytes are
/// evicted, obligations and keyed session state are not. Returns the new
/// entry's id.
fn push(layer: &mut ClaudeLayer, seq: u64, kind: FeedEntryKind) -> u64 {
    let id = layer.next_entry_id;
    layer.next_entry_id += 1;
    layer.entries.push_back(FeedEntry { id, seq, kind });
    if layer.entries.len() > FEED_RETAINED {
        layer.entries.pop_front();
        layer.evicted += 1;
    }
    id
}

/// Mutable access to a retained entry's kind by id; `None` when it was
/// evicted (ids below the front are gone, and that is tolerated
/// everywhere).
fn entry_kind_mut(layer: &mut ClaudeLayer, id: u64) -> Option<&mut FeedEntryKind> {
    let front = layer.entries.front()?.id;
    let index = id.checked_sub(front)? as usize;
    layer.entries.get_mut(index).map(|entry| &mut entry.kind)
}

fn entry_kind(layer: &ClaudeLayer, id: u64) -> Option<&FeedEntryKind> {
    let front = layer.entries.front()?.id;
    let index = id.checked_sub(front)? as usize;
    layer.entries.get(index).map(|entry| &entry.kind)
}
