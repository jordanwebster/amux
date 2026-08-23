# Claude Code transcript semantics — chat v1 grounding spec

Status: from-first-principles spec of the transcript JSONL rows a chat
client receives through amux's `claude_pty_transcript_v1` stream. Grounds
requirements B2, B3, B7, C5, E1. Written 2026-08-11.

The provider-neutral message envelope, delivery fallback, and family
lifecycle are owned by [`A2A.md`](./A2A.md). This document owns only the
Claude rows those carriers produce.

Evidence base:

- 13 main session files + 30+ subagent files under `~/.claude/projects/`
  (~10,100 rows), Claude Code versions **2.1.198, 2.1.220, 2.1.221,
  2.1.226, 2.1.227**. All quoted rows are structure-only; prose, paths,
  and code are redacted as `…`.
- One **live observation**: a subagent transcript sampled mid-stream
  while its message was still being generated (the only way to see
  write-timing behavior that completed files hide).
- Official docs: sessions (<https://code.claude.com/docs/en/sessions.md>),
  hooks reference (<https://code.claude.com/docs/en/hooks.md>). The docs
  explicitly say the entry format "is internal to Claude Code and changes
  between versions". So: **files win over docs, and this spec must be
  re-validated against `version` drift** (see §21).
- Third-party parsers agree on the load-bearing rules (dedupe by
  `message.id`, `parentUuid` DAG, `toolUseResult.agentId` subagent
  linkage): e.g. <https://piebald.ai/blog/messages-as-commits-claude-codes-git-like-dag-of-conversations>,
  <https://www.adityabawankule.io/blog/claude-code-session-jsonl-format>,
  <https://danyuchn.github.io/blog/posts/en/claude-code-jsonl-internals/>.

Confidence vocabulary used throughout:

- **FACT** — stated by a row field; no interpretation beyond reading it.
- **INFERRED** — derived by a stated rule; failure modes listed.
- **UNOBSERVED** — not present in local evidence; rule proposed from docs
  only and must be confirmed by the real-Claude E2E suite (req. H).

---

## 1. What the client actually receives (the amux stream)

The chat layer never reads the file. It receives, in order, from
`StructuredLogSource` (`crates/amux/src/agents/log_source.rs`,
`crates/amux/src/agents/claude/transcript.rs`):

1. **Transcript rows** — each non-empty line of the linked JSONL file,
   parsed as opaque JSON, replayed from the start of the file (catch-up),
   then live-tailed. Invalid/partial lines are silently skipped.
2. **`{"type":"amux.transcript_ready"}`** — synthetic marker emitted once
   per link when catch-up reaches EOF. Everything before it is replay
   (B10's "loading"); everything after is live.
3. **amux hook rows** — the raw Claude Code hook JSON with a `type` field
   injected (`crates/amux/src/agents/claude/session/hooks.rs`):
   `hook.permission_request`, `hook.stop`, `hook.notification`.
   SessionStart/SessionEnd are consumed internally, not emitted.
4. On **relink** (session file changes — `/clear`, resume, fork): the
   buffer is cleared, the new file is replayed from its beginning, and a
   fresh `amux.transcript_ready` follows. Seq numbers keep increasing.

Consequences:

- Hook rows interleave with transcript rows in **arrival** order, not
  transcript order. A `hook.stop` can arrive before the tail has caught
  up to the turn's final rows. Never assume a hook row sits at a
  transcript position.
- Catch-up replays the whole file; the bounded buffer (1000 entries)
  means long sessions replay only a tail → B9's explicit "earlier
  history unavailable" boundary.
- The tailer only recovers from file *shrinkage* (seek to 0 → full
  re-replay). Clients must treat a repeated prefix after
  `amux.transcript_ready` as a re-replay, not new content (idempotent
  fold by row `uuid`).

### amux hook row shapes

The payload is Claude Code's hook stdin JSON, passed through raw.
Verified common fields (parsed by `agents/claude/hooks.rs`; also in the
hooks doc): `session_id` (uuid string), `transcript_path`, `cwd`,
`hook_event_name`. Docs add `permission_mode`, `prompt_id`, and (in
subagents) `agent_id`/`agent_type`. Event-specific fields (docs; not
capturable from files since hook payloads aren't persisted verbatim):

| type | hook_event_name | extra fields |
|---|---|---|
| `hook.permission_request` | `PermissionRequest` | `tool_name`, `tool_input` (fixture-verified in amux tests); docs also show a `hookSpecificOutput.decision` *response* shape, not input |
| `hook.stop` | `Stop` | `stop_hook_active` (docs) |
| `hook.notification` | `Notification` | `message` (docs); a notification kind exists — transcript `async_hook_response` attachments record hook names `Notification:idle_prompt`, `Notification:permission_prompt`, `Notification:auth_success` |

Treat all extra fields as tolerate-unknown; amux injects only `type`.

---

## 2. File layout on disk (context for relinks and subagents)

```
~/.claude/projects/<project-slug>/
  <sessionId>.jsonl                     # main transcript (amux tails this)
  <sessionId>/subagents/agent-<id>.jsonl        # one per subagent (isSidechain)
  <sessionId>/subagents/agent-<id>.meta.json    # {agentType, description, toolUseId, parentAgentId, spawnDepth}
  <sessionId>/tool-results/<id>.txt             # persisted oversized tool outputs
  memory/                               # not transcript data
```

- `sessionId` inside rows always equals the file's basename (verified: no
  foreign sessionIds in any file). `session_id` (snake_case), present on
  some rows, always equals `sessionId`.
- `/clear` starts a **new session file** (docs); amux relinks via the
  SessionStart hook. `/compact` stays **in the same file** (verified:
  `compact_boundary` rows mid-file).

---

## 3. Row taxonomy and frequency

Top-level `type`, all files, versions 2.1.198–2.1.227:

| type | main files | subagent files | freq | uuid/parentUuid? |
|---|---:|---:|---|---|
| `assistant` | 2021 | 2427 | high | yes |
| `user` | 1190 | 1728 | high | yes |
| `attachment` | 543 | 99 | high | yes |
| `system` | 461 | 0 | med | yes |
| `permission-mode` | 306 | 0 | med | **no** |
| `mode` | 306 | 0 | med | **no** |
| `last-prompt` | 302 | 0 | med | **no** |
| `ai-title` | 300 | 0 | med | **no** |
| `file-history-snapshot` | 157 | 0 | med | **no** |
| `queue-operation` | 144 | 0 | med | **no** |
| `file-history-delta` | 97 | 0 | low | **no** |

**Absent in these versions** (present in older format generations that
existing clients/heuristics were built against): `summary`, `progress`,
`agent-name`. The fixtures in `transcript.rs` tests that exercise
`progress` / `agent-name` / `system.turn_duration`-with-`duration_ms`
shapes do not match current files (current field is `durationMs`).
G1's tolerate-unknown entry is the correct stance in both directions.

---

## 4. Common envelope (rows with `uuid`)

`user`, `assistant`, `system`, `attachment` rows share:

| field | type | semantics |
|---|---|---|
| `uuid` | uuid string | row identity. Unique per row; the upsert/idempotency key for the fold |
| `parentUuid` | uuid \| null | DAG edge (see below) |
| `timestamp` | ISO-8601 Z, ms | capture time of the row's *content* (for assistant rows: block completion), **not** guaranteed write time; not strictly monotonic in file order (38/1305 small inversions observed, typically 1–2 ms around attachments) |
| `sessionId` | uuid string | = file basename |
| `session_id` | uuid string | present on a subset of rows; always equals `sessionId` |
| `type` | string | row type |
| `isSidechain` | bool | `false` in main files, `true` in subagent files (never absent) |
| `userType` | `"external"` | only value observed |
| `cwd`, `gitBranch` | string | environment at row time |
| `version` | semver string | Claude Code version that wrote the row; can change mid-file (upgrade + resume) |
| `entrypoint` | `"cli"` | only value observed |
| `slug` | string | optional; auto-generated session slug; appears once assigned, then on subsequent rows |
| `isMeta` | bool | optional; `true` marks injected/meta rows not typed by the user |

### parentUuid semantics

- Rows form a **DAG, not a list**: 99% of rows parent to the immediately
  preceding uuid-bearing row, but **parallel tool use branches**: each
  `tool_result` user row parents to the `assistant` row that carries its
  `tool_use` block, so with two parallel tools the observed pattern is
  `A1←A2, A1←R1, A2←R2` while file order is `A1 A2 R1 R2`. The next
  assistant row parents to the *last* result row (merge).
- `parentUuid: null` occurs at: session's first user row, subagent
  file's first row, and `compact_boundary` rows (which instead carry
  `logicalParentUuid` = pre-compact leaf).
- **Rule for the chat fold: render in file order; use `parentUuid` only
  for pairing/attribution, never for ordering.** Branch/altered-history
  cases (resume-with-edit) were not observed locally but are documented
  by third parties; file order remains the correct render order because
  Claude Code rewrites/append-continues the active branch.

---

## 5. `user` rows

`message` is always `{role: "user", content}`; `content` is a **string**
(typed prompts, injected meta text) or an **array** of blocks
(`tool_result`, rarely `text`).

Variants, distinguished by fields (not by a subtype):

| variant | discriminator | notes |
|---|---|---|
| Human prompt | `content` string; recent versions: `origin.kind:"human"`, `promptSource:"typed"` (also seen: `"queued"`, `"suggestion_accepted"`); `promptId` uuid | B1 reconciliation target. `promptId` is carried by every subsequent `tool_result` user row of the turn → turn grouping key |
| Tool result | `content[0].type == "tool_result"`; `sourceToolAssistantUUID` = uuid of the assistant row carrying the matching `tool_use` (verified); `toolUseResult` sidecar (§12) | one row per tool_result |
| Task notification | `origin.kind:"task-notification"`, `promptSource:"system"` | background-subagent completion notices (§13) |
| Meta/injected | `isMeta: true`, content string or `[text]` | local-command caveats, hook feedback |
| Interrupt | see §17 | `interruptedMessageId` |
| Compact summary | `isCompactSummary: true`, `isVisibleInTranscriptOnly: true` | see §16 |

Rare fields: `mcpMeta: {structuredContent}` (MCP tool results),
`classifierMetaLines` (string), `toolDenialKind` (§18),
`sourceToolUseID` (meta rows generated by a tool).

---

## 6. `assistant` rows — message identity, upserts, write timing

**One content block per row.** `message.content` always has length 1
(4529/4529 rows). A single API message spans 1–25 rows (commonly 1–3),
all sharing:

- `message.id` (`msg_*`) — the **message identity / upsert key** (B2).
- `requestId` (`req_*`) — 1:1 with `message.id` (verified; absent on
  synthetic error rows).

`message` fields: `id`, `type:"message"`, `role:"assistant"`, `model`,
`content[1]`, `stop_reason`, `stop_sequence`, `stop_details` (null in
all observed rows), `usage` (§19), `diagnostics` (null or
`{cache_miss_reason}`). Rare: `container`, `context_management`.
Top-level extras: `effort` (`"medium"` / `"xhigh"` observed — thinking
effort), `attributionMcpServer`/`attributionMcpTool`,
`attributionSkill` (rows produced under MCP/skill attribution).

### Write timing — the critical main/sidechain split (verified live)

| | subagent (`isSidechain`) files | main session files |
|---|---|---|
| when a block row is appended | **as each block finishes streaming** | **all rows of the message appear at (or after) message completion** |
| `stop_reason` on non-final rows | `null` until the final block row | final value on *every* row |
| `usage` on non-final rows | partial (streaming counts, e.g. `output_tokens: 3 → 851`) | final value on every row |
| observed proof | live mid-message tail: last row had `stop_reason: null`, partial usage; completed messages retain the nulls forever | 0 null `stop_reason` in 4 of 5 live-current files; the single null row was an interrupt flush (§17) |

`timestamp` on each row is the **block completion time** in both cases
(verified: multi-row main-file messages carry timestamps spread over
~12 s even though the rows land together), so durations computed from
timestamps remain valid even where arrival is bursty.

**Upsert rule (B2):**

1. Key on `message.id`. Append each row's single block in file order.
   Row `uuid` dedupes re-replays.
2. A message is **final** (FACT) when any of its rows carries a non-null
   `stop_reason` (`end_turn`, `tool_use`, `stop_sequence`, `refusal`…).
3. A message is **abandoned** if a new `message.id`, a user row, or an
   interrupt row (§17) arrives while `stop_reason` is still null.
4. **Streaming display**: in main-file tailing there is *no* mid-message
   row flow — "streaming" cannot be rendered from partial rows and must
   not be promised; in sidechain tailing rows do stream per block.
   Treat "message still streaming" as INFERRED from `stop_reason == null`
   on the newest row.

Do not assume main files never stream per block (the burst behavior is
an observed property, not a contract); rule 1–3 is correct under both
behaviors.

---

## 7. Content blocks

| block | fields | notes |
|---|---|---|
| `text` | `text` | markdown source for B2 rendering |
| `thinking` | `thinking`, `signature` | §15 |
| `redacted_thinking` | opaque `data` | observed in 4 files; render as a redacted-thinking marker |
| `tool_use` | `id` (`toolu_*`), `name`, `input` | one per row |
| `tool_result` (user rows) | `tool_use_id`, `content` (string \| array of `text` / `tool_reference`), optional `is_error` | `tool_reference` blocks point at persisted `tool-results/` files |

---

## 8. `system` rows (`subtype` discriminated)

| subtype | count | fields beyond envelope | meaning |
|---|---:|---|---|
| `turn_duration` | 199 | `durationMs`, `messageCount`, `isMeta:false`, opt `pendingBackgroundAgentCount` | end-of-turn marker. `durationMs` = wall time from the turn's user prompt row to this row (verified twice: 56722 ms vs 56735 ms wall; 13325 vs 13331). `messageCount` = cumulative conversation messages |
| `stop_hook_summary` | 196 | `hookCount`, `hookInfos:[{command}]`, `hookErrors:[]`, `hookAdditionalContext`, `preventedContinuation:false`, `stopReason:""`, `hasOutput`, `level:"suggestion"`, `toolUseID` (uuid, not `toolu_*`) | Stop-hook execution report; written ~2 ms **before** its paired `turn_duration`. `preventedContinuation:true` would mean a stop hook blocked the stop (UNOBSERVED) |
| `away_summary` | 64 | `content` (string, redacted), `isMeta:false` | summary generated when the user returns after idle; appears minutes after a turn ended. Not a turn boundary |
| `local_command` | 2 | `content`, `level:"info"` | record of a local slash/`!` command |
| `compact_boundary` | 2 | §16 | compaction marker |

Turn-end row order (FACT, both observed versions):
`…last assistant row ← stop_hook_summary ← turn_duration`, chained by
`parentUuid`.

---

## 9. `attachment` rows

Envelope + `attachment: {type, …}`. These are system-injected context
riding a turn (they chain via `parentUuid` right after the user row they
accompany, with timestamps that may precede the prompt row by 1 ms —
the observed monotonicity inversions). Observed `attachment.type`:

| attachment.type | count | fields |
|---|---:|---|
| `async_hook_response` | 318 | `hookEvent`, `hookName`, `exitCode`, `processId`, `response`, `stdout`, `stderr` — the transcript's own record of async hooks (amux's!) having run: `Stop/Stop`, `Notification:idle_prompt`, `Notification:permission_prompt`, `PermissionRequest:AskUserQuestion`, `SessionStart:startup`, `SessionStart:compact` |
| `task_reminder` | 118 | `content`, `itemCount` |
| `deferred_tools_delta` | 58 | `addedLines`, `addedNames`, `readdedNames`, `removedNames`, opt `pendingMcpServers` |
| `skill_listing` | 56 | `content`, `isInitial`, `names`, `skillCount` |
| `edited_text_file` | 30 | `filename`, `snippet` |
| `read_truncation_notice` | 18 | `banner`, `toolUseID` |
| `agent_listing_delta` | 15 | `addedLines`, `addedTypes`, `isInitial`, `removedTypes`, `showConcurrencyNote` |
| `queued_command` | 6 | `commandMode`, `origin`, `prompt`, `source_uuid`, `timestamp` — a queued message being delivered into the turn |
| `auto_mode` | 6 | `autoModeConsentFlow` |
| `file` | 5 | `content`, `displayPath`, `filename` |
| `date_change` | 5 | `newDate` |
| `compact_file_reference` | 5 | `displayPath`, `filename` |
| `command_permissions` | 4 | `allowedTools` |

Chat treatment: not feed content; fold into phase/meta where useful
(`async_hook_response` of `hookEvent:"Stop"` confirms amux's own hook
round-trip on the *next* turn; `queued_command` confirms queue delivery).

### `queued_command` and peer-delivery shapes (Claude 2.1.240)

The graduated carrier captures add two version-specific facts. A PTY paste
received while Claude is busy produces `queue-operation` `enqueue`/`remove`,
then this attachment when the queued input enters the turn:

```json
{"type":"attachment","attachment":{"type":"queued_command","commandMode":"prompt","origin":{"kind":"human"},"prompt":"<amux …>…</amux>","source_uuid":"…","timestamp":"…"}}
```

Socket delivery uses a different peer shape: `queue-operation`
`enqueue`/`dequeue`, followed by an `isMeta:true` user row whose
`origin` is `{kind:"peer", name, from, fromMode, body, selfSent}` and whose
message content contains the original `<cross-session-message …>` plus
Claude's peer-safety context. In the 2.1.240 capture this path emitted no
`queued_command` attachment, whether delivered idle or busy. Therefore
`queued_command` is evidence for queued PTY input in this version, not a
general socket-delivery confirmation; the peer-origin user row is the socket
confirmation present in the transcript.

---

## 10. Session-state rows (no uuid, no parentUuid — "latest wins")

Keyed by `sessionId` only; re-emitted throughout the file (at session
start, after resume, before compaction). A client folds each as
replaceable state, never as feed entries:

| type | fields | meaning |
|---|---|---|
| `mode` | `mode` (only `"normal"` observed) | UI mode |
| `permission-mode` | `permissionMode` (`"auto"` in the survey; `"plan"` observed in the Phase 0 plan captures — matches the `--permission-mode plan` launch arg; docs enumerate `default`, `plan`, `acceptEdits`, `bypassPermissions`) | **D4 source — RESOLVED (§18d): a mid-session Shift+Tab cycle emits NO row** (and plan approval, manual or auto, never flips it). The row is a launch-time/bookkeeping signal only; the effective mode's live source is the hook payloads' `permission_mode` field, latest-wins across both |
| `ai-title` | `aiTitle` | generated conversation title (updates over time) |
| `last-prompt` | `lastPrompt` (may be absent), `leafUuid` | `leafUuid` resolves to an existing row uuid (verified) — the current DAG leaf |
| `queue-operation` | `operation`: `enqueue` (with `content` string), `dequeue` (`content` null), `remove` (with `content`), `popAll` (with `content`); `timestamp` | queued-message lifecycle; `dequeue` precedes the queued prompt's user row (`promptSource:"queued"`) |

---

## 11. File-history rows (checkpointing; ignore for chat)

| type | fields |
|---|---|
| `file-history-snapshot` | `messageId` (uuid of the prompt row it checkpoints), `snapshot: {messageId, timestamp, trackedFileBackups}`, `isSnapshotUpdate` (all `false` observed) |
| `file-history-delta` | `messageId`, `snapshotMessageId`, `trackingPath`, `backup` (object), `timestamp` |

---

## 12. Tool use — pairing, results, background tasks

**Pairing rule (FACT):** `tool_use.id` (`toolu_*`) ↔ user row whose
`message.content[0].tool_use_id` matches. Redundant confirmations:
`sourceToolAssistantUUID` on the result row = the tool_use row's `uuid`;
`parentUuid` of the result row = that same uuid (branch under parallel
tools, §4). An unpaired `tool_use` in a *final* message = tool still
running (or obligation pending, §18) — INFERRED-pending, FACT once the
result row lands.

`toolUseResult` (top-level sidecar on the result row) is the typed
result Claude Code keeps beside the API-visible `content`. It is a
string (plain results, including error text) or an object; observed
object shapes:

| shape (key set) | tool family |
|---|---|
| `{interrupted, isImage, noOutputExpected, stderr, stdout}` (+opt `backgroundTaskId`, `gitOperation`, `returnCodeInterpretation`, `timedOutAfterMs`, `backgroundCwdHint`, `persistedOutputPath`, `persistedOutputSize`) | Bash. `backgroundTaskId` = background task handle; `persistedOutputPath` points into `tool-results/` |
| `{type:"text", file:{…}}` / `{type:"create"|"update", …}` | Read / Write / Edit-adjacent |
| `{filePath, oldString, newString, originalFile, replaceAll, structuredPatch, userModified}` (+opt `memdirStamped`, `staleRecovered`) | Edit — B4's file name + change magnitude come from `filePath` + `structuredPatch` |
| `{agentId, canReadOutputFile, description, isAsync:true, outputFile, prompt, resolvedModel, status:"async_launched"}` | Agent/Task launch ack (background) |
| `{agentId, agentType, content, prompt, resolvedModel, status:"completed", toolStats, totalDurationMs, totalTokens, totalToolUseCount, usage}` | Agent/Task synchronous completion |
| `{questions, answers, annotations}` | AskUserQuestion (§18) |
| `{durationSeconds, query, results, searchCount}` / `{bytes, code, codeText, durationMs, result, url}` | WebSearch / WebFetch |
| `{matches, query, total_deferred_tools}` | ToolSearch |
| `{statusChange, success, taskId, updatedFields}`, `{task}`, `{persistent, taskId, timeoutMs}`, `{message, pin, resumedAgentId, success}` | Task* / Monitor / SendMessage |
| `{allowedTools, commandName, success}` | command permissions |
| `{json|message, status}` | MCP tools |

Oversized outputs: the `tool_result` block's `content` contains a
truncation notice + `tool_reference`; full text lives at
`<sessionId>/tool-results/<id>.txt` (verified by producing one).

---

## 13. Subagents / sidechains (B7)

- Child rows live in **separate files**:
  `<sessionId>/subagents/agent-<agentId>.jsonl`. They never appear in
  the main file. amux does not tail them today — B7's actor tree gets
  only what the main file shows unless tailing is added.
- Linkage (FACT): `Agent` `tool_use` (input: `description`, `prompt`,
  `subagent_type`, opt `model`, `run_in_background`) → result row's
  `toolUseResult.agentId` = the `<agentId>` in the child filename. The
  child's `agent-<agentId>.meta.json` carries the reverse link:
  `{agentType, description, toolUseId, parentAgentId, spawnDepth}` where
  `toolUseId` is the spawning `toolu_*` id (`parentAgentId` set when the
  spawner is itself a subagent).
- Child file rows: only `user`/`assistant`/`attachment`, all
  `isSidechain: true`; first row is the task prompt as a user row with
  `parentUuid: null`; per-block streaming writes (§6).
- Completion (main-file view):
  - Synchronous Task: the `tool_result` + `toolUseResult.status:
    "completed"` row (FACT).
  - Background Task: launch ack `status:"async_launched"` (FACT it
    launched), then later a **user row** with
    `origin.kind:"task-notification"`, `promptSource:"system"` carrying
    the completion notice (FACT it finished, but content is prose);
    `turn_duration` rows expose `pendingBackgroundAgentCount` (FACT
    count at turn end).
  - Child-file view: last assistant row with `stop_reason:"end_turn"`
    (INFERRED — the child could be resumed with a follow-up message).

---

## 14. Turn boundaries and durations

Turn start (FACT): a user row with string content (human prompt) —
recent versions make this explicit with `origin.kind:"human"` /
`promptSource`; `promptId` groups the whole turn's tool_result rows.

Turn end, three independent signals:

| signal | nature | duration |
|---|---|---|
| `system/turn_duration` row | FACT, in-transcript, ordered after the final assistant rows | `durationMs` = prompt-row→now wall time (verified exact) |
| `system/stop_hook_summary` row | FACT (fires even with no meaningful hooks; 196≈199 counts) | none |
| amux `hook.stop` row | FACT, but **arrival-ordered**, may precede the transcript tail catching up; `stop_hook_active:true` means a stop hook forced continuation | none |
| newest assistant row `stop_reason:"end_turn"` | FACT for message end; INFERRED for turn end (a stop hook or queued message can extend the turn) | timestamps |

**Recommended rule:** working→idle on `turn_duration` (authoritative,
carries the duration); use `hook.stop` as the low-latency trigger and
reconcile when `turn_duration` lands. `stop_reason:"tool_use"` on the
newest final message + missing tool_result = still working (tool
executing) — INFERRED.

D5's elapsed-time indicator: start at the prompt row's `timestamp`,
tick locally, replace with `durationMs` when the turn closes.

---

## 15. Thinking (B3)

- A `thinking` block row is written **when the block finishes** (its
  `timestamp` is block-completion; in sidechain files that is also its
  arrival time; in main files it arrives in the end-of-message burst).
- **"Claude is thinking right now" is never FACT from the transcript.**
  There is no block-start row. Best rule (INFERRED): phase=working and
  the newest row is the prompt/tool_result → "thinking/working…";
  upgrade to a concrete "thought for Ns" only retroactively.
  Failure modes: indistinguishable from API latency, queueing, retries;
  in main files nothing arrives mid-message at all, so any live
  "thinking" indicator is timer-driven, not row-driven.
- **"Thought for Ns" (retroactive, INFERRED but tight):**
  `N = thinking_row.timestamp − previous_row.timestamp`, where
  previous_row is the prior uuid row in file order (prompt, tool_result,
  or earlier block). Observed examples: 21 s, 6 s, 11 s. Caveats: the
  gap includes request setup/first-token latency (overstates by roughly
  the API latency); consecutive thinking rows 2–5 ms apart occur (tiny
  second block — clamp near-zero durations); do not compute across an
  interrupt or compact boundary; `redacted_thinking` durations are
  computed the same way.
- `signature` is opaque; never render. Thinking text render is deferred
  (requirements), but the marker + duration need only the above.

---

## 16. Compaction and clearing (B3, B9)

`/compact` (same file, FACT markers):

1. `mode` + `permission-mode` state rows are re-emitted.
2. `system/compact_boundary` row: `parentUuid: null`,
   `logicalParentUuid` = uuid of the last pre-compact row, `level:
   "info"`, `compactMetadata: {trigger: "manual"|"auto", preTokens,
   postTokens, cumulativeDroppedTokens, durationMs,
   preCompactDiscoveredTools, preservedSegment: {headUuid, anchorUuid,
   tailUuid}, preservedMessages: {anchorUuid, uuids, allUuids}}`.
3. A user row with `isCompactSummary: true`,
   `isVisibleInTranscriptOnly: true`, string content (the summary),
   `parentUuid` = the boundary row's uuid; its `uuid` equals
   `preservedMessages.anchorUuid`. Preserved pre-compact rows are *not*
   re-appended — `preservedMessages.uuids` names rows already in the
   file that stay in context.

Client duty at a boundary: render a compaction marker (with token
counts if desired — they're FACT); do not recompute durations across
it; the summary row is renderable but flagged transcript-only.
`SessionStart:compact` async_hook_response attachments confirm hooks
observed the compaction.

`/clear`: **no in-file marker** — a new session file is created; the
client sees amux's relink (buffer clear + full replay of a new short
file + fresh `amux.transcript_ready`). A relink is the only reliable
/clear signal (FACT at the amux layer, not the row layer). Subagent
auto-compaction exists per GitHub issue #16944 (isCompactSummary rows
with `isSidechain:true`) — UNOBSERVED locally.

