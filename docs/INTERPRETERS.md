# Interpreters

*For developers adding or changing how a provider's output becomes chat items.*

An interpreter turns one provider's output into the items and snapshots a
chat shows. There is one per agent kind, in
[`crates/interpret`](../crates/interpret/src/lib.rs):

| Kind | Provider | Module |
|---|---|---|
| `claude_pty` | Claude Code in a terminal | [`interpret::claude_pty`](../crates/interpret/src/claude_pty/mod.rs) |
| `claude_sdk` | Headless Claude over stream-JSON | [`interpret::claude_sdk`](../crates/interpret/src/claude_sdk/mod.rs) |
| `codex` | The Codex app server over JSON-RPC | [`interpret::codex`](../crates/interpret/src/codex/mod.rs) |

The interpreters are the only place provider facts are read, and, with the
client's thin per-kind layer, the only code that decodes an item or snapshot
body. They run inside [the agent process](AGENT_PROCESS.md), which feeds
them events and carries out what they ask; what they emit is journaled,
committed by the daemon and served to clients unchanged (see
[the journal and the store](JOURNAL_AND_STORE.md)). The items themselves are
described from the client's side in [the chat vocabulary](CHAT_VOCABULARY.md).

## The interface

An interpreter is pure. It has no clock (time arrives as a tick), generates
no ids (they arrive on inputs and facts) and does no I/O. It implements
`interpret::Interpreter`:

| Member | Purpose |
|---|---|
| `type State: Checkpoint` | Everything it holds; serialized as the checkpoint |
| `const KIND` | The kind tag its items and snapshots carry |
| `initial(spec, version)` | The starting state and the first journal frame: a snapshot with phase starting and every field at its explicit unknown |
| `step(state, event)` | One event in; a `Stepped` out: the journal `Step` and the `Effect`s to perform |
| `reincarnate(state, spec, version)` | Continues a checkpointed state in the agent's next incarnation |
| `redact(target)` | Removes secrets from a body, a facts-ring entry, a checkpoint, a spec, a step or an inventory row, keeping its structure |
| `unknown_snapshot()` | The snapshot body with every field unknown |
| `pending_messages(state)` | Agent messages the provider accepted and has not consumed yet; a one-shot agent waits for this to be empty |
| `describe_item`, `describe_snapshot`, `fixture_input`, `recording` | What the golden harness needs to render bodies, build authored inputs and read recorded corpora |

An `Event` is a provider `Fact`, an `Input` from the daemon, a `Tick`, the
provider's exit, the daemon being lost, a stop request, or the process
exiting. A fact carries its `Channel`: `Transcript`, `Hook` and `Agent` for
terminal Claude, `Stream` for headless Claude, `Rpc` for Codex.

An `Effect` is something the agent process does on the interpreter's behalf,
in order: write bytes to the provider (`ProviderWrite`), reply to an input
(`Reply`), hand an agent message to the provider's own injection channel
(`Inject`), type a semantic input into terminal Claude (`Terminal`), send a
headless user message (`UserMessage`), follow a transcript file
(`FollowTranscript`), send a Codex turn whose input carries attachments
(`CodexTurnInput`), write a blob an item refers to (`WriteBlob`), or end the
process (`Exit`).

## The step rule

Every event yields exactly one step: zero or more items, the snapshot when it
changed, and a turn end when the step ended a turn. The agent appends the
step to the journal as one frame; a step with nothing in it writes nothing.

- **Items are self-contained.** Each item is emitted at its full current
  state under a stable key. Emitting the same key again is a revision; a key
  never changes its body's arm. A client never needs an earlier revision to
  understand a later one.
- **One appendable field.** An item that is still streaming is emitted open,
  and later steps extend its `text` with appends. Once it is emitted
  complete, no append follows. Only the newest item may be appended to.
- **Order is journal order.** The interpreter numbers nothing. The daemon
  gives a key its place in the chat when the key first commits, in the order
  frames were written and items sit in a step, and a revision number on
  every commit; a later revision keeps the key's place.
- **The snapshot rides along when it changed.** `Shared::finish` builds the
  snapshot from the kind-neutral state and the kind's body and includes it
  only when something other than its timestamp differs from the last one
  emitted.
