# Agent-to-agent messaging and families

**Status**: implemented (2026-09-04). This document owns amux's
agent-to-agent message envelope, provider carriers, model-facing tools, and
parent/child lifecycle. [`PROTOCOL.md`](./PROTOCOL.md) owns the links and
tunnels that carry remote calls; [`ARCHITECTURE.md`](./ARCHITECTURE.md) owns
the daemon service boundaries; [`UI.md`](./UI.md) and [`CHAT.md`](./CHAT.md)
own client derivation and presentation. Provider-specific row details remain
in [`CODEX.md`](./CODEX.md) and
[`CLAUDE_TRANSCRIPT.md`](./CLAUDE_TRANSCRIPT.md). The shared attachment
element, artifact lifetime, and attachment delivery are specified in
[`ATTACHMENTS.md`](./ATTACHMENTS.md).

## The model

amux adds one relationship and one communication primitive:

- An agent may have a `parent`, identified by agent id and owning host id.
  The child remains an ordinary fleet agent: it can be attached, renamed,
  suspended, resumed, messaged, and stopped independently.
- A daemon-authored `Envelope` carries `{id, context?, from, to, kind, text}`.
  `kind` is `message`, `completed`, or `exited`. The daemon resolves `from`
  to a live agent's id, name, host, and kind; a request without an agent
  identity is authored by the human. A caller cannot choose arbitrary
  provenance.

The envelope is fire-and-forget. A reply is another envelope, optionally
carrying the original envelope id as `context`. There is no streaming reply
channel and no blocking wait operation.

Every agent may also publish `working_on { text, updated_at }`. It is advisory
fleet state rather than a lock: the `status` tool sets or clears it, spawning
a child initializes it from the first line of the prompt (at most 80
characters), and a completed turn clears it. Parent and work status survive
daemon suspend/resume.

## The six agent tools

Managed Claude and Codex sessions receive the same definitions from an
amux-owned stdio MCP server. Its hidden CLI spelling is `amux mcp agent`.
The daemon launches that server with the owning agent and host identities in
`AMUX_AGENT_ID` and `AMUX_HOST_ID`; neither identity is accepted as a tool
argument.

Managed launches freeze the daemon's exact executable, effective file-backed
config when one exists, and socket rather than relying on a pre-existing
provider process's environment. Codex installs that route in the thread-local
configuration for start, cold resume, and reconnect, marks the server required,
allowlists exactly these six tools, and preapproves them. Claude appends the
same route through `--mcp-config` and preapproves `mcp__amux__*` through
`--allowedTools`. Repeated values for both flags accumulate, so user-supplied
MCP servers and allow rules remain active; amux deliberately does not use
`--strict-mcp-config`, which would suppress the user's other configured
servers. Neither provider path edits persistent user configuration.

Claude's five lifecycle hooks arrive through an additive `--settings` object
on the managed launch. User settings supplied as JSON or a file are deep-merged
into that object, with hook arrays concatenated. Every hook command uses the
same absolute amux executable as the MCP route. amux installs no Claude plugin:
sessions started outside amux therefore receive neither the amux tools nor
amux-managed hooks.

| tool | input | result | meaning |
|---|---|---|---|
| `agents` | `{}` | fleet rows | List names, kinds, hosts, liveness, work, parent, and the caller marker. |
| `send` | `{to, text, context?}` | `{id}` | Send text to an agent by name. Replies to an `amux:` address must use this tool. |
| `spawn` | `{kind, prompt, name?, cwd?}` | `{name, id, initial_prompt_delivery?}` | Create a Claude or Codex child and deliver its initial prompt. |
| `stop` | `{name}` | `{}` | Stop a direct child of the caller. |
| `status` | `{working_on: string|null}` | `{}` | Set or clear the caller's current work. |
| `attach` | `{path, name?}` | canonical attachment element | Store a file from this agent's host and return the exact text to include in a reply. |

The descriptions steer models to amux for cross-kind work while leaving
Claude's native same-kind agent tools and Codex's native multi-agent features
available.

`attach` reads `path` in the managed MCP process, recognizes PNG, JPEG, GIF,
and WebP as images, and stores other inputs as generic files. The authenticated
caller, not a tool argument, selects the owner. The daemon pins the artifact
and emits its stream ref before the model's reply arrives, so the returned
element renders for every viewer. This is attachment production by one agent;
the `send` envelope itself still carries text only. Its canonical syntax and
that deferred A2A extension are owned by `ATTACHMENTS.md`.

## Spawning and lifecycle