---

## 17. Interruption (B8)

Observed artifacts (all FACT):

- Mid-generation interrupt: the partial message's streamed rows are
  flushed with `stop_reason: null` (the one null-sr row in main files),
  followed by a user row `content: [{type:"text", text:"[Request
  interrupted by user]"}]` with `interruptedMessageId` = the `msg_*` id
  it cut off (verified pairing).
- Tool-approval interrupt: user row text `"[Request interrupted by user
  for tool use]"`; the rejected tool's result row carries
  `is_error:true` with canonical text starting "The user doesn't want
  to proceed with this tool use…" and top-level
  `toolDenialKind:"user-rejected"`.
- `toolDenialKind:"automode-blocked"` — a tool blocked by auto-mode
  policy rather than the user.

Rule: any message with null `stop_reason` followed by an interrupt row
is final-as-interrupted; never leave it "streaming".

---

## 18. Obligation resolution (C5)

What confirms an optimistic answer landed:

| obligation | pending signal | resolution FACT in transcript |
|---|---|---|
| Permission request | amux `hook.permission_request` row (arrival-ordered); transcript-side the final message's `tool_use` has no paired result | **allow** → tool_result row for that `tool_use_id` with `is_error` absent/false (tool actually ran); **deny** → tool_result `is_error:true` + canonical denial text + `toolDenialKind:"user-rejected"`. "Allow for session" additionally emits a `command_permissions` attachment (`allowedTools`) — observed 4× |
| AskUserQuestion | `tool_use` name `AskUserQuestion` (`input.questions[]: {header, question, options[], multiSelect?}`) unpaired | result row `toolUseResult: {questions, answers, annotations}` — `answers` is an object keyed by the question **text** with **string** values (multi-select joins selections into one string); `annotations` optional (empty `{}` when unused). The API-visible `content` is a string form of the same. **CORRECTED 2026-08-11 (Phase 0 capture, claude 2.1.228)**: earlier text said keyed by *header* — captured reality keys by the `question` string, not `header` (see §18a) |
| Plan review (ExitPlanMode) | amux `hook.permission_request` with `tool_name:"ExitPlanMode"` (arrival-ordered); transcript-side the ExitPlanMode `tool_use` unpaired | **OBSERVED 2026-08-11 (Phase 0 capture, claude 2.1.228 — H.5)**: `ExitPlanMode` `tool_use.input` = `{plan, planFilePath}`. **Approve** → `tool_result` for that `tool_use_id` with `is_error` absent, `content` = canonical `"User has approved your plan. You can now start coding…\nYour plan has been saved to: <path>"`; `toolUseResult` sidecar `{filePath, isAgent, plan}`. **Reject** → `tool_result` `is_error:true` with the §17 canonical denial text (`"The user doesn't want to proceed with this tool use…"`). See §18a for the **permission-mode drift** (no mode-change row on manual approve) |
| Interrupt-as-answer | — | §17 rows |

