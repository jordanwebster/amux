# The amux chat TUI

Status: normative, implemented — chat V1 shipped across Phases 0–7 and its
semantic-input boundary was refreshed against Claude Code 2.1.251
(2026-08-30; see DEVLOG and the provider recordings for the build record).
Companion to `docs/UI.md`, which owns the client layer this view stands
on — the reducer core, the kernel/per-agent split, the facts/translation/
interpretation boundary, and the chrome-first TUI rules all bind here
and are not restated. `docs/PROTOCOL.md` owns the wire,
`docs/ARCHITECTURE.md` the system. The executable half of this document
is live — `claude::specs::pty`, the derived amux rows, the amux-ui chat
spec chapters, the golden-frame suites, and the opt-in `claude_pty_live`
suite; where prose and a passing specification disagree, the specification
wins. Row semantics below began with an evidence survey of ~10,100 transcript
rows across Claude Code 2.1.198–2.1.227 and are now pinned by the provider
corpus recorded at 2.1.251. Every derived state keeps that survey's
fact-vs-inferred discipline. Requirement IDs (A1…H) are cited in
parentheses so review can trace them.

Agent-to-agent envelopes and parent/child lifecycle are specified in
`docs/A2A.md`; this document owns how their recipient rows and family actions
appear in the chat.

## Vocabulary

- **Chat** — the shared structured conversation screen over a known agent
  session. Claude's native layer consumes `claude_pty_transcript_v1`
  transcript and hook rows and sends seq-guarded semantic intents that the
  daemon encodes for the session's observed Claude version; Codex's native
  layer consumes its app-server control plane and sends typed requests.
  Both enter the same presentation shell without sharing a content model.
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
structured subscriptions (proved by `claude_pty_live`'s
`two_terminal_fanout` process scenario), so this is a UX
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
reading and scrolling work fine without scrollback. The feed remains
resize-independent and the Model stays the single source of truth. Mouse
capture gives the wheel to the feed; Shift+drag invokes the terminal's native
selection override, and the copy binding emits OSC 52, so copying does not
depend on terminal scrollback.

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
  created content's line count — `✔ Write plans/x.md (+20)`. Other tools
  render as compact one-liners. An unpaired `tool_use` in a final message renders as
  running (INFERRED-pending; FACT once the result lands). Oversized
  outputs render the truncation notice; the full text stays on disk
  behind the Effect seam. Consecutive reads, searches, and globs collapse into
  one exploration-run summary when there are at least two; see **Exploration
  runs** below. Edits, writes, and commands are consequential and always stay
  visible as individual blocks.
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
- **Agent messages.** A Claude user row whose content is an amux `<amux>` tag
  or a native `<cross-session-message from="amux:…">` folds to an inbound
  agent-message entry rather than a human prompt. An ordinary message renders
  its full body, a `completed` envelope wears a finished mark and closes to its
  first line until `<leader> m` opens completion bodies, and `exited` is a
  one-line notice. `mcp__amux__send` remains an outbound tool entry showing
  its target and a summary. These rows raise no attention by themselves.
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

### Block vocabulary and surfaces

Both agents paint through the same finite block vocabulary, but each agent
chooses blocks from its own native entry kinds. The vocabulary is: user prompt,
assistant markdown, thinking marker, tool one-liner, collapsed exploration
run, file change with magnitude, unified diff, ask panel, collapsed ask fact,
plan with its reader affordance, subagent line, agent-to-agent message, turn
rule, compaction rule, error, Codex MCP startup, and unrecognized row. The
composer and header are shared frame elements rather than feed blocks.

Surfaces are deliberately restrained. A user prompt has a tinted
`user_surface` and an accent bar in column zero. Unified diffs and ask panels
use the `panel` surface; added and removed diff rows add their semantic tints.
Assistant text, thinking markers, tool and file-change lines, plans, messages,
rules, errors, MCP startup, and unrecognized rows sit directly on the
background. Block focus is a focus-coloured bar at column zero, not another
box. Blank rows separate blocks; there is no outer border, left gutter, top
rule, or sidebar around the chat.

This is a presentation vocabulary, not a shared content representation. The
Claude layer folds Claude transcript entries and the Codex layer folds Codex
control-plane entries; neither projects into a common feed-entry enum. The
painters receive already-formatted facts and cannot infer agent semantics.

