# Codex in amux

Status: implemented (current provider baseline: codex-cli 0.150.1). This
document owns the OpenAI Codex integration —
which process owns what, the two planes a codex agent exposes, the row
vocabulary on the structured plane, and the client-side layer that folds
it. Companions: `docs/PROTOCOL.md` owns the wire, `docs/ARCHITECTURE.md`
owns the system, `docs/UI.md` owns the client layer's doctrine, and
`docs/CHAT.md` owns the Claude chat surface this one deliberately does
not imitate. `docs/A2A.md` owns the shared agent-message envelope, tools,
and family lifecycle.

The executable half of this document is the canonical `codex` crate's
recorded specifications, the amux backend derivation tests, the
`crates/amux-ui/tests/spec/` folds (`codex_feed`, `codex_asks`,
`codex_write`, `codex_agreement`), and the opt-in `codex_live` suite. Where
prose and a passing specification disagree, the specification wins.

## Model, effort and permissions

The host asks the session's app-server for every `model/list` page. Native
clients receive the selected model and effort, available models and each
model's effort levels through `amux_ui::provider::facts`. Missing discovery
leaves the choice lists empty. The UI never supplies a built-in catalogue.

`SetModel`, `SetEffort` and `SetPreset` are typed structured inputs. A model
change selects that model's reported default effort. Unreported models and
unsupported efforts fail without changing the selection. Settings use the
ordinary idle send gate, including replay, observation-only and input-in-flight
refusals. Claude PTY returns `PtySettingsUnavailable`.

Codex accepts these overrides on `turn/start`. The provider control retains the
selection across transport reconnects and for subsequent prompt and empty turns;
it does not start a turn just
to change settings. An acknowledgement means the host selected the next-turn
configuration. `amux.codex_ready.session` carries initial metadata and
`amux.codex_settings.session` replaces it after a selection. These rows update
session facts in place, with no transcript row or optimistic model label.

## What a codex agent is

`amux new codex` creates an agent whose backend is a **thread on a Codex
app-server**, not a PTY running a CLI. That is the whole reason codex
needed its own integration rather than a config entry: Claude's drivers are
processes amux owns, while codex is a *server* amux talks to, and the terminal
UI is one of two consumers rather than the source of truth.

Provider behavior lives in the repository's canonical `crates/codex` crate.
Its public daemon boundary is
`codex::Session { events: ThreadEventStream, control: ThreadControl }`.
The code under `crates/amux/src/agents/codex` is an adapter that turns those
events into amux rows, routes typed controls, implements delivery, and records
the thread id for suspension; it is not a second provider implementation.

```
amux daemon
  └── codex::Session (one per agent)  ──┐
  └── codex::Session                  ──┼──► one shared Codex client
  └── codex::Session                  ──┘      └── codex app-server (supervised)
                                                   └── thread per agent
```

One app-server serves every codex agent in the daemon. Threads are the
unit of identity: an agent *is* a thread id, which is why suspend and
resume survive a restart of both amux and the server.

### A fresh thread must be resumed before publication

`thread/start` creates a live, non-ephemeral thread and reports a prospective
rollout path before writing its history file. The existing amux connection can
use that thread immediately, but the vanilla TUI opens a second connection and
calls `thread/resume` for the same id. amux also resumes by id after transport
loss, suspension, and app-server restart. A ready agent must support those
paths even before its first message.

Previously amux named the thread to force Codex to persist its rollout.
Codex 0.153.4's paginated history stores the name in SQLite and the name index
without writing the rollout. Naming succeeds, but the TUI's metadata-only
resume (`excludeTurns: true`) fails while constructing pagination cursors:
`invalid paginated history lineage ... missing source rollout`. Sending a
first message through amux's structured UI writes the history and makes later
raw attachment work.

amux therefore follows fresh creation with `thread/resume` on the same
connection and thread id, explicitly setting `excludeTurns: false`. In this
Codex version, the loaded paginated-thread resume handler persists the rollout
when history is requested. No user message is injected, no model turn starts,
and no second conversation is created.