A seq-mismatch/send failure never has a transcript artifact — C5's
resurface path is purely client-side.

### 18a. Phase 0 capture corrections (claude 2.1.228, 2026-08-11)

Fixtures: `crates/amux/tests/fixtures/chat-v1/`. Captured by the Phase 0
harness driving a real claude; every quoted row is redacted structure.

- **AskUserQuestion `answers` is keyed by the question TEXT, not the header.**
  A single-select question with `header:"Color"`, `question:"Which color do
  you prefer?"` produced `answers: {"Which color do you prefer?": "Red"}`.
  This corrects §18 and **docs/CHAT.md B5** (`? storage → …` and the "question
  answers from `toolUseResult.{questions,answers}`" wording assume header
  keying). Phase 2's ask/answer correlation must key by question text.
- **AskUserQuestion `options[]` are objects, not strings**: each option is
  `{label, description}` (e.g. `{"label":"Red","description":"The color
  red"}`). B5/C4 option rendering must read `.label`/`.description`.
- **`annotations` is `{}` when unused** (not absent) in these captures.
- **Multi-select answer is a comma-joined string; the Other value only lands
  if its checkbox is explicitly committed.** The captured `question_multi`
  answer is `"Hammer, Saw, a torque wrench "` — predefined selections and the
  Other free-text joined by `", "` into ONE string (trailing space on the
  Other value preserved). **Keystroke encoding (claude 2.1.228, empirically
  verified for Phase 3 — the C6 module):** options toggle with **Space** while
  ↑/↓ move the cursor; the appended "Type something" (Other) row opens an
  inline editor on **Enter**, and typing + **Enter saves the text but does NOT
  check the box** — a following **Space commits (checks) the custom option**.
  Without that trailing Space the Other value is silently dropped from the
  submitted answer (Other-alone-without-Space submits `""`; Other-alone-with-
  Space submits the text). Submit is a **two-step confirm**: Tab to the Submit
  tab → Enter (opens a "Review your answers · Submit answers / Cancel"
  screen) → Enter confirms. This is the "hardest keystroke table" (H.3); the
  exact bytes are now known.