### Exploration runs

Two or more consecutive native read, search, or glob operations collapse to
one summary such as `⌄ 2 reads · 1 search · src/lib.rs · C-a o expand`.
`<leader> o` opens or closes the run under the focus bar, preserving member
order. A single exploration operation remains a normal tool line. Any edit,
write, command, ask, plan, or other consequential operation splits a run and
stays visible in both collapsed and expanded states. Grouping is computed by
the owning agent layer from its native kinds, never by inspecting rendered
text or command names; Codex currently has no native exploration kinds and
therefore does not synthesize runs from shell commands.

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

### Unified diffs and the reader's artifacts

Every landed diff uses one unified, single-column layout at every supported
width. A fixed gutter carries independent old and new line numbers; metadata,
context, added, and removed rows are explicit kinds. Added and removed rows
use semantic foreground and background tints on the `panel` surface. A
side-by-side layout is deliberately absent: at the standard 120-column
viewport it would leave roughly 55 columns per side and wrap ordinary code.

There are three native producers feeding one pure row painter:

- **Claude post-hoc** (feed): `toolUseResult.structuredPatch` hunks are
  restated verbatim — absolute line numbers, FACT magnitude for the
  feed line's `(+9 -2)`. The transcript already states every landed
  edit; the client never recomputes one.
- **Codex landed changes** (feed): the Codex layer parses its native unified
  patch into the same numbered row facts. Headerless or malformed patch text
  yields no speculative diff rows; the file-change line remains visible.
- **Ask-time** (permission panel): no hunks exist yet — the Claude hook
  carries only `old_string`/`new_string` — so the mini-diff is
  computed in the fold (`similar`, line-level, context 3) and
  rendered **numberless**, with an *estimated* magnitude; a
  `replace_all` edit says `(replaces every occurrence)` instead. A
  Write ask shows a `+` block head with `(N lines)`;
  create-vs-overwrite is unknowable before the tool runs, and the
  header claims neither.

Appearance, panel and reader alike: old number, new number, sign, then
content. Context rows carry both numbers, removals only the old number, and
additions only the new number; the counters advance independently. Long lines
wrap — never horizontal scroll — with blank-gutter continuation rows so the
gutter never lies; tabs expand before width math. The panel preview budget is at
most 8 screen rows (wrapped rows count), cut with a remainder line
that always states the arithmetic: `⋮ +K more lines · f full diff`.
The semantic colour family is `diff_added_fg`, `diff_added_bg`,
`diff_removed_fg`, `diff_removed_bg`, `diff_context`, `diff_meta`, and
`gutter`; both shipped themes and imported themes resolve all seven.

The reader is one overlay over a typed artifact model — a match, not
a viewer framework: **Plan** (markdown, the B2 renderer reused),
**Diff** (above), **NewFile** (Write content as a numbered `+`
block); **Text** (oversized tool output through the Effect seam) and
**Image** are reserved kinds, so B4's truncation notice and future
image placeholders have a stated destination. A new kind is an enum
variant, a match arm, and golden frames. Ask-time artifacts live with
their ask (evict bytes, never obligations); accepted plans keep B6's
keyed retention; nothing else is retained in V1.

### The semantic input seam (C6)

Views and reducers never author Claude key bytes. A prompt, interrupt,
permission-mode cycle, or answer becomes a typed `ClaudeEffect::SendIntent`
carrying the current transcript sequence and, for an answer, the ask id. The
wire preserves that shape in `ClaudePtyTranscriptV1Input`; arbitrary bytes are
available only through the separate raw terminal protocol.

The daemon converts the wire value exhaustively to `claude::pty::Intent` and
passes it to the provider session's control handle. `claude::pty::keymap` then
selects a versioned keymap, validates text and observed menu facts, chooses the
fixed binary-owned program for the intent, and writes the resulting bounded
key steps to the PTY. Menu digits, cursor movement, toggles, plan-review keys,
fixed delays, paste, and typed menu text are data in that keymap; which program
answers an intent remains code. An unverified shape or unsafe text refuses
before any byte is written.

The structured stream records `amux.claude.keymap` at session start and relink
and `amux.claude.input_result` for each attempt, including the keymap identity,
resolution basis, program, and outcome. [`KEYMAPS.md`](./KEYMAPS.md) owns the
format, resolution, provenance, and no-screen-detection limit.

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