The resume returns a replacement SDK handle and event registration. Startup
adopts that handle, including events staged before the response, before it
publishes the id, emits readiness, or allows a raw PTY or suspended state to
use it. A resume RPC failure keeps startup private and retries the same id.
Transport loss carries the unconfirmed id into reconnect recovery, which tries
that id before considering a replacement. The bootstrap resume does not mark a
new agent as a previously resumed conversation in the UI.

Naming remains metadata. Every thread keeps its existing recognizable label:
the agent's name or `amux-<first 8 hex of the agent id>` for an unnamed agent.
The latter stays unnamed in amux itself. A failed name update does not block a
successful resume; the serialized name reconciler retries it after publication.

This is a version-specific compatibility workaround, not a documented Codex
durable-creation API. The regression suite must prove zero-turn raw attachment
and recovery against the installed provider version. Revisit the extra resume
when upstream supports this lifecycle explicitly or fixes metadata-only resume
to satisfy its own history-cursor prerequisites. Owning a private app-server
does not change this requirement: both shared and private modes use the same
thread creation and TUI attachment protocol.

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

## Agent tools use thread-scoped MCP

Every thread amux owns receives one required stdio MCP server named `amux` in
the request-local config used for `thread/start` and cold `thread/resume`.
Reconnect reuses the same config. The server exposes exactly `agents`, `send`,
`spawn`, `stop`, and `status`; the allowlist is derived from the shared tool
definitions, automatic approval is enabled, startup is bounded to 10 seconds,
and each call is bounded to 60 seconds. No persistent Codex configuration is
read-modified-written.

The route is captured by the daemon that created the session. It names the
absolute running amux executable, passes `mcp agent --socket-path` with the
exact absolute daemon socket, and injects the owning agent and host UUIDs. When
the daemon loaded a config file, its normalized absolute path is also supplied
as `AMUX_CONFIG`; a true-default configuration is explicitly distinguished and
omits that variable. A vanished or relative route, a config/socket mismatch,
an unreachable daemon, or a stale or cross-host identity fails required-server
startup before any tool is exposed.

The server preflights once, then opens a fresh daemon connection for every tool
call. If a connection or RPC is interrupted, that call returns an MCP error and
is not retried because its mutation may already have happened; the next call
gets a new connection and can recover after a daemon restart. A raw TUI which
joins an already-running amux-owned thread shares this MCP runtime. amux does
not claim that it can inject the route into a vanilla thread while another
client already owns that live thread; a later cold resume under amux ownership
is the supported boundary.

## The row vocabulary (frozen)

The structured plane carries **verbatim upstream rows** —
`{"type": "<upstream method>", ...params}`, unmodified — plus exactly eight
rows amux synthesizes. The synthesized set is closed; adding to it is a
protocol change.

| row | means |
|---|---|
| `amux.codex_ready{resumed?:true, session}` | the session is connected and its stream is authoritative; `resumed:true` marks only the first successful attachment from an initially persisted thread id |
| `amux.codex_settings{session}` | the host selected a new next-turn configuration; replaces session facts in place |
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
the native Codex transcript does not expose injected items. Agent-tool calls
arrive as `mcpToolCall` rows from server `amux`. A registered tool on that
server is presented as amux fleet work; calls from another MCP server and every
`dynamicToolCall` remain generic upstream work.

`amux.codex_approval_resolved.reason` is one of `answered`,
`response_failed`, `answered_elsewhere`, `connection_lost`,
`queue_overflow`, `event_stream_error`, `session_stopped`. An approval
that vanishes because another client answered it is a different fact
from one that vanished because the connection died, and the UI says so.

`amux.codex_ready` carries session metadata on fresh attachment and later
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

amux's own agent tools are preapproved in the thread-scoped MCP policy and do
not become approval asks. amux registers no Codex dynamic tools. If another
integration produces an `item/tool/call` or `dynamicToolCall`, the structured
client keeps it generic and preserves its ordinary approval path rather than
attributing it to amux.

