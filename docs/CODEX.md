# Codex in amux

Status: implemented. This document owns the OpenAI Codex integration —
which process owns what, the two planes a codex agent exposes, the row
vocabulary on the structured plane, and the client-side layer that folds
it. Companions: `docs/PROTOCOL.md` owns the wire, `docs/ARCHITECTURE.md`
owns the system, `docs/UI.md` owns the client layer's doctrine, and
`docs/CHAT.md` owns the Claude chat surface this one deliberately does
not imitate. `docs/A2A.md` owns the shared agent-message envelope, tools,
and family lifecycle.

The executable half of this document is `crates/amux-ui/tests/spec/`
(`codex_feed`, `codex_asks`, `codex_write`, `codex_agreement`) plus the
opt-in real-Codex C suite in `crates/amux/tests/codex_capture.rs`. Where
prose and passing spec disagree, the spec wins.

## What a codex agent is

`amux new codex` creates an agent whose backend is a **thread on a Codex
app-server**, not a PTY running a CLI. That is the whole reason codex
needed its own integration rather than a config entry: Claude is a
process amux owns and reads; codex is a *server* amux talks to, and the
terminal UI is one of two consumers rather than the source of truth.

```
amux daemon
  └── CodexSession (one per agent)  ──┐
  └── CodexSession                  ──┼──► one shared CodexClient
  └── CodexSession                  ──┘      └── codex app-server (supervised)
                                                   └── thread per agent
```

One app-server serves every codex agent in the daemon. Threads are the
unit of identity: an agent *is* a thread id, which is why suspend and
resume survive a restart of both amux and the server.

### A started thread has no rollout yet

`thread/start` creates a live, non-ephemeral thread and reports the
prospective rollout path, but does not materialize that rollout. (Starting
the app-server can write other Codex-home state.) Operations that require
the rollout — including `thread/resume`, which the raw TUI runs at
bootstrap, and `thread/archive` — fail with `no rollout found for thread
id` until an unrelated mutation persists it.

Codex 0.147 exposes no persist call. Several side effects materialize a
thread: naming, memory mode, Git metadata updates, injected history, and
feature-gated goals all do; settings updates do not. Naming is the least
invasive option that applies universally. Memory mode is experimental and
behavioral, Git metadata is neither universal nor inert, injected items
alter the transcript, and goals are feature-gated and can start work. A
name is standard, visible, and replaceable, though 0.147 cannot restore it
to `None`.

So **every codex thread amux creates is named**, whether or not the agent
is. The two names are separate: an unnamed agent gets the bootstrap label
`amux-<first 8 hex of the agent id>` on its *thread*, and stays unnamed
itself, showing the usual `name → provider label → short id` fallback in
the clients. Naming the agent later overwrites the bootstrap label;
clearing the name restores it.

The initial structured attachment can use the live in-memory thread, but
that does not remove the need for eager materialization: amux's structured
reconnect, suspend/resume, and daemon-recovery paths all issue
`thread/resume`. Fresh attachments therefore materialize before publishing
their durable thread id. A successful resume is already authoritative, so
later naming failure remains retryable metadata work and never disables raw
attach.

This is a workaround for upstream behaviour that is arguably a defect — a
server that hands you a thread and then refuses to resume it — and should
be revisited if codex grows a real persist call.

## The two planes

A codex agent exposes two independent subscription protocols. Both can
be live at once, on the same agent, from different terminals — checkpoint
#3 drove exactly that, and C.7 locks it.

| plane | protocol | carries | consumers |
|---|---|---|---|
| structured | `codex_sdk_v1` | JSON-RPC rows folded into a typed layer | the native amux chat screen |
| raw | `terminal_v1` | live PTY bytes from `codex resume` | the genuine codex TUI, any number of terminals |

`terminal_v1` is agent-independent (P1) — it is the same byte plane any
agent's PTY uses, and codex simply has one. The raw plane is not a
fallback or a degraded mode: it is the real codex TUI, and it answers
things the structured plane deliberately cannot (see *Unanswerable
obligations*).

The raw PTY is spawned lazily on the first `terminal_v1` subscription.
Concurrent raw subscribers share that PTY and receive the same live bytes.
Dropping the final raw subscription terminates its process group and retires
the cached PTY; a later subscription starts a fresh `codex resume` for the
same durable thread. Codex's own upstream replay restores the TUI history —
amux does not retain detached PTY bytes or processes. The structured
`codex_sdk_v1` attachment is independent and stays live across raw
detach/reattach.