These are 120-column schematics of the full-screen frame. Header and footer
fields are right-aligned to the terminal edge; blank-row runs are shortened
here, while the renderer and its committed goldens always produce all 40 rows. The
screen has no box around it: the terminal background is the frame. Feed blocks
begin at column zero with an accent or focus bar where applicable, the composer
is a block at the bottom, and the remaining rows belong to the feed. `▌` is the
text cursor.

### Idle

```
  fix-auth · claude @ mbp                                                                                   chat · idle

▎   add retry with backoff to the sync client

    I added exponential backoff to Client::reconnect — 6 attempts,
    jitter capped at 500 ms. SyncOptions grew a RetryConfig.

  ⌄ 1 read · 1 search · sync/client.rs · C-a o expand

  ✎ Edit sync/client.rs · +18 −4

  ✔ Bash cargo test -p amux-sync
    └ 34 passed, 0 failed (2.1s)

  ─ turn · 1m 42s ──────────────────────────────────────────────────────────────────────────────────────────────────────

                                                    [feed owns the remaining rows]

▎   Type a message▌

    enter send · ctrl+j newline · ? help                                                                   mode default
```

### Working

```
  fix-auth · claude @ mbp                                                                                chat · working

▎   now make the retry count configurable

  ~ thought for 6s

    The cap belongs in RetryConfig; I'll thread it through SyncOptions.

  ✔ Read sync/config.rs

  ▸ Bash cargo check -p amux-sync
    └ running

                                                    [feed owns the remaining rows]

  ◐ working · 24s · ctrl+x interrupt

▎   and please document it▌

    draft kept — send gated while working                                                                  mode default
```

The blank row above the working line is reserved for the queued-input
preview when queueing lands (deferred, door open).

### Permission ask

```
  fix-auth · claude @ mbp                                                                              chat · needs you

▎   now make the retry count configurable

    The cap belongs in RetryConfig; I'll thread it through SyncOptions.

  ✔ Read sync/config.rs

                                                    [feed owns the remaining rows]

    permission — Edit sync/config.rs (+4 -1) · 1 of 2

      pub struct RetryConfig {
     -    pub max_attempts: u8,
     +    pub max_attempts: u8,        // capped at 6
     +    pub jitter_ms: u16,
      ⋮ +1 more lines · f full diff

    › 1. Allow once
      2. Always allow access to /work from this project
      3. Deny — tell the agent why (optional)
    1-3/↑↓ select · enter confirm · f full diff · esc back (never answers)
```

### Question ask (multi-question, multi-select)

```
  fix-auth · claude @ mbp                                                                              chat · needs you

▎   before I write the migration, two decisions

                                                    [feed owns the remaining rows]

    questions

      [storage*] [rollout] [submit]
      Which stores should the migration cover? (select all that apply)

      1. [x] trust store       pairing + relay trust records
      2. [ ] session index     bounded tail metadata
    › 3. [x] recorder dumps    panic-hook recordings
      4. [ ] Other…            type your own answer
    1-4/↑↓ select · space toggle · tab next question · enter advance · esc back (never answers)
```

### Codex approval (the same frame)

```
  codex-retry · codex @ mbp                                                                            chat · needs you
  model=gpt-5.4 · approval=on-request · sandbox=workspace-write

  ⚠ $ cargo test --workspace · awaiting approval
    └ cwd /work/amux

                                                    [feed owns the remaining rows]

    approval — command

    $ cargo test --workspace
      └ cwd /work/amux
      └ Run the repository test suite?

    › 1. accept once
      2. accept and allow similar commands · unavailable in V1
      3. decline
      4. cancel
    ↑↓/1-9 select · enter confirm · ctrl+x interrupt
```

### Plan review (reader, fullscreen)

```
  plan                                                                                                   lines 1-31/31

────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  ## Approach

  1. Wrap Client::reconnect in retry_with_backoff
  - exponential base 200 ms, cap 5 s, jitter bounded
  2. Thread RetryConfig through SyncOptions
  3. Spec chapter sync::retry — cold start, mid-stream drop, give-up-after-cap

                                                    [reader body owns the remaining rows]

────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
  › 1. Approve — auto       agent proceeds, edits apply without asking
    2. Approve — manual     agent asks before each edit
    3. Request changes      feedback required

    ↑↓/pgup scroll plan · 1-3 select · enter confirm · esc back (plan stays)
```

