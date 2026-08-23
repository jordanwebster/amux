# The amux chat TUI

Status: normative, implemented — chat V1 shipped across Phases 0–7
(2026-08-11/12; see DEVLOG and notes/chat-v1/ for the build record).
Companion to `docs/UI.md`, which owns the client layer this view stands
on — the reducer core, the kernel/per-agent split, the facts/translation/
interpretation boundary, and the chrome-first TUI rules all bind here
and are not restated. `docs/PROTOCOL.md` owns the wire,
`docs/ARCHITECTURE.md` the system. The executable half of this document
is live — the amux-ui chat spec chapters, the golden-frame suites, and
the opt-in real-Claude H suite (crates/amux/tests/capture); where prose
and passing spec disagree, the spec wins. Row semantics below are
grounded in an evidence survey of ~10,100 transcript rows across Claude
Code 2.1.198–2.1.227 (working spec in `notes/chat-v1/`, graduating into
committed fixtures via H); every derived state keeps that survey's
fact-vs-inferred discipline. Requirement IDs (A1…H) are cited in
parentheses so review can trace them.

## Vocabulary

- **Chat** — the structured conversation view over a Claude session,
  built on the `claude_pty_transcript_v1` io_protocol: Claude Code's
  transcript rows interleaved with amux hook rows; input by seq-guarded
  keystroke injection into the session PTY.
- **Feed** — the scrolling sequence of conversation entries. An
  **entry** is one rendered unit: a prompt, a message, a tool line, a
  marker, a collapsed ask fact.
- **Composer** — the user's input surface, docked at the bottom.
- **Ask** — an agent-initiated blocking request that needs the user
  before the session proceeds. Exactly two kinds: **permission** and
  **question**. Plan review is a permission variant — the
  `ExitPlanMode` tool with a plan payload and a specialized fullscreen
  presentation — not a third kind. An ask is the chat-layer surface of
  what `docs/UI.md` calls a live obligation; its retention rule (evict
  bytes, never obligations) applies to asks verbatim.
- **Phase** — the derived activity state of the session (working, idle,
  needs-you, errored, unknown). Every phase value is tagged fact vs
  inferred (E1).
- **Reader** — the one fullscreen overlay over typed artifacts (plan,
  diff, new-file content), scrollable, with an action row only when a
  writable ask is open.

Borrowed terms, credited: "composer" and the allow-once/allow-for-
session phrasing are common to Codex CLI and opencode; "working" as the
busy-phase word is Codex's; sticky-bottom "following" is opencode's.
"Ask" is ours. Nothing is inherited from the React Native app.

## Modes and entry (A)

From the fleet, a Claude agent opens in one of two modes: **raw attach**
(the existing byte passthrough) or **chat**. Enter opens the default
mode; which mode is default is a client setting in the standard amux
config (mobile clients are chat-only and carry no such setting), and
the shipped default is raw attach — the battle-tested path stays the path of least
surprise while chat earns its keep; flipping the default later is a
settings change, not a migration (A1). The non-default
mode opens via Ctrl+Enter, which plain terminals cannot distinguish
from Enter without the kitty keyboard protocol; the client probes at
startup, and the guaranteed plain-key fallback is `o` in the fleet
("open in the other mode"). Hints advertise only bindings that work in
the running terminal.

Read-only agents open in chat only; raw attach is absent for them — not
disabled-with-an-error, absent (A3). There is no in-session mode
switching in V1: the mode is chosen at open, with no toggle inside a
chat or an attach (A4). The protocol allows concurrent raw and
structured subscriptions (proved by E2E scenario H.8), so this is a UX
decision that stays reversible, not a technical constraint.

Chat renders on the alternate screen inside the existing chrome,
scrolls via renderer ViewState, and never writes terminal scrollback
(A5). This is empirical, not aesthetic: Codex ships the opposite design
— finalized history inserted into scrollback under an inline viewport —
and pays for it with a debounced resize-reflow state machine
(`transcript_reflow.rs`), hardcoded per-emulator scrollback caps
(VS Code 1 000, WezTerm 3 500, Windows Terminal 9 001, Alacritty
10 000), a dedicated Zellij escape path, and a user-facing `/raw`
command as the apology. Their own alt-screen transcript overlay proves
reading and scrolling work fine without scrollback. The one real cost —
no native search/copy over chat history — is accepted; the feed remains
resize-independent and the Model stays the single source of truth.

## The feed (B)

The Claude layer folds native transcript rows into typed entries.
Render order is file order; `parentUuid` is used only for pairing and
attribution, never for ordering (rows form a DAG under parallel tool
use — each tool_result parents to the assistant row carrying its
tool_use). Row `uuid` is the idempotency key: a re-replay after source
truncation recovery must fold to the identical Model.

### Entry kinds

- **User prompts** (B1). Sent prompts render immediately as optimistic
  local echo with a dim `sending…` marker, reconciled against the
  transcript's user row when it lands (string content plus
  `origin.kind:"human"` / `promptSource` on ≥2.1.22x; string-content +
  non-meta discriminators as the fallback for older rows — FACT).
  A failed send resurfaces the draft with the failure stated. A bare
  local-command row (no origin/promptSource — observed for `/compact`)
  renders as a prompt with unstated source and never starts a turn.
- **Assistant messages** (B2). Rendered as terminal markdown: headings
  bold (keeping their `#`), fenced code blocks, lists, inline
  emphasis/code; markdown tables render as preformatted blocks in V1;
  URL-aware wrapping never splits a link. Identity and upsert: one
  content block per row, all rows of an API message share `message.id`
  — key on it, append blocks in file order, dedupe rows by `uuid`
  (FACT). A message is final when any of its rows carries a non-null
  `stop_reason` (FACT). Main-session files burst-write: every row of a
  message lands at completion, so **"streaming" is not a main-feed
  state** and must not be promised — the working indicator carries
  liveness, and block-level streaming exists only in subagent files.
  A message whose newest row still has a null `stop_reason` when a new
  `message.id`, user row, or interrupt row arrives is closed as
  abandoned/interrupted — never left "streaming".
- **Thinking markers** (B3). No block-start row exists, so "thinking
  right now" is never a row-level fact; the live working line may say
  `thinking…` as an inference. Retroactively the feed shows a dim
  `~ thought for Ns` marker, N = thinking row timestamp minus the
  previous row's (INFERRED from FACT timestamps; includes API latency,
  clamped near zero for tiny second blocks, never computed across an
  interrupt or compaction). `redacted_thinking` renders the same marker
  flagged redacted. Thinking text expansion is deferred.
