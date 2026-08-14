# Codex in amux

Status: implemented. This document owns the OpenAI Codex integration —
which process owns what, the two planes a codex agent exposes, the row
vocabulary on the structured plane, and the client-side layer that folds
it. Companions: `docs/PROTOCOL.md` owns the wire, `docs/ARCHITECTURE.md`
owns the system, `docs/UI.md` owns the client layer's doctrine, and
`docs/CHAT.md` owns the Claude chat surface this one deliberately does
not imitate.

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

The raw PTY is spawned lazily on first `terminal_v1` subscription and
**retained after the last detach**, so reattach is instant. That is a
deliberate, recorded trade: a codex process pair stays parked per
ever-attached agent. See *Known gaps*.

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
`{"type": "<upstream method>", ...params}`, unmodified — plus exactly six
rows amux synthesizes. The synthesized set is closed; adding to it is a
protocol change.

| row | means |
|---|---|
| `amux.codex_ready` | the session is connected and its stream is authoritative |
| `amux.codex_gap{reason}` | continuity was lost; what follows is not contiguous |
| `amux.codex_reconnect_error{error}` | a recovery attempt failed |
| `amux.codex_approval_required{request_id, availableDecisions}` | an approval is outstanding — `availableDecisions` is wire-verbatim |
| `amux.codex_approval_resolved{request_id, reason}` | it is no longer outstanding, and why |
| `amux.input_result{input_id, ok\|error{message}}` | the fate of one submitted input |

`amux.codex_approval_resolved.reason` is one of `answered`,
`response_failed`, `answered_elsewhere`, `connection_lost`,
`queue_overflow`, `event_stream_error`, `session_stopped`. An approval
that vanishes because another client answered it is a different fact
from one that vanished because the connection died, and the UI says so.

**Unrecognized rows render honestly as unrecognized.** They are never
dropped and never guessed at. Upstream drift is data: a new
`mcpServer/startupStatus/updated` row appears as an unrecognized row, is
captured, and is diffed — it does not break a fold.

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

Locked by `codex_agreement` (a 16-state chapter) and by a checked
projection invariant that panics in debug and dumps in release:

- `phase == Unknown` implies `attention == Unknown`.
- read-only outranks a pending ask in attention (→ `Unknown`).
- `thread/closed` refuses sends but stays recoverable.
- while the stream is `Opening` or `Replaying`, attention is `Unknown` —
  rows seen so far are a prefix, not a conclusion.

The invariant is checked after every Msg of every registered spec
sequence (`wire_free.rs`), so it is a CI control, not only a runtime one.

### Writes

Four actions, one derivation — `write_permission(model, agent, action)`:

| action | permitted when |
|---|---|
| prompt | the session is live and no turn is active |
| steer | a turn is active |
| interrupt | there is an active turn, **including while an input is in flight** |
| answer | an approval is pending |

Session-level refusals (unavailable, exited, closed, replaying,
read-only, unknown) are subtracted *before* an action rule runs, in the
type: `session_state()` returns `Result<LiveState, &'static str>`, so a
per-action rule cannot see a state it has no business ruling on. Views
ask this API; they do not re-derive preconditions from raw layer fields.

## Prompts, steers and echoes

Prompts are **protocol-sourced**: a normal prompt arrives back as a
`userMessage` item and is rendered from that, with no local echo. Steers
have no such item, so a retained `input_id` is promoted to a
`PromptSource::SteerEcho` entry only on a correlated `amux.input_result`
success. Nothing appears in the feed that the server did not confirm.

## Approvals, and unanswerable obligations

Command and patch approvals are answerable: the decision set is
wire-verbatim and the answer goes back over the same connection.
Dynamic tool-call asks are also answerable — the backend maps
`accept`/`acceptForSession` to success and `decline`/`cancel` to failure
— but the wire carries no decision list for them, so the *layer* supplies
`accept`/`decline` while the wire field keeps its verbatim null.

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

## Testing

Three tiers, in increasing cost:

1. **`crates/amux-ui/tests/spec/`** — pure reducer folds, no clock, no
   IO. Every registered sequence is swept for invariant violations after
   every Msg.
2. **`crates/amux/tests/codex_capture_waiters.rs`** — the structural
   waiters, driven offline from redacted rows captured off real failing
   runs. No codex process, no credentials, no network.
3. **`crates/amux/tests/codex_capture.rs`** — the **C suite**: opt-in,
   real codex, eight scenarios (C.1–C.8) covering create+pong, approval
   allow and deny *with filesystem assertions*, interrupt and reuse,
   suspend/resume across a server restart, real process-group daemon
   recovery, raw+structured coexistence, and two-subscriber byte fanout.

The C suite is inert in `cargo test --workspace` — with no scenario named
it prints a skip note and exits 0 before creating a scratch directory,
server, process, or request. `SCENARIOS` binds id, requirement, timeout
and runner in one row so they cannot drift apart.

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

## Known gaps

- **Post-resume feed.** After suspend→resume the structured feed is
  empty with no truncation or reset marker, though the thread retains
  full context (a resumed agent recalls tokens from before the restart).
  Continuity lives in the codex thread, not in replayed amux rows. C.5
  asserts it by *content*, not by row count.
- **Startup noise.** `mcpServer/startupStatus/updated` rows are the
  entire first screen of a new codex chat, rendered as unrecognized. A
  suppression-or-typing policy is undecided.
- **Approval labels.** One approval choice renders its raw wire JSON as
  its label.
- **Non-TTY create.** `amux new codex` without a TTY creates the agent,
  then exits with a raw errno.
- **Retained raw PTYs.** Kept forever after last detach, unmeasured.
- **`readonly` is enforced by views, not by the gates.** The write
  permissions do not consult it, so the gate is not yet the source of
  truth it claims to be. No user-visible effect today, because the views
  do block.

`notes/codex-impl/INVESTIGATIONS.md` carries the full standing list with
evidence.