- **ExitPlanMode resolution — permission-mode does NOT change on manual
  approve.** The captured `plan_approve` run approved via "Approve — manual"
  and the `permission-mode` row value stayed `plan` throughout; there is **no
  `permission-mode` row flip to `default`/`acceptEdits`**. The docs/community
  rule "approval → tool_result success **plus a `permission-mode` row
  change**" (docs/CHAT.md C3/§18, §22) is **half wrong**: the reliable
  approval FACT is the `tool_result` success + the canonical "User has
  approved your plan" `content`, not a mode-change row. (The auto-approve
  path — "Approve — auto" — was not captured; whether *it* flips the mode is
  still UNOBSERVED. Manual approve definitively does not.)
- **ExitPlanMode carries `planFilePath`** and the plan is additionally
  written to `~/.claude/plans/<slug>.md` by a preceding Write; the approval
  `content` echoes the saved path. A new artifact location the plan reader
  (CHAT.md B6/C3) can point at, though V1 retains the plan payload from the
  `tool_use.input.plan` field directly.
- **`permission-mode` rows re-emit mid-file at turn boundaries** with the
  *same* value (observed twice in `plan_approve`, both `plan`). This is
  re-emission without a value change — it does **not** by itself answer D4's
  open question (does a mid-session Shift+Tab *cycle* emit a new row?), which
  still needs a capture that actually cycles the mode.