## Connecting: four modes, one fallback

`ensure_daemon_with_fallback` resolves to one of:

- **Existing** — a healthy server already owns the well-known socket.
  amux attaches. This is what makes a plain `codex` session and an amux
  agent coexist.
- **Spawned** — amux spawned and supervises the server on the well-known
  socket.
- **Private** — the well-known socket was unusable (occupied, or a
  `CODEX_HOME` long enough to threaten the 103-byte `SUN_LEN` cap), so
  amux spawned a server on its own socket.
- **PrivateExisting** — amux's private socket already had a healthy
  server.

Dropping a supervised `DaemonProcess` terminates its **process group**,
not just the child.

There is exactly one path to a working app-server, and the auth
preflight uses it: `amux new codex` reaches the same cached,
fallback-capable connection a turn would, calls `account/read`, and
fails with *run `codex login`* rather than an opaque connect error. An
earlier version had a second, simpler connection path for the preflight
alone; it aborted in precisely the configurations the backend was built
to support. Two paths that can disagree is one too many.

## The row vocabulary (frozen)

The structured plane carries **verbatim upstream rows** —
`{"type": "<upstream method>", ...params}`, unmodified — plus exactly seven
rows amux synthesizes. The synthesized set is closed; adding to it is a
protocol change.

| row | means |
|---|---|
| `amux.codex_ready{resumed?:true}` | the session is connected and its stream is authoritative; `resumed:true` marks only the first successful attachment from an initially persisted thread id |
| `amux.codex_gap{reason}` | continuity was lost; what follows is not contiguous |
| `amux.codex_reconnect_error{error}` | a recovery attempt failed |
| `amux.codex_approval_required{request_id, availableDecisions}` | an approval is outstanding — `availableDecisions` is wire-verbatim |
| `amux.codex_approval_resolved{request_id, reason}` | it is no longer outstanding, and why |
| `amux.input_result{input_id, ok\|error{message}}` | the fate of one submitted input |
| `amux.codex_message{id, kind, from, from_id?, context?, text, delivery}` | a daemon-authored agent message accepted by the Codex carrier; `delivery` is `inject_queued`, `inject_started`, or fallback `turn_started` |

Agent delivery uses `thread/inject_items`. An idle thread then receives an
empty-input `turn/start`; a busy thread keeps its active turn and queues the
injected message. If injection fails, amux starts a visible turn with the same
tagged text. The synthesized row above is the recipient-side record because
the native Codex transcript does not expose injected items. Managed threads
also receive the shared `agents`, `send`, `spawn`, `stop`, and `status`
definitions as dynamic tools; calls execute through the owning agent's daemon
identity and are answered as dynamic-tool results.

`amux.codex_approval_resolved.reason` is one of `answered`,
`response_failed`, `answered_elsewhere`, `connection_lost`,
`queue_overflow`, `event_stream_error`, `session_stopped`. An approval
that vanishes because another client answered it is a different fact
from one that vanished because the connection died, and the UI says so.

Bare `amux.codex_ready` remains the shape for fresh attachment and later
same-process reconnects. `resumed:true` does not report a gap: earlier feed
rows are not re-rendered, while the persisted Codex thread keeps its context.
The marker is one-shot. The first successful ready-producing attachment
consumes it regardless of provenance and includes it only when that attachment
resumed the initially persisted id; an ambiguous recovery settled as a fresh
thread cannot mark a later reconnect. `CodexLayer` renders the marked boundary
at the top of the feed as
`resumed · earlier history not re-rendered · context intact`.

**Unrecognized rows render honestly as unrecognized.** They are never
dropped and never guessed at. The one startup family promoted into typed
state is `mcpServer/startupStatus/updated`: an exact row with a nonempty
server name and `starting`, `ready`, `failed`, or `cancelled` updates one
retained aggregate in place by name, preserving the entry's original id and
creating sequence. `error` and `failureReason` may be absent, null, or strings.
The renderer shows one compact count line instead of one row per server.
Malformed rows, missing or non-string names, invalid diagnostic fields, and
future status spellings remain unrecognized. Upstream drift is still data: it
is captured and diffed, and never breaks the fold.