- **A refusal is a reply and nothing else.** An input the interpreter
  rejects produces a `Reply` effect with the reason and no item and no
  snapshot change.

The kind-neutral part every interpreter composes is `interpret::Shared`
([`shared.rs`](../crates/interpret/src/shared.rs)): the person's queue, the
open asks, the set of accepted but unconsumed agent messages, `working_on`,
the turn counter, the prompts awaiting their reflection, the items still
open, and the last snapshot. A person's prompt goes to the provider at once
when it is ready (started, no turn running, no ask open) and nothing waits
ahead of it, and is answered accepted; otherwise it waits in the queue,
answered accepted and queued, and goes when the provider next becomes ready.
`Shared` also derives the phase: needs you whenever an ask is open; starting
until the provider accepts input; working while a turn runs; idle otherwise.

The first frame is always a snapshot with phase starting and every body field
at its unknown. [`unknown.rs`](../crates/interpret/src/unknown.rs) builds
those unknowns with exhaustive struct literals, so a field added to a
snapshot body does not compile until someone decides what its unknown is.

amux's own tools are recognised in the provider's record, never drawn as tool
calls: a `status` call sets `working_on` in the next snapshot, and a `send`
call becomes the agent message it sent.

## Checkpoints

The state is the checkpoint. Every `State` implements `Checkpoint`: it
serializes as JSON (protobuf values and byte ids inside it as hex, through
`serde_pb`) so a dump reader can see it, and `resume` returns the state with
the step a resume starts with: every open item again in full on its existing
key, and the snapshot.