- **Turn markers** (B3). Every completed turn closes with a dim rule
  `─ turn · 1m 42s ─`. The authority is the `system/turn_duration` row
  (FACT, wall-time-verified `durationMs`); the amux `hook.stop` row is
  the low-latency pre-signal, reconciled when `turn_duration` lands
  (hook rows are arrival-ordered and may precede the transcript tail).
  An interrupt yields an inferred elapsed-from-prompt marker,
  reconciled in place if the authority lands — tool-denial interrupts
  ARE followed by `turn_duration` (Phase 1, fixture-verified); the
  purely-inferred marker stands only when the authority never
  arrives.
- **Compaction boundaries** (B3). The `system/compact_boundary` row is
  FACT; render a titled rule with the pre/post token counts. Durations
  are never computed across it. `/clear` writes no row — the only
  reliable signal is the amux relink (buffer clear, replay of the new
  file, fresh `transcript_ready`), a fact at the amux layer.
- **Tool use** (B4). Pairing is FACT: `tool_use.id` ↔ the user row
  whose `tool_result` block carries the matching `tool_use_id`.
  File-modifying tools (Edit/Write) are distinct entries with file name
  and change magnitude from the `toolUseResult` sidecar
  (`filePath` + `structuredPatch`), e.g. `✔ Edit sync/config.rs
  (+9 -2)`. A Write that creates a file carries an EMPTY
  `structuredPatch` (Phase 1, observed); its magnitude FACT is the
  created content's line count — `✔ Write plans/x.md (+20)`. Other tools render as compact one-liners; consecutive
  read/search one-liners group with no blank line between them, so runs
  of exploration compress without extra chrome — grouping is computed
  in the Claude-layer fold from entry kinds, never by renderer layout
  introspection. An unpaired `tool_use` in a final message renders as
  running (INFERRED-pending; FACT once the result lands). Oversized
  outputs render the truncation notice; the full text stays on disk
  behind the Effect seam.
- **Collapsed ask facts** (B5). Resolved asks collapse to one-line
  facts: `✔ allowed for session — Edit sync/config.rs`,
  `✗ denied — Bash rm -rf …`, `? storage → trust store, recorder
  dumps`, `✔ plan approved (manual)`. Sources are transcript facts, not
  heuristics: allow ⇔ a non-error tool_result for that id; deny ⇔
  `is_error:true` plus `toolDenialKind:"user-rejected"`; question
  answers from `toolUseResult.{questions,answers}`; command-rule
  session grants additionally emit a `command_permissions` attachment
  — directory-scope grants emit none (Phase 3, observed).
- **Plans** (B6). An accepted plan appears in the feed truncated to its
  first ~6 lines with a `plan · ctrl+t to read` affordance, and stays
  permanently re-openable in the reader (opencode's plan-stays-
  addressable property, which neither study's subject actually ships
  for plans). Accepted plan payloads are retained as session state
  keyed by tool_use id, outside feed windowing, bounded by count; a
  plan that predates available history after relink is honestly absent.
- **Subagents** (B7). The `Task` tool_use renders as one line with the
  child's description; synchronous completion is FACT
  (`toolUseResult.status:"completed"`, with duration and tool count);
  background launch is FACT (`async_launched`), completion arrives as a
  task-notification user row (FACT that it finished), running is
  INFERRED (launched ∧ ¬done, capped by a staleness timer —
  `pendingBackgroundAgentCount` on turn ends is the FACT count).
  Task-notification notices render as their own one-line entry: the
  row carries no agent-id key, so the fold cannot correlate the prose
  to a specific child (Phase 1, observed). Child transcript files are
  not tailed in V1; nested timelines are deferred.
- **Status entries** (B8). API errors are FACT
  (`isApiErrorMessage:true` rows) and render as an error entry. Retry
  progress is written nowhere in the transcript — "retrying 3/10" is
  terminal-only — so the chat never fabricates it; the honest signal is
  a working indicator that quietly exceeds normal latency. Interruption
  entries render from the §-verified artifacts: the canonical interrupt
  user rows, `interruptedMessageId` pairing, and tool-denial rows.
- **Unrecognized rows** (G1). Unknown row types are retained and
  rendered as an explicit unrecognized entry, never silently dropped —
  the format is documented as internal and version-drifting, and the
  survey shows whole row generations (`summary`, `progress`,
  `agent-name`) appearing and vanishing across versions.

Session-state rows (`permission-mode`, `mode`, `ai-title`,
`last-prompt`, `queue-operation`) and attachment rows are not feed
entries; they fold into phase, composer, and header state as
latest-wins facts.

### Retention and windowing (B9)

Retention is bounded and honest. The source buffer is a bounded tail
(1000 entries today), so a fold that starts past the beginning renders
an explicit `─ earlier history unavailable ─` boundary as the feed's
first line — a statement, not an apology. Evicting content never evicts
asks: a pending permission or question survives any window, per
UI.md's retention rule.

This section resolves `docs/UI.md`'s deferred **content windowing**
decision for the chat milestone: the feed is the first windowed
transcript-scale entity. The window is the bounded source tail; deltas
apply within the window; the relink is the epoch that guards
snapshot/live reconciliation (buffer cleared, new file replayed, fresh
synchronized marker). Nothing outside the window is fetched in V1;
future backfill goes through the Effect seam.

### Replay and live (B10)

Everything before the `amux.transcript_ready` marker is replay;
everything after is live. During catch-up the chat renders a
`⟳ loading transcript…` band where the feed will be — visually distinct
from an empty chat, which renders the composer with a placeholder and
no band. A fresh session has no transcript file until its first turn —
creation is lazy (Phase 0, observed) — so a new agent renders the
empty-chat state, not the loading band, and `transcript_ready` arrives
with the first turn. On source-shrink recovery the tailer re-replays from the
start; the fold treats a repeated prefix as re-replay (idempotent by
row `uuid`), not new content.

## Asks (C)

### Model and lifecycle

An ask is `{id, kind, payload}` in the Model: id is the `tool_use` id
(questions, plan review) or, for permissions, the hook row's content
identity — hook payloads carry no tool_use id (Phase 2,
fixture-verified); `tool_name` + `tool_input` equals the transcript
`tool_use.input` byte-for-byte, and that equality is the correlation.
Kind is permission or question; the payload is typed per kind, and a
permission carrying an `ExitPlanMode` plan is the plan-review
variant. Pending signals: the amux
`hook.permission_request` row (FACT, arrival-ordered), which fires
for tool permissions AND for `AskUserQuestion`/`ExitPlanMode` (Phase
0, observed) — extraction routes on its `tool_name`, never assuming a
plain permission; the unpaired tool_use in a final message is the
transcript-only signal (FACT-grade pairing rule) and the fallback
where hook rows are absent. Multiple pending asks queue; the head is
shown with an honest `(1 of N)` count.