## The client layer

`CodexLayer` (`crates/amux-ui/src/codex/`) is a typed per-agent layer, a
sibling of `ClaudeLayer`, not a specialization of anything shared. Per
`docs/UI.md`, asymmetry between agents is expressed, not papered over:
there is no generic intermediate representation and `AgentLayer` is an
exhaustive enum.

### One classification, projected

Everything the UI asks about a codex session derives from **one private
`Situation`**:

```rust
struct Situation {
    state: SituationState,   // Exited | Closed | Unknown | ReadOnly |
                             // Replaying | AwaitingApproval | ... | Idle
    active_turn: bool,       // orthogonal facts, deliberately NOT folded
    input_in_flight: bool,   // into `state`
    observer_readonly: bool, // inventory policy, not observed session phase
}
```

`phase()`, `attention()`, `send_gate()` and the four write permissions
are all projections of it. The kernel's stream lifecycle is wrapped in
at the same point, so there is one place where "opening/replaying is not
yet authoritative" is decided.

The orthogonal booleans are load-bearing and were learned the hard way.
An earlier `Situation` was an enum in which `InputInFlight` *replaced*
the active-turn state, and permissions were derived from the collapsed
`SendGate` value. A turn can be active *and* have an input in flight;
collapsing them meant a stalled RPC made an active turn permanently
uninterruptible — the escape hatch gone at exactly the moment it is
needed. **A single source of truth must be lossless with respect to
every question asked of it**, or it will answer some of them wrong with
the full confidence of a deliberate architecture.

### Rules that hold

Locked by `codex_agreement` and by a checked projection invariant over the
public phase, cached `AgentCard` attention, and send gate. Observation-time
offline and staleness degradation remains owned by `Model::effective_attention`:

- `phase == Unknown` implies `attention == Unknown`.
- app-server reconnect read-only outranks a pending ask in attention
  (→ `Unknown`).
- `thread/closed` refuses sends but stays recoverable.
- while the stream is `Opening` or `Replaying`, attention is `Unknown` —
  rows seen so far are a prefix, not a conclusion.

The invariant is checked after every Msg of every registered spec sequence
(`wire_free.rs`), so it is a CI control, not only a runtime one. In ordinary
runtime operation, a violation logs at error level, attempts one recorder dump
per violation kind, sets a sticky warning visible in fleet and both native
chats, and keeps folding even if the dump fails. Exact
`AMUX_INVARIANT_FATAL=1` restores a panic in every build.

### Writes

Four actions, one derivation — `write_permission(model, agent, action)`:

| action | permitted when |
|---|---|
| prompt | the session is live and no turn is active |
| steer | a turn is active |
| interrupt | there is an active turn, **including while an input is in flight** |
| answer | an approval is pending |

Session-level refusals (unavailable, exited, closed, replaying,
app-server read-only, unknown) are subtracted *before* an action rule runs, in
the type: `session_state()` returns `Result<LiveState, &'static str>`, so a
per-action rule cannot see a state it has no business ruling on. Codex's
app-server reconnect `ReadOnly` is a lifecycle fact: it projects
`CodexPhase::ReadOnly`, `Attention::Unknown`, and `SendGate::ReadOnly` with its
reconnect-specific refusal. Inventory `Agent.readonly` is different,
orthogonal observation policy. It preserves the session's visible phase and
attention, but after lifecycle refusals yields `SendGate::ObserverReadOnly`
and `agent is read-only — you are observing this session`.

Reducers consult the same permission before emitting input or optimistic
state. Views and key handlers ask the classified action queries; they do not
re-derive preconditions from raw layer fields. A writable active turn remains
interruptible while an input is in flight, but an observation-only one does
not.

## Prompts, steers and echoes

Prompts are **protocol-sourced**: a normal prompt arrives back as a
`userMessage` item and is rendered from that, with no local echo. Steers
have no such item, so a retained `input_id` is promoted to a
`PromptSource::SteerEcho` entry only on a correlated `amux.input_result`
success. Nothing appears in the feed that the server did not confirm.

## Approvals, and unanswerable obligations