### Scrolled back

```
  fix-auth · claude @ mbp                                                                                chat · working

▎   follow-up question 1

    It refuses the write and requests a dump — dispatch.rs::guard_seq.

  ✔ Read src/dispatch.rs

▎   follow-up question 2

                                                    [older feed rows remain above]

  ↓ scrolled back · wheel or pgdn to catch up · ctrl+end for the newest
  ◓ working · 9s · ctrl+x interrupt

▎   draft preserved while reading▌

    C-a k/j focus · C-a y copy                                                                             mode default
```

Sticky-bottom until the user scrolls; the paused rule with a new-entry
count is the honesty indicator opencode lacks, and the working line
stays visible so scrolling never hides that the agent is active.

### Read-only

```
  ci-triage · claude @ mbp                                                               chat · read-only · needs owner

▎   fix the flaky teardown

    The flake is a teardown race in the testnet harness; serializing the shutdown.

  ✔ Bash cargo test -p amux --test spec
    └ 2 failed: token_refresh, expired_session

                                                    [feed owns the remaining rows]

    the agent is asking permission — Edit testnet/harness.rs (+1)

      fn teardown() {
     +    drain();
          kill();
      }
    waiting for a writable client · f read the diff

  ⊘ read-only — you are observing this session

    pgup/pgdn scroll · f view diff · q back to fleet
```

## State transitions

Three orthogonal axes. **Phase** lives in the Model, changes only on
native agent facts, and is never changed by keys. **Input owner** is renderer
ViewState: `COMPOSER` (free typing), `ASK` (docked panel or plan
reader owns keys), `PENDING` (answer in flight), `READER` (read-only
full plan/diff). **Block focus** is an optional key in the shared feed: it is
the target for copy and exploration expansion and is independent of the input
owner. Scroll is a third renderer-local scalar; every input-owner state may be
scrolled.

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
| chat | `<leader> k` / `<leader> j` | same | focus the older / newer feed block and keep it visible |
| chat | `<leader> y` | same | copy the focused block, or newest block when none is focused, via OSC 52 |
| focused exploration run | `<leader> o` | same | expand / collapse the run in place |
| COMPOSER | Ctrl+T | READER (plan) | newest accepted plan; ←/→ steps between plans |
| READER | Esc | previous state | close; nothing answered |

Esc is one deterministic chain, view-only, checked in order:
**1** close the reader (a plan-review reader drops to its docked
panel) → **2** clear block focus → **3** step back ask stages, flooring at
the menu stage — never dismissing the panel → **4** reset feed scroll to
following (empty draft only) → **5** nothing. Esc never interrupts and never
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

Bindings are derived, not accumulated: this section records the principles
and the full derivation, and is the
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

That rule reaches the overlay, not just the hint rows: a key whose
effect depends on what is on screen is listed only while it has one.
The three family chords each track their own fact — `<leader> n` needs
somewhere else in this family to go, `<leader> m` a completion with a
body behind its first line, `<leader> a` a child's ask this chat could
host — and the fleet's `z` needs a family on the fleet. A hint row that
cannot fit an optional chord drops that chord rather than the row.

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
| wheel | chat feed | scroll three feed rows per notch; reaching bottom resumes following | plain |
| Shift+drag | chat | select text with the terminal's native selection override | plain |
| `<leader> k` / `<leader> j` | chat feed | focus older / newer block and keep it visible | plain |
| Ctrl+↑ / Ctrl+↓ | chat feed | focus older / newer block | ext |
| `<leader> y` | chat feed | copy focused block (newest when none) to the clipboard with OSC 52 | plain |
| `<leader> o` | focused exploration run | expand / collapse the run | plain |
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

## Themes

The chat and fleet consume one semantic `Theme`; painters name roles rather
than terminal colours. amux ships exactly two hand-tuned palettes, `dark` and
`light`. A third-party theme is a YAML base16 or base24 scheme with optional
semantic overrides:

```yaml
scheme: my theme             # optional metadata
variant: dark                # optional: dark or light
base00: "#101418"
base01: "#1b2229"
base02: "#242d35"
base03: "#52606d"
base04: "#73808c"
base05: "#d8dee9"
base06: "#edf1f5"
base07: "#ffffff"
base08: "#d06f79"
base09: "#d49a58"
base0A: "#d2b86c"
base0B: "#82b482"
base0C: "#70a9a1"
base0D: "#6599b3"
base0E: "#9a86c8"
base0F: "#aa7d66"
tokens:
  accent: "#5fb3c6"
  diff_added_bg: "#16261b"
```

All `base00` through `base0F` values are required. Supplying any base24
extension makes all of `base10` through `base17` required as well. Colours are
six-digit hexadecimal RGB strings, with or without `#`; malformed colours and
unknown token names are startup errors that name the key.

| Semantic roles | base16 | base24 |
| --- | --- | --- |
| background | `base00` | `base00` |
| user surface | `base01` | `base01` |
| panel | `base02` | `base02` |
| muted and gutter | `base03` | `base03` |
| diff metadata | `base04` | `base04` |
| text and diff context | `base05` | `base05` |
| emphasis | `base06` | `base06` |
| error and removed foreground | `base08` | `base12` |
| warning | `base09` | `base14` |
| success and added foreground | `base0B` | `base13` |
| code | `base0C` | `base17` |
| accent | `base0D` | `base15` |
| focus | `base0E` | `base16` |
| added / removed diff backgrounds | `base01` tinted with `base0B` / `base08` | `base01` tinted with `base13` / `base12` |

The `tokens:` map is applied after that mapping and may override any of
`background`, `text`, `muted`, `emphasis`, `accent`, `user_surface`, `panel`,
`focus`, `code`, `ok`, `warn`, `error`, `diff_added_fg`, `diff_added_bg`,
`diff_removed_fg`, `diff_removed_bg`, `diff_context`, `diff_meta`, or `gutter`.
Base16 has no diff-background slots: unless directly overridden, amux starts
both backgrounds from `base01` and tints the added and removed surfaces with
the scheme's own success and error hues.

After mapping, amux repairs the mechanically mapped palette for the surfaces
the TUI actually paints. Foregrounds that miss their contrast floor are moved
only along HSL lightness until they clear it, so the rendered RGB value may
differ from the file's hexadecimal value while its hue and saturation survive.
A token named explicitly under `tokens:` is the theme author's final word: it
is taken literally, skipped by this repair, and carries no readability
guarantee.

Select the palette and terminal colour policy in the ordinary amux config:

```yaml
ui:
  theme: dark                  # dark, light, or a YAML file path
  color: auto                  # auto, truecolor, or ansi
```

A relative `ui.theme` path resolves beside the active config file, not the
process working directory. Theme files are loaded before amux enters the
alternate screen, so an invalid file fails as a normal startup error. In
`auto`, amux uses truecolor only when `COLORTERM` says `truecolor` or `24bit`
and `NO_COLOR` is unset; `truecolor` and `ansi` force the mode. Imported RGB
values that came through the base mapping degrade to named 16-colour ANSI
faces chosen by a preservation score: contrast shortfall and loss of
chromatic-or-neutral identity, hue, and lightness are all penalized. This is
not nearest-RGB rounding, which can collapse a dark ramp onto one invisible
face. Direct `tokens:` overrides skip that repair: their RGB value stays
literal and their ANSI face stays at its initial named-colour mapping, with no
readability guarantee. Painters keep the same semantic roles without branching
on terminal capability.

## Reproducible screenshots with amux-shot

The committed `amux-shot` developer tool renders named production fixtures
through the same pure render boundary as the text goldens. Every capture is a
120-column by 40-row terminal rasterized with vendored fonts; it does not open
a PTY, connect to a daemon, or depend on the local terminal. From the repository
root:

```sh
cargo run -p amux-shot -- list
cargo run -p amux-shot -- render claude-idle --out target/shot/claude-idle.png
cargo run -p amux-shot -- render claude-idle --theme light --color ansi \
  --out target/shot/claude-idle-light-ansi.png
cargo run -p amux-shot -- render-set themes --out target/shot/themes
cargo run -p amux-shot -- record-scroll claude --out target/shot/scroll
cargo run -p amux-shot -- verify target/shot
```