Lifecycle (C5): **pending** → panel open → answer submitted
**optimistically** (a dim pending marker holds the collapsed entry) →
**confirmed** by the transcript's resolution fact (§B5 sources) or
**failed** — a seq-mismatch or send failure resurfaces the ask with the
failure stated. A lost outcome must never leave a spinner. An ask can
also resolve **remotely** (another client answered, or the user
interrupted): the panel dismisses and the fact renders. Send failures
have no transcript artifact; resurfacing is purely client-side state.

An active ask takes over the composer area (C1) — the keystroke channel
*is* the remote menu, so free typing and menu answers cannot coexist.
Asks dock at the composer position, never as centered modals: the feed
above is exactly the context needed to decide, and both UX studies
converged on this. The composer draft is ViewState and survives the
takeover untouched (D1).

Esc **never answers an ask**. It steps back one stage — an open text
field or reader closes toward the menu stage — and floors there: the
panel is not dismissible while its ask is pending. The agent is
blocked either way, the feed stays scrollable behind the docked panel,
and a panel that cannot vanish is a panel whose state cannot be lost.
Deny exists only as a labeled option. Both studies rejected their
subjects' Esc-answers behavior (Codex: Esc = deny + interrupt;
opencode: single Esc rejects a whole multi-question form) — a reflex
"back" key must not answer a remote agent, and must never lose typed
form state.

### Permission asks (C2)

Header: tool identity. Body per tool: a diff for Edit/Write (truncated,
`f` opens the full reader), the `$ command` line for Bash, a compact
typed fallback for everything else, including unknown tools. Actions,
as a numbered list — the one list idiom every ask uses:

    › 1. Allow once
      2. Allow for this session
      3. Deny — tell the agent why (optional)

Digits or ↑/↓ select; Enter confirms. Deny opens an optional one-line
feedback stage (Enter with empty text is a plain deny). Option labels
state plain outcomes and scopes, never rule syntax.

