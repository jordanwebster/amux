# Agent-to-agent messaging and families

**Status**: implemented (2026-08-23). This document owns amux's
agent-to-agent message envelope, provider carriers, model-facing tools, and
parent/child lifecycle. [`PROTOCOL.md`](./PROTOCOL.md) owns the links and
tunnels that carry remote calls; [`ARCHITECTURE.md`](./ARCHITECTURE.md) owns
the daemon service boundaries; [`UI.md`](./UI.md) and [`CHAT.md`](./CHAT.md)
own client derivation and presentation. Provider-specific row details remain
in [`CODEX.md`](./CODEX.md) and
[`CLAUDE_TRANSCRIPT.md`](./CLAUDE_TRANSCRIPT.md).

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

## The five agent tools

Claude receives the tools from the hidden `amux mcp claude` stdio server;
Codex receives the same definitions as per-thread dynamic tools. The caller's
identity comes from the carrier (`AMUX_AGENT_ID` for Claude, the owning Codex
thread for Codex), never from tool arguments.

| tool | input | result | meaning |
|---|---|---|---|
| `agents` | `{}` | fleet rows | List names, kinds, hosts, liveness, work, parent, and the caller marker. |
| `send` | `{to, text, context?}` | `{id}` | Send text to an agent by name. Replies to an `amux:` address must use this tool. |
| `spawn` | `{kind, prompt, name?, cwd?}` | `{name, id}` | Create a Claude or Codex child and deliver its initial prompt. |
| `stop` | `{name}` | `{}` | Stop a direct child of the caller. |
| `status` | `{working_on: string|null}` | `{}` | Set or clear the caller's current work. |

The descriptions steer models to amux for cross-kind work while leaving
Claude's native same-kind agent tools and Codex's native multi-agent features
available.

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
`completed` envelope. Claude supplies the text through the `Stop` hook; Codex
uses the last `agentMessage` observed before `turn/completed`. If the child
session ends, the parent instead receives an `exited` envelope with an empty
body. Completion leaves the child alive and idle so a later message can start
another turn.

Model-facing stopping is lineage-scoped in `ClientService`: Claude's MCP
server and Codex's dynamic tool attach the calling agent id, and the daemon
accepts the delete only when the target is that caller's direct child. Human
deletion remains an unscoped administrative action. Deleting
a parent cascades through all descendants, including agents on paired hosts.
Reachable descendants are removed deepest-first. Descendants on an unreachable
owning host are reported as still running rather than silently forgotten. The
command-line `rm`
refuses a family with a non-empty work claim unless `--force` is supplied.

## Delivery carriers

The owning daemon hands the envelope to the recipient's `AgentBackend`; each
provider decides how its native session accepts input.

### Claude

Every spawned Claude session receives an amux-owned `--name`. Claude Code
versions at or above 2.1.224 also receive a per-agent
`--messaging-socket-path`. The daemon discovers the installed version once with
an asynchronous `claude --version` probe bounded to ten seconds. A failed or
timed-out probe leaves socket delivery disabled. Later process starts read the
cached result without another probe; any transcript row reporting a different
`version` refreshes the daemon cache for subsequent sessions. Hook calls
forward only `CLAUDE_CODE_MESSAGING_SOCKET` and
`CLAUDE_CODE_MESSAGING_TOKEN`, allowing the session to refresh the credentials
without exposing the rest of its environment. Externally started sessions
discovered through hooks remain
transcript-only and readonly: they are never message delivery targets, even
when their hooks expose live messaging credentials.

For an agent sender with a ready socket, amux posts Claude's native
`<cross-session-message>` wrapper. The body begins with an amux header carrying
the envelope id, kind, sender kind, and optional context, then returns to the
sender without waiting for transcript confirmation. The session checks in the
background for an id-bearing peer user row or `queued_command` attachment over
the next five seconds. Before any socket delivery has been confirmed, a miss
makes the session PTY-only and resends the same envelope through the fallback.
After a confirmation, one miss resends only that envelope and keeps the socket;
two consecutive misses make the session PTY-only. A confirmation resets the
miss count. The resend retains the envelope id so the recipient can recognize a
duplicate.

The fallback is a bracketed PTY paste of the generic `<amux>` tag followed by
Enter. It is also used for human messages, older Claude versions, and sessions
without socket credentials. Before the paste is built, tabs become spaces and
carriage-return line endings become newlines; other control characters except
newline are dropped. The remaining message is still delivered. Claude queues
input received during a running turn.

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