`--theme` accepts `dark`, `light`, or a YAML path; `--color` accepts
`truecolor` or `ansi`. The declared sets are `chat`, `agent-specific`,
`gallery`, `scroll`, `copy`, `collapse`, `themes`, `fleet`, and `all`.
`record-scroll` accepts `claude` or `codex` and drives real mouse-wheel events
through the production handler. Each render records its state, theme, colour
mode, viewport, pixel dimensions, filename, and SHA-256 digest in a manifest;
`verify` checks hashes, dimensions, and decodability. These PNGs and GIFs are
repeatable review proof, not an image-golden CI gate; text and semantic
style-map goldens remain the regression gate.

## Architecture constraints (G)

`docs/UI.md` legislates the architecture; this section only pins the
chat-specific consequences.

- The Claude chat layer is a typed child model consuming native rows —
  no intermediate representation, no capability flags (G1).
  Tolerate-unknown runs in both directions: unknown row types render as
  explicit unrecognized entries, and rules never gate on key-set
  equality or crash on absent fields, because the format is internal
  and drifts by version.
- The Codex chat layer independently consumes native app-server entries and
  retains Codex-only approvals, network-policy amendments, MCP startup rows,
  and token usage. Neither layer implements a shared content trait. The shared
  shell accepts painted blocks and owns presentation mechanics only.
- Interpretation happens only in layer folds; core transports rows
  opaquely; nothing new rides the peer wire (G2). Views format, never
  decide — no error-string sniffing, no heuristics in renderers.
- All chat Msgs flow through the recorder from day one; redacted
  real-session recordings graduate into committed Tier-1 fixtures; the
  differential fold-equals-live property extends to the Claude layer
  (G3). High-rate transcript replay is coalesced before recording.
- Golden-frame coverage exists for every feed entry kind and every ask
  form, including the read-only fact variants (G4).

## Executable specifications and live verification (H)

The former monolithic real-Claude test leg is split at the provider boundary.
`claude::specs::pty` owns 18 executable claims: prompt and multiline prompt,
tools, permission variants, plan variants, question forms, interrupt,
permission-mode cycle, and compact/clear relinks. Each claim is the same
function in record and verify modes. Verification runs offline against strict,
sanitized recordings captured at Claude Code 2.1.251, including byte-for-byte
intent writes, hook and transcript transports, provenance inventories, orphan
checks, and the minimum supported version.

`crates/amux/tests/claude_pty_live.rs` retains only facts recording replay
cannot establish, plus one end-to-end semantic-chat witness. Its current
scenarios cover semantic chat, stale-sequence refusal, two-terminal fan-out,
external read-only hook discovery, native-socket and PTY-fallback A2A delivery,
agent MCP tools, and cross-kind completion. It is opt-in, always run under
`timeout`, uses Haiku by default, and prints the observed Claude Code version
and model first.

The committed `crates/amux/tests/fixtures/rows/claude-pty/` rows are not hand
captures. `derived_rows` replays `crates/claude/fixtures/pty/` through
`claude::pty::from_recording` and the real amux PTY adapter, then compares all
18 row fixtures byte for byte. Assertions use row
structure, ids, answer objects, and filesystem outcomes rather than assistant
wording. `claude-probe` runs every provider specification against an installed
binary, appends passing versions to recording and keymap ledgers, re-records
only broken claims, and writes additive drift for review.

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
  they shipped an escape flag for it. Mouse capture exists so the wheel
  reliably scrolls the feed; expansion and copy remain keyboard actions,
  Shift+drag reaches native terminal selection, and OSC 52 provides a
  selection-independent clipboard path.
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
- **Message-level retry and archive.**
- **Composer prompt-history recall** — adopt opencode's edge-of-buffer
  + unchanged-buffer guard when it lands.
- **Command palette** — the binding table is palette-ready; V1
  discoverability is the footer line + `?` overlay.
- **Syntax highlighting** in fenced code and diffs — additive renderer
  work; plain blocks ship first.
- **Mouse support beyond wheel scrolling** — clicks, drags, hover, and
  click-to-focus remain absent; any future affordance must stay
  keyboard-reachable and renderer-pure.
- **Additional built-in themes.** Dark and light are the only shipped
  palettes. Other palettes belong in base16/base24 files rather than amux's
  source tree.