`spawn` creates the child record with the authenticated caller as its parent,
starts the backend, waits up to 30 seconds for its delivery target to become
live, then sends the initial prompt through the same message carrier used
later. For managed Claude sessions, the `SessionStart` hook marks that
transition, as does transcript ingest reaching its `amux.transcript_ready`
marker; Codex uses its shared thread-attachment state. The readiness wait
is scoped to this initial delivery, so an ordinary send to an unavailable
session fails immediately. The prompt arrives with parent provenance instead of
masquerading as human input. If readiness or delivery fails, spawn removes the
new child and reports failure rather than leaving an orphan. With no explicit
`cwd`, the child inherits the parent's working directory. Claude children
inherit only the parent's permission-mode arguments; Codex children inherit
approval and sandbox policy.

When a child turn ends, its last assistant message is sent to the parent as a
`completed` envelope. Claude PTY supplies the text through the `Stop` hook;
Claude SDK uses the successful result row's `result` text; Codex uses the
last `agentMessage` observed before `turn/completed`. If the child
session ends, the parent receives an `exited` envelope with an empty
body. Completion leaves the child alive and idle so a later message can start
another turn.

Model-facing stopping is lineage-scoped in `ClientService`: the MCP server
attaches the authenticated calling agent id, and the daemon accepts the delete
only when the target is that caller's direct child. Human
deletion remains an unscoped administrative action. Deleting
a parent cascades through all descendants, including agents on paired hosts.
Reachable descendants are removed deepest-first. Descendants on an unreachable
owning host are reported as still running rather than silently forgotten. The
command-line `rm`
refuses a family with a non-empty work claim unless `--force` is supplied.

## Delivery carriers

The owning daemon hands the envelope to the recipient's `AgentBackend`; each
provider decides how its native session accepts input.

### Claude PTY

Every spawned Claude session receives an amux-owned `--name`. Claude Code
versions at or above 2.1.224 also receive a per-agent
`--messaging-socket-path`. The daemon discovers the installed version once with
an asynchronous `claude --version` probe bounded to ten seconds. A failed or
timed-out probe leaves socket delivery disabled. Later process starts read the
cached result without another probe; any transcript row from a session the
daemon started that reports a different `version` refreshes the cache for
subsequent sessions — including filling in a version the probe could not
determine, which re-enables socket delivery from the next session on.
Externally started sessions never feed the cache: the binary a user ran by
hand says nothing about the one the daemon launches. Managed hook calls
forward only `CLAUDE_CODE_MESSAGING_SOCKET` and
`CLAUDE_CODE_MESSAGING_TOKEN`, allowing the session to refresh the credentials
without exposing the rest of its environment. amux does not register hooks for
externally started sessions. If a user independently registers the hook
command, any external session it discovers remains transcript-only and
readonly: it is never a message delivery target, even when its hook exposes
live messaging credentials.

For an agent sender with a ready socket, amux posts Claude's native
`<cross-session-message>` wrapper. The body begins with an amux header carrying
the envelope id, kind, sender kind, and optional context. The send then waits,
up to two seconds, for a transcript row carrying that envelope id: the
`queue-operation` `enqueue` row Claude writes as it takes the message off the
socket, or the later peer user row or `queued_command` attachment. The enqueue
row appears within milliseconds whether the recipient is idle or mid-turn, so a
healthy delivery confirms immediately and the sender is told the message
arrived rather than that it was merely posted. The later rows are written only
when the queued message enters a turn, which for a busy recipient is whenever
its current turn ends; accepting the enqueue row is what keeps a long turn from
looking like a lost message.

Exhausting the window means the socket took the bytes and the session never
queued them — a wedged recipient rather than a busy one. The message is then
delivered by the fallback and the session stops using its socket until its next
process start. A wait that ends because the transcript stream ended instead
falls back for that message only: a session on its way out says nothing about
whether its socket works. Against a wedged recipient the two waits compose, so
a spawn's first prompt can cost the readiness timeout and then the confirmation
window before it pastes.

The fallback is a bracketed PTY paste of the generic `<amux>` tag followed by
Enter. It is also used for human messages, older Claude versions, and sessions
without socket credentials. Before the paste is built, tabs become spaces and
carriage-return line endings become newlines; other control characters except
newline are dropped. The remaining message is still delivered. Claude queues
input received during a running turn.

### Claude SDK

The SDK driver is a separate carrier over the same daemon-authored envelope.
Before the provider session emits ready its delivery target is pending; after
exit it is unavailable. While live, the adapter formats the generic `<amux>`
envelope as a stream-JSON user message and sends it through
`claude::sdk::Control::prompt`. This uses the provider crate's ordinary control
handle rather than a driver-specific path in fleet routing.