Command and patch approvals are answerable: each retained action keeps its
wire value and the answer goes back over the same connection. Known object
choices receive human labels only when they agree with typed facts from the
correlated request: exec-policy amendments become “accept and allow similar
commands”, and network-policy amendments become
“apply network policy change · {allow|deny} {bounded host}”. Unknown objects
show only a bounded, control-sanitized kind and scalar detail, never raw
serialized JSON; object choices remain unavailable in structured V1.

Dynamic tool calls are answered by amux itself through the owning agent's
daemon identity. They no longer become approval asks, so the structured
client's dynamic-tool approval rendering is retired; its reserved path remains
in place for a separate client-side follow-up.

`item/tool/requestUserInput` is the one known **unanswerable**
obligation. Answering it needs free-form content the frozen input shape
cannot express, so amux renders it as visibly blocked rather than
offering a decline-only half-answer that would look like an answer. The
escape hatches are interrupt, or raw mode — where codex's own TUI
answers it properly, in band. This is a deliberate, recorded limit.

## Modes

`default_open_mode` (`chat` | `raw`) decides what `amux attach` opens;
both remain available per agent. Chat mode is the native amux screen —
composer, feed, approvals, `Ctrl+X` to interrupt. Raw mode is
`codex resume` on a PTY, byte-identical for every subscriber.
Because both current creation modes open interactively, `amux new codex`
requires TTY stdin and stdout and refuses before creating anything when either
is absent.

## Testing

Three tiers, in increasing cost:

1. **`crates/amux-ui/tests/spec/`** — pure reducer folds, no clock, no
   IO. Every registered sequence is swept for invariant violations after
   every Msg.
2. **`crates/amux/tests/codex_capture_waiters.rs`** — the structural
   waiters, driven offline from redacted rows captured off real failing
   runs. No codex process, no credentials, no network.
3. **`crates/amux/tests/codex_capture.rs`** — the **C suite**: opt-in,
   real codex, fourteen scenarios (C.1–C.14) covering create+pong, approval
   allow and deny *with filesystem assertions*, interrupt and reuse,
   suspend/resume across a server restart (including the first post-restart
   `resumed:true`, stable thread identity, and remembered context), real
   process-group daemon recovery, raw+structured coexistence, two-subscriber
   byte fanout, and
   raw attach on an *unnamed* agent — the product default, and the one
   parameter a hardcoded fixture hid for the whole of P9 — including
   final-detach teardown and fresh raw reattach, plus unnamed zero-turn
   suspend/resume. C.11–C.14 pin dynamic tools, idle and busy injected
   messages, and the final-assistant-message ordering at turn completion.

C.9 proves its independently checked process facts: final-detach process-group
exit, a newly created raw process on reattach, and survival of the original
structured stream. Its screen-content oracle did **not** establish that the
resumed raw Codex composer is usable. The owner-authorized VT100 attempt was
inconclusive and no further oracle is part of this close-out, so C.9 must not be
cited as a composer witness.

The C suite is inert in `cargo test --workspace` — with no scenario named
it prints a skip note and exits 0 before creating a scratch directory,
server, process, or request. `SCENARIOS` binds id, requirement, timeout
and runner in one row so they cannot drift apart.

It drives the prebuilt `target/debug/amux`, which `cargo test -p amux
--test codex_capture` does not rebuild, so a stale binary would report on
code that is not in the tree — passing changes it never executed, and
"passing" reverts too. The harness reads Cargo's `target/debug/amux.d`
depfile and refuses to start when the binary is older than one of its
actual prerequisites. This covers Rust, proto, plugin, and generated
inputs without inventing a second workspace dependency graph.

```sh
cargo build -p amux-cli
AMUX_CODEX_CAPTURE_DIR=target/codex-capture \
  timeout 600 cargo test -p amux --test codex_capture -- c-all
```

Two standing rules, both bought with pain elsewhere in this repo:

- **Waiters parse structure, never raw text.** No substring matching on
  serialized JSON, and no assumptions about the count or position of
  upstream startup noise — that count was observed to vary between 4 and
  9 in a single afternoon.
- **Committed fixtures are regression anchors, not a cache.** They are
  never rolled forward wholesale. Graduation is a deliberate, separate,
  reviewed change.

Every capture records codex version, model, date and scenario id.
Upstream drift is recorded and diffed, never guessed at.

## Decisions