Two Phase 3 facts shape the encoding beneath this surface. Claude's
remote menu is GENERATED from the hook's `permission_suggestions`
(1 Yes · one digit per suggestion · No last), so option 2's label
derives from the suggestion (e.g. "always allow access to <dir> from
this project") rather than a fixed phrase — and menu shapes with
suggestion counts other than the verified one refuse with a typed
error until captured. And claude's own deny is immediate, with no
feedback field (it carries the interrupt artifacts and ends the
turn): the optional-feedback stage is delivered as one composed
program — deny digit, then a follow-up prompt (verified). After resolution the panel collapses to the B5 fact with its
pending marker until the transcript confirms.

### Question asks (C4)

Built on opencode's question form, the strongest surface either study
found. A single question with single-select renders as a numbered
option list with dim per-option descriptions; digit/↑↓ select, Enter
submits. Multi-question and/or multi-select adds a tab row of question
headers plus a final **submit** tab: Tab/Shift+Tab (or ←/→) cycles,
answered tabs brighten, multi-select uses `[x]` checkboxes toggled with
Space, and every question carries an appended `Other…` option opening
an inline free-text field. The submit tab shows a review list with
unanswered items in error color; Enter there submits all — the
mandatory submit step whenever there is more than one question or any
multi-select. Codex's per-question Tab-notes are folded into `Other…`:
one free-text idiom, not two. Claude's own form appends a
`Chat about this` option beside `Other…` (Phase 3, observed) —
unmodeled in V1 panels, tolerated in the transcript facts.

### Plan review (C3, B6)

A plan-review ask opens the **reader** directly, fullscreen: the full
plan is the point, and a 15-row docked panel cannot honor "read the
full plan". The body scrolls (↑↓/PgUp/PgDn) with a position indicator;
the action row is:

    › 1. Approve — auto       agent proceeds, edits apply without asking
      2. Approve — manual     agent asks before each edit
      3. Request changes      feedback required

Request-changes swaps the action row for a feedback composer and will
not submit empty (C3). A request-changes rejection does not end the
turn (Phase 2, fixture-verified): the agent may revise and re-ask,
which is a NEW ask — the docked panel re-appears with the revised
plan. Rejection rows carry `toolDenialKind:"user-rejected"`, like
tool denials. Esc closes the reader without answering,
dropping to the docked panel form of the ask — truncated plan, the
same three actions, `f` back to the full reader. After approval the
feed carries the truncated plan entry and Ctrl+T reopens the reader on
the newest accepted plan (←/→ steps between plans when several exist),
with no action row once resolved.

The `ExitPlanMode` resolution rules are fixture-grounded (Phase 0
captures, claude 2.1.228): approval ⇒ a non-error tool_result with
the canonical "User has approved your plan" content — manual approval
emits NO `permission-mode` row change, contrary to the earlier
docs-sourced rule; rejection ⇒ `is_error:true`. The tool input
carries `{plan, planFilePath}`; the plan is also written under
`~/.claude/plans/`. H.5 resolved (Phase 3): the plan menu is 1 approve-auto /
2 approve-manual / 3 request-changes; approve-auto does NOT flip the
`permission-mode` row either — the effective mode becomes acceptEdits
per hook facts, and edits proceed ask-free.

### Diffs and the reader's artifacts

Diff rendering is grounded in `notes/chat-v1/diff-rendering.md`
(both reference TUIs converge on unified-at-80, numbered gutter,
wrap-not-scroll, no intra-line emphasis). Two producers, both
Claude-layer folds, one pure renderer:

- **Post-hoc** (feed): `toolUseResult.structuredPatch` hunks are
  restated verbatim — absolute line numbers, FACT magnitude for the
  feed line's `(+9 -2)`. The transcript already states every landed
  edit; the client never recomputes one.
- **Ask-time** (permission panel): no hunks exist yet — the hook
  carries only `old_string`/`new_string` — so the mini-diff is
  computed in the fold (`similar`, line-level, context 3) and
  rendered **numberless**, with an *estimated* magnitude; a
  `replace_all` edit says `(replaces every occurrence)` instead. A
  Write ask shows a `+` block head with `(N lines)`;
  create-vs-overwrite is unknowable before the tool runs, and the
  header claims neither.

Appearance, panel and reader alike: sign column, then content; in
numbered form the number column right-aligns to the widest number,
and a replaced pair repeats its number (`15 -` / `15 +` describe one
position in two file versions). Long lines wrap — never horizontal
scroll — with blank-gutter continuation rows so the gutter never
lies; tabs expand before width math. The panel preview budget is at
most 8 screen rows (wrapped rows count), cut with a remainder line
that always states the arithmetic: `⋮ +K more lines · f full diff`.
Four semantic tokens carry color in both themes — `diff.added`,
`diff.removed`, `diff.context`, `diff.meta` — foreground-only in V1;
background tints are a named additive extension.

The reader is one overlay over a typed artifact model — a match, not
a viewer framework: **Plan** (markdown, the B2 renderer reused),
**Diff** (above), **NewFile** (Write content as a numbered `+`
block); **Text** (oversized tool output through the Effect seam) and
**Image** are reserved kinds, so B4's truncation notice and future
image placeholders have a stated destination. A new kind is an enum
variant, a match arm, and golden frames. Ask-time artifacts live with
their ask (evict bytes, never obligations); accepted plans keep B6's
keyed retention; nothing else is retained in V1.

### The keystroke seam (C6)

Every answer, prompt submission, and interrupt becomes a keystroke
program: an encoded byte sequence injected into the session PTY under
the seq guard. All encodings live in one spec-tested Claude-layer
module — menu digits, arrow navigation, multi-select toggles and the
joined-selection submit, plan-review keys, the interrupt Esc. Views
never encode keys; renderers dispatch typed Commands and the module
owns the bytes. This module is also the seam for a future native
structured-input protocol: when one exists, the module's typed surface
stays and its PTY backend is replaced. The module is
`amux-ui/src/claude/encoding.rs`; every table carries its
verification provenance (claude version, capture run), and an
unverified menu shape refuses with a typed error — never guessed
bytes.

## Composer and control (D)

The composer is a multiline textarea, one to six rows, auto-growing;
the draft is renderer ViewState and survives ask takeovers, scrolling,
and phase changes (D1). Enter sends; Ctrl+J is the guaranteed newline
in any terminal, with Shift+Enter as kitty-detected sugar. Editing is
readline, spec-tested: arrows and C-b/C-f/C-p/C-n motion, Home/End
line start/end, C-w/C-u/C-k kills (word, to line start, to line end),
C-d delete-forward, C-y yank — every kill lands in a single-slot kill
buffer, so no clearing key is destructive. Ctrl+C abandons the whole
draft as a kill; on an empty draft it arms the chrome quit guard (see
Keybindings). Ctrl+A is *not* a composer binding — it is the chrome
leader (configurable, default ctrl-a) and chat never shadows it; C-e
and End serve line-end, Home line-start. Word motion rides
Ctrl+Left/Right where the terminal delivers it — convenience, never
the only path.

The draft is always editable; send is gated on phase (D2): while the
agent works, Enter is a no-op and the footer states the gate plainly
("draft kept — send gated while working"). Queueing while working is
deferred; Tab and the preview row above the composer are explicitly
reserved for it so the Codex queue-preview pattern can land additively.

Interrupt is a distinct, deliberate binding: **Ctrl+X**, allowed in
every focus state including open ask panels, even while send is gated
(D3). It is never on Esc, and the feed records the interruption entry
(B8). An interrupted turn never eats the draft.

Permission mode displays in the footer's right segment and cycles with
Shift+Tab when the composer has focus (D4). The current mode is sourced
from the hook payloads' `permission_mode` field — the live source
(Phase 3, fixture-verified: mid-session cycling emits NO
`permission-mode` row; that row is a launch-time/bookkeeping signal).
Cycle order: default → acceptEdits → plan → default, with bypass
offered only to a session launched with it.

The working indicator renders while phase is working: spinner, elapsed
time, interrupt hint — `◐ working · 24s · ctrl+x interrupt` (D5).
Elapsed starts at the prompt row's timestamp, ticks locally, and is
replaced by the authoritative `durationMs` when the turn closes. One
1 Hz Tick drives both the elapsed text and the spinner frame, scheduled
only while the indicator is visible (UI.md's event-driven rule); a
static glyph replaces the spinner when animations are off. A token
count appears when cheaply available — usage summed with dedupe by
`message.id`, since per-row summing overcounts by row multiplicity;
the cost/context header is deferred.

## Phase and attention (E)

The Model derives one session phase, each value tagged fact vs inferred
(E1), grounded in the survey's derivation table:

| phase | rule | tag | failure modes |
|---|---|---|---|
| replaying | before `amux.transcript_ready` | FACT (amux-layer) | — |
| working | prompt row seen, no turn-end signal | INFERRED | crash leaves it stuck — capped by a staleness timer into unknown |
| idle | `turn_duration` seen, nothing after | FACT at the signal, decays to INFERRED | an external session may already be typing |
| needs-you: permission | `hook.permission_request` newer than any resolving result | FACT (request); resolution FACT per B5 | hook rows are amux-side; transcript-only detection (read-only) is INFERRED and laggy |
| needs-you: question | final message's `AskUserQuestion` unpaired | FACT-grade | user may interrupt instead of answering — the interrupt rows close it |
| errored | `isApiErrorMessage:true` row | FACT | retry progress invisible; recovery only visible as the next normal message |
| unknown | truncation/reset/staleness degradation | — | never guess; UI.md: degrade to Unknown, not a wrong badge |

The decay and staleness thresholds are named constants — idle
Fact→Inferred at 60 s, the working staleness cap into unknown at
600 s — so E2E can assert them (Phase 2).

There is exactly one fold (E2): chat phase derivation and the fleet
attention summarizer share row interpretation. Notification-wording
heuristics are forbidden ground for that fold: the plan-approval
notification says "needs your approval" — no "permission" substring
(Phase 1, fixture-verified) — so interpretation routes on
`hook.permission_request.tool_name`, never on notification text.
Hook delivery is at-least-once by construction — registration may
exist in multiple scopes — so the daemon dedupes and emits each fact
once (core-side, Phase 2), and folds tolerate historical duplicates
regardless. Attention remains the
kernel's chrome vocabulary ("this agent needs you"), derived from the
same facts, so fleet attention behaves identically whether or not the
chat is open (E3) — opening a chat changes what is rendered, never what
is known. The kernel vocabulary has no errored badge: chat phase
carries the errored FACT and the fleet maps it to Unknown — by
design, not omission; wording heuristics must not be reintroduced to
"fix" it.

## Read-only chats (F)

A read-only chat renders everything live: the same feed, the same
markers, the same phase (F1). Asks render as fact panels — "the agent
is asking permission: …", with read affordances only (`f` opens the
diff or plan in the reader, no action row). Write affordances are
absent, not disabled: no composer, no option lists, no interrupt. One
consistent indicator marks the state in both the header and the footer:
`⊘ read-only`. Detection honesty: without amux hook rows a read-only
observer infers pending permissions from unpaired tool_use only —
INFERRED and laggy, rendered as such, never upgraded to a fact badge.
Fork-into-writable is deferred (F2).

## Wireframes

One visual language, fixed here: user lines prefixed `› `; assistant
text plain at a two-column indent; tool lines glyphed by outcome
(`✔` done, `▸` running, `✗` failed/denied) with dim `└` continuations;
markers and boundaries as dim `─` rules; ask panels docked above the
footer behind a dim rule, headed `⚠ permission` / `? question`; options
as numbered lists with a `›` cursor. Header line: agent · host on the
left, `chat · <phase>` on the right. Footer: one hint line — hints
separated by ` · ` on the left, the permission mode on the right,
rendered from the effective binding table. `▌` is the text cursor.

### Idle

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ fix-auth · claude @ mbp                                          chat · idle │
│──────────────────────────────────────────────────────────────────────────────│
│ › add retry with backoff to the sync client                                  │
│                                                                              │
│   I added exponential backoff to Client::reconnect — 6 attempts,             │
│   jitter capped at 500 ms. SyncOptions grew a RetryConfig.                   │
│                                                                              │
│ ✔ Read sync/client.rs · Grep "reconnect" (4 matches)                         │
│ ✔ Edit sync/client.rs  (+18 -4)                                              │
│ ✔ Bash cargo test -p amux-sync                                               │
│   └ 34 passed, 0 failed (2.1s)                                               │
│                                                                              │
│ ─ turn · 1m 42s ─────────────────────────────────────────────────────────────│
│                                                                              │
│ › Type a message▌                                                            │
│                                                                              │
│   enter send · ctrl+j newline · ? help                          mode default │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Working

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ fix-auth · claude @ mbp                                       chat · working │
│──────────────────────────────────────────────────────────────────────────────│
│ › now make the retry count configurable                                      │
│                                                                              │
│ ~ thought for 6s                                                             │
│                                                                              │
│   The cap belongs in RetryConfig; I'll thread it through SyncOptions.        │
│                                                                              │
│ ✔ Read sync/config.rs                                                        │
│ ▸ Bash cargo check -p amux-sync                                              │
│   └ running · 8s                                                             │
│                                                                              │
│                                                                              │
│ ◐ working · 24s · 12.4k tok · ctrl+x interrupt                               │
│                                                                              │
│ › and please document it▌                                                    │
│                                                                              │
│   draft kept — send gated while working                       mode default   │
└──────────────────────────────────────────────────────────────────────────────┘
```

The blank row above the working line is reserved for the queued-input
preview when queueing lands (deferred, door open).

### Permission ask

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ fix-auth · claude @ mbp                                     chat · needs you │
│──────────────────────────────────────────────────────────────────────────────│
│   I'll update the retry cap in the config too:                               │
│                                                                              │
│ ✔ Read sync/config.rs                                                        │
│                                                                              │
│──────────────────────────────────────────────────────────────────────────────│
│ ⚠ permission — Edit sync/config.rs  (+2 -1)                         (1 of 2) │
│                                                                              │
│      pub struct RetryConfig {                                                │
│    -     pub max_attempts: u8,                                               │
│    +     pub max_attempts: u8,        // capped at 6                         │
│    +     pub jitter_ms: u16,                                                 │
│   ⋮  +5 more lines · f full diff                                             │
│                                                                              │
│ › 1. Allow once                                                              │
│   2. Allow for this session                                                  │
│   3. Deny — tell the agent why (optional)                                    │
│                                                                              │
│   1-3/↑↓ select · enter confirm · f full diff · esc back (never answers)     │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Question ask (multi-question, multi-select)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ fix-auth · claude @ mbp                                     chat · needs you │
│──────────────────────────────────────────────────────────────────────────────│
│   Before I write the migration, two decisions:                               │
│                                                                              │
│──────────────────────────────────────────────────────────────────────────────│
│ ? questions   [storage*] [rollout] [submit]                                  │
│                                                                              │
│   Which stores should the migration cover? (select all that apply)           │
│                                                                              │
│ › 1. [x] trust store       pairing + relay trust records                     │
│   2. [ ] session index     bounded tail metadata                             │
│   3. [x] recorder dumps    panic-hook recordings                             │
│   4. [ ] Other…            type your own answer                              │
│                                                                              │
│   1-4/↑↓ select · space toggle · tab next question · enter advance           │
│   · esc back (never answers)                                                 │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Plan review (reader, fullscreen)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ plan — add retry with backoff                                  lines 1-11/52 │
│──────────────────────────────────────────────────────────────────────────────│
│  ## Approach                                                                 │
│                                                                              │
│  1. Wrap Client::reconnect in retry_with_backoff                             │
│     - exponential base 200 ms, cap 5 s, jitter ≤ 500 ms                      │
│  2. Thread RetryConfig through SyncOptions                                   │
│  3. Spec chapter sync::retry — cold start, mid-stream drop,                  │
│     give-up-after-cap                                                        │
│                                                                              │
│  ## Out of scope                                                             │
│                                                                              │
│  - relay-side backpressure                                                   │
│                                                                              │
│──────────────────────────────────────────────────────────────────────────────│
│ › 1. Approve — auto       agent proceeds, edits apply without asking         │
│   2. Approve — manual     agent asks before each edit                        │
│   3. Request changes      feedback required                                  │
│                                                                              │
│   ↑↓/pgup scroll plan · 1-3 select · enter confirm · esc back (plan stays)   │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Scrolled back

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ fix-auth · claude @ mbp                                       chat · working │
│─ earlier history unavailable ────────────────────────────────────────────────│
│ › what does the dispatcher do on a seq mismatch?                             │
│                                                                              │
│   It refuses the write and requests a dump — dispatch.rs::guard_seq.         │
│   The client then resurfaces the ask with the failure stated.                │
│                                                                              │
│ ✔ Read src/dispatch.rs                                                       │
│ ─ turn · 12s ────────────────────────────────────────────────────────────────│
│                                                                              │
│ › and on replay?                                                             │
│                                                                              │
│─ ↓ following paused · 3 new entries · pgdn to resume ─────────────── 43% ────│
│ ◐ working · 1m 12s · ctrl+x interrupt                                        │
│                                                                              │
│ › draft preserved while reading▌                                             │
│                                                                              │
│   pgup/pgdn scroll · end newest · ? help                      mode default   │
└──────────────────────────────────────────────────────────────────────────────┘
```

Sticky-bottom until the user scrolls; the paused rule with a new-entry
count is the honesty indicator opencode lacks, and the working line
stays visible so scrolling never hides that the agent is active.

### Read-only

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ ci-triage · claude @ buildhost                chat · read-only · needs owner │
│──────────────────────────────────────────────────────────────────────────────│
│   The flake is a teardown race in the testnet harness; serializing           │
│   the shutdown.                                                              │
│                                                                              │
│ ✔ Bash cargo test -p amux --test spec                                        │
│   └ 2 failed: token_refresh, expired_session                                 │
│                                                                              │
│──────────────────────────────────────────────────────────────────────────────│
│ ⚠ the agent is asking permission — Edit testnet/harness.rs  (+3 -1)          │
│   waiting for a writable client · f read the diff                            │
│                                                                              │
│ ⊘ read-only — you are observing this session                                 │
│                                                                              │
│   pgup/pgdn scroll · f view diff · q back to fleet                           │
└──────────────────────────────────────────────────────────────────────────────┘
```

## State transitions

Two orthogonal axes. **Phase** lives in the Model, changes only on
transcript facts, and is never changed by keys. **Focus** is renderer
ViewState: `COMPOSER` (free typing), `ASK` (docked panel or plan
reader owns keys), `PENDING` (answer in flight), `READER` (read-only
full plan/diff). Scroll is a third orthogonal scalar; any focus state
may be scrolled.

| From | Event / key | To | Notes |
|---|---|---|---|
| COMPOSER (idle) | Enter, non-empty draft | COMPOSER (working) | optimistic echo + `sending…` (B1) |
| COMPOSER (working) | Enter | — | no-op; footer states the gate (D2); Tab reserved |
| any | Ctrl+X | same | interrupt sent (D3); interruption entry lands (B8) |
| COMPOSER | ask head appears | ASK | panel takes the composer area; draft preserved (C1, D1) |
| ASK | 1–9 / ↑↓ | ASK | select option |
| ASK | Space | ASK | toggle (multi-select) |
| ASK | Tab / Shift+Tab / ←→ | ASK | cycle question tabs (when tabs exist) |
| ASK | f | ASK (reader body) | full diff / full plan |
| ASK | Enter | next stage or PENDING | scope/feedback/submit stages advance; final Enter submits |
| ASK | Esc | previous stage | steps back; floors at the menu stage — the panel stays; never answers |
| PENDING | transcript confirms | COMPOSER (or next ask) | collapses to the B5 fact |
| PENDING | seq mismatch / send failure | ASK | resurfaced with the failure stated; no stuck spinner (C5) |
| any | ask resolved remotely / interrupt | COMPOSER | panel dismissed, fact rendered |
| any | PgUp / wheel | same, scrolled | following paused; new-entry count shown |
| scrolled | PgDn at bottom (or Esc, empty draft) | same, following | snap to newest |
| COMPOSER | Ctrl+T | READER (plan) | newest accepted plan; ←/→ steps between plans |
| READER | Esc | previous state | close; nothing answered |

Esc is one deterministic chain, view-only, checked in order:
**1** close the reader (a plan-review reader drops to its docked
panel) → **2** step back ask stages, flooring at the menu stage —
never dismissing the panel → **3** reset feed scroll to following
(empty draft only) → **4** nothing. Esc never interrupts and never
answers; interrupt is Ctrl+X alone. Read-only chats have a single viewing focus: scroll keys,
`f`, and `q` only (F1).

Leaving the chat is a chrome affair, never an Esc stage: the leader
chords from raw attach apply unchanged — `<leader> s` back to the
fleet, `<leader> d` detach to the shell. A pending ask survives
leaving; the fleet row shows needs-you and reopening the chat re-docks
its panel.

Moving between the agents of one family is the same kind of act, so it
is the same kind of chord: `<leader> n` opens the next agent in this
agent's family and wraps past the last one back to the top, which makes
one repeated key both the way in and the way out. The header states how
many agents are down there (`⋯ 3 subagents`) and says nothing when there
are none. `<leader> m` opens and closes the completions children have
sent — a chat-wide state rather than a per-row affordance, because the
feed has no cursor to point at one row with.

A child waiting on a person can be answered without leaving: `<leader>
a` docks the ask the banner names where the composer sits, drawn by the
child's own layer with the child's id, and confirming it dispatches that
layer's own command addressed to the child. It is the same act as
answering in the child's chat — one path, no copied state — so an ask
answered anywhere takes the panel away by re-derivation, exactly as it
takes the banner. Esc sends the guest back. Only one ask is on screen at
a time: while this agent has an ask of its own, that one holds the
bottom block and the banner withholds the chord. While a guest is
docked, Enter and Ctrl+X belong to it — Ctrl+X interrupts the agent
whose ask is on screen — so the activity and composer rows stop naming
them.

## Keybindings

Bindings are derived, not accumulated: `notes/chat-v1/keybindings.md`
records the principles and the full derivation; this section is the
normative result. Three tiers: **plain** — guaranteed ANSI bytes,
always hintable; **ext** — standard CSI most emulators deliver
(convenience only, never the sole path to an action, marked
terminal-dependent in the `?` overlay); **kitty** — kitty-keyboard-
protocol sugar, feature-detected, hidden when the terminal cannot
deliver it. Alt/Option is constitutionally unbound: plain terminals
encode it as an ESC prefix, byte-identical to Esc-then-key, which no
deterministic Esc chain can coexist with (and macOS Option types
glyphs). One named binding table feeds dispatch, the footer hints, the
`?` overlay, and any future palette; hints never advertise dead keys
and substitute the configured leader.

**Ctrl+C** is the guarded abandon key, one rule chrome-wide (fleet
included — the chrome's single-press quit gains the guard): with a
focused non-empty text field it clears that field, as a kill (Ctrl+Y
restores); otherwise it arms the quit guard — the footer hint line
becomes `press ctrl+c again to quit` in warning color, and a second
press within 3 s quits. The clearing press never arms; any other key
or the timeout disarms; the arm Tick is scheduled only while armed.
The invariant, teachable in one line: **a single Ctrl+C never quits,
never interrupts, and never loses text it didn't visibly kill.** Raw
attach is untouched — passthrough forwards ^C to the PTY, leader
excepted, as today.

| Key | Context | Action | Tier |
|---|---|---|---|
| Enter | fleet | open agent in default mode (A1) | plain |
| Ctrl+Enter | fleet | open in non-default mode | kitty; fallback `o` |
| Ctrl+C | everywhere | clear focused field / arm-then-quit (above) | plain |
| Ctrl+X | chat, all focus states | interrupt (D3) | plain |
| Esc | chat | the view-only chain; never answers, never interrupts | plain |
| Enter | composer, idle | send | plain |
| Ctrl+J | composer | newline (canonical) | plain |
| Shift+Enter | composer | newline (sugar) | kitty |
| readline set | any text field | C-b/C-f/C-p/C-n motion, Home/End, C-w/C-u/C-k kills, C-d, C-y yank | plain |
| Ctrl+← / Ctrl+→ | any text field | word motion | ext |
| 1–9 | ask menu | select option (never submits) | plain |
| ↑ / ↓ | ask / reader | move selection / scroll | plain |
| Space | ask menu | toggle (multi-select) | plain |
| Tab / Shift+Tab, ← → | ask with tabs | cycle question tabs | plain |
| Enter | ask | confirm / advance stage / submit | plain |
| f | ask menu, read-only ask fact | full diff / full plan in the reader | plain |
| Ctrl+T | chat, accepted plan exists | plan reader; ←/→ steps between plans | plain |
| Shift+Tab | composer | cycle permission mode (D4) | plain (CSI Z) |
| Tab | composer | reserved (future queueing) | — |
| PgUp / PgDn | chat | scroll feed; reaching bottom resumes following | plain |
| Ctrl+Home / Ctrl+End | chat | feed oldest / newest + follow | ext |
| wheel | chat | deferred (see Deferred decisions) — PgUp/PgDn is the guaranteed path | — |
| j k, g G, Home/End | reader, read-only chat | pager motion (line, top/bottom) | plain |
| q | reader, read-only chat | close reader / back to fleet | plain |
| ? | composer, empty | help overlay (full key list); types `?` otherwise | plain |
| Ctrl+A (leader) | anywhere | chrome leader, configurable; chat never shadows it | plain |
| Ctrl+A s / Ctrl+A d | chat | back to fleet / detach to shell (as in raw attach) | plain |
| Ctrl+A n | chat, agent in a family | next agent in this family, wrapping past the last back to the top | plain |
| Ctrl+A m | chat | open / close the completions a child sent | plain |

Deliberately unbound, each an act of restraint: **Ctrl+G** (the emacs
abort reflex must never fire agent actions — no-op), **Ctrl+R**
(reserved for composer history search), **Ctrl+O** (reserved; freed
when the parkable panel was cut), **Ctrl+L** (shell redraw reflex —
harmless), **Ctrl+S/Ctrl+Q** (terminal flow control), **Ctrl+V**
(paste reflex; bracketed paste owns pasting), **Ctrl+H/I/M/[**
(byte-aliases of Backspace/Tab/Enter/Esc), **Ctrl+Z** (job control),
**Alt+anything** (permanently).

Discoverability is two layers only: the one footer hint line (at most
four items, derived purely from Model + ViewState — no stored footer
mode) and the `?` help overlay listing every effective binding with
its tier.

## Architecture constraints (G)

`docs/UI.md` legislates the architecture; this section only pins the
chat-specific consequences.

- The Claude chat layer is a typed child model consuming native rows —
  no intermediate representation, no capability flags (G1).
  Tolerate-unknown runs in both directions: unknown row types render as
  explicit unrecognized entries, and rules never gate on key-set
  equality or crash on absent fields, because the format is internal
  and drifts by version.
- Interpretation happens only in layer folds; core transports rows
  opaquely; nothing new rides the peer wire (G2). Views format, never
  decide — no error-string sniffing, no heuristics in renderers.
- All chat Msgs flow through the recorder from day one; redacted
  real-session recordings graduate into committed Tier-1 fixtures; the
  differential fold-equals-live property extends to the Claude layer
  (G3). High-rate transcript replay is coalesced before recording.
- Golden-frame coverage exists for every feed entry kind and every ask
  form, including the read-only fact variants (G4).

## The real-Claude E2E suite (H)

An opt-in leg driving the real `claude` binary with real credentials:
never default CI, always under `timeout`. Scenario sessions run the
cheapest sufficient model — haiku 4.5 by default, sonnet 5 where a
scenario proves tool-unreliable — and every capture records the model
and Claude Code version it observed. A version difference alone is
never a failure: drift is recorded and diffed against the semantics
spec; only actual semantic breakage fails a scenario. Scenarios:

1. Prompt round-trip ("Reply with exactly PONG").
2. Question, single-select (via "Use the AskUserQuestion tool to …").
3. Question, multi-select + Other — the hardest keystroke table,
   including the joined-selection answer encoding.
4. Permission allow and deny — assert the world (the file exists or
   does not), plus the denial facts (`toolDenialKind`).
5. Plan review, approve and request-changes — **the fixture-capture
   scenario for the UNOBSERVED `ExitPlanMode` rows**; the plan
   surface's resolution rules are gated on these fixtures.
6. Interrupt mid-turn (null-`stop_reason` flush + interrupt row).
7. Stale-seq input race → retryable resurfaced ask, not a crash.
8. Raw and chat subscriptions coexisting without disturbance (A4's
   open door).
9. Read-only observation of a hook-discovered external session.

The suite also confirms the open semantics questions this spec flags:
whether mid-session permission-mode cycling re-emits the
`permission-mode` row (D4's fallback trigger), and subagent
auto-compaction rows.

Assertions are structure, never prose: assert row shapes, pairing ids,
answer objects, files on disk — never match assistant wording, which
changes with every model. Each run records its Msg stream; redacted
recordings graduate into committed regression fixtures, so encoding
drift across Claude Code versions is caught here first, before any
user sees it.

## Rejected alternatives

Recorded with their evidence so they are not helpfully reintroduced.

- **Writing chat history to terminal scrollback** (Codex). Their tree
  is the proof: a resize-reflow state machine, per-emulator scrollback
  caps, a Zellij escape path, and `/raw` as the apology — exactly the
  failure UI.md predicts. Alt screen + ViewState scroll instead (A5).
- **Esc answers an ask** (Codex Esc = deny + interrupt; opencode single
  Esc rejects the form). A reflex key must not answer a remote agent or
  discard typed form state; Esc steps back, deny is a labeled option.
- **Esc-Esc armed interrupt** (opencode). Re-overloads Esc and adds a
  hidden armed-timer state beside a chain that must stay deterministic;
  interrupt is the dedicated Ctrl+X.
- **Digit-instant-submit on asks** (opencode questions). Answers are
  remote and optimistic; digit-select + Enter-confirm keeps them
  deliberate at the cost of one keystroke.
- **Plan approval as plain chat / silent mode flip** (Codex; opencode).
  Neither ships a review surface; B6/C3 require reading the full plan
  before an explicit three-way decision.
- **A third ask kind for plan review.** ExitPlanMode is a permission
  with a plan payload; a separate kind would fork the lifecycle and
  encodings for zero semantic gain.
- **Centered modal dialogs for asks.** Bottom-docked panels keep the
  feed — the context you decide with — visible; both studies converged.
- **"Streaming" as a main-feed state.** Main-session files burst-write
  whole messages; a streaming promise the stream cannot keep. The
  working indicator carries liveness (B2).
- **Click-only expansion, hover states, copy-on-select** (opencode).
  Keyboard-unreachable affordances are an accessibility gap; hover is
  impure under FrameContext; copy-on-select fights native selection —
  they shipped an escape flag for it. Keyboard-first; wheel scrolling
  only, no mouse capture.
- **Error-string sniffing in views** (opencode renders "denied" from
  `error.includes("rejected permission")`). The heuristics-in-views
  failure G2 forbids; denial is a typed fact (`toolDenialKind`).
- **Continuous-redraw spinners** (Codex shimmer; opencode 60 fps
  gradients). Decoration priced at unconditional periodic redraw, which
  UI.md forbids; one 1 Hz Tick drives spinner and elapsed together.
- **A slash-command junk drawer** (Codex: 45 commands, `/exit` beside
  `/pets`). `/` stays unclaimed in the composer grammar until slash
  commands land deliberately.
- **Turn-separator noise floor** (Codex: only turns > 60 s). A magic
  threshold hides turn boundaries; a dim one-line rule is already
  quiet.
- **Horizontal action chips** (opencode permissions). One numbered-list
  idiom serves every ask; two selection models is one too many.
- **Unguarded Ctrl+C** (opencode: clear when non-empty, quit
  *immediately* when empty — the missing guard earned them a Windows
  hack). The buffer branch itself is adopted, guarded: clear is a
  yankable kill and quit takes a rendered second press. What stays
  rejected is single-press quit and any ^C-interrupt meaning, ever.
- **A parkable ask panel** (close without answering, plus a reopen
  chord and a "pinned/parked" state). Two concepts and a binding for
  a state the docked panel already renders; the owner read "pinned"
  cold and did not understand it. The panel is simply not dismissible
  while pending; reversible if real usage demands the hiding.
- **Hiding completed tools behind a toggle** (opencode). Entries that
  silently vanish conflict with B9's honesty; collapse, never hide.
- **Exit farewell card written to scrollback** (opencode). Charming,
  but never-write-scrollback is unconditional; resume hints belong to
  the chrome.
- **Typed quit words** (`exit`, `:q` in the composer). Prompts *about*
  quitting exist; magic words in a prompt buffer are a footgun.
- **Per-chat OSC notifications, bell, terminal title** (Codex).
  Attention is fleet vocabulary owned by the chrome summarizer (E2/E3);
  side channels around the Model are how badges start lying.
- **A context sidebar** (opencode's 42-col column). A second
  information surface; the fleet chrome exists, and todo rendering is
  deferred.
- **A diff widget dependency, or recomputing file diffs post hoc.**
  `structuredPatch` already states every landed edit; the only diff
  ever computed is the ask-time preview from
  `old_string`/`new_string` (`similar`, in the fold). Neither
  reference TUI ships a reusable diff widget either. Render facts;
  never re-derive them.
- **Horizontal scroll for long diff lines.** The feed and overlays
  never scroll horizontally; wrapped continuations with a blank
  gutter keep the number column honest — both subjects agree.

## Deferred decisions

Doors left open on purpose; each stays additive under the constraints
above.

- **Per-agent last-used-mode memory** (A2) — a client setting refinement
  over A1's default.
- **Message queueing while working** (D2) and steering: Tab and the
  preview row above the working line are reserved; the Codex `↳` queue
  preview with pop-to-edit is the pattern to adopt. Steering is a
  protocol question before it is a UX one.
- **Fork-into-writable** from a read-only chat (F2).
- **Attachments and images** — paste placeholders (`[Pasted N lines]`,
  `[Image #1]`) are the model when they land.
- **Slash commands and @-mentions** — `/` and `@` are unclaimed
  composer grammar; the binding table already feeds a future palette.
- **Shell-passthrough composer mode** (`!` prefix precedent).
- **Thinking-text expansion** — V1 renders marker + duration only.
- **Todo-list rendering.**
- **Cost/context-window header** — V1's extent is the working-line
  token count (D5).
- **Nested subagent timelines** — requires tailing child transcript
  files; B7's one-line summaries stand until then.
- **Message-level copy/retry; archive.**
- **Composer prompt-history recall** — adopt opencode's edge-of-buffer
  + unchanged-buffer guard when it lands.
- **Command palette** — the binding table is palette-ready; V1
  discoverability is the footer line + `?` overlay.
- **Syntax highlighting** in fenced code and diffs — additive renderer
  work; plain blocks ship first.
- **Wheel scrolling** (Phase 6 drift). Alternate-scroll mode delivers
  wheel motion as arrow keys indistinguishable from the keyboard's —
  but the composer owns arrows for line motion and ↑-at-top is
  reserved for history recall, so wheel-as-arrows would move the
  cursor, not the feed, whenever the composer has focus. Branching on
  focus would put meaning on invisible state (P3). Deferred until a
  design honors both; PgUp/PgDn is the guaranteed path, and mouse
  capture remains rejected.
- **Mouse support generally** — any future affordance must
  stay keyboard-reachable and renderer-pure.
- **Theming beyond dark/light on semantic tokens** — the token
  vocabulary (background/panel, text/muted, semantic accents, diff
  family) is fixed; palettes may multiply later.