The agent process writes a checkpoint at the start of each facts-ring segment
and at the start of every incarnation (see
[the facts ring](AGENT_PROCESS.md#the-facts-ring)). Rebuilding the state is
the newest checkpoint plus the ring's entries after it, fed through `step`
again, so everything a kind needs to continue must live in `State`: open
asks, the queue and the prompts awaiting reflection, streaming accumulators,
the provider's session or thread, counters behind item keys.

`reincarnate` continues that state under the next spec: the spec's prompt
joins the queue, what belonged to the previous provider process starts over,
and every counter behind an item key carries on, so a later incarnation never
reuses a key.

The golden harness holds every interpreter to this: for every fixture, it
checkpoints after every prefix of the events, resumes, runs the rest, and
requires the same items and final snapshot as the uninterrupted run, with a
resume that re-emits only keys readers already have, at their current
content.

## Redaction

Dumps leave the machine, so every body in them is redacted, and only an
interpreter can decode its bodies. `interpret::redact`
([`redact.rs`](../crates/interpret/src/redact.rs)) dispatches to the kind's
`Interpreter::redact` for each `RedactTarget`:

- Protobuf values are walked by their descriptor: every string field is
  redacted as text, or as JSON when it holds a JSON object or array; every
  `*_json` bytes field as JSON; the per-kind `body` bytes as the kind's body
  message. Ids and hashes are kept, and a field the descriptor does not name
  is dropped.
- Facts are walked as JSON. A checkpoint is decoded as the kind's state and
  written again with every protobuf value inside it walked the same way.
- The values of environment maps are blanked, as are secret-named
  environment assignments in text and tokens with well-known prefixes.
  Provider session and thread ids become, wherever they appear,
  `<SESSION-` followed by eight hex digits of a hash of the id and `>`, so one
  session stays recognisable across a dump.

Underneath is [`crates/redaction`](../crates/redaction/src/lib.rs), the
structural redactor for free text and JSON: secret-named keys, known token
shapes, machine paths, emails, the local user and host. It knows no record's
schema. The specs' capture sanitizer and reports use it directly.

`crates/interpret/tests/redaction.rs` runs a short session per kind with
secrets planted in a tool's input and output, the prompts, an answer, an
environment-bearing payload and the session id, and checks that no planted
secret survives in any item body, snapshot body, facts-ring entry or
checkpoint, that each still decodes as what it was, and that redacting again
changes nothing.

## Terminal Claude

`claude_pty` reads four kinds of fact: rows of Claude's transcript file, hook
payloads, the agent process's own facts (the launch with Claude's version and
keymap, the folder-trust dialog, and "ready"), and the terminal's exit.
Transcript rows arrive whole, so nothing here streams: every item is emitted
complete and revised by re-emission.

Terminal Claude has no protocol for its asks, so this is the one interpreter
that infers them. Every inference rule is written down in the module's own
documentation at the top of
[`claude_pty/mod.rs`](../crates/interpret/src/claude_pty/mod.rs); change the
rules there and here together.

### Rows come from the transcript

Every row of the conversation (prompts, replies, thinking, tool calls,
steers, slash commands, compactions, errors, the turn's end) comes from its
transcript row, in transcript order:

- A user row is the person's prompt (its pasted block unwrapped, since amux
  types a prompt as a paste). A prompt sent through amux gets its input id
  from the queue of prompts awaiting reflection.
- An assistant row's blocks become message, thinking and tool-call items,
  keyed by the row's uuid (`<uuid>#<index>` for the second block on).
- A `tool_use` block opens a call, running, keyed by its tool-use id. The
  `tool_result` in a later user row lands it: succeeded, failed, denied or
  cancelled, with its output, any images it read (written as blobs), and its
  end time. Claude writes the call's row as it starts and its result as it
  ends, so a running call is live in the chat and the text that introduced
  it sits above it.
- A queued-command attachment row is a prompt Claude took into the running
  turn at a tool boundary: a steer item.
- Slash commands (`<command-name>` rows and their `<local-command-stdout>`
  output), compaction boundaries and summaries, API error rows and the
  interruption row (one beginning `[Request interrupted by user`) each become
  their item.
- A user row of task-notification origin carries a background subagent's
  result onto the Agent call that launched it; that call stays running,
  across the end of the turn that launched it, until the notification
  arrives.
- A prompt's row precedes the rows of the turn it began, so nothing waits for
  it.

The rest of the items a chat shows for terminal Claude do not come from hooks
either: the agent-message item is written when the interpreter accepts the
message (Claude's reflection of it only marks it consumed), the turn item
comes from the transcript's own turn-duration row, the folder-trust question
from the agent's trust-dialog fact, and the exit and daemon-lost boundaries
from the agent process.

### What hooks fill in

Hooks carry only what the transcript never does:

| Hook | What it does |
|---|---|
| `SessionStart` | Names the session and the transcript path, and the interpreter asks the agent to follow that file. It marks the session boundary (started, resumed, cleared or compacted), and it means input is live. |
| `PermissionRequest` | Opens the permission card: an ask carrying the hook's tool, input and scope suggestions (or a question or plan ask for `AskUserQuestion` and `ExitPlanMode`). The hook names no tool-use id, and Claude writes a gated call's row only once the call is decided, so the card points at no row until a row with the same tool and input lands, and then at that row. |
| `Stop` | Records how many background tasks are still running, clears the running marks, and closes every ask still open with its outcome unknown, drawn dismissed until the call's result says how it went. It does **not** end the turn. |
| `Notification` | For a tool server's form or link that Claude shows in its own terminal (notification types `elicitation_dialog` and `elicitation_url_dialog`), opens an unanswerable ask with an item of its own, pointing at the call that was running. No hook can answer it: it closes cancelled on an interrupt through amux, and dismissed on that call's result, a row from a later assistant message, or any fact that closes every ask. Other notification types draw nothing. |
| `PreToolUse`, `PostToolUse`, `PostToolUseFailure` | Mark a call running, and then not running, for the snapshot's running calls (the activity line while the call's row is on the way). They never make a row. |
| `UserPromptSubmit` | Input is live. |
| `SessionEnd` | Closes every open ask with its outcome unknown. |

The turn ends on the transcript's `system` row with subtype `turn_duration`,
which closes any ask still open and writes the turn item with the duration
Claude measured and the step's turn end. An interruption row also ends the
turn, as interrupted. A slash command that runs no model turn ends when its
output row lands, without a turn end, since no turn happened. The provider
exiting cuts a turn short with no turn end at all.

Besides the session boundary `SessionStart` marks, the unanswerable dialog is
the only item a hook makes. When the call it was raised under has only been
announced by `PreToolUse` and its row has not landed, the dialog's item
waits for that row, so it sits below the call.

A subagent's calls reach the hooks socket carrying its agent id and never
reach the followed transcript. They are counted as steps on the Agent row
that started the subagent, never drawn as rows of their own. An ask a
subagent raises points at that Agent row and closes on the subagent's own
`PostToolUse` for the call.

### Asks and answers

An ask closes on an answer sent through amux (outcome known), on the call's
tool result (answered in the terminal: a refusal is a deny, anything else an
allow), or on a later fact that proves the call is over without saying how: a
prompt, an interruption, the turn's end, a session change, the provider
exiting, or a row from a later assistant message. Nothing else closes one; a
tick never does.

An answer through amux is checked against the open ask's shape, then typed
into the terminal's menu as a semantic `TerminalInput`: allow once, allow
with the scope at an index of the offered suggestions, deny with a note, a
plan choice, one answer per question, or trusting the folder. An answer the
menu cannot express is refused before anything is typed. A scope is offered
only for the permission menus the resolved keymap can type (its verified menu
shapes); on any other menu the card offers allow once and deny.

Terminal Claude offers no list of models, efforts or commands a program could
read, so its snapshot offers none and it takes no model or effort input: a
person types `/model <name>` or `/effort <level>` as a prompt and the
command's own rows reflect it. The permission mode changes only by cycling,
since the cycle's order depends on how Claude was launched. Its inputs are
prompt, withdraw, send now, interrupt, clear, a named key, and answer.

### Keys as programs

The interpreter never produces bytes for the terminal. It emits a semantic
`TerminalInput`, and the agent process turns it into keystrokes with a
keymap: versioned data that parameterizes a fixed set of programs, in
[`crates/claude/src/pty/keymap.rs`](../crates/claude/src/pty/keymap.rs).

The keymaps are TOML files under
[`crates/claude/keymaps/`](../crates/claude/keymaps/claude-2.1.toml), baked
into the binary (`BAKED_KEYMAPS`). The shipped one is `claude-2.1`, with
`applies_to = ">=2.1.228, <2.2.0"`. A keymap holds:

- named key bytes (`[keys]`) and bounded delays (`[delays]`);
- menu entry positions (`[menus.permission.entries]`, `[menus.plan.entries]`);
- verified menu shapes (`[verified_shapes.permission_menu]`): the permission
  suggestion counts whose menu is verified to have one scoped entry per
  suggestion, and those verified to put every suggestion into one entry;
- the steps of six fixed programs, `prompt`, `interrupt`, `mode_cycle`,
  `permission_menu`, `plan_menu` and `question_form`, each marked `stable` or
  `menu`, and the fixed table from intent to program;
- `[provenance]`: the Claude version, model, dates and specs it was
  transcribed from;
- `verified`: the evidence list, one entry per Claude version, probe run id
  and spec that passed live.

A program is built from a closed step vocabulary (`key`, `paste`, `type`,
`digit`, `delay`, `repeat`, `for_each`, `if`, `move_to`, `call`) whose counts
and branches come only from the typed ask and answer. There is no general
loop, no shell, no screen query. A data file can repair keys, timing, menus
and already-modelled shapes; adding an intent, a condition or a step means
changing the amux binary.

At every start the agent process asks Claude for its version and resolves a
keymap for it, so an updated Claude gets the right map without another spec:

1. a keymap with a verified entry for exactly that version (basis
   `Verified`);
2. among keymaps that have no verified entry yet, the newest whose range
   contains the version (`InRange`);
3. otherwise, extrapolation from the nearest verified version below, or above
   when none is below (`Extrapolated`);
4. otherwise the newest keymap (`Unknown`).

`stable` programs run on any basis. `menu` programs extrapolate only within
the same minor version of Claude, and refuse outside it or on an unknown
basis. The resolved keymap's name is recorded on the boundary item beside
Claude's version.

Only the Claude probe appends to `verified`, and only after the spec passes
live (see [re-recording](#re-recording)). The test
[`crates/claude-specs/tests/keymap_provenance.rs`](../crates/claude-specs/tests/keymap_provenance.rs)
requires every verified entry in a baked keymap to name a registered PTY spec
whose recording's manifest carries the same version and run id, as the
recording itself or as a verification. The resolver also refuses a keymap
from outside the binary that claims verified versions; the agent process
resolves against the baked keymaps only.

## Headless Claude

`claude_sdk` reads the stream-JSON lines headless Claude writes on stdout,
events and control requests alike. Asks arrive as named control requests, so
nothing is inferred.

- **Streaming.** Text and thinking stream: `content_block_start` opens an
  item keyed `<message id>:<block index>`, each delta is an append, and
  `content_block_stop` completes it. Tool calls are keyed by their tool-use
  id and land on their `tool_result`.
- **Correlation by id.** Every user message the interpreter hands Claude
  carries a UUID derived from its input or envelope id (`client_uuid`).
  Claude echoes it on the message's replay and on its `command_lifecycle`
  frames, so a reflection is matched to what was sent by id. Messages
  written while a turn runs join that turn, so arrival order would mismatch
  them. The prompt's item is written when it is submitted.
- **Asks.** A `can_use_tool` control request opens a permission ask (with its
  scope suggestions, reason and description), a question ask for
  `AskUserQuestion`, or a plan ask for `ExitPlanMode`, pointing at the call
  it names. An `elicitation` request opens a tool server's form or link, with
  an item of its own. Answers go back as control responses. A control
  request of any other subtype is answered with an error.
- **Offers.** The answer to the agent process's `initialize` request lists
  the models, each with its effort levels, and the slash commands; every
  later snapshot carries them. Nothing comes from a client-side catalogue.
- **Inputs.** Prompt, withdraw, send now (a message at default priority,
  which joins the running turn at its next tool boundary), interrupt (a
  control request), clear (sent as `/clear`), permission mode and model
  (control requests), and answer. Effort is refused: headless Claude takes
  it at launch only.
- **Turns.** A turn starts with a submission or a `message_start`, and ends on
  the `result` line: completed, interrupted, or failed (with an error item
  for a non-success subtype), with its duration and cost on the turn item.
  The result dismisses any ask still open.
- **Boundaries.** Claude repeats its `init` at every turn; only a process
  start or a changed session id is a boundary, the latter a fork.
- **Agent messages** go as a user message on stdin, an immediate hand-off to
  Claude's own queue. The agent-message item is written at acceptance, and
  the message is consumed when Claude reports taking that UUID.

## Codex

`codex` reads the JSON-RPC messages the app server writes: notifications
about the thread, its turns and their items, requests that ask the person
something, and responses to the requests the interpreter sent.

- **Requests.** The agent process performs the handshake; the interpreter
  reads the thread from its answer or from `thread/started`, and from then on
  writes every request with ids of its own (`amux-<n>`), so each
  acknowledgement is matched to what it acknowledges.
- **Offers.** Once the thread is known, the interpreter asks `model/list`
  (following `nextCursor` to the last page, leaving hidden models out) and
  `skills/list`, once per server; the snapshot carries the models with their
  reasoning efforts, and the skills as commands. A refused or missing answer
  leaves its list empty.
- **Asks** are the server's requests:

  | Request | Ask |
  |---|---|
  | `item/commandExecution/requestApproval` | Command approval, with the decisions Codex offers |
  | `item/fileChange/requestApproval` | File-change approval |
  | `item/permissions/requestApproval` | Access grant |
  | `item/tool/requestUserInput` | Question |
  | `mcpServer/elicitation/request` | A tool server's form or link, or, when Codex marks it as a tool-call approval, approval of the running tool server call. An approval for amux's own tool server is answered approved at once (for the session when Codex offers it) and opens no ask: amux's own tools never ask, on any kind |

  `item/tool/call` is answered at once that this client hosts no dynamic
  tools. Any other request is answered with a method-not-found error and
  drawn as an unrecognized item.
- **Turns.** `turn/started` begins one and records its id; `turn/completed`
  ends it with its status (completed, interrupted or failed, with an error
  item when the turn carries one) and dismisses every ask still open. A
  prompt starts a turn with `turn/start`, and its item is written then, keyed
  by its input id; Codex's own reflection of it is left out. Send now steers
  the running turn with `turn/steer`; a steer Codex refuses, or one still
  unanswered when the turn ends, goes back to waiting in the queue. An
  interrupt is `turn/interrupt`, held until `turn/started` names the turn
  when it has not yet; a held interrupt is dropped when that `turn/start`
  fails or Codex exits, so it never lands on a later turn. A `/compact` prompt is `thread/compact/start`.
- **Model, effort, approval.** These inputs are accepted at once and ride on
  the next `turn/start` as overrides.
- **Streaming.** Agent-message, plan and reasoning deltas and command output
  are appends to their open items.
- **Agent messages** go to Codex with `thread/inject_items`. Codex reports
  nothing about an injected item, so the interpreter writes the agent-message
  item itself, at acceptance. A probe of codex-cli 0.157.0 settled when the
  message counts as consumed (`CODEX_INJECT_CONSUMPTION`): a running turn
  answers items injected into it before it ends, so a message injected
  mid-turn is consumed at the inject's acknowledgement; an inject into an idle
  thread is answered by an empty turn the interpreter starts for it, and is
  consumed at that turn's acknowledgement. A message that arrives before the
  thread is running is held until it is; if a prompt is waiting for the
  thread as well (a spawn's task), the held messages are injected and ride
  that prompt's turn instead of an empty one, so the model reads its task
  with them. Both arms have goldens of their own.

## Provider transport crates

| Crate | What it holds | Who uses it |
|---|---|---|
| [`claude`](../crates/claude/src/lib.rs) | Claude Code's surfaces as data: hook payloads and their forwarding over the hook socket (`hooks`), launch settings and the environment scrub (`launch`), the messaging socket client (`messaging`), keymaps and their program interpreter (`pty`, behind the `pty` feature), stream-JSON messages and the control protocol (`sdk`), transcript tailing (`transcript`), session files (`history`) and version probing (`version`); also a standalone `claude-hook` forwarder binary | The agent process (hooks, launch, messaging, keymaps, version, session files), `amux hooks claude`, and `claude-specs` |
| [`codex`](../crates/codex/src/lib.rs) | A typed client for the Codex app server: spawn, initialize, threads, turns, approvals, notifications | `codex-specs`, which drives Codex to record scenarios. The agent process and the interpreter speak the app server's JSON-RPC directly |
| [`pty-host`](../crates/pty-host/src/lib.rs) | Provider-neutral pseudo-terminal hosting: spawn in a process group, one output stream, input, resize, process-group signals, graceful termination | The agent process (terminal Claude and Codex views) and `claude-specs` |

## Recordings and re-recording

The interpreters are tested against real provider traffic, recorded by the
spec crates and sanitized so it replays offline:

| Crate | Recordings |
|---|---|
| [`claude-specs`](../crates/claude-specs/src/lib.rs) | `fixtures/sdk/<spec>/` for headless Claude, `fixtures/pty/<spec>/` for terminal Claude, and two transcript captures in `fixtures/claude-pty/` that cannot be regenerated in the tree ([their README](../crates/claude-specs/fixtures/claude-pty/README.md)) |
| [`codex-specs`](../crates/codex-specs/src/lib.rs) | `fixtures/runtime/<spec>/`, with the capture conditions in [`PROVENANCE.md`](../crates/codex-specs/fixtures/runtime/PROVENANCE.md) |

A recording is a directory holding `manifest.json` (the provider, version and
model it was recorded with, its content hashes, the fields it observed, and
its verifications), `io.jsonl` (every line in both directions, with its time)
and `spawn.jsonl`. Captures are sanitized with the shared redaction rules:
machine paths, credentials and personal identifiers become placeholders, while
provider ids, timestamps, payloads and ordering are kept.

An interpreter fixture names its recording by a relative path and a format
(`claude_pty_io`, `transcript_rows`, `claude_sdk_io` or `codex_io`), so the
same bytes feed the recorded replays in the spec crates, the provider fakes'
conformance tests and the interpreter goldens.

### Re-recording

The probes run the real provider under your own login, so they are never
part of an ordinary test run.

```sh
# Claude: list the specs and what each was recorded and verified with.
cargo run -p claude-specs --bin claude-probe -- list [--sdk|--pty]
# Record one or more specs again, terminal or headless.
cargo run -p claude-specs --bin claude-probe -- record --pty <spec>...
cargo run -p claude-specs --bin claude-probe -- record --sdk <spec>...
# Run every spec live and record the evidence.
cargo run -p claude-specs --bin claude-probe -- probe [--sdk] [--pty] [--out <dir>]

# Codex, the same three commands.
cargo run -p codex-specs --bin codex-probe -- list
cargo run -p codex-specs --bin codex-probe -- record <spec>...
cargo run -p codex-specs --bin codex-probe -- probe [--out <dir>]
```

`claude-probe` runs the `claude` on your `PATH`, or the one named by
`CLAUDE_REAL_PATH`; `codex-probe` runs `codex` and records with the model its
specs pin (`CAPTURE_MODEL` in `crates/codex-specs/src/specs.rs`).

`probe` runs every registered spec live and updates only the evidence its
outcome permits. A spec that passes gets a verification (version, time, run
id) appended to its manifest; for a terminal Claude spec, the same version,
run id and spec name are appended to the baked keymap's `verified` list. A
spec that fails live is recorded again, and reported failed if recording
fails too. Each run writes `probe.json` (every spec's
outcome) and `drift.json` (how the live shapes differ from the recorded
ones) under `target/claude-probe/<run id>/` or `target/codex-probe/<run id>/`,
or the directory `--out` names. `record --pty` also appends the keymap
evidence, once the fresh recording has passed strict replay.

After re-recording, run the replays and review the diffs:

```sh
just test-crate claude-specs
just test-crate codex-specs -- --test spec_replay
INTERPRET_UPDATE_GOLDENS=1 just test-crate interpret   # then review the golden diff
```

## Goldens

A fixture under `crates/interpret/fixtures/<kind>/` is a JSON file: what it
shows (`about`), a spec, an optional recording, authored events (facts,
inputs, ticks, checkpoints, with expected replies, phases, queues and asks),
and what may be left over at the end. Recorded events run first; authored
events that must come before them go in a `prelude`. The rendered emission is
the golden beside it, `<name>.golden`, written only when
`INTERPRET_UPDATE_GOLDENS=1` is set. The format is documented at the top of
[`golden.rs`](../crates/interpret/src/golden.rs).

Every run also checks the invariants every journal obeys
(`interpret::check_invariants`): the first frame is a starting snapshot with
the unknown body and no items; a key appears once per step and never changes
arm; no item reopens after its final revision; appends go only to the newest
open item; every open ask points at an item that exists; the queue drains
and every stream ends complete, unless the fixture says otherwise. And it
checks the checkpoint property described [above](#checkpoints).

```sh
just test-crate interpret -- --lib --test claude_pty --test claude_sdk --test codex --test coverage
```

## The coverage test

[`crates/interpret/tests/coverage.rs`](../crates/interpret/tests/coverage.rs)
ties the interpreters to the chat vocabulary's catalogue of what a chat can
show. The catalogue is encoded in
[`vocabulary.toml`](../crates/interpret/tests/vocabulary.toml) beside it: one
entry per catalogue row, with each kind's availability mark (`full`,
`partial`, `none` or `na`), the carriers that hold the row, and the pattern a
golden line must match. The test enforces that:

- the file holds all 31 catalogue rows, in order;
- a row a kind can show (`full` or `partial`) names at least one carrier, as
  `item:<arm>[.<field>…]` in the kind's item body, `snapshot:<field>` in its
  snapshot body or `ask:<arm>` in its ask, and each carrier is a real field,
  checked against the wire schema's descriptors;
- such a row has a golden pattern, and some line of some golden under
  `fixtures/<kind>/` contains every string in it and none of the strings
  prefixed with `!`;
- a row a kind cannot show (`none` or `na`) names no carriers and no goldens.

A change to what a kind can show is therefore a change to `vocabulary.toml`,
to the kind's bodies and to a golden, together. Run it on its own with
`just test-crate interpret -- --test coverage`.

## Adding or changing an interpreter

- Keep every rule about a provider's facts in its module, and state the
  inference rules in the module's documentation.
- Put everything a restart must continue from in `State`; the checkpoint
  property will catch what is missing.
- Give every item a key derived from the provider's own ids, stable across
  re-emission and across incarnations.
- When a snapshot body gains a field, give it an unknown in `unknown.rs`.
- Record the behaviour you depend on with the spec crate's probe, point a
  fixture at the recording, and author fixtures only for what no recording
  can show.
- Update `vocabulary.toml` and a golden when the change affects what a chat
  can show, and the client's per-kind layer when a body changes (see
  [the client](CLIENT.md)).