### Raw PTY idle lifecycle

The raw Codex PTY tears down on last detach. This is a measured policy, not a
guess: on macOS, `ps` RSS for the detached zero-turn `codex resume` process
pair was sampled five times, one second apart, after a five-second settle.
The samples totalled 151,232, 151,184, 151,248, 151,248, and 151,248 KiB.
Their median was **151,248 KiB (147.703125 MiB)**, above the signed
**25,600 KiB** threshold. The measurement covered the raw PTY group leader
and its direct child after the sole `terminal_v1` subscriber had detached;
no model turn was sent.

Therefore the last raw detach retires and terminates that PTY epoch. Reattach
lazily runs a new `codex resume`, relying on the durable Codex thread and its
upstream replay. Structured attachment retention is unaffected.

### Invariant failure policy

Invariant violations are loud but nonfatal by default in every build: log at
error level, attempt one bounded recorder dump per violation kind, set the
sticky visible warning, and continue. Exact `AMUX_INVARIANT_FATAL=1` restores
the panic. The panic found a real projection defect during a hand-drive and
remains valuable for CI and deliberate verification; it is not an acceptable
default for an ordinary first-run client.

CI test and E2E steps and the required close-out test invocations opt into
fatal mode. A bare local `cargo test` intentionally inherits the nonfatal
runtime default, while the differential spec independently asserts invariant
emptiness after every registered Msg.

### Retained Model state (D9)

Dump provenance is not enough to keep an otherwise unread field unless its
opposite-layer twin is retained on the same basis. Applying that rule across
Claude and Codex deleted six serialization-only, asymmetric fields:
`FileChange.diff`, `TokenUsage.cached_input_tokens`,
`codex::Ask.available_decisions`, `claude::PromptEntry.at`,
`claude::MessageEntry.at`, and `AcceptedPlan.plan_file_path`. The private Model
dump shape changed intentionally; amux is unreleased and promises no dump
compatibility.

`PromptEcho.at` remains because it is no longer dump-only provenance. Claude's
staleness classifier reads the dispatch timestamp so a fresh optimistic send
outranks an old transcript, while an unresolved send can still age to
`Unknown`. Codex has authoritative turn lifecycle rows and needs no artificial
timestamp twin.

### MCP startup aggregation (D10)

The typed option fit inside the focused scope, so startup rows are aggregated
rather than suppressed. Exact `starting`, `ready`, `failed`, and `cancelled`
updates share one entry keyed by server name and update it without changing the
entry id or creating sequence. Its compact line is
`MCP servers · N starting · N ready · N failed[ · N cancelled]`. Failure
selects the error glyph/style; otherwise starting selects neutral,
cancellation selects warning, and only an all-ready/no-cancelled set selects
success. Malformed and future rows remain unrecognized.

## Parked / future work

- External live-thread sync (P10) and adopting externally-started
  threads (`amux attach` external pickup). Optional from day one;
  blocked culturally on the upstream co-presence RFC (openai/codex
  #21551) anyway.
- Transcript capture by tailing `~/.codex/sessions/**`.
- Windows support (codex agents are unix-only; the daemon-side refusal
  already exists).
- Tracking the co-presence RFC for live SDK mirroring.
- The typed-command generalization ("dispatching a write without
  passing the gate is not expressible"). Right instinct, real design
  project, not this workstream. This is the named astronaut item.
- **Transactional agent creation** ("failed `amux new` leaves nothing").
  Parked deliberately: a real leave-nothing guarantee cannot be
  client-driven (the client can die mid-attach), so it implies a
  daemon-side provisional-agent lifecycle — the largest hidden design
  item on the original list. Post-create failures are client-side events
  happening to a *healthy* agent (auth is preflighted daemon-side in
  `PtyAgentHost::create` before anything commits), and auto-destroying a
  healthy agent contradicts "agents outlive terminals". Batch B ships
  the cheap 90% instead. Record this reasoning so it is not re-litigated.
- `SUN_LEN` symlink pre-check edge (INVESTIGATIONS Tier 2 item 7) — the
  SDK fails loudly; accepted as honesty-of-error-message residual.
- `Model::claude(id)` / `AgentCard::claude()` naming tidy and the
  `AgentDeps` non-unix collapse (Tier 3) — real, harmless, not close-out.