- **Transcript file is created LAZILY on the first user turn**, not at
  SessionStart. The SessionStart hook reports the intended `transcript_path`
  before the file exists; amux's tailer (`transcript.rs`) waits for the file
  and only emits `amux.transcript_ready` once it appears. Consequence for
  §1/§B10: on a brand-new session, `transcript_ready` does **not** fire until
  the first turn begins — the chat's `⟳ loading` vs empty-composer distinction
  (CHAT.md B10) holds, but "replay complete" for a fresh session coincides
  with the first turn, not with session open. (Fixture harness had to send the
  first prompt before it could observe `transcript_ready`.)
- **`compact_boundary.compactMetadata`** in this version carried
  `{cumulativeDroppedTokens, durationMs, postTokens, preTokens,
  preservedMessages, preservedSegment, trigger}` — no `preCompactDiscoveredTools`
  key (§16 listed it). Tolerate-unknown covers the delta; noted for drift.

### 18b. Phase 1 fixture-read corrections (claude 2.1.228, 2026-08-12)

Found while building the amux-ui Claude layer against the Phase 0 fixtures
(`crates/amux/tests/fixtures/chat-v1/`); each item names the fixture that
evidences it.

- **`agent-name` rows are BACK in 2.1.228** (`plan_approve` line 30:
  `{agentName, sessionId, type:"agent-name"}`). §3 lists the type as absent
  in 2.1.198–227 — it returned as a session-state row (no uuid, latest
  wins). The chat fold treats it as §10 state (`SessionFacts.agent_name`),
  not an unrecognized entry.