`item/tool/requestUserInput` is the one known **unanswerable**
obligation. Answering it needs free-form content the frozen input shape
cannot express, so amux renders it as visibly blocked rather than
offering a decline-only half-answer that would look like an answer. The
escape hatches are interrupt, or raw mode — where codex's own TUI
answers it properly, in band. This is a deliberate, recorded limit.

## Modes

`default_open_mode` (`chat` | `raw`) decides what `amux attach` opens;
both remain available per agent. Chat mode is the native amux screen —
composer, feed, approvals, `Ctrl+X` to interrupt. It states the session's
model, how it asks and what it may touch on the header's right, beside
the phase; the row between the feed and the composer states what the
thread costs against the window the session reported (`ctx 30.0k/272.0k`).
That number is what the context holds after the most recent turn, not
every turn's tokens added together — the app-server reports both, and
only the first can be read against a window. `<leader> c` opens the
totals behind it: input and output, with cached input, cache writes and
reasoning indented as shares of those two rather than listed beside them,
which is all the app-server reports, and the overlay says so. `<leader> o` on a pasted attachment or a sent review
opens the same fullscreen reader the Claude chats use. Raw mode is
`codex resume` on a PTY, byte-identical for every subscriber.
Because both current creation modes open interactively, `amux new codex`
requires TTY stdin and stdout and refuses before creating anything when either
is absent.

## Testing

Four tiers, in increasing cost:

1. **`codex` unit tests and executable specifications.** Unit tests cover
   framing and deterministic behavior. `codex::specs` runs one function per
   claim either against codex-cli or against a strict recording. The registry
   enforces its minimum supported version, allowed model, recording inventory,
   and orphan checks. The committed corpus was recorded with codex-cli 0.150.1
   and `gpt-5.6-luna` passed explicitly.
2. **Daemon adapter derivation.** `crates/amux/tests/derived_rows.rs` opens
   recorded `codex::Session`s, feeds them through the real amux adapter, and
   proves the committed `crates/amux/tests/fixtures/rows/codex/` rows reproduce
   byte for byte from `crates/codex/fixtures/`.
   `a2a_fixtures` separately covers the thread-scoped MCP route and carrier
   facts that belong at the daemon boundary.
3. **`crates/amux-ui/tests/spec/`.** Pure reducer folds use the derived rows;
   no clock or provider process is involved. Every registered sequence is
   swept for invariant violations after every Msg.
4. **`crates/amux/tests/codex_live.rs`.** The opt-in `codex_live` suite keeps
   process-level facts that transport replay cannot prove: live create and
   turn behavior, approvals with filesystem assertions, interrupt and reuse,
   suspend/resume across app-server restart, raw and structured coexistence,
   PTY fan-out and teardown, MCP tools, A2A injection, and cross-kind child
   completion. It prints the observed codex-cli version and model first and is
   inert when no scenario is named.

```sh
wt test
AMUX_CODEX_LIVE_MODEL=gpt-5.6-luna wt run codex-live -- all
```

The live suite is pinned to codex-cli 0.153.4. To check startup without model
turns, run `wt run codex-live -- raw_unnamed raw_named unnamed_reconnect`.
These scenarios require the vanilla TUI's local `/status` command to identify
the expected thread, including after detach/reattach and app-server restart.
Initial ANSI output alone is insufficient: Codex draws its startup screen
before a failed resume exits. Put the intended Codex installation first on
`PATH` when several versions are installed; captures record the actual version.

Two standing rules, both bought with pain elsewhere in this repo:

- **Waiters parse structure, never raw text.** No substring matching on
  serialized JSON, and no assumptions about the count or position of
  upstream startup noise — that count was observed to vary between 4 and
  9 in a single afternoon.
- **Committed fixtures are regression anchors, not a cache.** They are
  never rolled forward wholesale. Graduation is a deliberate, separate,
  reviewed change.

Every recording carries provider version, model, capture date, a content
inventory, and an append-only live-verification ledger. `codex-probe` lists the
registry and runs the specifications against the installed binary; passing
claims append the ledger, failing claims alone are re-recorded, and additive
drift is written beside the run rather than guessed at.

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