Only provider acceptance completes delivery. After `Control::prompt` succeeds,
the recipient log writes `amux.claude_sdk.message` with the complete envelope
and `delivery: "stream"`. That row is the recipient-owned durable record. A
sender response, attempted prompt, or pre-ready queue is never treated as
delivery evidence.

The offline carrier fixtures in `crates/amux/tests/fixtures/a2a/sdk_*` use
synthetic stream-JSON subprocesses through the real provider crate, SDK
adapter, local registry, and lifecycle loop. They freeze the recipient rows
for a parent message, a child's successful result, and its process exit;
only the host-generated completion and exit envelope IDs are normalized.
The subprocess echoes each received prompt so the tests verify the wire text
as well as the durable row. They do not require a provider login.

Run `timeout 900 wt test -- a2a_fixture` to replay them. To deliberately
regenerate the SDK rows, run `UPDATE_A2A_FIXTURES=1 timeout 900 wt test --
a2a_fixture_claude_sdk`, then replay without the update flag.

### Codex

amux first calls `thread/inject_items` with a user-role message. If the thread
is idle, it follows with an empty-input `turn/start`; during an active turn it
only injects, allowing Codex to queue the message into that turn. If injection
fails, amux starts a visible turn carrying the tagged text.

Because injected items do not appear in Codex's native transcript, every
accepted delivery also writes an `amux.codex_message` structured row with the
envelope fields and the carrier used: `inject_queued`, `inject_started`, or
`turn_started`.

## Transcript-safe provenance

The generic Claude PTY and Codex carriers inject this shape:

```text
<amux id="<uuid>" kind="message|completed|exited" from="<name>/<host>|human" from-id="<agent uuid>" from-kind="<kind>" context="<uuid>">
<body>
</amux>
```

Optional attributes are omitted. Attribute values and bodies escape `&`,
`<`, `>`, and `"`; an arbitrary body therefore cannot close the wrapper or
forge another envelope. The shared parser accepts both this form and Claude's
native cross-session wrapper. Clients derive provenance from recipient-owned
rows: Claude folds the delivered user row, while Codex folds
`amux.codex_message`. No sender-side synthetic chat record is treated as
delivery evidence.

## Routing, trust, and failure

A local `ClientService.SendMessage` resolves the recipient and sender. Local
delivery calls the backend directly; remote delivery forwards the complete
daemon-authored envelope to `AgentService.SendMessage` on the recipient's
owning host through the existing paired tunnel. `SetAgentStatus`, child
creation, and cascade deletion use the same local-or-remote service split.
Agent messaging adds no link frames, routing rules, protocol version, or new
authorization boundary.

Pairing remains the trust boundary: any agent inside the paired trust domain
may list, message, or spawn agents, while the parent edge limits only `stop`
and lifecycle relationships. If a recipient host is unreachable, a human send
returns `Unavailable`; an agent send is logged and dropped so model-facing
messaging remains fire-and-forget. Delivery logs include the envelope id and
carrier.

The MCP server fails before exposing tools when its file-backed config and
explicit socket disagree, the daemon is unavailable, or an injected identity
is partial, stale, or belongs to another host. After that preflight it opens a
fresh daemon connection for every call, so a server process can recover on the
next call after a daemon restart. An interrupted call returns an error and is
never retried in place: a lost response cannot prove that a mutating operation
did not already take effect.

## Clients and command line

The client model derives a family from parent edges. The fleet ranks a family
as one unit using its highest effective attention and hides descendants until
expanded. A parent row carries the descendant count; expanded children are
indented and retain their own attention and work status. A child's human-needed
state is composed into the parent's chat at observation time, so the banner
and inline ask disappear as soon as the child's own state clears — nothing is
written into the parent's stream.

Inbound message, completion, and exit rows render in the recipient's native
chat. Outbound `send` calls remain ordinary tool rows. The TUI can cycle
through a family, expand completion bodies, and host a child's own ask panel
inside the parent's chat while dispatching the answer to the child's layer.

`amux list` shows one row per family with a descendant count and current-work
column; `amux list --all` expands and indents every generation. `amux rm`
prints removed and unreachable descendants, marks active work, and requires
`--force` when any descendant reports work.

## Verification boundaries

The whole-daemon specification covers provenance, local and remote delivery,
spawn, completion, work status, suspend/resume, and cascade deletion with
credential-free test agents. Reducer specifications prove family and message
folds after every input, and TUI goldens cover collapsed and expanded fleets,
message rows, child asks, and deletion confirmation. Provider behavior is
pinned by versioned Claude and Codex captures; the real-harness round trips
remain opt-in because they require the operator's authenticated installations.