- **New attachment types `plan_mode` and `plan_mode_exit`**
  (`plan_approve` lines 6/28): `plan_mode` carries `{isSubAgent,
  planExists, planFilePath, reminderType}`; `plan_mode_exit` carries
  `{planExists, planFilePath}`. Attachments stay non-entries; noted for
  the §9 census.
- **A typed local command lands as a BARE user row first** (`compact` line
  24): content `"/compact"` (string), `promptId` present, **no `origin`,
  no `promptSource`, no `isMeta`** — then the `<command-name>` /
  `<local-command-stdout>` rows (also no `isMeta`, with `promptId`) and an
  `isMeta` caveat row follow later. §5's variant table missed the bare
  form. Chat stance: the bare row renders as a prompt with an *unstated*
  source (it is what the user typed) but never starts a turn; the
  XML-tagged records fold to nothing.
- **A Write that CREATES a file carries an EMPTY `structuredPatch`**
  (`plan_approve` line 17: sidecar `{type:"create", structuredPatch: [],
  content, filePath, …}`). §12's magnitude rule (filePath +
  structuredPatch) yields (+0 −0) for creates; the honest magnitude is the
  created `content`'s line count. Update-shaped Writes were not captured.
- **Rows of one message resume after a tool_result user row**
  (`plan_reject`: `msg_…vuXtTEqNY9iits3Q` has rows text, tool_use, then a
  tool_result user row, then ANOTHER tool_use row of the same
  `message.id`; every row carries `stop_reason:"tool_use"`). Message
  upsert must tolerate non-contiguous same-id rows and rows arriving for
  an already-final message. B2's abandoned-closure rule is unaffected (it
  only fires on null `stop_reason`).
- **`turn_duration` DOES close tool-denial interrupt turns** (`permission`
  deny turn: denial + `[Request interrupted by user for tool use]` row,
  then `turn_duration` `durationMs:4172`; same in `plan_reject`,
  81265 ms). §14/§22 say interrupt-ended turns lack `turn_duration` — that
  holds for the mid-generation Esc interrupt (`interrupt` fixture shows
  none), but the tool-approval-denial artifacts ARE followed by the
  authority. Client rule: an interrupt row yields an inferred
  elapsed-from-prompt marker that is reconciled in place if the authority
  lands after all. Also: the deny turn had **no `stop_hook_summary` and no
  `hook.stop`** — `turn_duration` alone closed it.
- **Every amux hook row arrives TWICE** (all 9 fixtures: `hook.stop`,
  `hook.permission_request`, `hook.notification` each appear as adjacent
  duplicate rows with identical payloads). Hook rows carry no uuid, so
  folds over them must be idempotent by construction. Root cause (amux
  double-registration vs Claude Code double-fire) not yet identified —
  flagged to the orchestrator.
- **`hook.stop` payloads are richer than the hooks doc**: observed fields
  include `background_tasks[]`, `last_assistant_message`,
  `permission_mode`, `prompt_id`, `session_crons[]`, `stop_hook_active`.
  `hook.permission_request` adds `permission_suggestions`, `prompt_id`,
  `permission_mode` beside `tool_name`/`tool_input`.
- **The plan-approval notification wording has no "permission" in it**
  (`plan_reject` line 38: `hook.notification` message `"Claude Code needs
  your approval for the plan"`). The fleet summarizer's KNOWN-FRAGILE
  substring split classifies this as *Question*, not *Permission* — Phase
  2's summarizer unification must route on the `hook.permission_request`
  `tool_name` instead of notification wording.
- **Human prompt rows now carry a top-level `permissionMode`**
  (`"default"`/`"plan"`/`"bypassPermissions"` across the fixtures) — an
  envelope addition to §4/§5, and a possible supplementary D4 source.
- **The pong fixture contains NO assistant rows and no `turn_duration`**:
  the capture window closed when the arrival-ordered `hook.stop` landed,
  before the transcript tail caught up. Not semantics drift — it is the
  §1 arrival-ordering consequence demonstrated — but the fixture README's
  claim that pong locks in "assistant, system/turn_duration" was wrong
  (corrected in the README).

### 18c. Phase 2 findings (2026-08-12)

Established while building the ask model and the summarizer unification;
evidence is the Phase 0 fixtures plus the live machine's own records.

- **Hook-row duplication ROOT CAUSE RESOLVED (closes §18b's open item):
  double hook registration, recorded by the transcript itself.** The
  capture transcripts' `system/stop_hook_summary` rows carry
  `hookCount: 2, hookInfos: [{"command": "amux-dev hooks claude"},
  {"command": "amux hooks claude"}]` — Claude Code ran TWO registered
  amux hook commands per event: a legacy user-scope
  `~/.claude/settings.json` entry (`amux-dev hooks claude`, the owner's
  pre-plugin dev setup; no current amux code writes settings.json)
  beside the plugin's `amux hooks claude`. Both read the same stdin and
  delivered byte-identical payloads to the same daemon, which wrote
  each. Claude Code dedupes only identical command strings, so distinct
  spellings both run. Hook delivery is therefore **at-least-once by
  construction** (user settings, project settings, and plugins may all
  carry a registration). Fixed at the daemon seam: `ClaudeSession`
  fingerprints emitted hook payloads and drops a byte-identical
  re-delivery within 2 s. Client folds still tolerate duplicates
  (bounded content-hash dedupe) — historical streams and replays carry
  them.
- **`hook.permission_request` carries NO tool_use id.** Payload fields
  (fixtures): `tool_name`, `tool_input`, `permission_mode`, `prompt_id`,
  `session_id`, `transcript_path`, `cwd`, opt `permission_suggestions`.
  Ask identity therefore cannot be the `toolu_*` id at hook time.
- **The hook's `tool_input` equals the transcript `tool_use.input`
  byte-for-byte** (canonical-JSON equality verified on every fixture,
  ExitPlanMode's `planFilePath` included). This is the correlation rule:
  a hook-born ask matches its transcript row by content identity
  (tool_name + canonical input), then resolves by the paired result.
- **`prompt_id` is NOT unique per ask.** plan_reject's two ExitPlanMode
  requests (original and revised plan) share one `prompt_id` — it is a
  turn key, not a request key.
- **Plan request-changes does NOT end the turn.** plan_reject: rejection
  #1 (`is_error:true` + `toolDenialKind:"user-rejected"`, NO interrupt
  row) is followed by the agent revising the plan file and asking AGAIN
  (a second `hook.permission_request` + `ExitPlanMode` tool_use with the
  revised plan). Only the final rejection is followed by the §17
  interrupt row + `turn_duration`. So: reject-with-feedback = denial
  facts only, turn continues; interrupt-reject = denial + interrupt row.
  Plan rejection DOES carry `toolDenialKind:"user-rejected"` (§18's
  reject row description under-stated this).
- **`hook.notification` carries no signal the fold needs**: every
  notification observed accompanies a better-typed source
  (permission_request for asks, turn signals for idleness). The unified
  fold ignores it entirely — wording interpretation is forbidden (E2).

### 18d. Phase 3 live keystroke verification (claude 2.1.228, 2026-08-12)

Every C6 encoding was driven against a real claude by the extended capture
harness (`crates/amux/tests/capture/`, scenarios `permission_session`,
`permission_deny_feedback`, `question_tabs`, `question_other_single`,
`question_mixed`, `plan_auto`, `mode_cycle`, `prompt_multiline`; all haiku;
fixtures committed under `crates/amux/tests/fixtures/chat-v1/`). Findings,
each fixture-evidenced:

- **D4 ANSWERED: mid-session Shift+Tab cycling emits NO `permission-mode`
  row.** Three CSI Z presses across two full turns wrote zero
  `permission-mode` rows (`mode_cycle`); the effective mode is visible only
  in hook payloads (`hook.stop`/`hook.permission_request`
  `permission_mode`), which tracked default → acceptEdits → default
  exactly. §10's "STILL UNOBSERVED" is resolved: the row does NOT re-emit
  on a cycle — D4 MUST supplement the row with the hook field. (One
  non-committed evidence run also showed the row CAN reappear later with
  the new value when claude's own bookkeeping re-emits session state —
  observed entering the plan flow — so the row is a lagging, sometimes
  never-arriving signal, not a cycle fact.)
- **Cycle order (haiku, no bypass flag): default → acceptEdits → plan →
  default**, one CSI Z (`\x1b[Z`) per step.
- **H.5 sub-capture: plan approve-AUTO does not flip the `permission-mode`
  row either** (`plan_auto`: rows stay `plan`), but the effective mode
  becomes `acceptEdits` (hook.stop fact) and subsequent Write/Edit landed
  with no permission asks. Menu digits verified: **1** = approve
  auto-accept, **2** = approve manual (Phase 0), **3** = request changes
  with feedback field (Phase 0; feedback rides the denial row's
  `userFeedback` + appended to the canonical content; the turn continues).
- **The permission menu is generated from the hook's
  `permission_suggestions`**: observed menu = `1. Yes`, one option per
  suggestion (observed: `addDirectories`, destination `session` → "Yes,
  and always allow access to <dir> from this project"), last = `No`.
  Digit 1 = allow once. Digit 2 (with exactly one suggestion) = allow +
  apply the suggestion — for a directory grant there is **NO
  `command_permissions` attachment** (§18's "allow for session emits
  command_permissions" holds only for command-rule grants), and a
  different Bash command re-asks. Digit 3 (= 2 + suggestion count) = deny
  **immediately**: typed `user-rejected` denial + `[Request interrupted
  by user for tool use]` + `turn_duration` — the permission menu has **no
  feedback field** (unlike the plan menu). Deny-with-feedback is composed
  as digit 3 followed by the feedback as a normal prompt
  (`permission_deny_feedback`).
- **Question forms: a DIGIT selects the option and auto-advances** — no
  Enter per question. A single-question single-select form **submits on
  selection** (no review); a multi-question or multi-select form ends on
  the review step (`1. Submit answers` preselected / `2. Cancel`) where
  one Enter submits — claude's own form implements C4's
  mandatory-submit rule. Digit 2 on the review = Cancel → the
  question-decline denial + interrupt artifacts (captured in a
  question_tabs pre-run). Every question carries TWO appended options:
  `Type something.` (Other) and `Chat about this` (new in 2.1.228 —
  unmodeled; ask payloads should tolerate it).
- **Other flows**: single-select — digit(options+1) opens the inline
  editor, type + Enter commits (`question_other_single`); multi-select —
  Enter opens, type, Enter saves, **trailing Space checks the box**
  (re-verified byte-for-byte: `"Hammer, Saw, a torque wrench "`), Tab to
  the Submit tab, Enter (review list), Enter (confirm).
- **Mixed forms compose**: Space toggles on the multi-select question,
  **Tab advances to the NEXT question tab** (not straight to Submit),
  digit auto-advances the single-select, one Enter on the review submits
  (`question_mixed`).
- **Multiline prompt injection: bracketed paste works.** `ESC[200~ text
  ESC[201~` + `\r` produced ONE user row whose string content is
  **byte-identical** to the sent text, newline included
  (`prompt_multiline`, `content_equals_sent: true`). B1's optimistic-echo
  reconciliation can key on content equality (string content +
  `origin.kind:"human"`); `promptId` is a turn key, not a request key
  (§18c), so content+turn evidence is the correlation.
- **Hook rows now arrive singly** in these captures — the Phase 2 daemon
  dedupe at the emission seam, live-confirmed (every Phase 0 fixture
  carried adjacent duplicates; folds still tolerate them for historical
  streams).

---

## 19. Usage and tokens (D5)

`message.usage` on every assistant row of a message; values are **per
message** (not cumulative) and identical/final on main-file rows,
partial on in-flight sidechain rows (§6). **Dedupe by `message.id`
before summing** — summing rows overcounts by the row multiplicity.

Fields: `input_tokens`, `output_tokens`, `cache_creation_input_tokens`,
`cache_read_input_tokens`, `cache_creation:
{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}`,
`service_tier:"standard"`, `inference_geo`, opt `server_tool_use:
{web_fetch_requests, web_search_requests}`, `speed`, `iterations`.

Context-size estimate (INFERRED, community-corroborated):
`input_tokens + cache_creation_input_tokens + cache_read_input_tokens`
of the newest final message. `message.model` is reliable per message
(mixed models within one session are normal — effort/model switching);
`"<synthetic>"` marks non-API rows (§20).

---

## 20. API errors and retries (B8)

Observed shape (2 rows): an `assistant` row with top-level
`isApiErrorMessage: true`, `error` string (`"authentication_failed"`,
`"server_error"`), no `requestId`, and a synthetic `message`:
`id` = plain uuid (not `msg_*`), `model:"<synthetic>"`,
`stop_reason:"stop_sequence"`, text content (canonical error prose).

Retry progress ("retrying 3/10…") is **not** written to the transcript
— it is terminal-only. Phase "errored" is FACT on such a row; "retrying"
is UNOBSERVABLE from rows (a working indicator that quietly exceeds
normal latency is the only inference available).

---

## 21. Version drift and client stance

Observed across 2.1.198 → 2.1.227 (five versions, one month):

- Row-type population is stable within this window, but differs from
  the older generation other clients were built on: `summary`,
  `progress`, `agent-name` rows are gone; `queue-operation`,
  `file-history-delta`, `ai-title`, `last-prompt`, `mode`,
  `attachment` (this shape), `away_summary`, `stop_hook_summary` are
  newer than most community write-ups.
- Field accretion is constant and additive within the window
  (`promptSource`/`origin` appear on prompts only in ≥2.1.22x;
  `pendingBackgroundAgentCount`, `attributionSkill`, `mcpMeta`,
  `effort` come and go by feature use). Key-set sampling shows ~50
  distinct key sets across the four uuid row types.
- Official stance: format is internal and breaks between versions
  (sessions doc). Ours (G1): **tolerate-unknown everywhere** — unknown
  `type` → explicit unrecognized feed entry; unknown `subtype`,
  `attachment.type`, block type, `toolUseResult` shape → generic
  rendering; never gate on key-set equality; never crash on absent
  fields. Re-run this spec's sampling when `version` changes (the field
  is on every uuid row — cheap to watch, and H's fixtures catch drift
  first).

---

## 22. Fact vs inferred — chat-state derivations

| derivation | rule | FACT / INFERRED | failure modes |
|---|---|---|---|
| Streaming vs final message | final ⇔ some row of `message.id` has non-null `stop_reason`; streaming ⇔ newest row's `stop_reason` null | **FACT** (final); **INFERRED** (streaming) | main files deliver whole messages at once, so "streaming" is visible only in sidechain tails; interrupts leave null-sr messages — close them on the interrupt row |
| Thinking "now" | phase=working ∧ newest row is prompt/tool_result | **INFERRED** | indistinguishable from API latency/retry; no block-start rows exist; main-file burst writes mean no row arrives mid-message at all |
| "Thought for Ns" | `thinking_row.ts − prev_row.ts` (same file-order chain, same turn) | **INFERRED** (from FACT timestamps) | includes request latency; ~0 for tiny second blocks; invalid across interrupt/compact boundaries; clock is writer-local |
| Turn completion | `system/turn_duration` row (paired `stop_hook_summary` just before); low-latency pre-signal: amux `hook.stop` | **FACT** | `hook.stop` is arrival-ordered (may precede tail catch-up); `stop_hook_active` may continue the turn; `end_turn` alone can be extended by queued messages/stop hooks |
| Turn duration | `turn_duration.durationMs` | **FACT** (wall-time verified) | absent for turns that end by interrupt (render elapsed-from-prompt instead — INFERRED) |
| Compaction marker | `system/compact_boundary` (+ summary row `isCompactSummary`) | **FACT** | `/clear` has no row marker — only the amux relink signals it; auto-compact trigger value unobserved |
| Phase: working | prompt row seen, no turn-end signal yet | **INFERRED** | crash/kill leaves it stuck — cap with hook.stop absence + staleness timer |
| Phase: idle | turn-end signal, nothing after | **FACT** at the signal, decays to INFERRED (external session may be typing) | — |
| Phase: blocked-on-permission | `hook.permission_request` newer than any resolving tool_result/denial | **FACT** (request) / resolution FACT per §18 | hook row is amux-side; a read-only client without hooks must fall back to unpaired-tool_use + `Notification:permission_prompt` attachment on the *next* turn (too late) — i.e. transcript-only detection is INFERRED and laggy |
| Phase: blocked-on-question | final message's `AskUserQuestion` tool_use unpaired | **FACT**-grade (pairing rule) | user may interrupt instead of answering (§17 closes it) |
| Phase: errored | `isApiErrorMessage:true` row | **FACT** | retry progress invisible; recovery only visible as the next normal assistant message |
| Subagent status | launched: `toolUseResult.status:"async_launched"`; done: `status:"completed"` or task-notification user row; running: launched ∧ ¬done (+`pendingBackgroundAgentCount`) | **FACT** (launch/done) / **INFERRED** (running) | child files aren't tailed; a killed subagent may never emit a done artifact — staleness timer required |
| Obligation pending→resolved (C5) | optimistic answer confirmed by §18 resolution row | **FACT** | plan-review flow **now OBSERVED** (§18/§18a): approve = tool_result success + canonical content (NOT a permission-mode change); reject = `is_error:true`. Send-failure has no transcript artifact (client-side resurface) |
| Permission mode (D4) | latest `permission-mode` row | **FACT** (at emission) | re-emits mid-file at turn boundaries with the same value; a plan approve does not flip it (§18a). Mid-session *cycle* re-emission still unverified — E2E must confirm, else supplement with hook `permission_mode` |
| User prompt echo (B1) | match optimistic echo to user row (string content, `origin.kind:"human"`) | **FACT** | `origin` absent in ≤2.1.220 — fall back to string-content + non-meta discriminators |
