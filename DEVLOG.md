# amux Development Log

This file tracks significant development work, decisions made, and current state. Update this file after completing a chunk of work.

---

2026-08-23 — **Spawned Claude sessions do not inherit messaging secrets.**
The child environment scrub now removes the Claude messaging token alongside
the socket path, preventing a daemon launched from Claude from forwarding its
parent session's credentials into a new managed session.

---

2026-08-23 — **External Claude sessions remain observation-only.**
Claude delivery targets retain the session's readonly state and reject agent
messages with `FailedPrecondition` before considering socket credentials or a
PTY. A daemon-level test bootstraps an external hook with messaging credentials
and proves an agent-authored send receives the clear readonly error.

---

2026-08-23 — **Spawn delivers the first prompt or removes the child.**
Message targets expose backend liveness, and delivery waits for up to 30
seconds before sending. This covers Codex's interval between record creation
and thread attachment. If readiness or the first delivery fails,
`ClientService` stops and withdraws the new child before returning the error,
so a failed spawn does not leave an orphan. A delayed test backend proves the
wait path; permanently unavailable and live-but-rejecting backends prove both
rollback paths.

---

2026-08-23 — **Every model-facing stop is enforced at the daemon boundary.**
Claude MCP now carries its authenticated agent id into deletion, and
`ClientService` accepts that delete only when the target records the caller as
its direct parent. Codex uses the same checked request; human administrative
deletion remains unscoped. Daemon coverage refuses siblings, top-level agents,
and unknown targets before proving a direct child can still be stopped.

---

2026-08-23 — **Operator acceptance script for agent-to-agent messaging.**
`e2e-tests/a2a_acceptance.sh` walks a human through the one proof that needs
both real harnesses under their own login: start a Claude parent, ask it to
spawn a Codex child through the amux `spawn` tool, watch the family appear in
`amux list --all`, and confirm the child's completion reached the parent. It
drives the production boundary (typing into the parent) rather than any
internal API, and records PASS/FAIL to an optional result file.

---

2026-08-23 — **DEVLOG follow-up for `4c45513` (`Graduate Codex dynamic tool
capture`).** That chunk graduated the live Codex 0.148.0 C.11 dynamic-tool
exchange, its provenance, and its structural fixture waiter. The capture
shows `dynamicTools` registration, a model-issued `send` call, and the
structured response that completed the turn.

2026-08-23 — **Documented agent messaging as one end-to-end contract.**
The documentation map now points to one owner for the message envelope,
model-facing tools, Claude and Codex carriers, parent/child lifecycle, routing
and failure behavior, family presentation, and command-line views. The wire,
architecture, provider-row, client-model, and chat documents each name only
their part of that contract and link back to the owner, so the trust boundary
and recipient-owned provenance are stated consistently without duplicating a
second design.

2026-08-23 — **The family keys are listed where they work, and nowhere else.**
The binding table now takes the screen's own facts: each family chord appears
in a chat's `?` overlay only while it would do something — somewhere in the
family to go, a completion with a body behind it, a child's ask this chat
could host — and `z` appears in the fleet's overlay and hint row only with a
family on the fleet. Two omissions surfaced doing it: the Codex overlay had
never named the two chords its chat already answered, and the fleet's hint row
had never named the fold key. A hint row too narrow for the optional chord
drops that chord rather than the whole row.

2026-08-23 — **A delete confirmation names the cascade.**
Deleting an agent that started others takes its whole subtree, and a family
is one row on screen, so the confirmation now lists every agent that would go
— indented to its generation, flagged where one is mid-task and saying what it
says it is doing, with a count of what a short viewport could not show. It
does not block: an idle child costs no extra keystroke and a working one is
flagged rather than refused, because the person is looking straight at the
list. An agent that started nobody keeps the one-line prompt it always had.

2026-08-23 — **A child's ask can be answered from its parent's chat.**
The banner naming a waiting child now leads somewhere: one chord docks that
child's own ask panel where the parent's composer sits, drawn by the child's
layer under the child's id, and confirming it dispatches that layer's own
command addressed to the child. Nothing is copied out of the child, so this is
a second place to reach the one answer path rather than a second way to
answer: the child's own chat is unchanged, and an ask resolved anywhere takes
the panel away by the same re-derivation that takes the banner. Only one ask
is ever on screen — an agent's own obligations hold the bottom block — and
while a guest is docked, Enter and Ctrl+X belong to the agent whose ask it is,
so the rows that used to claim them fall silent.

2026-08-23 — **A completion reads as a completion in every chat.**
What a delivered message makes of itself on screen now follows from its
envelope kind, decided once in the client kernel: an ordinary message is a
sender marker over its body, a completion wears a finished mark over a body
that closes to its first line, and an exit is a one-line notice with nothing
to open, because the daemon sends it with an empty body. Both native chats
read that one decision instead of each classifying the kind themselves, and a
kind this build does not know is still shown as the message it plainly is.

2026-08-23 — **A child's ask surfaces in its parent's chat.**
The client answers which of an agent's descendants are waiting on the human,
each named by the child's id and the layer that draws its ask, so a parent's
chat can host the child's own prompt without any record being written into the
parent's stream. The answer is re-derived from each child's current card and
uses no attention reason the fleet did not already have: answering the ask
anywhere empties the list with nothing to clear, and a child on an unreachable
host asks for nobody.

2026-08-23 — **Suspended families resume intact.**
Suspend snapshots now restore a child's parent edge and exact work-status
timestamp when the daemon restarts, and resumed inventory republishes both.

2026-08-23 — **Agent work status follows the task lifecycle.**
Child creation derives a bounded task label from the initial prompt, explicit
status changes publish timestamped fleet updates, and turn completion clears
the status while leaving the child available for later messages. Runtime
metadata remains independent of provider-specific session implementations.

2026-08-23 — **Agent stop is scoped to direct children.**
The model-facing stop tool now verifies the authenticated caller owns the
target's parent edge. Stopping a child leaves its parent and unrelated agents
untouched; human deletion remains unchanged.

2026-08-23 — **Deleting an agent cascades through its family.**
The client daemon walks mirrored parent relationships deepest-first, removes
reachable local and remote descendants, and reports children whose owning host
cannot be reached without silently dropping their records.

2026-08-23 — **Child sessions inherit provider permission policy.**
Claude children retain only the parent's permission-mode launch arguments,
while Codex children inherit the parent's structured approval and sandbox
policies. Explicit working-directory overrides remain independent of policy.

2026-08-23 — **Child startup uses the authenticated message path.**
Creating a child records its parent relationship, keeps the inherited working
directory, and delivers the initial task through the child's normal carrier
after the backend has started. The envelope author is resolved from the live
parent rather than trusted from the create request.

2026-08-23 — **The Codex structured row vocabulary includes agent messages.**
The closed taxonomy now names the daemon-synthesized message row and its
authenticated provenance, envelope context, body, and carrier fields. The
backend projection fixture retains that complete shape instead of treating it
as an unknown type, and the maintained real-Codex suite documentation covers
the four carrier scenarios already registered as C.11–C.14.

2026-08-23 — **Agent messages enter Codex through its native thread API.**
Delivery appends a daemon-authored user message to the managed thread, starts
an empty turn when the thread is idle, and leaves an active turn running when
it is busy. If injection is unavailable, the backend starts a visible turn
with the same tagged text. Every accepted message also emits an
`amux.codex_message` row with its authenticated provenance and carrier, so the
native transcript remains the source of delivery history.

2026-08-23 — **Codex threads execute the shared amux agent tools in-process.**
Every managed thread registers the same five schemas and model-facing
descriptions as the Claude MCP server. Dynamic tool calls are parsed through
that shared contract, authenticated with the owning agent ID, routed through
ClientService for fleet, message, create, delete, and status handling, and
answered automatically with Codex input-text content. A scripted app-server
transport proves registration, authenticated request mapping, and the returned
content shape.

2026-08-23 — **Codex child turns report their final assistant message to the
parent.** Codex event ingestion retains the latest completed agent-message text
for each active turn. When that turn completes, child sessions publish the text
through the daemon's session-event path as a completed envelope addressed to
their recorded parent; standalone sessions publish nothing. An offline replay
of the captured two-message Codex 0.148.0 turn proves the final answer wins over
earlier commentary and is emitted exactly once.

2026-08-23 — **The Codex SDK exposes the captured agent-message carriers.**
Thread creation accepts typed dynamic function tools and serializes them on the
experimental `dynamicTools` field. Thread handles can append raw model-history
items with `thread/inject_items` and start an empty-input turn to consume idle
injections. Offline replays exercise the full dynamic-tool request/response
exchange and the injected-message turn against the graduated Codex 0.148.0
captures.

2026-08-23 — **Claude's amux tools state their routing and inheritance rules.**
The send description directs every reply to an `amux:` sender through amux and
reserves the cross-kind path for Claude-to-Codex communication while leaving
same-kind native tools available. Spawn states the same adoption boundary and
the default working-directory and permission-policy inheritance. A checked text
fixture pins all five model-facing descriptions against silent drift.

2026-08-23 — **Every spawned Claude session receives the amux MCP tools.**
Claude argv now registers the running amux executable as an inline stdio MCP
server and pre-allows its tool namespace, while filtering caller-supplied
overrides of both managed flags. The bundled Claude plugin also publishes the
same server for discovery; its version bump makes `amux new claude` materialize
and reapply the expanded bundle for existing installations.

2026-08-23 — **Claude MCP calls carry the owning amux agent identity.** The
stdio server reads `AMUX_AGENT_ID` once at startup, authenticates sends with
that UUID, targets status changes to it, marks the matching fleet row as the
caller, and resolves its host before recording it as a spawned child's parent.
Malformed and stale identities fail explicitly instead of degrading to human or
orphaned operations.

2026-08-23 — **Claude can discover amux agent operations over stdio MCP.** The
CLI has a hidden `mcp claude` plumbing command that negotiates JSON-RPC over
newline-delimited stdio, advertises the `agents`, `send`, `spawn`, `stop`, and
`status` tools with closed input schemas, and maps their calls onto the public
daemon client. Tool results use MCP text content containing the documented JSON
shapes; protocol tests cover discovery and every request mapping.

2026-08-23 — **Claude agent messages have a safe PTY carrier.** Daemon-authored
message envelopes are submitted through Claude's bracketed-paste program with
the verified render delay and Enter terminator. The carrier shares the PTY
program executor with structured input, normalizes carriage returns exactly as
the composer does, and refuses escape, tab, NUL, and other unsupported control
characters before writing any bytes.

2026-08-23 — **Claude sessions own their native name and inbox socket flags.**
Every spawned Claude process receives the amux agent name, with the agent UUID
as the unnamed fallback. Claude Code versions at or above 2.1.224 also receive
a per-agent messaging socket beneath the daemon's configured runtime directory;
older, malformed, and unavailable version probes stay on PTY delivery. Managed
name/socket arguments cannot be overridden through extra argv, and inherited
Claude messaging sockets remain scrubbed from the child environment.

2026-08-23 — **Agent messages route across paired devices.** The whole-daemon
specification now sends from a local live agent to a remote echo agent over
both a direct TCP pairing and two cloud-only device links. In each topology,
the sender daemon resolves the mirrored recipient, forwards the authenticated
envelope through the peer service, and the owning daemon delivers it through
the recipient's local backend.

2026-08-23 — **Agent-authored messages retain daemon-resolved provenance.** The
generic transcript tag now carries the sender's agent kind alongside its id,
name, and host, and the shared parser exposes that field for downstream native
chat folds. A whole-daemon echo specification proves a client-supplied local
agent id is expanded from the live registry before delivery.

2026-08-23 — **Human agent messages now reach local backends as authenticated transcript text.** Every agent backend has a message-delivery seam with an explicit carrier result. The test-agent backend formats the daemon-authored envelope and writes it to its PTY, while unsupported production carriers retain a typed unimplemented result until their native delivery paths land. A whole-daemon spec sends through `ClientService` and proves the echo agent's own output contains the matching envelope id, human provenance, and body.

2026-08-23 — **The public client can create child agents, send authenticated messages, and publish agent status.** `CreateAgentRequest` now carries the optional parent edge and initial prompt through the client wire boundary. `Client::send_message` accepts a recipient, text, optional context, and optional local sender identity and returns the daemon-issued envelope ID; `Client::set_agent_status` sets or clears `working_on`. Existing callers state their standalone-agent defaults explicitly, and the UI crate continues to compile through its kernel `Agent` re-export.

2026-08-23 — **Workspace fixtures now construct the expanded agent inventory shape explicitly.** CLI, reducer, and terminal golden helpers state that their existing standalone agents have no parent and no current-work description, keeping all-target workspace verification aligned with the new optional inventory fields.

2026-08-23 — **Agent messages now have one authenticated envelope and transcript-safe text form.** The protocol defines agent and human senders plus message, completion, and exit kinds. The public `amux::envelope` module formats the generic injected tag and Claude's native cross-session wrapper, parses both into the same provenance view, and escapes XML-significant body and attribute characters. Generated arbitrary Unicode bodies round-trip, while an attempted closing tag remains inert text.

2026-08-23 — **Agent inventory records can express family lineage and current work.** The agent protocol, runtime record, public DTO, wire codecs, and suspended-session representation now carry an optional parent host/agent pair and an optional timestamped `working_on` description. Round-trip coverage pins UUID and timestamp validation across the record-to-wire path and persistence preserves both fields. The opt-in real-harness binaries recognize non-scenario A2A unit-test filters, allowing the offline ledger checks to sweep all test targets without accidentally launching or rejecting a live capture.

2026-08-23 — **The A2A capture suite is clean under the workspace lint oracle.** The capture-only graduation helper explicitly tolerates being unused by integration-test targets that include its module without invoking promotion, and the dynamic-tool event matcher follows the workspace's current Clippy requirement without changing capture behavior.

2026-08-23 — **Claude Stop hooks now have a current-version completion fixture.** The isolated 2.1.240 capture records `hook.stop` with the exact `last_assistant_message` from a completed turn, giving the completion path a redacted, structurally checked offline witness.

2026-08-23 — **Claude 2.1.240 PTY delivery is captured for idle and busy turns.** A bracketed-paste `<amux …>` envelope lands byte-for-byte in an idle user row. During a Bash-backed active turn, Claude first records an enqueue operation and then a `queued_command` attachment carrying the same envelope, proving the fallback carrier is queued rather than dropped.

2026-08-23 — **Claude 2.1.240 socket captures now have a self-contained live harness.** Each disposable capture project installs the repository hook manifest, avoiding stale globally installed hook commands, and records the ephemeral messaging socket/token only long enough to authenticate the probe. The socket carrier capture proves idle and busy delivery through the native peer user rows, with an enqueue `queue-operation` preceding each row. It also extends redaction for late-arriving MCP instruction attachments, so private local tool configuration cannot enter the fixture corpus.

2026-08-23 — **Codex A2A carrier captures now pin idle injection, busy injection, and completion ordering.** The isolated C.12–C.14 harness uses a direct experimental app-server client to record `thread/inject_items` without expanding the production SDK surface. On Codex 0.148.0, an idle injected user message is consumed by a subsequent empty-input turn; a message injected after `turn/started` queues into that same turn after its first assistant response; and two completed assistant messages arrive commentary then final before `turn/completed`, whose summary retains the final message. Redacted IO and provenance metas plus structural fixture waiters preserve those observations for offline replay.

2026-08-23 — **Claude capture startup now accepts the local-instructions import confirmation.** The isolated capture harness recognizes the follow-up prompt emitted by Claude Code 2.1.240 after trusting a fresh scenario directory, records the input in its keystroke provenance, and continues to the composer without conflating the confirmation with transcript evidence.

2026-08-23 — **The Codex capture suite now exercises experimental dynamic tools.** C.11 launches an isolated direct app-server connection, passes `dynamicTools` at thread creation, requires the model to call `send`, answers the tool request with structured content, and records the full JSON-RPC exchange for fixture graduation.

2026-08-22 — **Cloud subscription refusals now degrade visibly without affecting local agents.** `/api/connect` distinguishes `403 {"error":"payment_required"}` from authentication and retriable transport/server failures, while other `4xx` rejections are terminal except intermediary timeouts and rate limits (`408`/`429`). Payment-required signals use a distinct protocol code and separate UI model state. The daemon persists subscription-required state through the same reporter pattern as update state; compact CLI output reads its marker directly, while the fleet runtime watches it and renders an `amux.sh/account` banner with the local fleet still usable. The cloud task keeps that state visible while calmly probing `/api/connect` every 120 seconds, clears it after entitlement recovery, and does not mislabel subscription lapses as authentication audit failures; classification, retry and recovery policy, state propagation, token refresh, reducer state, protocol mapping, and the rendered frame have regression coverage.

2026-08-18 — **The rev10 Handoff machinery is built and this repo's footprint shrank to the one-line trigger.** Codex rebuilt the `~/source/skills` repo in five components: bundled handoff scripts (single staleness rule, sticky audit selection), the reviewer and audit contracts, the `handoff` and `tasks` skills, and the selective installer. The AGENTS.md obligations blob is replaced by the standing trigger — every substantive chunk of work ends with a handoff — with all discipline living in the globally installed skill.

2026-08-18 — **The review machinery was renamed Packet Review → Handoff.** The machinery now lives in the generic `~/source/skills` repo (Handoff is its founding resident), the consumer config is `.handoff.toml`, and the AGENTS.md machinery section points at the new path. The spec was rewritten as rev10 (proofs, four-section front page, `handoff` + `tasks` skills), amended after an external Codex review (independent-check status line, convergence rule, sticky audit selection, merge-ready default endpoint), and awaits the rebuild; this repo's obligations section will shrink to the one-line trigger when the rebuilt skills land.

2026-08-18 — **Local working notes are no longer versioned.** Removed the remaining tracked `notes/codex-impl/CLOSED.md` from the Git index while preserving it locally; the existing `notes/` ignore rule now covers the entire notes tree consistently.

2026-08-18 — **CI now reproduces from a clean checkout on Rust 1.97 and nightly formatting.** The Claude transcript taxonomy consumed by capture tests moved from ignored working notes into tracked `docs/CLAUDE_TRANSCRIPT.md`, so `include_str!` no longer depends on local-only files. Both native-chat scroll percentages use saturating multiplication plus checked division for the current denied Clippy lint, the bare-help E2E contract now includes the shipped `amux rm` command, and the workspace has been normalized with the same nightly rustfmt command used by CI. The Codex-session subscription unit that constructs a real Codex backend is Unix-gated consistently with the product backend, keeping the Windows test matrix honest. Capture redaction now scrubs and verifies both native and JSON-escaped path forms, including Windows backslash paths; the prebuilt-binary depfile guard likewise understands Windows drive prefixes, native separators, CRLF continuations, and Make-escaped spaces.

2026-08-18 — **The Codex integration workstream is closed.** The corrective and Batch B/D arc made thread publication authoritative and transport-recoverable, added honest non-TTY/post-create recovery plus exact `amux rm`, typed structured-protocol selection and unknown-agent degradation, all-build nonfatal invariant handling, measured final-detach raw-PTY teardown, and raw preparation outside the agent-registry lock. Batch C landed separate lossless Claude/Codex classifiers, independent agreement controls at their correct attention altitudes, authoritative reducer/view/key/read-only gates, the ten-row historical mutation proof, typed MCP startup aggregation, bounded approval labels, and the one-shot resumed-feed boundary. Batch E removed six asymmetric serialization-only fields while retaining operational `PromptEcho.at`, corrected patch-only work classification, completed the full-range review, refreshed real Codex 0.147.0 C.1–C.10 evidence, and brought CODEX/UI doctrine current. C.9's automated leg remains process/stream smoke rather than a composer oracle. C.9 owner witness passed: post-teardown reattach spawned a fresh codex resume and the composer accepted and echoed input on codex 0.147.0. All plan-created capture/auth/process residue was removed and independently verified without touching historical owner data or a later user-started empty server. `INVESTIGATIONS.md` is fully dispositioned, the former defect-class study is superseded, `CLOSED.md` records the evidence boundary, and future VT100 automation plus the owner-authored Packet Review path move to `closeout/DISCOVERED.md` rather than extending this container.

2026-08-18 — **Codex and UI doctrine now match the completed close-out behavior.** `docs/CODEX.md` now records the all-build invariant policy, the measured 151,248 KiB (147.703125 MiB) raw-PTY teardown decision, the symmetric six-field D9 deletion with operational `PromptEcho.at` retention, and the chosen typed MCP startup aggregate. It documents bounded typed approval labels, the one-shot `resumed:true` ready payload and exact context-intact divider, authoritative observation-only gates, and C.9's honest process/stream proof without claiming a usable raw composer; the four completed “Known gaps” are replaced by the signed parked/future-work ledger verbatim. `docs/UI.md` now describes the shipped Claude/Codex native chats, independent per-layer classifiers and agreement matrices, reducer/key/view gate authority, read-only distinctions, explicit unknown-agent card degradation, and the precise fatal-test opt-ins. `docs/ARCHITECTURE.md` already described D5's two-phase raw preparation boundary and remains unchanged; owner-authored `AGENTS.md` is deferred separately.

2026-08-18 — **The closeout simplification sweep removed asymmetric dump-only state and aligned Codex file-work classification.** Deleted six fields whose only production consumer was Model serialization and which had no equally retained Claude/Codex twin: per-change raw diffs, cached input-token counts, raw available-decision arrays, Claude prompt/message timestamps, and accepted-plan paths. Retained `PromptEcho.at` because it is operational staleness evidence, not dump-only provenance: a fresh optimistic send must outrank an old transcript and still age safely if unresolved. Codex `patchUpdated` rows now take the same open-work path as file output deltas, so a patch-only active turn projects `Executing` without changing its Working attention or active-turn write permission. A focused regression locks that intermediate state; the full invariant/spec/golden controls remain unchanged.

2026-08-18 — **Persisted Codex resumes now declare their feed boundary without claiming history loss.** The first successful attachment created from an initially persisted thread id emits the existing `amux.codex_ready` method with the one-shot payload `resumed:true`; fresh starts, ambiguous fresh-thread recovery, and later same-process reconnects retain the bare row. Codex's UI folds that fact into a typed top-of-feed divider reading `resumed · earlier history not re-rendered · context intact`, while C.5 now requires the marker alongside its existing thread-identity and remembered-context proof.

2026-08-18 — **Codex startup and approval noise now render as typed human state.** `mcpServer/startupStatus/updated` folds into one retained Codex-local feed entry whose `BTreeMap` holds each server's latest starting, ready, failed, or cancelled state; updates preserve the entry identity and creating sequence, while malformed rows and future status spellings still surface as unrecognized protocol drift. The TUI renders that entry as one compact aggregate rather than a screen of raw rows. Command approval contexts now retain typed exec-policy and network-policy proposals, allowing matching object-valued choices to receive bounded human labels without trusting their wire payload; unknown objects expose only a sanitized, display-width-bounded kind and scalar summary. Object choices remain unavailable in structured V1. Focused fold, label-safety, and key tests pass, and the separately observed and accepted golden delta is limited to the new MCP startup pair plus the approval-pending pair.

2026-08-18 — **amux is the first Packet Review consumer.** The task-review machinery designed over the last two days now lives in its own repo (`~/source/packet-review`, founded from the rev9 plan; skills authored once in the open agent-skills standard and symlinked into both Claude Code and Codex). amux's install is the deliberately tiny consumer footprint: the standing-obligations section in `AGENTS.md` and `.packet-review.toml` naming the Linear backend, the spec suite and `docs/` as theory homes, and amux's ownership of the protocol and connect-token contracts. Task branches will carry `packets/<task-id>/` evidence; `docs/decisions/` appears when the first ratified decision is filed. The first real amux task through `/task` is the machinery's end-to-end test.

2026-08-18 — **Observation-only policy now lives in both native write gates.** Codex and Claude classifiers carry `Agent.readonly` orthogonally, so phase and attention continue to describe the observed session while every direct write refuses locally with `agent is read-only — you are observing this session`. Codex keeps its structured reconnect-read-only state and wording distinct, with lifecycle refusal precedence intact; active-turn input in flight remains interruptible only for a writable observer. Claude adds an explicit read-only send gate, classified answer and broad interrupt queries, and reducer checks before encoding, optimistic mutation, or input emission; its independent public-projection matrix now covers read-only across replay, error, working, rest, finished, asks, sends, offline/stale degradation, and exited precedence. Direct-command matrices cover all four actions for each layer with no pending input or source-state mutation. Only the redundant Codex working-hint and Claude reader raw-readonly action filters were deleted; viewer keymaps, paste/focus policy, navigation, headers, read-only bottoms, and reader presentation remain intact.

2026-08-18 — **Native chat affordances now follow their write gates.** Codex's working line asks steer and interrupt permission independently, so an active turn with input in flight keeps the interrupt escape hatch without advertising a refused steer. Approval rows use `allows_answer` as their sole session-actionability fact; raw input correlation now controls only the `sending decision…` wording, and replaying/stale asks expose neither a cursor nor a confirm hint while unsupported wire decisions remain labelled unavailable. Claude gained a thin classified `allows_answer` query, and one TUI reader-actionability fact now combines it with the temporary view read-only guard, pending resolved ask, and verified menu shape for rendering, ordinary answer keys, Ctrl+C focus, and paste. A retained Claude ask therefore cannot change hidden cursor, stage, or feedback state or dispatch after its stream becomes unavailable, while gated Codex approvals likewise cannot move their hidden choice cursor; both retain their read/navigation escape paths. Focused render/key regressions pass, both existing chat golden suites remain byte-identical, and read-only viewing/focus branches remain in place for C4.

2026-08-18 — **Claude projection agreement is now a checked invariant.** Every folded Claude card is checked after each reducer message by an explicit relational matrix over the public chat phase, effective fleet attention, and send gate—not by comparing classifier projections back to the same classifier. The matrix independently encodes orderly exit, replay/error precedence, optimistic sends, matching ask reasons, and the intentional offline-host and send-staleness degradations; observation-only cards and unverified menus preserve their visible obligations pending the separately owned interaction gates. Positive and negative tables cover every relation, and a corruption test proves this invariant fires without the older cached-attention mismatch. The agreement chapter also registers the seven previously uncovered lifecycles—retryable reopen/replay, exit/re-upsert, retained-card deletion close, prompt echo plus ask, `/clear` relink replay, an offline folded layer, and a non-truncated ask mid-replay—and locks that replay prefixes suppress apparent asks until the ready fact arrives. Invariant-fatal specs pass after every message and all existing goldens remain byte-identical.

2026-08-18 — **Codex and Claude now classify each session once per layer.** Codex's private `Situation` now folds kernel stream lifecycle together with layer state while retaining active-turn and input-in-flight facts; phase, cached attention, send gate, all four write permissions, and the checked agreement invariant project from that value. Claude gained its own private `ChatCondition`, preserving ask, error, working, finished, interrupted, resting, and optimistic-send distinctions with explicit observation time; cached attention remains time-free and `Model::effective_attention` still owns offline/staleness degradation. The corrected not-live order keeps rows-seen/truncated prefixes replaying ahead of asks, lets a hook ask outrank only fresh-empty rest, and leaves fresh-empty chats idle. For live states, send-in-flight now projects Working attention before asks/finished/rest while unavailable, exited, replaying, unknown, and errored states retain precedence. Focused classifier tables cover every stream branch and the lossless cross-products; Codex and new Claude agreement matrices pass after every folded message. Formatting, 159 invariant-fatal UI specs, private classifier tests, and workspace clippy pass with only the two accepted tracked-listener warnings; no golden changed. Remediation now ages optimistic Working attention from the newest dated prompt dispatch or transcript delivery: a fresh echo outranks an old transcript, an unresolved echo eventually degrades to Unknown without reopening its `SendInFlight` gate, and reconciliation, failure, offline, undated, and echo-free behavior retain explicit-time semantics. Live runtime dispatch now stamps observation time from the real clock immediately before command reduction, and an open Claude chat ticks a fresh pending echo only while its effective attention remains Working, stopping after age-out or offline degradation without reopening the gate.

2026-08-17 — **Raw PTY preparation no longer blocks the agent registry.** Raw subscription lookup now validates the protocol and extracts an owned backend target under the host-state read guard, then releases it before Codex socket connection, PTY open, and `forkpty`. Codex preparation is serialized per session without holding its runtime mutex across blocking work; endpoint and stop-state revalidation prevents an old removed/reconnected session from publishing a stale PTY, while the existing epoch lease keeps shared fanout, final-detach teardown, stale-exit safety, and fresh reattach intact. A synchronization-controlled regression pauses preparation and proves a registry writer acquires within a bounded timeout; focused error, non-Codex lifetime, stopped-snapshot, and D4 lifecycle tests pass.

2026-08-17 — **Codex raw PTYs now tear down on final detach.** A five-sample macOS `ps` measurement put the zero-turn detached `codex resume` pair at a median 151,248 KiB (147.703125 MiB), above the signed 25,600 KiB threshold. Codex raw streams now own epoch-checked leases: concurrent subscribers still share byte-identical fanout, while the final stream exit retires its cached epoch before terminating the process group so reattach must lazily spawn a fresh `codex resume`; structured Codex and non-Codex PTY lifetimes remain independent. Deterministic lifecycle regressions cover partial/final detach, stale epochs, fresh reattach, and unchanged test-agent lifetime, and zero-turn C.9 now drops and recreates the real raw screen before boundedly polling the original structured subscription for closure or errors.

2026-08-17 — **Agent instructions reduced to operating rules.** `AGENTS.md` is now the single canonical rules file (both Claude and Codex read it) and `CLAUDE.md` imports it, removing the duplicated no-backcompat statement that had already diverged in wording between the two. The per-document source map moved out of the instruction layer into a new `docs/README.md`, alongside entries for `CHAT.md`, `CODEX.md`, and `HOW_IT_WORKS.md`, which the old map never listed. `AGENTS.md` now states its own boundary: rules about how agents operate live there; documentation about the source lives in `docs/` and component READMEs.

2026-08-17 — **Model invariant violations are loud but non-fatal by default in every build.** Each newly observed violation kind now logs at error level, attempts one bounded recorder dump, and sets a sticky Model fact rendered as the same diagnostic banner in fleet and native Claude/Codex chat chrome; the monotonic fact is mirrored into the recorder checkpoint so every later dump preserves the warning through real replay, while dump failures do not stop the fold. `AMUX_INVARIANT_FATAL=1` is the only panic opt-in and is set on CI test/E2E steps. Process-isolated policy regressions cover default and non-`1` behavior, fatal behavior, coherent state, dump throttling, replay retention, and all three render paths; required formatting, clippy, and package-suite verification is recorded in the closeout report.

2026-08-17 — **Unknown structured protocols now degrade safely and known stream dispatch is typed.** Added one exhaustive `StructuredProtocol` enum at the agent-layer seam and carried it through `Effect::OpenStream`, runtime subscription argument encoding, entry decoding, and native TUI view/watermark selection. Unknown or missing agents retain their fleet card, cannot construct a Claude chat, and report a neutral watermark; focused Claude, Codex, and fabricated-protocol regressions pass. Formatting and the required `amux-ui`/`amux-tui` suites pass with all existing goldens unchanged; workspace clippy reports only the two accepted tracked-listener warnings.

2026-08-17 — **Creation now fails safely outside terminals and recovers honestly after open errors.** `amux new` checks both stdin and stdout before creation for the current interactive chat/raw modes. After any post-create open/UI failure it now verifies the created UUID through the held client: only a present agent gets the running/recovery diagnostic, an absent agent returns the underlying error unchanged, and a failed check adds uncertainty without asserting liveness. Recovery commands always target the UUID while a requested name remains display-only. Added scriptable exact-name-or-UUID `amux rm <target>` over the existing deletion RPC, with missing and duplicate-name refusals; daemon-down handling intentionally emits no shell-specific target command. Focused boundary/recovery tests, fmt, clippy with only the two accepted tracked-listener warnings, and all 67 amux-cli tests pass; no golden changed.

2026-08-14 — **Fresh Codex attachment now reconnects after bootstrap transport loss.** Materialization keeps RPC refusals on the same-client backoff loop, but returns `TransportClosed` to the ingest supervisor with the uncommitted candidate id. A fresh connection authoritatively resumes that candidate and publishes it, or replaces it only when `thread/resume` reports `no rollout found`; neither path exposes an unproven id. Two mocked supervisor regressions close the initial naming transport and cover both ambiguity resolutions. Build, fmt, clippy (the two accepted warnings), warm workspace tests, 44/44 testnet specs, and zero-turn C.9/C.10 pass; the cold workspace run hit the documented macOS test-binary-startup timeout and passed on the required warm rerun.

2026-08-14 — **The real-Codex harness now trusts Cargo's binary dependency graph.** Replaced the workspace-wide `src/` walk with parsing and statting `target/debug/amux.d` prerequisites. Missing binary/depfile/prerequisite failures name the exact recovery build, unrelated crates no longer wedge the suite, and non-Rust inputs such as proto, plugin, and generated files participate. Added parser and staleness-control regressions.

2026-08-14 — **Codex attachment now commits only authoritative, raw-ready thread identities.** Fresh starts remain private until their bootstrap/name RPC succeeds (or a successful `thread/resume` settles an ambiguous response); resumed threads publish immediately and remote naming is independent retryable reconciliation. Deleted the duplicated `ThreadPersistence` rollout guess and its raw gate. Remote labels now converge through one serialized generation worker, so bootstrap-time changes are not lost, rapid renames cannot complete out of order, and clearing a name restores `amux-<short-id>`. Added mocked protocol regressions for all four boundaries plus C.10, an unnamed zero-turn suspend/resume scenario.

2026-08-14 — **Codex threads are now always named to materialize their rollout.** `amux new codex` with no `--name` failed on first contact with a real machine: `thread/resume failed … no rollout found for thread id`. Codex 0.147's `thread/start` creates a live, non-ephemeral thread and reports its prospective rollout path, but does not materialize that rollout; both `thread/resume` and `thread/archive` refuse it until an unrelated mutation persists it. There is no explicit persist RPC. Naming, memory mode, Git metadata updates, injected history, and feature-gated goals all materialize; settings updates do not. amux uses naming because it is the least invasive universally applicable choice: memory and goals are behavioral, Git metadata is not universal or inert, and injected items alter history. A name is standard, visible, and replaceable, though Codex 0.147 cannot restore it to `None`.

Every thread is created with a name (`thread_name_for`), an unnamed agent getting the bootstrap label `amux-<8 hex>` on its *thread* while the agent itself stays unnamed and keeps the usual display fallback. A fresh thread remains private until materialization succeeds (or successful resume settles an ambiguous response), then its id and attachment publish together. A resumed thread publishes as soon as `thread/resume` succeeds; later naming failure cannot revoke that authoritative fact or block raw attach. `CODEX_RAW_THREAD_NOT_READY` now means only that no attachment has been published. Remote names converge through one serialized desired-generation worker, so a bootstrap-time rename is not lost, rapid changes cannot complete out of order, and clearing a name restores the bootstrap label. This eager step is required beyond immediate raw attach: amux's structured reconnect, suspend/resume, and daemon-recovery paths also issue `thread/resume`.

Coverage: the harness takes `Option<&str>` so an unnamed agent is expressible, C.9 `c9_raw_unnamed` drives raw attach on the product default, and C.10 suspends and reconnects an unnamed zero-turn agent. Neither scenario sends a turn. Mocked protocol tests cover resumed-name failure, bootstrap-time rename reconciliation, rapid rename serialization, and private fresh-start recovery. The C suite drives the prebuilt `target/debug/amux`, which `cargo test -p amux --test codex_capture` does not rebuild; its staleness control therefore consumes Cargo's own `target/debug/amux.d` prerequisites, including proto/plugin/generated inputs, instead of reconstructing a conflicting workspace source graph.

Why nothing caught it: `codex_capture/harness.rs` hardcoded `name: Some(…)`, and it is the only real-codex agent construction in the tree — so C.7 and C.8 drove raw attach on a zero-turn thread and passed. The P5a acceptance criterion recorded in `GUIDE.md` read "*the thread exists in `thread/list`, name is set*", and `MORNING.md`'s "exact commands" passed `--name`. The fixture, the acceptance criterion and the handoff doc all agreed with each other, and none of them agreed with the product default. A seventh instance of the run's defect class, one rung up from the rest: **a fact established only as an incidental side effect of an unrelated feature, with no test that ever ran without that feature.**

2026-08-14 — **Codex integration: retrospective.** `amux new codex` ships. A codex agent is a thread on a shared, supervised Codex app-server — not a PTY amux owns — and it exposes two live planes at once: the structured `codex_sdk_v1` rows folded into a typed `CodexLayer` and rendered by a native amux chat screen, and the agent-independent `terminal_v1` byte plane serving the genuine codex TUI to any number of terminals. Suspend/resume survives a restart of both amux and the server, because identity is the thread id. Checkpoint #3 drove all seven steps of the goal live in a debug build with zero invariant violations, and the eight-scenario opt-in C suite makes it repeatable. Docs: `docs/CODEX.md` (new), surgical updates to `docs/UI.md` and `docs/ARCHITECTURE.md`.

Shape of the work: ten phases and three architectural checkpoints, 42 commits, ~22.1k insertions across 144 files. amux-ui reducer specs 109 → 152. 14 codex goldens added; all 54 Claude goldens byte-identical from first commit to last. Checkpoint #2 deleted 1054 lines of codex-sdk surface nothing had earned.

The finding worth keeping: **one fact derived in two places, with tests covering each derivation in isolation and never their agreement.** Six instances, every one invisible to a fully green gate. Three lessons, in order of how much they cost to learn. (1) A written instruction is not a control — the P8 brief explicitly forbade re-deriving a phase-like conclusion in the view, and it happened anyway, in a new file, on the first attempt. The invariant is the control. (2) "One source of truth" is not sufficient; the source of truth must be **lossless with respect to every question asked of it**. Routing all four write permissions through one `SendGate` shipped a P1 regression, because "a turn is active" and "an input is in flight" can both be true and only one survived the collapse — a stalled RPC made an active turn permanently uninterruptible. (3) A documented asymmetry can be a bug wearing a comment: `AgentLayer::attention` said "Claude retains its existing layer-only projection" for months, and that was the identical cross-altitude gap Codex shipped a panic for, observable in 45 states.

What actually found these: agreement invariants (assert two derivations agree, not that each is right), empirical probes (write a throwaway test, RUN it, observe, revert, report verified facts instead of suspicions), asking the implementer model to sweep its own diff for behavior-preserving-*in-intent* changes, and cross-model review — findings were repeatedly surfaced by one model, verified against history by a second, and qualified by a third. Standing follow-ups, with evidence, in `notes/codex-impl/INVESTIGATIONS.md`; the `readonly` flag is still enforced by view early-returns rather than by the gates, which is the open generalization with a name.

2026-08-14 — P9 real-Codex E2E suite: replaced the ad-hoc capture rig with an inert-by-default, table-driven C.1–C.8 suite covering create/pong, approval allow and deny with world assertions, interrupt/reuse, suspend/resume continuity, real process-group recovery, raw/structured coexistence, and two-terminal fanout. Added parsed structural waiters with four offline fixture tests, isolated non-secret Codex setup, redacted version-stamped captures, and no product fault seam. All eight live legs passed against codex-cli 0.147.0 / gpt-5.6-sol; formatting, clippy, workspace, and 44/44 testnet spec gates pass.

2026-08-14 — Checkpoint 3: deleted `CodexLayer::latest_token_usage`, which had exactly one occurrence in the tracked tree — its own definition. The underlying `latest_usage` field stays; it is read internally by the fold, which copies it onto `TurnEntry.token_usage`, and that is what the renderer displays. Same delete-rather-than-deprecate standard checkpoint 2 applied to the SDK surface.

2026-08-14 — Checkpoint 3: closed the Claude half of the cross-altitude attention gap. `claude::projected_attention` now caches `Unknown` while the kernel stream is opening/replaying, so the fleet badge stops claiming "idle" for a whole replay window — including one holding unanswered permission asks — while `phase` and `send_gate` both say `Replaying`. Same fix Codex got in `41f5433`, on the arm left behind; probe found 45 such observations across already-registered sequences. Red/green spec test plus the replay-window-with-asks lifecycle registered. All 54 Claude goldens byte-identical.

2026-08-14 — P8 simplification: narrowed the Codex write gate through a `session_state` -> `LiveState` result so the compiler, not three `unreachable!` arms, keeps session-level refusal membership in one place; registered the reconnect/reopen/mid-replay lifecycles the spec suite never reached; deleted the never-read in-flight steer/interrupt turn ids. Gates green, goldens byte-identical.

2026-08-14 — P8 review round 2: made all Codex write permissions project directly from the non-lossy `Situation` classification, restoring interrupt during an in-flight steer; red/green regressions, full workspace tests, 44/44 testnet specs, and byte-identical goldens pass.

2026-08-13 — P8 review round 1: moved Codex account preflight onto the host's shared fallback-capable app-server connection and made prompt, steer, interrupt, answer, and footer affordances consume one authoritative `SendGate`; full gates pass with no golden changes.

2026-08-13 — P8 projection fix: Codex fleet attention now degrades to Unknown while the kernel stream is opening/replaying, with fresh and resumed Runtime sequences locking every fold; Claude projections and all goldens remain unchanged.

2026-08-13 — P8 completion: hand-drove the native Codex screen through prompt,
streaming, command/cwd, steer, interrupt, help, scroll, and fleet return; raw
creation now retries the bounded thread-id publication race and attaches with
the captured terminal size.

2026-08-13 — P8 create/attach chunk: Codex creation now preflights
`account/read`, exposes typed model/approval/sandbox flags, honors the configured
new-agent open mode, and shows those choices on the initial chat screen;
`amux attach <codex>` enters structured chat while fleet raw attach remains
available.

2026-08-13 — P8 TUI chunk: split chat into native Claude/Codex screens, lifted
the proven shared composer/markdown/layout pieces, kept Claude-only diff and
reader semantics local, and added 14 two-theme Codex full-frame goldens across
the seven required states without changing a Claude golden byte.

## How to Maintain This Log

1. **Add new entries at the top** (reverse chronological order)
2. **Use the standard entry format** shown below
3. **Be concise but complete** - future you will thank present you
4. **Include verification results** - what was tested and how
5. **Document decisions and rationale** - not just what, but why

### Entry Template

```markdown
## YYYY-MM-DD: Brief Title

### Summary
One paragraph describing what was done.

### Changes
- List of files created/modified
- Key structural changes

### Decisions Made
- Decision 1: rationale
- Decision 2: rationale

### Verification
- What was tested
- Results

### Next Steps
- What remains to be done
```

---

2026-08-13 — P7 completion unified Codex phase/attention classification, closed-thread and read-only-answer gating, honest ask-overflow history loss, and a checked projection-agreement invariant; all required gates pass.

2026-08-13 — P7 simplification pass: shared Codex work-entry/kind constructors, gate-sourced refusal text, and dropped the four dead pending-approval clears the row guard already owns; no behavior, spec count, or golden byte changed.

2026-08-13 — P7 review round 2 made reconnect failure outrank pre-ready replay and aligned fleet attention with ready-bound Codex liveness; all required gates pass.

2026-08-13 — P7 review round 1 fixed dynamic-tool decisions, ready-bound startup gating, sticky gap history loss, read-only turn writes, and row-keyed plan snapshots; all required gates pass.

## 2026-08-13: Add the typed Codex UI layer (P7)

### Summary
Implemented the native `codex_sdk_v1` reducer layer: protocol rows now fold
into Codex-owned prompts, messages, reasoning, work, turns, boundaries,
errors, obligations, phase, gates, and attention. A live 0.147 capture proved
that `userMessage` start/completion rows arrive, so normal prompts are
protocol-sourced. Successful steers use a typed correlated echo because the
upstream protocol does not emit a steer `userMessage`.

### Changes
- Added `crates/amux-ui/src/codex/` with the native entry/command/input,
  approval, phase, retention, and invariant vocabularies.
- Registered Codex through the P6 enums and runtime protocol codec; no common
  feed, phase, ask, or content shape was introduced.
- Added three prose-spec chapters covering row folding, approval correlation,
  unsupported questions, row-derived interrupts, input-result correlation,
  and serde/differential replay.
- Added the four honest Codex labels to the TUI's pure `command_verb` dispatch
  under the orchestrator's fence amendment; no layout, screen, key, renderer,
  or golden changed.

### Decisions Made
- Normal `Prompt` entries come only from upstream `userMessage` items. Steer
  text is retained in-flight and becomes `PromptSource::SteerEcho` only after
  its matching `amux.input_result` succeeds; failure leaves no false echo.
- Gaps and re-sync markers reset turn accumulators but retain the layer's
  explicitly observed active turn id, so interrupt never uses the backend's
  empty-current sentinel.
- Object-valued approval decisions remain wire-verbatim and visible but are
  disabled in V1; the frozen backend accepts only four named decisions.

### Verification

- `cargo fmt --all` — pass.
- `timeout 600 cargo clippy --workspace --all-targets` — pass with the two
  accepted tracked-listener dead-code warnings.
- `cargo test -p amux-ui --all-targets -- --test-threads=1` — 34 unit, one
  runtime, and 138 spec tests pass.
- `timeout 600 cargo test --workspace` — pass after warming host test-binary
  startup; the complete untruncated workspace run also passed.
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44/44 pass.
- TUI golden directory status is empty after the gate; byte-identical.

### Next Steps

- P8 can add Codex-specific rendering, layouts, and keys over this typed
  layer; those surfaces remain intentionally absent from P7.

## 2026-08-13: Generalize the amux-ui kernel for typed agent layers

### Summary
Removed Claude assumptions from the reducer kernel while preserving every
Claude surface byte-for-byte. Agent state, commands, input payloads, gates,
stream protocols, and invariant violations now dispatch through exhaustive
typed enums; the public input API also exposes the correlation id needed to
match structured input-result rows.

### Changes
- `amux-ui`: `AgentCard.layer: Option<AgentLayer>`, namespaced
  `Command::Claude(ClaudeCommand)`, `InputPayload::Claude`, per-layer protocol
  selection, and `Violation::Claude(ClaudeViolation)` with stable keys.
- Moved Claude command reduction, phase/send/mode gates, optimistic failure
  handling, and invariant vocabulary into `amux-ui::claude`.
- `SendInputRequest.input_id` is required and forwarded verbatim; UI input uses
  the operation UUID bytes, and Claude stale-sequence retries reuse that id.
- TUI command call sites were mechanically namespaced; generated create names
  now use the selected agent type (`claude-N`, `codex-N`).

### Decisions Made
- Protocol strings are owned by their typed layers. The kernel selects a layer
  from advertised `io_protocols`; it does not infer one from `agent_type`.
- `OpId` is the reducer-minted input correlation identity. This preserves pure
  replay without adding another id to `Msg` and naturally survives retries.
- No generic content or input representation was introduced: each new agent
  adds exhaustive enum arms with its native model and payload.

### Verification
- `cargo fmt --all`; `timeout 600 cargo clippy --workspace --all-targets` — no
  new warnings (the two existing tracked-listener test warnings remain).
- Warm `timeout 600 cargo test --workspace` — pass. The first cold run reached
  the final amux-ui spec binary with no failures before the known slow macOS
  test-binary startup exhausted the wrapper.
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44/44.
- amux-ui specs — 123/123, including serde and `wire_free`; amux-tui goldens —
  54 chat + 21 fleet, with no golden-file changes.

### Next Steps
- P7 adds the Codex layer as sibling enum arms and a new module; the kernel
  dispatch structure does not need another reshape.

Follow-up (simplification): the phase left each Claude gate derived twice — `ClaudeLayer::{phase,send_gate,mode_cycle_gate}` took the kernel bits (`AgentPhase`, `StreamState`, `now`) that their only callers, the `claude` module's `Model`-level functions of the same names, had just extracted; the methods folded into those functions (`folded_phase` stays private), and the speculative `Model::layer` accessor (no callers) plus the `AgentLayer` re-export (P7 adds it when a renderer needs it) were dropped.

Follow-up (regression): the move also inverted both gates' check order, so an exited agent whose structured stream never produced layer evidence refused with "chat input unavailable for this agent" instead of "agent exited". Exit is authoritative — `AgentCard::phase` reaches `Exited` only from the `StreamCloseReason::AgentExited` fact, never inferred from layer evidence — so the pre-P6 order (card → exit → layer) is restored in both gates and locked by a test; nothing had covered it, which is why a green gate missed it.

## 2026-08-13: Checkpoint #2 — one owner for the Codex disconnect invariant

### Summary
`set_runtime_error` and `mark_disconnected` both dropped the connection-local
Codex handles, but only one of them drained the pending approval table. That
the attach-failure path was safe depended on knowing, non-locally, that
`pending` is always empty there. Folded them into one function so the
invariant is structural: dropping the live handles and handing back the
obligations they owned is now a single indivisible act.

### Changes
- `crates/amux/src/agents/codex/session.rs`: `mark_disconnected(runtime,
  error)` replaces both functions; every caller resolves the request IDs it
  returns.

### Decisions Made
- The error message stays an `Option` parameter rather than an unconditional
  assignment, so stopping a degraded session cannot silently clear a recorded
  startup error.

### Verification
- `cargo fmt --all`; `timeout 600 cargo clippy --workspace --all-targets`
  (only the two accepted tracked-listener warnings).
- `timeout 600 cargo test --workspace` — pass; amux library 430/430.
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44/44.

### Next Steps
- P6, carrying the checkpoint-2 directives.

## 2026-08-13: Checkpoint #2 — delete unreached codex-sdk surface

### Summary
`codex-sdk` was copied in-tree with no upstream to sync against, so its
unreached API is amux's maintenance cost and nothing else. Removed every
surface with no product consumer and no path to one in P6-P8: the per-turn
`TurnStream`, the turn/review/compact/rollback/fork/archive/read/prompt/
list_models/read_config/exec_command families, and the auto-`ApprovalHandler`
path amux never uses (it always answers approvals manually).

### Changes
- Deleted `crates/codex-sdk/src/turn_stream.rs` and the `TurnSlot` receiver
  borrow it needed; `Thread::events()` is now the only event consumer.
- Trimmed `Codex` to the methods amux and the live probe actually call.
- Deleted `ApprovalHandler`/`AutoApprove` and the dispatch branch that
  answered approvals without a consumer.
- Removed the orphaned type blocks (model list, exec command, review,
  `ThreadReadResponse`, `ConfigReadParams`).

### Decisions Made
- Retarget rather than drop tests whose subject survives: the 512 KiB
  single-frame WebSocket regression now drives `thread/name/set`, the
  added-notification replay test drives `events()` + `start_turn`, and the
  `item/tool/requestUserInput` regression keeps its load-bearing half.
- Keep `list_threads`, `read_account`, and `take_notifications`: the first
  is exercised by the live probe and the envelope anchor, the other two have
  named jobs in the checkpoint-2 directives.

### Verification
- `cargo fmt --all`; `timeout 600 cargo clippy --workspace --all-targets`
  (only the two accepted tracked-listener warnings).
- `timeout 600 cargo test --workspace` — pass.
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44/44.
- Live: `amux new codex` + structured prompt + raw `terminal_v1` codex TUI +
  suspend/restart/resume + second prompt, all green against codex-cli 0.147.0.

### Next Steps
- P6 (amux-ui kernel generalization), carrying the checkpoint-2 directives.

## 2026-08-13: P5c — Codex raw TUI, suspension, and lifecycle recovery

### Summary
Codex agents now expose a lazy, shared `terminal_v1` PTY running
`codex resume` against the exact app-server socket used by structured ingest.
Their persistent thread identity survives amux suspend/restart, and supervised
app-server death is detected immediately and fed into the existing reconnect
and `thread/resume` path.

### Changes
- The first raw subscription spawns one TUI per Codex agent; later subscribers
  share its byte ring. A TUI that exits is forgotten and can be spawned again.
- `SuspendedAgent::Codex` persists the thread, daemon mode, creation settings,
  name, cwd, and creation time. Create and resume now share one `AgentDeps`
  bundle containing host-owned backend resources.
- Private fallback sockets live beside the configured amux socket and use a
  short stable hash of the configured path, avoiding cross-server contention.
- Supervised daemon exit proactively closes the SDK transport. Codex PTY stop
  explicitly terminates the whole PTY process group so the Node shim and native
  child cannot outlive the agent.
- Review round 1 forwards stored Codex policy overrides to the raw TUI, permits
  pre-attach resume identities to suspend, bounds fallback socket paths, and
  keeps the socket regression tests Unix-only.
- Review round 2 secures the long-path `/tmp/amux-<euid>` fallback with `lstat` ownership checks and private permissions before Codex can use it.
- Simplification pass: one `DaemonMode` match yields both the connection socket
  and the exit token, `suspended_state` reads attached state once, resume clones
  host deps once instead of per agent, and the private-socket and daemon-mode
  tests assert against the functions that own the behavior.

### Decisions Made
- Once spawned, a raw PTY remains alive with zero subscribers; Codex resume is
  cheap, but retaining the live TUI makes detach/reattach immediate. It is
  reaped only on TUI exit or agent stop.
- A pre-thread-id Codex agent remains honestly non-suspendable. Raw spawn errors
  are returned to the subscriber without withdrawing the structured session.
- The existing initial terminal size is sufficient for the Codex TUI; the
  pre-existing absence of later client-side resize messages remains unchanged.

### Verification
- Focused tests cover suspended-state serialization/reconstruction, configured
  socket isolation, raw pre-ready failure, supervised-process exit signaling,
  and PTY process-group termination.
- Live `codex-cli 0.147.0` smoke: CLI-created agent, structured turns, two raw
  subscribers receiving identical live byte streams, interactive raw driving
  observed by structured rows, suspend/server restart/resume, lazy raw respawn,
  and both existing and amux-supervised daemon kill/recovery.

---

## 2026-08-13: P5b simplification — one attach path, one attached-state accessor

Independent pass over P5b. The reconnect supervisor had three consecutive
`match … { Err(error) => set error, write reconnect row, back off, bump retry,
continue } }` arms — one each for connect, start/resume, and taking the event
stream — so the retry policy was written three times and could drift. They are
now one `attach_thread` helper returning the connection, thread, and stream, with
a single error arm in the loop. The helper takes `&mut Option<String>` for the
thread ID because that is the honest contract: it reads the ID to resume and
writes back a newly started one before the stream is taken, so a failure after
`thread/start` resumes that thread instead of orphaning it (the previous
statement ordering did the same thing implicitly).

Six sites open-coded `runtime.lock().unwrap_or_else(poison).attached.as_mut()`
to touch one field; five of them wanted nothing back and are now
`update_attached(runtime, |attached| …)`. `mark_disconnected`/`resolve_pending`
passed `PendingRequestKind` through only to discard it at the loop head, so the
drained pending table is now a plain `Vec<RequestId>`.

Verified rather than trusted, no changes needed: no lock is held across an
await on either the ingest or input path (`codex_input_target` drops the
registry read guard before `send`); the row ring's `write` broadcasts with
`try_send` and drops slow subscribers, so a subscriber can never stall the
ingest; every decoded `CodexSdkV1Input` writes exactly one `amux.input_result`;
the log source is created once per session and nothing calls `clear()`, so seqs
survive gaps and reconnects. The overflow/close signaling in `ThreadEventStream`
and `TurnStream` is also correct: `Notify::notified()` records the
`notify_waiters` count at creation, so building the future before checking the
channel state genuinely closes the wakeup race.

Gate: `cargo fmt --all`, workspace clippy (only the two known tracked-listener
dead-code warnings), `cargo test --workspace` (exit 0), and the 44-test testnet
spec.

---

## 2026-08-13: P5b — Codex structured stream, input, reconnect, and capture

### Summary
Completed the `codex_sdk_v1` structured plane end to end. Each Codex session now
continuously persists verbatim upstream method/params rows, accepts typed turn,
steer, interrupt, and approval inputs with correlated result rows, and resumes
its thread after transport loss or bounded-queue overflow.

### Changes
- The SDK exposes raw+typed `ThreadEvent`, a continuous `ThreadEventStream`,
  receiver-independent `start_turn`, fresh pre-RPC resume registration, and
  drain-before-overflow signaling. Optional raw wire recording tees both JSONL
  directions.
- `CodexRuntime` now nests attached/live state and owns ingest plus pending
  request state. The host connection cache discards closed transports, while a
  per-session supervisor retries resume with capped backoff and emits visible
  ready/gap/reconnect-error boundaries.
- Codex input has a dedicated backend-owned dispatch handle beside Claude's
  sequenced input seam. The retained row ring is deliberately 8192 entries.
- Added the opt-in `codex_capture` rig and a provenance-stamped backend replay
  fixture covering pong, allow, deny, file approval, interrupt, and history
  resume.
- Review round 1 stages unbounded history only while `thread/resume` is pending
  and always forwards explicit interrupt IDs to the daemon.

### Decisions Made
- User turns always call `turn/start`; steering is explicit and supplies the
  caller's `expectedTurnId`. Interrupt with no active turn succeeds as a no-op.
- Saturation is detected exactly by the SDK's poisoned registration. Buffered
  rows drain first, then amux emits `amux.codex_gap` and `thread/resume`s into a
  fresh registration. Sequence numbers never reset.
- A writer-lock conflict is a degraded read-only-until-retry state: the session
  stays registered, emits the upstream error, and retries resume.

### Verification
- Focused SDK (47 unit tests) and amux backend replay tests pass; clippy with
  `-D warnings` passes for both changed crates.
- Live `codex-cli 0.147.0` captures passed for PONG, command allow/deny,
  file-change approval, interrupt, history resume (`HISTORY_OK`), and forced
  SDK transport loss (`ready → gap → ready → RECONNECTED`).
- The full workspace test gate passes, and the testnet spec passes all 44 tests.

### Next Steps
- P5c can attach the lazy terminal plane to the nested live runtime. P7 should
  fold the opaque row vocabulary without changing backend tags.

## 2026-08-13: P5a simplification — speculative proto surface and remediation scar tissue

Independent pass over P5a after the review round. Deleted `CodexSdkV1Control`:
an empty message with no codec, no Rust reference, and no phase briefed to use
it — codex control has no use case in sight (steer/interrupt/approvals are all
`CodexSdkV1Input`), so the `codex_sdk_v1` control branch of `send_session_input`
now rejects control events the way Claude's does instead of promising P5b work
that nothing needs. Collapsed the remediation's `set_initial_thread_name` — a
generic higher-order function whose three type parameters existed only so a mock
closure could be injected — into one non-generic `rename_thread` helper shared by
startup and `set_local_name`; the two copies of "rename, warn, continue" are now
one, and the reviewer's actual fix (publish the thread handles before naming,
never propagate the naming error) is unchanged. Its unit test went with it: it
asserted that a closure it supplied returned `Err`, and could not have caught a
regression in the startup ordering it was written for.

In `session_rpc`, `SessionOutputReader::Structured` carried both an
`Option<Vec<u8>>` cursor and a `StructuredCodec` tag, so "Claude without a
cursor" and "Codex with one" were expressible; the cursor now lives in
`StructuredCodec::Claude { replay_cursor }`. Both unsupported-io_protocol errors
list `codex_sdk_v1` now that it dispatches. `daemon_mode` is `&'static str` end
to end instead of a `String` cloned per session, the start task's keepalive is
`wait_for` rather than a hand-rolled borrow/changed loop, and `agents::codex` is
`pub(crate)` like `agents::claude` (the codec surface is re-exported through
`amux::codex_io`, which is the only public path). Net −36 lines; no behavior
change beyond the two error spellings.

Not changed, recorded for later: `CodexRuntime` still expresses impossible
states (client/thread/thread_id/daemon_mode are set together and only the first
two are cleared on stop) — the tighter nesting costs more than it saves until
P5b adds the ingest handle and pending-approval table to the same cell.
`amux-ui`'s `agent_type_label` still falls through to `"test-agent"`, so codex
pending rows are mislabeled until P6 fixes it. The private-daemon fallback socket
comes from `config::default_socket_dir()` rather than the running server's
configured directory, so two differently-configured amux servers would contend
for one `codex.sock`.

### Verification
- `cargo fmt --all`, `cargo clippy --workspace --all-targets` (only the two
  pre-existing tracked-listener dead-code warnings), `cargo test --workspace`,
  and `cargo test -p amux --features testnet --test spec` (44 tests) all green.

## 2026-08-13: P5a — Codex protocol, session skeleton, and real thread create

### Summary
Added Codex as a third local agent backend. `amux new codex` now registers
immediately, then lazily establishes one shared app-server connection per amux
agent host and asynchronously starts or resumes a persistent Codex thread.

### Changes
- Added `codex.proto`, both create-oneof arms, and the public `codex_io` codec
  surface for `codex_sdk_v1` replay args and opaque JSON-row output.
- Added `CodexSession`, host-owned `CodexClient`, daemon-mode/thread-id debug
  metadata, the empty structured replay stream, CLI parsing, and capability
  advertisement.
- Changed the in-tree SDK's daemon/socket/raw-I/O connect APIs to require and
  honor `CodexConfig`; amux now identifies itself in `initialize`.
- Review round 1 gated the Codex backend to Unix, made initial naming non-fatal, and rejected Codex argv.

### Decisions Made
- Codex startup stays asynchronous behind synchronous `AgentBackend::start`, so
  create never waits for app-server availability and no Codex await occurs under
  the agent registry guard.
- Codex advertises `codex_sdk_v1` and `terminal_v1` unconditionally; structured
  ingest/input and lazy PTY creation remain P5b/P5c work.
- P5a retention is 1000 rows. P5b must resize it deliberately for delta-heavy
  traffic.

### Verification
- Full fmt, workspace clippy, workspace tests, and 44-test spec gate passed.
- Live smoke created and named two real threads through one shared connection,
  listed both amux agents, stopped the daemon cleanly, and verified persisted
  thread/name state with `thread/read`.

### Next Steps
- P5b adds continuous event ingest, typed input, loss handling, reconnect, and a
  larger deliberately chosen structured-row retention.

## 2026-08-13: P4 simplification — dead adopted surface and dispatch scar tissue

Independent pass over the adopted crates after both remediation rounds.
`ServerInner::handle_server_request` grew four near-identical
route-to-thread-or-error blocks across the rounds; they collapse into one
`deliver_or_error` helper, and the `item/tool/requestUserInput` pre-block is
gone entirely — once user input stopped being modeled as an approval it was
just the generic correlated-request path with a different error code. Response
correlation no longer maps a non-numeric id to pending request 0. `ensure_well_known`
no longer creates the socket parent twice (`resolve_socket_path` already does).
Deleted dead adopted surface: `Question`/`QuestionOption` (unreferenced since
user input became a raw correlated `ServerRequest`, and carrying a phantom
`#[serde(skip)] multi_select`), `ListThreadsParams::status` (a `#[serde(skip)]`
filter that never reached the wire), the two "legacy compatibility shim"
`ReviewTarget` variants that synthesized English review instructions inside the
SDK, and `replay-support`'s `spec`/`matcher` modules — a YAML scenario-DSL
runner with claude-sdk vocabulary (`permission_mode`, `max_budget_usd`) that
nothing in this workspace runs and that the guide explicitly declines to adopt.
That drops `serde`/`serde_yaml` from `replay-support`. `Error::ThreadNotFound`
is gone too — never constructed; unknown threads come back as RPC errors.
Behavior unchanged; the
P4 regression tests for turn filtering, overflow, string request ids, user-input
surfacing, and shared registrations all still pass unmodified.

## 2026-08-13: P4 — in-tree Codex SDK and daemon transport

Copied `codex-sdk` and `replay-support` from claude-sdk commit
`f935f6233e143524f9965fb730c956e00fdff5c9` as first-class workspace crates.
The SDK now matches the verified codex-cli 0.147.0 surface: initialize records
`codexHome` and platform fields, `thread/list` consumes the `data` envelope,
`account/read` is typed, the four added notification families are parsed, dead
v1 approval aliases are gone, and `item/tool/call` is surfaced with correlated
typed responses or an explicit JSON-RPC error. Child stderr is forwarded to
tracing with a 16 KiB per-line cap. Per-thread delivery is non-blocking so the
socket reader always drains independently of consumers.

Added WebSocket-over-UDS transport plus the amended daemon lifecycle: real
handshake probing, managed-daemon fall-through, supervised well-known server,
stale-socket-only removal, resolved socket parents, conservative UDS path
validation, optional private fallback, and process-group shutdown for the npm
shim/native child tree. `tokio-tungstenite` handles framing and handshake;
`tokio-util::CancellationToken` remains the shared shutdown primitive. Three
provenance-stamped replay smokes and opt-in/local transport tests cover the new
surface. The real 0.147.0 probe passed in `Spawned` mode through initialize and
`thread/list`; explicit shutdown left no spawned app-server process. Workspace
clippy and the 44-test testnet spec gate pass; the full workspace test gate is
green (with the two pre-existing test-only tracked-listener warnings).
Review round 1 remediated all nine accepted lifecycle, protocol-shape, turn-stream,
and replay-controller findings against a freshly generated codex-cli 0.147.0 schema.
Review round 2 remediated all eight final queue-delivery, child-lifecycle, sandbox-serde,
WebSocket-framing, request-routing, turn-correlation, registration, and UDS-path findings.

## 2026-08-13: Checkpoint #1 — P1–P3 seam audit

Fable checkpoint between the refactor phases and the codex build-out.
Verdicts on the queued items: `StructuredLogSource` stays (it narrows the
generic `BroadcastBuffer` surface to the sink contract at the backend seam —
not a pure pass-through); per-agent retention consts stay per-agent
(per-protocol policy, deliberately not centralized); `TerminalSize` stays
declared in `claude.proto` (amux.proto imports claude.proto permanently for
`ClaudeCreateConfig`, so a third proto file would add a file without removing
a dependency); `to_agent` stays a provided trait method; the hook-under-
registry-write-guard exception in `host.rs` stays (Claude-only exposure,
bounded awaits — but it sets the anti-pattern P5a must not repeat with the
shared codex client). One deletion applied: the dead client half of the
terminal_v1 resize control (`encode_terminal_v1_control` and the public
`TerminalV1Control` re-export had zero in-repo callers and zero tests); the
wire message, server decode, and pty resize dispatch remain. Full gate green.

---

## 2026-08-13: P3 — trait-dispatched agent backends

Replaced the `AgentSession` enum and its repeated Claude/test-agent match arms
with an object-safe `AgentBackend` trait stored as `Box<dyn AgentBackend>`.
Claude and test-agent now implement their instance behavior in their own
modules; shared code contains only the new-agent and suspended-state factories,
plus the explicitly Claude-owned external-hook bootstrap. Structured input is
an optional backend-neutral owned handle, captured under the session-registry
lock and awaited after the lock is released; Claude retains its sequence guard.
The shared terminal protocol advertisement remains centralized. Formatting,
workspace clippy/tests, and the 44-test testnet spec gate pass with no fixture,
golden, spec, protocol, UI, or TUI source changes.

Follow-up (simplification): `AgentBackend::io_protocols` lost its default body
(both backends override it, so the default was never reached) and now documents
`terminal_io_protocols` as the shared piece to build on; Claude's structured
input target folded its single-caller `send_structured_input` into the
`StructuredInput::send` impl and dropped the `Clone` derive the deleted
`StructuredInputTarget` enum had required; the `agents::session` module doc now
describes the trait/handle/factories it actually holds instead of PTY plumbing
that lives in `agents::pty`. No behavior change.

---

## 2026-08-12: P2 — agent-agnostic structured log sink

Split `StructuredLogSource` into a retained, sequenced sink with caller-selected
retention and a Claude-owned `TranscriptIngest` that now owns transcript path,
tailer, relink/clear, shutdown, and debug serialization. Claude and test-agent
sessions each select the existing 1000-entry policy; generic PTY spawning no
longer creates structured state, while concrete session exit wrappers preserve
automatic sink cleanup and ensure Claude stops its tailer before closing the
sink. The existing transcript marker, replay, same-path no-op, relink-clear,
and sequence behavior are unchanged. Formatting, workspace clippy/tests, and
the 44-test testnet spec gate pass with no fixture, golden, or spec changes.

Follow-up (orchestrator): untracked the two phase reports that had
been force-added under gitignored `notes/` against repo convention;
reports stay on disk, untracked.

Follow-up (simplification): dropped the test agent's vestigial
`"transcript": {}` debug field for an honest `has_structured_log` bool
(a test agent has no transcript), refreshed the `agents::session`
module doc that still claimed `spawn_pty_agent` returns a
`StructuredLogSource`, and restored the std/external import break in
`claude/transcript.rs`. No production behavior change.

---

## 2026-08-12: P1 — agent-independent `terminal_v1` byte plane

### Summary
Renamed the raw PTY byte-plane protocol from the Claude-owned
`claude_raw_v1` surface to core-owned `terminal_v1` with no compatibility
alias: the protobuf payloads moved from `claude.proto` into `amux.proto` as
`TerminalV1Args`, `TerminalV1ReplayQuery`, and `TerminalV1Control`; their Rust
codec moved from `agents/claude/io.rs` to `agents/terminal_io.rs` and is
publicly exposed through `amux::terminal_io`; PTY advertisement, server
dispatch, CLI attach, capture support, and UI/TUI test builders now use the new
name, while `claude_io` retains only `claude_pty_transcript_v1`. Formatting,
workspace clippy/tests, and the testnet spec suite pass without fixture,
snapshot, or assertion changes.
Follow-up (orchestrator): docs/UI.md updated — raw attach is now
agent-independent via `terminal_v1`, the core-prerequisite caveat and the
deferred-decision clause it satisfied are retired.
Follow-up (simplification): the generic `decode_optional_args` helper in
`agents/claude/io.rs` lost its second caller in the move and is inlined into
`decode_pty_transcript_v1_args`, so both codec modules decode their own args
the same way; identical error text, no behavior change.

---

## 2026-08-12: Chat V1 — retrospective; docs/CHAT.md flips to implemented

### Summary
The chat V1 milestone is complete: eight phases (0–7) from the
transcript-persistence bug to the live-verified H suite, ~35 commits,
executed as an orchestrated overnight-and-morning run (Claude
subagents for Phases 0–6, codex/gpt-5.6-sol implementing from Phase
6's remediation onward, Claude simplification passes throughout,
codex reviews at every gate). docs/CHAT.md's status flips to
implemented; its executable half is live (amux-ui chat spec
chapters 123, amux-tui goldens 54+21, capture_unit 10, the opt-in H
suite 11/11 on claude 2.1.228). Also committed here: the stale_seq
and subscriptions new-scenario fixtures the Phase 7 graduation
produced (new anchors, leak-checked; the graduation-policy revert
had left them untracked).

### The numbers
- 7 codex reviews, 32 findings, all triaged fix-with-locking-test:
  3 P1 (fixture privacy leak scrubbed pre-push; late-attach phases
  stuck Replaying forever; ask answers could bind past the queue
  head and approve the wrong permission), 29 P2.
- Real bugs found beyond the chat: transcript persistence starved by
  inherited CLAUDE_CODE_CHILD_SESSION markers (fixed at the spawn
  seam); every hook event delivered twice (legacy user-scope
  registration beside the plugin; deduped at the daemon seam); two
  live fleet-attention bugs (tool-denial stuck Working; plan
  notifications misclassified by wording heuristics).
- Spec-first discipline held: every fixture-contradicted rule in
  docs/CHAT.md was amended at the phase gate with evidence tags
  (Phase 0–3 corrections; wheel-scroll deferral), and the doc's
  Deferred/Rejected sections carry the full decision record.

### Next
- Dogfooding is the real gate now: `ui.default_open_mode = chat`
  (or Ctrl+Enter/`o` from the fleet) drives Claude sessions through
  the structured chat end-to-end.
- Owner actions still open: remove the legacy `amux-dev hooks
  claude` entry from ~/.claude/settings.json; re-add the plugin
  marketplace path (`claude plugin marketplace add
  ~/.local/share/amux/claude-marketplace`).
- Push when ready — main is many commits ahead of origin; nothing
  has been pushed all run.

---

## 2026-08-12: Phase 7 — simplification pass

### Summary
Simplification-only pass over the Phase 7 H-suite code (gate step 8). No
scenario behavior changed: every keystroke program, prompt, timeout, and
assertion is byte-identical. The remediation rounds had left bolt-on
seams; each is now one mechanism. `plan()` reuses the resolution id its
own waiter returns (as `plan_auto` already did) instead of re-deriving it
with a second full-capture scan, and its thrice-duplicated "new ask after
rejection" predicate is a single named closure. `wait_for_plan_resolution`
reads the matched row back by index (rows are append-only) instead of
deep-cloning the capture and re-running the offline finder — the finder
moved next to its only remaining callers, the capture_unit offline-waiter
tests. Row-walking now goes through one seam, `structure::message_blocks`.

### Changes
- `crates/amux/tests/capture/main.rs` — `latest_permission_suggestions`
  (4 sites), `bracketed_paste` (3 sites), `probe()` scenario-table
  constructor, `pong` uses `wait_for_turn_duration`, tooling `graduate`
  no longer verifies twice (graduate re-verifies internally).
- `crates/amux/tests/capture/harness.rs` — `Row::is_turn_duration` named
  fact (5 sites), block probes via `Row::blocks()`.
- `crates/amux/tests/capture/structure.rs` — `message_blocks` seam;
  `plan_resolution` finds the first tool_result block once instead of
  via `tool_result_id` plus a re-find (fn now folded in);
  `find_plan_resolution` moved to `capture_unit.rs`.

### Verification
All under `timeout 600`: capture_unit 10/10, amux --lib 404, testnet spec
44/44, amux-ui 30+123, amux-tui 109+54+21, amux-cli 53, capture binary
no-scenario path exits 0. Workspace clippy `--all-targets` with `testnet`
and `-D warnings` clean; `cargo fmt` clean. No live captures run.

## 2026-08-12: Chat V1 Phase 7 — codex review fixes

### Summary
Closed the final three Phase 7 review findings. Canceled capture
scenarios now abort and join both recorder tasks and delete their Claude
agent before finalization or a later scenario can run; H.7 waits for the
advanced stream cursor before issuing its retry; and the reduced waiter
captures no longer retain private tool-inventory counts. The waiter
fixture directory now passes the same fail-loud redaction verifier as
graduated fixtures.

### Changes
- `crates/amux/tests/capture/{main,harness}.rs` — scenario-scoped active
  session cleanup, recorder liveness guard, and H.7 cursor ordering wait.
- `crates/amux/tests/capture_unit.rs` — offline cancellation coverage and
  directory-wide waiter-fixture redaction verification (10 tests total).
- `crates/amux/tests/fixtures/capture-waiter/phase7-fixes2-*.rows.jsonl`
  — neutralized `total_deferred_tools` to zero.

### Decisions Made
- Register cleanup immediately after agent creation, before either stream
  subscription, so cancellation during partial session startup is covered
  too. Cleanup errors stop the run rather than allowing a possibly-live
  agent into the next scenario.
- Treat the daemon sequence and reducer cursor as separate authorities in
  H.7: retry only after the reducer has folded through `advanced_seq`.

### Verification
- `cargo fmt --all -- --check`; workspace clippy with all targets,
  `testnet`, and `-D warnings`; capture harness compile/opt-in skip; and
  `capture_unit` 10/10 — green under `timeout 600`.
- Sandbox-runnable tests green: amux 396, amux-ui 30 + spec 123,
  amux-tui 109 + 54 + 21, amux-cli 51. Full invocations were also attempted:
  the sandbox denied socket binding for 8 amux startup tests, the amux-ui
  runtime test, 2 amux-cli attach tests, all 44 testnet specs, and 13/14 E2E
  scenarios (`EPERM`, with downstream PTY `EIO`); E2E `bare_help` passed.
  No live-Claude legs were run, per the remediation brief.

## 2026-08-12: Chat V1 Phase 7 — the real-Claude E2E leg (H suite)

### Summary
The capture harness is formalized into the maintained opt-in H
suite, H.1–H.9 per docs/CHAT.md (implementation by codex/
gpt-5.6-sol; live legs run by the orchestrator — codex's sandbox
cannot reach the network or bind sockets). Live result: 11/11
scenarios PASS on claude 2.1.228, including the previously-parked
H.7 stale-seq race (typed SequenceNumberMismatch surfaces as a
resurfaced ask, never a crash) and H.9 read-only observation of a
hook-discovered external session built entirely on scratch state.
Two remediation rounds: order-sensitive raw-text JSON extraction
replaced with parsed-structure waiters, which then gained OFFLINE
unit tests driven by redacted rows from the actual failing captures
— waiter-vs-recorder disagreement is now catchable in-sandbox; and
plan_reject now asserts the revise-and-re-ask rule instead of
waiting for a turn end the spec says never comes. Redaction verify
and taxonomy drift tooling ship with the suite (drift is data: the
sonnet fallback legs revealed new row fields — usage.iterations,
stop_details, effort — recorded, not failed). Committed fixtures
were deliberately NOT rolled forward: they are the Tier-1 chapters'
regression anchors (a trial wholesale refresh broke 30 spec tests
and was reverted; graduation policy recorded in the orchestration
notes).

### Changes
- crates/amux/tests/capture/{main,harness,redact}.rs — the H suite,
  scenario grammar dedup, workspace-rooted AMUX_CAPTURE_OUT,
  parsed-structure waiters + offline waiter tests (capture_unit 8).
- crates/amux/tests/capture/graduate.{rs,sh} — the
  recording→redaction→fixture graduation tooling.
- crates/amux-ui/src/claude/mod.rs — minor surface the suite needed.

### Verification
- Live H suite 11/11; redaction verify + drift tooling green; fmt,
  workspace clippy `-D warnings` (testnet), amux-ui 30+1+123, amux
  --lib 404, spec 44, capture_unit 8, amux-tui 109+54+21, amux-cli
  53 — all green with fixtures at their committed anchors.

---

## 2026-08-12: Phase 6 simplification pass

### Summary
Render-neutral cleanups over the Phase 6 diff. Derived state:
`ViewState.leader_label` dropped — the label always was
`C-<leader>`, and that rule now lives once in
`bindings::Effective::new` (both help overlays derive through it).
One message: the armed-quit footer text moves onto
`QuitGuard::HINT`; the fleet status line and the chat's
`armed_quit_line` both pull from it (the chat footer's armed branch
now reuses `armed_quit_line` instead of hand-building the same
row). Clarity: `focused_field` loses its `Option<bool>`
intermediate — `ask_head` borrows only the Model, so the derivation
reads as one match. Surface: `kitty_active` and `mod bindings` are
`pub(crate)` — nothing outside the crate exercises them. Left
alone, deliberately: `fleet_agent_count`/`agent_count` now share a
body but state different contracts (fleet-visible vs. Model
entities — the spec asserts both); `note_clear`/`disarm` share a
body but name distinct guard transitions; `PanelContext` earns its
keep against clippy's arg-count line.

### Changes
- crates/amux-tui/src/{view,run,render,lib,terminal,bindings}.rs
- crates/amux-tui/src/chat/{keys,render}.rs

### Verification
- fmt; workspace clippy `-D warnings` (testnet); amux-tui
  109+54+21; amux-ui 30+1+123; amux --lib 404; spec 44;
  amux-cli 53; e2e 14/14. Goldens untouched — byte-identical.

---

## 2026-08-12: Phase 6 gate — wheel-scroll deferral in the spec

### Summary
docs/CHAT.md absorbs Phase 6's drift: wheel scrolling moves to
Deferred decisions with the conflict stated — alternate-scroll mode
delivers wheel motion as arrow keys indistinguishable from the
keyboard's, the composer owns arrows for line motion, and branching
on focus would put meaning on invisible state (P3); PgUp/PgDn is the
guaranteed path and mouse capture stays rejected. The working
wireframe also drops its `? help` hint beside a kept draft (`?`
types into non-empty drafts, so the hint would lie). Gate context:
codex review returned three P2s on the help/guard rendering — the
help goldens had locked an off-by-one — all fixed in `b1df217`.

### Changes
- docs/CHAT.md — wheel row → deferred; deferred-decisions entry;
  working-wireframe footer hint removed. Wireframes remain 80 cols.

---

## 2026-08-12: Chat V1 Phase 6 — codex review fixes

### Summary
Three review findings fixed (implementation by codex/gpt-5.6-sol —
the first codex-implemented chunk; committed by the orchestrator
after running the two suites codex's sandbox cannot: loopback binds
are denied there). Help overlay geometry: the six-row chrome was
budgeted as seven, leaving every help frame one row short with the
bottom border early — the two help goldens had locked the defect and
are regenerated; the never-panics sweep now asserts help-open frames
fill the viewport. Paste while the help overlay is open no longer
falls through to the hidden composer (overlay owns focus; paste
drops, same rule as read-only). Arming the quit guard at widths
where the ask-panel hint wraps now replaces the complete hint range,
not just the last continuation row (new narrow-width golden).

### Changes
- crates/amux-tui/src/chat/{render,keys,panel}.rs; goldens
  regenerated (2) + added (chat_quit_armed_panel_narrow).

### Verification
- fmt, workspace clippy `-D warnings` (testnet), amux-tui
  109+54+21, amux-ui 30+1+123, amux-cli 53 (codex, in-sandbox);
  amux --lib 404 and spec suite 44 (orchestrator, outside the
  sandbox — loopback-bind EPERM in codex's sandbox).

---

## 2026-08-12: Chat V1 Phase 6 — gate + phase report

### Summary
The full Phase 6 gate chain, in order, all green: fmt; workspace
clippy `-D warnings` (`--features amux/testnet`) — one finding fixed
(`items_after_test_module`: the QuitGuard tests moved below
`ViewState::clamp_selection` in view.rs); amux-tui 108 lib + 53 chat
golden + 21 fleet golden; amux-ui 30+1+123; amux --lib 404; amux spec
(testnet) 44; amux-cli 53; e2e-runner **14/14** (no leg asserted the
old single-press Ctrl+C — the legs drive CLI surfaces, not the TUI's
keys, so the deliberate contract change touched none). Wheel scroll is
recorded as the phase's one deferral, with a CHAT.md drift note for
the orchestrator: alternate-scroll (wheel→arrows) cannot honor the
table's "wheel scrolls the feed" while the composer owns arrows and
↑-at-top is reserved for history recall — no code shipped either way.
Phase report: `notes/chat-v1/phases/06-report.md`.

---

## 2026-08-12: Chat V1 Phase 6 — the one binding table + `?` overlay

### Summary
Discoverability's two layers close (`docs/CHAT.md` §Keybindings):
`crates/amux-tui/src/bindings.rs` is the one named binding table —
sections of typed rows `{keys, action, tier}` per context, palette-ready
data — and everything discoverable derives from it: the fleet help
overlay (rebuilt from `fleet_sections`; entry rows name the EFFECTIVE
modes from the A1 default, the ctrl+enter row exists only when the kitty
probe succeeded, the leader label substitutes into chords, the guard row
states two-press quit while `q` stays single-press) and the new chat `?`
overlay (`chat_sections`: chat/composer/ask/reader/read-only groups —
the full effective key list; ext rows marked `terminal-dependent`, kitty
rows hidden when undelivered). `?` opens the overlay from the composer
with an EMPTY draft only — with anything typed it types (P2) — and any
other key closes it; the leader chords and the Ctrl+C guard compose over
it like everywhere else in the chrome (no field is focused under the
overlay, so ^C arms). The footer's `? help` hint appears exactly when
the branch is live (empty draft), matching the wireframes' idle frame;
on short viewports the overlay's tail gives way behind an honest `⋮`
row. Dispatch stays in the key handlers — deriving dispatch from the
table too is palette-era work, not V1.

### Changes
- `bindings.rs` (new): Tier/Binding/Section/Effective +
  `fleet_sections`/`chat_sections`; unit tests for kitty-row gating,
  effective-mode naming, leader substitution, ext annotation.
- `render.rs`: `help_lines` rebuilt from the table; `tier_mark`.
- `chat/mod.rs`: `ChatView.{kitty, help}` (view-config copy + overlay
  flag); `chat/keys.rs`: `?` branch, any-key close, guard/leader
  precedence; `chat/render.rs`: `help_frame` + the `? help` footer hint
  derivation (`help_hinted`).
- Goldens: new `chat_help_overlay` (80x46, full list, plain terminal),
  `chat_help_overlay_kitty_short` (kitty row + the honest `⋮` cut);
  regenerated for the truthful `? help` hint on empty-draft footers
  (chat_idle, chat_empty, chat_loading, chat_markdown, chat_echo_sending,
  chat_entries_edge, chat_tools_edge, chat_truncated_top + the idle/
  markdown style maps) and for the rebuilt fleet help (help_overlay, now
  at 68x21 so the attach section fits). Frames with non-empty drafts
  (chat_working, chat_scrolled_back, chat_composer_multiline) and every
  panel/reader frame stayed byte-identical.

### Verification
- `timeout 600 cargo test -p amux-tui` — lib (incl. bindings tests) +
  chat goldens + fleet goldens all green.

---

## 2026-08-12: Chat V1 Phase 6 — fleet entry + leader chords in chat

### Summary
Chat is reachable (A1/A3): the fleet's Enter opens the settings-default
mode (`ui.default_open_mode`, shipped raw attach), Ctrl+Enter opens the
other one where the kitty probe says the terminal can deliver it, and
`o` is the guaranteed plain fallback ("open in the other mode") in
Normal mode — in Filter mode `o` types (P2), Enter/Ctrl+Enter work.
Read-only agents open in chat from EVERY entry key: raw attach is
absent, not disabled (A3). An offline host refuses both modes with the
dial error (chat needs the host's stream as attach needs its PTY). The
new `UiAction::OpenChat` stays inside the chrome — no terminal handoff
— and the run loop calls `runtime.note_attached` (Phase 5's note), so
the subscription policy widens exactly as for raw attach and read-only
feeds light up through `UserAttached`. Leader chords work from chat —
`<leader> s` fleet, `<leader> d` shell — from every focus including
read-only; the pending chord composes BEFORE every other binding
(Ctrl+C included, matching raw attach where the leader is the one byte
passthrough does not forward) and consumes unrecognized chord keys, so
`<leader> x` can never interrupt and no chord key leaks into a draft.
`TuiConfig` now carries `leader: char` + `default_open_mode` (label
derived); the filter hint says `enter open` (truthful for either
default).

### Changes
- `view.rs`: `OpenMode` (+`other()`), `UiAction::OpenChat`,
  `ViewState.{leader,default_open_mode}`; `open_chat` passes the leader.
- `keys.rs`: `attach_selected` → `open_selected(other_mode)` with the
  A1/A3 resolution; Enter/Ctrl+Enter/`o` bindings; entry-resolution
  tests (default raw, default chat, readonly, filter mode, offline).
- `chat/mod.rs` + `chat/keys.rs`: `ChatView.{leader,pending_leader}`;
  chord interception + tests (every focus, unrecognized-chord
  consumption, configured non-default leader).
- `run.rs`: `TuiConfig` reshape; OpenChat handling with note_attached
  + immediate reconcile.
- `amux-cli/src/ui.rs`: maps `config.ui.default_open_mode` and the
  leader char into `TuiConfig`.
- Golden regenerated: `picker_filtered` (filter hint `enter attach` →
  `enter open` — the only cell change). Chat goldens byte-identical.

### Verification
- `timeout 600 cargo test -p amux-tui` — 103 lib + 51 chat golden + 21
  fleet golden; `-p amux-cli` — 53.

---

## 2026-08-12: Chat V1 Phase 6 — kitty detection + Shift+Enter

### Summary
The kitty keyboard protocol is feature-detected inside the terminal-
guard lifecycle: `TerminalGuard::enter` probes once per process
(crossterm's `supports_keyboard_enhancement` — the CSI ? u / DA1 query,
needing raw mode) and, when answered, pushes
`DISAMBIGUATE_ESCAPE_CODES` each session so Ctrl+Enter (fleet entry,
next chunk) and Shift+Enter (composer newline sugar) arrive as distinct
events. Every restore path pops before leaving the alternate screen
(kitty keeps per-screen flag stacks): the orderly path guards the pop
on having pushed (legacy Windows consoles never see the CSI); the
async-signal-safe handler stays deliberately unconditional —
`RESTORE_BYTES` now leads with `CSI < 1 u`, and the lockstep test
asserts pop + `write_restore` byte-for-byte. `ViewState.kitty` carries
the probe result as view-config, feeding the tier gate for hints and
the `?` overlay; dispatch itself trusts delivered events (a plain
terminal cannot produce Enter+SHIFT). Shift+Enter in the composer
inserts a newline, never sends; Ctrl+J stays canonical.

### Verification
- `timeout 600 cargo test -p amux-tui --lib` — 95 (incl. the updated
  restore-bytes lockstep and the Shift+Enter newline test).
- crossterm 0.28.1 parses kitty CSI u back to the same
  KeyCode/modifier shapes the handlers already match (Tab+SHIFT →
  BackTab; Esc; Ctrl+C), so no legacy binding moves under the flags.

---

## 2026-08-12: Chat V1 Phase 6 — chrome-wide guarded Ctrl+C

### Summary
The behavior change this phase owns (`docs/CHAT.md` §Keybindings,
derivation `notes/chat-v1/keybindings.md` §2.1): Ctrl+C is ONE rule
everywhere in the TUI. A focused non-empty text field is cleared —
as a yankable kill in the chat's Composer-backed fields; the fleet's
filter/rename line-edits clear without a kill slot (they are bare
Strings; recorded) — and the clearing press never arms. Otherwise the
press arms `QuitGuard` (the footer hint line becomes `press ctrl+c
again to quit` in warning color) and a second press within 3 s quits;
any other key, paste, or the timeout disarms; a stale arm re-arms
instead of quitting. This REPLACES the fleet's single-press Ctrl+C
quit; `q` keeps single-press (a deliberately typed letter, not a
reflex — recorded in keys.rs where they meet). Raw attach passthrough
is untouched. The chat's three Phase 4/5 stubs (composer empty-draft
branch, panel-field empty branch, read-only chats) are filled by one
interception at the top of `handle_chat_key`, before any panel or
reader sees the key — so ^C can never answer, deny, or interrupt.

### Changes
- `view.rs`: `QuitGuard` (press/note_clear/disarm/is_armed/expire,
  WINDOW_SECS=3) + unit tests; `ViewState.quit_guard`.
- `keys.rs`: guarded branch replaces single-press quit; filter/rename
  clear; `now` param; tests rewritten for the two-press contract.
- `chat/mod.rs`: `ChatView.quit_guard`. `chat/keys.rs`: top-level ^C
  interception over a new `focused_field` derivation (mirrors
  key/paste focus routing: read-only → none, interactive ask head →
  its open text stage, plans reader/PENDING → none, else composer);
  `now` param; paste disarms; composer/field_key ^C arms removed.
- `run.rs`: handlers get `Utc::now()`; the tick gate extension —
  `QuitGuard::expire` checked only while armed, disarm owes a repaint.
- Rendering: fleet status line early-return (⚠ + warning); chat
  `footer_line` armed branch (mode segment kept); every other bottom
  block (panel hints, read-only footer, reader tail) swaps its hint
  row for `armed_quit_line` — the hint row IS the footer hint line in
  those shapes, and row counts are unchanged so scroll metrics agree.
- Goldens: `fleet_quit_armed`, `chat_quit_armed`,
  `chat_quit_armed_panel`. Warning color is named-ANSI yellow in both
  themes (theme-independent), so single-theme frames lock it.

### Verification
- `timeout 600 cargo test -p amux-tui` — 94 lib + 51 chat golden + 21
  fleet golden; all pre-existing goldens byte-identical (the armed
  line renders only while armed, and nothing arms in old fixtures).

---

## 2026-08-12: Chat V1 Phase 6 — readonly agents surface in the fleet (A3)

### Summary
`Model::fleet()` no longer hides readonly agents: A3 requires they exist
in the fleet and open in chat only, and Phase 5 shipped the read-only
chat that renders them. They follow the fleet's existing row idioms —
ranked by the same attention/recency rules, normal columns — with
`read-only` as their resting status word (an inventory fact, more
informative than `idle`/`–`); live attention words still take
precedence, so a captured session that needs its owner shows
`permission`/`question` like any row. The eager-subscription skip is
unchanged: surfacing a resting row buys no stream; opening one still
subscribes deliberately via `Msg::UserAttached`.

### Changes
- `crates/amux-ui/src/model.rs`: `fleet()`/`fleet_agent_count()` drop the
  readonly filter; `status_label` states `read-only` for readonly cards
  at Idle/Unknown.
- `crates/amux-ui/tests/spec/inventory.rs`: the hidden-from-fleet chapter
  becomes `readonly_agents_surface_in_the_fleet_without_an_eager_stream`
  (sequence renamed `inventory::readonly`).
- `crates/amux-tui/tests/golden.rs` + `tests/golden/fleet_readonly_row.txt`:
  new golden — a readonly row among the canonical fleet.

### Verification
- `timeout 600 cargo test -p amux-ui` green (spec 123).
- `timeout 600 cargo test -p amux-tui` green; every pre-existing golden
  byte-identical (no fixture contains a readonly agent).

---

## 2026-08-12: Chat V1 Phase 6 — default-open-mode setting (A1)

### Summary
The chat entry mode setting lands in the usual amux config
(`docs/CHAT.md` A1, owner directive "Config home"): `ui.
default_open_mode: raw | chat` in `config.yaml`, following the
`keybinds.leader` idiom — a serde-defaulted section struct with
`deny_unknown_fields`. The shipped default is `raw`; `chat` flips what
the fleet's Enter opens (the non-default mode stays reachable via
Ctrl+Enter/`o`, wired in the next chunks). Mobile clients are always
structured and read no such setting.

### Changes
- `crates/amux/src/config.rs`: `OpenMode` (raw|chat, Default=Raw),
  `UiSettings { default_open_mode }`, `Config.ui` field; tests for the
  shipped default, the YAML round-trip, and unknown-variant rejection.
- `crates/amux/src/lib.rs`: export `OpenMode`, `UiSettings`.

### Verification
- `timeout 600 cargo test -p amux --lib config::` — 25 passed.

---

## 2026-08-12: Phase 5 — simplification pass

### Summary
Behavior-preserving cleanup of the Phase 5 surfaces (orchestration step
8); all goldens byte-identical, no test assertion touched. The main
altitude fix: the ask panels' text fields and the main composer now
share ONE readline dispatch — `composer::readline_key` (motion, kills,
yank, printable insertion) — instead of two parallel key matches;
`composer_key` and `ask_ui::field_key` keep only their own layers
(multiline row motion / send / scroll vs. the consume-everything
one-line frame). Repeated idioms got one home each: `ReaderView::ask()`
for the four open-on-ask literals, `ChatView::ask_reader_open` for the
five source-match checks, `AskUi::menu_cursor` for the three cursor
extractions, and `render::push_right` for the three right-aligned
annotation computations (`(1 of N)`, the reader's position indicator).
`sync_ask` restructured from the `head_exists` boolean block into a
let-else early return so the no-head / new-head / in-flight rules read
in order; `reader_tail` filters the panel state by ask id once, killing
the `expect("checked above")`. Premature abstraction removed: panel.rs's
single-caller `StyleKind` enum folded into `answer_summary(theme)`
returning styles directly. Dead pub surface trimmed: `diff::
magnitude_text` and `diff::new_file_rows` drop to `pub(crate)`
(`reader_rows` stays pub — the golden suite drives it directly). Net
-91 lines.

### Verification
- `cargo fmt`; workspace clippy `--all-targets --features amux/testnet
  -D warnings` clean.
- `timeout 600 cargo test -p amux-tui` (goldens unchanged on disk),
  `-p amux-ui`, `-p amux --lib`, `-p amux --features testnet --test
  spec`, `-p amux-cli` — all green.

---

## 2026-08-12: Chat V1 Phase 5 — codex review fixes

### Summary
Five accepted findings from the phase's codex review, each fixed with a
covering test. (1) The ask-time diff no longer discards `similar`'s
missing-newline fact: a ± row lacking its final newline is followed by
the jsdiff/git `\ No newline at end of file` marker row (rendered dim
verbatim via the existing unknown-prefix path, no line number), so an
edit differing only by a final newline no longer shows visually
identical -/+ rows; Equal rows are exempt — an unchanged EOF missing
its newline on both sides states no difference the approval needs.
(2) Reader scroll metrics now compute bounds from the SAME tail
derivation the frame renders (`reader_tail`, extracted; one layout, two
consumers) instead of assuming the writable action-row tail — End/PgDn
in a resolved-plan or read-only reader land exactly on the render clamp
and the next Up moves immediately. (3) A synchronous refusal of an
answer submitted from the reader now closes the reader to the docked
panel, where the stated failure renders — the same drop an async
SendFailed takes, so no lost outcome hides behind the overlay.
(4) Paste routes on the model/focus state like keys do: read-only chats
drop it (no composer surface exists to retain it invisibly), and the
pre-reconciliation pending-ask window is closed by the same defensive
`sync_ask` — a docked menu stage drops the paste instead of feeding the
hidden composer. (5) The Other field uses the encoder's trimmed
emptiness rule everywhere (`ask_ui::other_present`): whitespace-only
text neither counts as answered, nor checks a box, nor rides a
response — no avoidable synchronous refusal, no lying review tab.

### Changes
- `crates/amux-ui/src/claude/artifact.rs`: missing-newline marker rows;
  test `a_newline_only_edit_states_the_missing_newline` (both
  directions).
- `crates/amux-tui/src/chat/reader.rs`: `reader_tail` extraction;
  `scroll_metrics` uses it (+ readonly derivation).
- `crates/amux-tui/src/chat/mod.rs`: reconcile closes an Ask-source
  reader when a sync refusal is stated.
- `crates/amux-tui/src/chat/keys.rs`: `handle_chat_paste(chat, model,
  text)` — read-only drop + sync-first routing; tests
  `paste_in_a_readonly_chat_retains_nothing`,
  `paste_with_a_pending_ask_before_reconcile_retains_nothing`,
  `end_then_up_moves_immediately_in_a_resolved_reader`,
  `a_refused_answer_from_the_reader_surfaces_in_the_docked_panel`
  (frame-asserted), `whitespace_only_other_stays_unanswered`.
- `crates/amux-tui/src/chat/{ask_ui,panel}.rs`: `other_present`
  (trimmed) replaces raw emptiness in answered/commit/toggle/display.
- `crates/amux-tui/src/run.rs`: paste call site passes the model.
- Golden: `chat_ask_permission_newline` (new); the unverified-shape
  fixture gained trailing newlines so its refusal stays the subject;
  every other golden byte-identical.

### Verification
- fmt clean; workspace clippy `-D warnings` (`--features
  amux/testnet`) clean.
- amux-tui 83 lib + 49 chat golden + 19 fleet golden; amux-ui 30 lib
  + 1 runtime + 123 spec; amux --lib 401; amux spec (testnet) 44;
  amux-cli 53 — all under `timeout 600`.

---

## 2026-08-12: Chat V1 Phase 5 — docked ask panels, the reader, read-only chat

### Summary
The chat's interactive surface: every ask form, the fullscreen reader,
and the read-only variant. A pending ask takes over the composer area
behind a dim rule (C1) — head-of-queue with an honest `(1 of N)`, the
draft untouched beneath (D1), not dismissible while pending: Esc steps
back stages and floors at the menu. The permission panel (C2) renders
tool identity + magnitude, the ask-time numberless mini-diff (≤8
screen rows, wraps counted, leading context dropped to one when tight,
`⋮ +K more lines · f full diff`), the Write `+` block with `(N
lines)`, `$ command` for Bash, a compact fallback otherwise; option
2's label derives from the hook's suggestion facts; unverified menu
shapes render read-only-style with the encoder's typed refusal stated;
deny opens the optional one-line feedback stage (empty = plain deny).
The question form (C4): digit/↑↓ select + Enter confirm (never
instant-submit), tab row + mandatory submit/review step when
multi-question or any multi-select, `[x]` Space toggles, inline
`Other…` editor, unanswered review items in error color. Plan review
(C3) opens the READER directly — full plan, position indicator, the
three-way action row; Esc docks to the truncated panel form, `f`
returns; request-changes swaps the action row for the mandatory
feedback field (q types there — P2). The reader is ONE overlay over
typed artifacts: Plan (Phase 4's markdown renderer), Diff (chat::diff
— absolute numbers walked from `structuredPatch`-shaped hunks with the
repeat-number convention, numberless ask-time, blank-gutter wrap
continuations, `⋮` hunk gaps, four fg-only diff.* tokens), NewFile
(numbered `+` block); pager keys j/k g/G PgUp/PgDn Home/End, q/Esc
close; Ctrl+T reopens accepted plans with ←/→ stepping (the feed's
plan entry now carries `· ctrl+t to read`). The read-only chat (F1):
same feed, asks as fact panels with the identical preview and
`waiting for a writable client · f read the diff`, `⊘ read-only`
where the composer would be, header `chat · read-only · needs owner`,
bare-letter pager keys, `q` back to the fleet — write affordances
absent, not disabled (no interrupt, no composer, no actions).
AnsweredOptimistic collapses the panel to the pending marker;
SendFailed resurfaces with the failure verbatim; remote resolution
dismisses panel and reader on reconcile; synchronous answer refusals
are watched and stated in the panel.

### Changes
- `crates/amux-tui/src/chat/diff.rs` (new, pub): the §4 diff renderer
  — number walk, gutters, wraps, preview budget/remainder, new-file
  blocks; unit-locked layout tables.
- `crates/amux-tui/src/chat/ask_ui.rs` (new): the C2/C3/C4 stage
  machines (AskUi/AskStage/QuestionUi), panel text-field readline.
- `crates/amux-tui/src/chat/panel.rs` (new): docked panel renderer,
  all forms + read-only fact panels.
- `crates/amux-tui/src/chat/reader.rs` (new): ReaderView + the
  fullscreen frame, artifact resolution from the Model, plans nav.
- `chat/{mod,keys,render}.rs`: focus routing (reader → panel →
  composer; read-only pager), the Esc chain's stages 1–2, reconcile
  syncs panel/reader to the ask head (plan reader-first), the bottom
  block replacing FIXED_ROWS (panel/read-only/composer variants, tail
  kept on short viewports), read-only header/working-line, plan
  affordance; `render.rs`: the four diff.* Theme tokens.
- `view.rs`/`run.rs`: `UiAction::CloseChat` (read-only `q`).
- Goldens: 27 new frames/style-maps (permission edit incl. CJK wrap +
  truncation arithmetic, write, bash, fallback, unverified refusal,
  deny feedback, pending marker, send-failed resurfacing, question
  single/tabs/other/review, plan reader/docked/feedback/resolved,
  ask diff reader, new-file reader, post-hoc numbered multi-hunk +
  both-theme style maps, read-only ×3, panel style maps ×2);
  chat_needs_you and chat_tools_edge regenerated (the two surfaces
  Phase 4 explicitly deferred to this phase); every other Phase 4 and
  fleet golden byte-identical. The never-panic sweep grew panel,
  question, plan-reader, docked-plan, and read-only states. 17 new
  key/state-machine unit tests.

### Verification
- fmt clean; workspace clippy `-D warnings` (`--features
  amux/testnet`) clean.
- amux-tui 78 lib + 48 chat golden + 19 fleet golden; amux-ui 29 lib
  + 1 runtime + 123 spec; amux --lib 401; amux spec (testnet) 44;
  amux-cli 53 — all under `timeout 600`.

### Next Steps
- Phase 5 report; orchestrator review gate + simplification pass.
- Phase 6 wires entry (fleet bindings, UserAttached on chat open),
  the chrome-wide Ctrl+C guard, and the `?` overlay.

---

## 2026-08-12: Chat V1 Phase 5 — ask artifacts in the layer, menu-shape refusal, readonly attach subscription

### Summary
The Model half of Phase 5 (ask panels/reader/read-only). Permission asks
now carry a typed body artifact computed once in the fold at ask
creation: `claude::artifact` defines the jsdiff-shaped hunk model
(`DiffArtifact`/`DiffHunk`, `DiffNumbering::{Absolute,None}`,
`DiffMagnitude::{Fact,Estimated,ReplacesEveryOccurrence}`) and
`ask_time_diff` — the ONE computed diff in the design (diff-rendering
§1.4: at ask time the transcript states no diff; everything post-hoc
restates `structuredPatch`). Edit asks get a numberless,
estimated-magnitude mini-diff via `similar` (line-level, context 3,
the jsdiff convention); `replace_all` states "replaces every
occurrence" instead of counts; Write asks retain their proposed
`content` (`AskArtifact::NewFile` — create-vs-overwrite unknowable
pre-run, the artifact claims neither); other tools carry `None`. The
artifact lives ON the ask (evict bytes, never obligations) so panel
and reader render it and never compute. `encoding::menu_shape_refusal`
exports the panel's read-only gate — the same one-suggestion check the
permission program table enforces, so a panel can never offer an
action the encoder would refuse. And `Msg::UserAttached` now
subscribes readonly agents' streams too (`StreamWanted::UserRequested`
vs the eager `InventoryPolicy` skip): opening a read-only chat (F1) IS
the interaction the subscription policy widens for; inventory alone
still opens nothing for them.

### Changes
- `crates/amux-ui/src/claude/artifact.rs` (new): hunk model + the
  ask-time producer, unit tests (grouping, magnitude, replace_all).
- `crates/amux-ui/src/claude/{mod,fold}.rs`: `Ask.artifact` field,
  computed in `fold_permission_request`; transcript-only fallback asks
  (question/plan) carry none.
- `crates/amux-ui/src/claude/encoding.rs`: `menu_shape_refusal` +
  `unverified_permission_menu` shared with `permission_program`.
- `crates/amux-ui/src/update.rs`: `ensure_stream` takes
  `StreamWanted`; readonly skip applies to inventory policy only.
- Workspace/amux-ui manifests: `similar = "2.7"`.
- Spec: asks chapter grows the artifact cases (+ differential sequence
  `asks::edit_artifact`); attention chapter grows the readonly-attach
  case (+ sequence `attention::readonly_attach`); inventory's
  readonly comment updated (its assertion unchanged).

### Verification
- `timeout 600 cargo test -p amux-ui`: 28 lib + 1 runtime + 123 spec.
- `cargo clippy -p amux-ui --all-targets -- -D warnings` clean; fmt.

### Next Steps
- The TUI half: docked panels, the reader, read-only chat (this
  phase's remaining chunks).

---

## 2026-08-12: Chat V1 Phase 4 — simplification pass

### Summary
The Phase 4 diff re-read for altitude and dead surface; every change is
behavior-preserving (all goldens byte-identical, no regeneration). The
chat layout's clamp arithmetic now reads as one rule: `FIXED_ROWS`
(3 above the feed + 4 below) and a free `extra_rows(working, paused)`
feed both the composer budget in `layout()` and a single
`feed_height_for(paused)` — previously the same sums lived in three
places as `3 + … + 4`, `7 + extra`, and a duplicated inline `extra`.
`composer_width` was `text_width` under a second name (composer and
feed text share TEXT_COL) — merged. The chat's `finished_blank`
duplicated the chrome's `blank_line` that this phase had already made
pub(crate) — the chrome helper now serves both screens. In markdown,
the two copies of the close-row-and-hang-indent step in `wrap_runs`
collapsed into one nested `break_row`; `find` lost its slice-of-one-
char generality; `wrap_runs` went private (no callers outside the
module). `kill_to_line_end` had hand-expanded `kill_range`'s body
(minus a no-op cursor self-assign) — routed through the one kill
primitive. Thin wrappers `plain_text_rows`/`message_lines` inlined to
match the direct `markdown::plain_rows` idiom of every other call
site; the unexercised root re-export of `handle_chat_key` dropped
(`chat::handle_chat_key` remains the path). Deliberately untouched:
`ViewState::open_chat`/`close_chat` (the documented Phase 6 seam), the
chat/fleet renderer split (their line-building needs genuinely
differ), and every test and golden.

### Changes
- `crates/amux-tui/src/chat/render.rs`: `FIXED_ROWS` + `extra_rows` +
  `feed_height_for`; `composer_width`→`text_width`;
  `finished_blank`→`crate::render::blank_line`; inlined
  `plain_text_rows`/`message_lines`.
- `crates/amux-tui/src/chat/markdown.rs`: `break_row` extraction;
  single-char `find`; `wrap_runs` private.
- `crates/amux-tui/src/chat/composer.rs`: `kill_to_line_end` via
  `kill_range`.
- `crates/amux-tui/src/lib.rs`: root re-export trimmed to `ChatView`.

### Verification
- fmt clean; workspace clippy `-D warnings` (`--features
  amux/testnet`) clean; amux-tui 51 lib + 21 chat golden + 19 fleet
  golden with every golden byte-identical; amux-ui, amux --lib, amux
  spec (testnet), and amux-cli suites green.

---

## 2026-08-12: Chat V1 Phase 4 — codex review fixes

### Summary
The four codex findings on the Phase 4 diff, all accepted. (1) A
command the reducer refuses SYNCHRONOUSLY (whitespace-only prompt,
disconnected fail-fast) finished before the run loop's reconcile ever
ran — the cleared draft could never resurface; the loop now reconciles
immediately after dispatch. (2) The paste deferral was overridden by
review: with bracketed paste disabled, a pasted CR arrived as Enter and
submitted a partial prompt — actively harmful. Bracketed paste joins
the terminal-hygiene set (enabled with the chrome, restored on every
exit path incl. the signal handler's locked byte string), and
`Event::Paste` inserts literally into the draft: CRLF/CR→LF, tabs
expand to four spaces at insertion (mirroring the reader's
tabs-expand-before-width-math policy, keeping the draft sendable past
the C6 control-byte validator), all other control bytes stripped
(invisible AND unsendable — a trap in both directions). (3) Composer
growth is clamped to the viewport's static-row budget, so the footer
and bottom border survive every height ≥ the (raised) minimum; the
never-panics sweep now asserts both at every size. (4) Wrapping and
border math measure display cells, not codepoints: `str_width`/
`clip_to_width` (unicode-width 0.2 + unicode-segmentation — the exact
versions ratatui renders with) drive markdown wrap, hard wrap, composer
rows, grouped-join fits, and `finish_line`'s clip/pad, so CJK/emoji
never displace the right border and combining marks never widen a row.

### Changes
- `crates/amux-tui/src/run.rs`: reconcile after dispatch;
  `Event::Paste` routes into the chat composer.
- `crates/amux-tui/src/terminal.rs`: Enable/DisableBracketedPaste in
  `write_enter_chrome`/`write_restore`; `RESTORE_BYTES` extended (the
  lockstep unit test still guards it).
- `crates/amux-tui/src/chat/composer.rs`: `paste()` with
  normalization/expansion/stripping + five tests.
- `crates/amux-tui/src/chat/keys.rs`: `handle_chat_paste`; tests for
  the synchronous-refusal resurface and paste dismissal.
- `crates/amux-tui/src/render.rs`: `str_width`/`clip_to_width`;
  `line_len`/`finish_line` measure and clip by cells (re-padding after
  a wide-grapheme clip so the border cannot drift).
- `crates/amux-tui/src/chat/{markdown,render}.rs`: width-based wrap
  paths; composer budget clamp; MIN_HEIGHT 10.
- `crates/amux-tui/tests/chat_golden.rs` + `tests/golden/
  chat_unicode.txt`: CJK/emoji/combining-marks frame with per-row
  border-cell assertions; the viewport sweep asserts frame height,
  footer, and bottom border at every viable size.

### Verification
- amux-tui 51 lib + 21 chat golden + 19 fleet golden; every
  pre-existing golden byte-identical. fmt clean; workspace clippy
  `-D warnings` (`--features amux/testnet`) clean; amux-ui, amux
  --lib, amux spec (testnet), amux-cli suites green.

---

## 2026-08-12: Chat V1 Phase 4 — the chat screen: feed + composer

### Summary
The chat screen lands in amux-tui as a screen within the existing
chrome (`docs/CHAT.md` §Wireframes normative): a new `chat` module with
the feed renderer, the multiline readline composer, scroll/follow
ViewState, and the D5 working line — all wired to Phase 1–3's Model
surface (`ClaudeLayer` entries/echoes/session facts,
`Model::claude_phase`, `claude_send_gate`, `claude_mode_cycle_gate`,
`Command::{SendPrompt, Interrupt, CyclePermissionMode}`). Rendering
stays a pure function of (Model, ViewState, FrameContext); every
derivation (phase, gate, magnitudes, counts) comes from the Model and
the views format only. The screen is reachable in-code via
`ViewState::open_chat` — the seam Phase 6's fleet bindings will invoke;
no fleet binding changes here.

### Changes
- `crates/amux-tui/src/chat/{mod,composer,markdown,render,keys}.rs`:
  the chat screen. Feed rendering for every Phase 1 entry kind
  (terminal markdown per B2 — headings bold keeping `#`, fenced code,
  lists, inline emphasis/code, preformatted tables, URL-aware wrap;
  tool lines with FACT magnitudes and dim `└` continuations; grouped
  read/search one-liners; thinking/turn/compaction markers; the
  truncation boundary; loading-vs-empty; optimistic echo with
  `sending…`; stated send failures), sticky-bottom scroll with the
  paused rule (`↓ following paused · N new entries · pgdn to resume` +
  position %), the 1–6 row auto-growing composer with the full
  readline set and single-slot kill buffer, Enter gated by
  `claude_send_gate` (draft kept, footer states the gate), Ctrl+X
  interrupt in every state, Shift+Tab mode cycle, the Esc chain with
  Phase 5's stages slotted, and the working line driven by the one
  1 Hz tick.
- `crates/amux-tui/src/render.rs`: `Theme` grew Dark/Light variants
  with semantic tokens (text/muted/emphasis/code/ok/warn/error); the
  fleet keeps its fixed styles (byte-identical goldens); line helpers
  opened pub(crate) for the chat module.
- `crates/amux-tui/src/{view,run,lib}.rs`: `ViewState.chat` +
  `open_chat`/`close_chat` seam; run-loop routing (chat owns keys when
  open — its Ctrl+C is clear-as-kill, never quit), dispatch op
  feedback for C5 failure resurfacing, tick gating extended to the
  chat working line.
- `crates/amux-tui/tests/chat_golden.rs` + `tests/golden/chat_*`: 20
  Tier-3 tests — idle/working/scrolled-back/truncated/loading/empty/
  echo/failure/needs-you/markdown/entry-edges/tool-edges frames, style
  maps for both themes, layout-equality across themes, stability, and
  a never-panics viewport sweep.

### Decisions Made
- Working-line token count omitted: the layer folds no usage facts
  yet (D5 words it "when cheaply available"); recorded for a later
  phase rather than half-derived in the view.
- Grouped tool entries join with ` · ` only when they fit the line;
  otherwise they fall back to their own row — grouping compresses,
  never clips.
- Footer hints tell the truth (P10): `? help` and `end newest` from
  the wireframe footers are absent until Phase 6 lands the overlay and
  the ext-tier jumps; the scrolled hint says `pgup/pgdn scroll · esc
  newest` (empty draft only).
- Ctrl+C on an empty draft is a deliberate no-op with a comment naming
  Phase 6's chrome-wide two-press guard; a single ^C never quits.

### Verification
- `cargo fmt`; amux-tui: 44 lib + 19 fleet-golden + 20 chat-golden
  tests green; fleet goldens byte-identical; workspace clippy clean.
- Full gate (amux-ui suite, amux lib, amux spec under testnet) run at
  phase close.

### Next Steps
- Phase 5: ask panels docked at the composer, the reader, read-only
  chats — the Esc chain's stages 1–2 slots are marked in
  `chat/keys.rs::esc_chain`.
- Phase 6: fleet entry bindings via `ViewState::open_chat`, chrome-wide
  guarded Ctrl+C, `?` overlay, kitty detection (Shift+Enter sugar).

---

## 2026-08-12: Phase 3 simplification pass

### Summary
The phase-gate simplification sweep over the Phase 3 diff. Three small
cuts, no behavior change: (1) dead surface — `STALE_INPUT_ERROR` was
re-exported from `amux_ui` but nothing outside `runtime.rs` uses it;
the re-export is gone and the const is module-private. (2) noise — six
`command.clone()` calls on `update.rs` refusal paths were redundant
(NLL lets the command move on every early-return path); refusals now
consume the command plainly. (3) copy-paste drift — the three new
question capture scenarios (`question_tabs`, `question_other_single`,
`question_mixed`) carried verbatim copies of the wait-for-answers /
press-Enter-for-a-lingering-submit-step loop; it is now one
`confirm_question_submit` helper returning the recorded
`extra_submit_steps`, leaving each scenario's documented keystroke
program in-line. Reviewed and deliberately left alone: the refusal
plumbing (already ONE mechanism — `refuse` plus typed `EncodingError`
rendered to stated messages, not parallel copies), the `InputDispatch`
parameter object (four callers; keeps `retry_stale` named at call
sites), `PromptEcho.at` / `SuggestionFact.directories` (fold facts with
a stated Phase 4 consumer), the encoding fns' `pub` visibility (the
module is the documented seam), and every spec chapter.

### Changes
- `crates/amux-ui/src/lib.rs`, `runtime.rs`: drop the unused
  `STALE_INPUT_ERROR` export; const is now private to the shell.
- `crates/amux-ui/src/update.rs`: refusal paths move `command` instead
  of cloning it.
- `crates/amux/tests/capture/main.rs`: shared
  `confirm_question_submit` for the three Phase 3 question scenarios
  (identical messages and retry bounds preserved).

### Verification
- fmt; workspace clippy `--all-targets --features amux/testnet`
  `-D warnings` clean; `timeout 600 cargo test -p amux-ui` (25 lib +
  119 spec + 1), `-p amux --lib` (401), `-p amux --features testnet
  --test spec` (44), `-p amux-tui` (22) all green. No spec assertion
  changed; no fixture touched.

---

## 2026-08-12: Chat V1 Phase 3 — codex review fixes

### Summary
Two accepted findings, both with locking spec cases. [P1] `AnswerAsk`
addressed the whole queue while claude's remote menu only ever displays
the HEAD: an answer for a later queued ask would encode that ask's
digits and apply them to the head's menu — potentially approving the
wrong tool — while marking the later ask optimistic. The reducer now
requires the target to equal `ask_head()`; a non-head target refuses
with a typed outcome ("ask is queued behind the current menu — answer
the head ask first"), no bytes, no state touched. [P2] Free text wrapped
in bracketed paste could carry a literal `ESC[201~` terminator: the
paste would close early and the remainder run as LIVE keystrokes in the
remote session (injection; also an echo desync wedging SendInFlight).
Decision: **rejection over neutralization** — the verified transcript-
transparency claim is printable + `\n` exactly (`prompt_multiline`), so
stripping/splitting ESC would assert reassembly knowledge no capture
confirms (the same honesty rule as unverified menu shapes). Every
control character except `\n` now refuses with the new typed
`EncodingError::ControlBytesUnsupported`, on EVERY free-text path — the
sweep covered the prompt paste, the deny-feedback paste (shared
`paste_block` helper), and the raw menu fields (Other text single- and
multi-select, plan request-changes feedback — all now through
`menu_text`, where an ESC would navigate the menu instead of typing).

### Changes
- crates/amux-ui/src/update.rs — the head-only answer guard.
- crates/amux-ui/src/msg.rs — AnswerAsk doc states the head rule.
- crates/amux-ui/src/claude/encoding.rs — `ControlBytesUnsupported`,
  `reject_control` + `paste_block`, menu_text sweep, plan feedback
  routed through menu_text; table test
  `control_bytes_in_free_text_refuse_on_every_path`.
- crates/amux-ui/tests/spec/write.rs — locking cases
  `an_answer_addressed_past_the_head_refuses_without_bytes` (two queued
  asks: non-head refusal, head answers normally; differential sequence
  `write::head_guard`) and
  `a_prompt_carrying_the_paste_terminator_is_refused` (no effect, no
  echo, gate stays Ready).

### Verification
- fmt clean; workspace clippy `-D warnings` clean.
- amux-ui 25 lib + 1 runtime + 119 spec; amux --lib 401; amux spec 44
  (testnet); amux-tui 3 + 19 — all green.

## 2026-08-12: Chat V1 Phase 3 — the write path (C6 module, Commands, C5 lifecycle)

### Summary
The chat write path lands: typed intents become keystroke programs
injected under the seq guard, with the C5 optimistic lifecycle in the
Model. One encoding module (`amux-ui/src/claude/encoding.rs`) owns every
Claude key byte in the workspace — typed intent in, `KeyStep` program
out, each table documented with its capture provenance (claude 2.1.228),
and any menu shape no capture confirmed returns a typed
`UnverifiedMenuShape` error instead of guessed bytes (the permission
menu is generated from the hook's `permission_suggestions`, so its digit
table is verified for the one-suggestion shape only). Four new Commands
(`SendPrompt`, `AnswerAsk`, `Interrupt`, `CyclePermissionMode`) dispatch
with OpIds and finish as state; the reducer encodes, gates (D2 via
`Model::claude_send_gate`, D4 via the mode-cycle gate, D3 ungated),
flips the optimistic state (prompt echoes with content-equality
reconciliation; `AskState::AnsweredOptimistic{op, answer}` carrying the
pending-marker data), and emits `Effect::SendInput` with the layer's
stream cursor as `expected_seq`. The shell maps steps onto the
`claude_pty_transcript_v1` actions and always answers — a stale-seq
interrupt (position-independent by design) retries mechanically with
the seq the refusal reported; everything else fails fast and resurfaces
with the failure stated. Remote resolution wins over local pending
state everywhere; readonly rejection is the server's and the model
states it. Hook payloads' `permission_mode` now folds into the session
facts (the D4 verdict made code).

### Changes
- crates/amux-ui/src/claude/encoding.rs — the C6 module + table tests.
- crates/amux-ui/src/claude/{mod,fold}.rs — ask `suggestions` facts,
  prompt echoes, seq cursor, hook permission_mode fold, ask-state
  mutators, echo invariant.
- crates/amux-ui/src/{msg,effect,update,model,runtime,lib}.rs — the
  Command/OpOutcome/Effect vocabulary, gates, dispatch, op-result
  resurfacing, `SendGate`, the shell executor.
- crates/amux-ui/tests/spec/write.rs — Chapter 13 (13 cases, 5
  differential sequences); wire_free serde variants extended; asks
  chapter asserts the suggestion facts.
- crates/amux-tui/src/render.rs — status-line verbs for the new
  commands (no chat rendering — that is Phase 4).

### Decisions Made
- Prompts always inject as bracketed paste: literal text (no `/`/`!`
  grammar triggers), newline-safe, and the transcript row lands
  byte-identical — which makes content equality the echo reconciliation
  key (verified).
- One optimistic echo in flight at a time (`SendGate::SendInFlight`):
  reconciliation stays unambiguous; claude queues raced sends anyway.
- Deny-with-feedback is ONE program (digit, settle, pasted feedback as
  a follow-up prompt) — the permission menu has no feedback field
  (verified; the plan menu's request-changes does and keeps its field
  encoding).
- `Effect::SendInput.retry_stale` carries the reducer's policy; the
  shell retries mechanically (bounded) with the server-reported seq —
  interrupt only.
- Stale-seq refusals are stated, not jargoned: the shell maps an
  unretried/exhausted `SequenceNumberMismatch` to `STALE_INPUT_ERROR`
  ("input raced the session — it moved on before the keys landed")
  with the technical detail in parentheses; `EncodingError` carries no
  serde — a refusal is a finished-op message, never Msg traffic.

### Verification
- fmt; workspace clippy `-D warnings` clean.
- amux-ui: 24 lib + 1 runtime + 117 spec (differential wraps the new
  sequences; fold==live after every Msg). amux --lib 401; amux spec 44
  (testnet); amux-tui 3+19 green.

## 2026-08-12: Chat V1 Phase 3 — live keystroke verification (C6 research)

### Summary
The write path's core risk retired first: every keystroke encoding the C6
module will state was driven against a REAL claude (2.1.228, haiku) via
six new capture-harness scenarios, and the open D4 question is answered.
Verdicts: a mid-session Shift+Tab cycle emits NO `permission-mode` row
(three presses, two turns, zero rows — the hook payloads' 
`permission_mode` is the live mode source; cycle order default →
acceptEdits → plan → default); plan approve-AUTO (digit 1, the owed H.5
sub-capture) does not flip the row either while the effective mode
becomes acceptEdits and edits land ask-free; the permission menu is
generated from the hook's `permission_suggestions` (1 Yes / one digit per
suggestion / last No) and its deny digit denies IMMEDIATELY with the
interrupt artifacts — no feedback field, so deny-with-feedback composes
as digit + follow-up prompt; question digits SELECT-and-advance (a
single single-select form submits on selection; multi-question/multi-
select forms end on a review step where one Enter submits — claude's own
form implements C4's mandatory-submit rule); the single-select Other is
digit + type + Enter; mixed forms compose with Tab advancing off the
multi-select question; and multiline prompts inject via bracketed paste
with the transcript row byte-identical to the sent text (B1's echo
correlation is content equality). Eight redacted fixtures graduated
(leak sweep clean); semantics recorded in transcript-semantics.md §18d.

### Changes
- crates/amux/tests/capture/main.rs — scenarios permission_session,
  permission_deny_feedback, question_tabs, question_other_single,
  question_mixed, plan_auto, mode_cycle, prompt_multiline.
- crates/amux/tests/fixtures/chat-v1/ — 8 new fixture pairs + README.
- notes/chat-v1/transcript-semantics.md — §18d; §10 permission-mode row
  entry resolved.

### Decisions Made
- Deny encoding: the last menu digit, never Esc — Esc-deny and digit-deny
  differ (both interrupt-close the turn on the plain permission menu, but
  the digit is the labeled option C2 requires; the plan menu's digit 3
  keeps the turn alive with a feedback field).
- The question_tabs pre-run that CANCELLED the form (digit+Enter surplus
  keys walked the review onto Cancel) is kept as negative evidence in the
  phase report — digits auto-advance, so per-question Enters are wrong.

### Verification
- All 8 scenarios green against claude 2.1.228 on haiku (no sonnet
  needed); every run under the auto-update guards on a poisoned scratch
  daemon (the spawn-seam scrub live-proved again).
- Fixture leak sweep: username/home/hostname/scratch/mcp/credential
  patterns × 17 fixture pairs = 0 hits.

## 2026-08-12: Phase 2 gate — simplification pass

### Summary
The Phase 2 simplification pass over the ask/phase build. Three small
cuts, no behavior change: `structured_type()` in the core hook seam was
a single-caller helper whose `None` arm was unreachable at its call
site (the internal-only kinds are matched above it), leaving a dead
`else { return }` — the emitted kinds now name their `type` tag
directly in `handle_hook`'s one flat match; `fold.rs` carried the
FNV-1a loop twice (`content_key`, `ask_key`), now one `fnv1a` helper
with identical hash output (the constants stay explicit because ask
keys are serialized model state); two doc comments in `model.rs` still
said "summarizer" after the E2 deletion. Reviewed and deliberately
left: the phase()/attention() parallel derivations (attention is not a
function of ChatPhase — Authority-close and the stop pre-signal map to
NeedsYou(Finished) on the badge but Idle on the phase, so the mirrored
precedence IS the one story), `observe_exit`'s field clearing beside
the `exited` early-return (accessors like `ask_count()` are
spec-asserted post-exit), the `AskState` variants only Phase 3
constructs (documented shape-ahead), and the capture harness's
`env_map`/assert split (the guard's point is checking exactly what the
daemon receives).

### Changes
- crates/amux/src/agents/claude/session/hooks.rs — helper inlined.
- crates/amux-ui/src/claude/fold.rs — one FNV-1a implementation.
- crates/amux-ui/src/model.rs — stale summarizer wording in two docs.

### Verification
- cargo fmt; workspace clippy `-D warnings` clean.
- amux-ui 104-spec suite, amux --lib (401), amux spec (44, testnet),
  amux-tui (19 + goldens): all green, no assertion touched.

---

## 2026-08-12: Phase 3 gate — spec corrections from the write path

### Summary
docs/CHAT.md absorbs the Phase 3 live-verified corrections: D4
resolved (mid-session cycling emits NO `permission-mode` row; hook
payloads' `permission_mode` is the live source; cycle order default →
acceptEdits → plan → default), H.5 resolved (plan menu 1/2/3;
approve-auto never flips the row either — effective mode via hook
facts), C2 gains the suggestion-generated menu reality (option
labels derive from `permission_suggestions`; unverified menu shapes
refuse typed; deny is immediate with feedback as a composed
follow-up prompt), B5's `command_permissions` claim narrowed to
command-rule grants, C6 cites the encoding module and its refusal
rule, C4 notes claude's appended "Chat about this" option. Gate
context: codex review returned a P1 (answers could bind past the
queue head and approve the wrong permission — now head-only with a
typed refusal) and a P2 (bracketed-paste terminator injection —
resolved by rejecting control bytes on every free-text path,
rejection chosen over neutralization per the honesty rule); both
fixed with locking spec cases (`a637fa8`).

### Changes
- docs/CHAT.md — six corrections, each evidence-tagged "Phase 3".

### Verification
- Prose only; wireframes remain exactly 80 columns.

---

## 2026-08-12: Phase 2 gate — spec corrections from the ask/phase build

### Summary
docs/CHAT.md absorbs the Phase 2 fixture-grounded corrections:
plan request-changes spawns a NEW ask without ending the turn
(rejection carries `toolDenialKind:"user-rejected"`); permission-ask
correlation is content identity (hooks carry no tool_use id;
`tool_name` + byte-equal `tool_input` is the join); the phase decay/
staleness thresholds are named constants (60 s idle decay, 600 s
working cap) so E2E can assert them; hook delivery is documented
at-least-once with core-side dedupe; the fleet's errored→Unknown
mapping is design, not omission. Gate context: codex review returned
one P1 (truncated live windows stuck Replaying forever for
late-attaching clients) and two P2s — all fixed with locking spec
cases (`9d1df25`) — plus capture-env hardening after the Phase 0
installer incident (the owner's `~/.local/bin/claude` symlink was
repointed into a capture temp dir; restored to the durable install,
and the harness now hard-locks claude's updater in the spawn env).

### Changes
- docs/CHAT.md — five corrections, each evidence-tagged "Phase 2".

### Verification
- Prose only; wireframes remain exactly 80 columns.

---

## 2026-08-12: Phase 1 gate — simplification pass

### Summary
The Phase 1 simplification pass over the Claude layer. One real seam:
the review fixes had left two copies of the message-closure machinery
(`close_open_messages` with a hidden targeted-fallback mode, plus the
bolted-on `close_others_as_abandoned`), each duplicating the
close-and-retag loop. Both collapse into one `close_messages(layer,
closure, selects)` whose slot-selection rule now reads at each call
site — the interrupt's pair-by-`interruptedMessageId`-else-all-open
fallback moved to `fold_interrupt`, where §17 states it. Two
micro-cleanups: the thinking arm binds its already-matched block type
instead of re-reading the discriminator, and `retain_plan`'s fallback
flattens to `find` + `?`. Behavior frozen: no spec chapter changed.
Left alone deliberately: `prompt_at()`/`session_id()` (documented
D5/header read surfaces), the `with_claude_summarizer`/
`with_claude_layer` gate duplication (the summarizer is deleted by
Phase 2's unification), and `entry_kind`/`entry_kind_mut` (mut/non-mut
pair documenting the eviction-tolerant read).

### Changes
- crates/amux-ui/src/claude/fold.rs — closure machinery unified; net −8
  lines with the rules stated at the call sites.

### Verification
- cargo fmt + clippy `-D warnings` clean; `timeout 600 cargo test -p
  amux-ui` (69 spec + unit tests) and `timeout 600 cargo test -p amux
  --features testnet --test spec` (44) all green, assertions untouched.

---

## 2026-08-12: Chat V1 Phase 2 — codex review fixes + capture hardening

### Summary
Three accepted codex findings plus one incident hardening item.

[P1] **Truncated live windows never unlocked.** A long-running session
writes past the bounded source tail, so a late attacher's truncated
window no longer CONTAINS the `amux.transcript_ready` marker — the layer
reported Replaying (and attention Unknown) forever, suppressing live
prompts and permission hooks on the most common real-world attach.
`StreamMsg::ReplayComplete` is now the out-of-band unlock: the layer
records `replay_complete`, and liveness is `ready-marker OR
replay-complete`. Truncation honesty (B9's boundary) is unchanged; a
relink resets the latch and is unlocked by the new file's own fresh
marker (B10's loading band survives). Locked by
`phase::a_truncated_live_window_unlocks_after_replay_complete`
(mid-replay Replaying → unlock → tail's Working surfaces → live
permission hook surfaces on phase AND badge, history_truncated stays).

[P2] **Stale badge beside a stale-free label.** After the 600 s cap,
`effective_attention` degraded to Unknown while `status_label_for`
still rendered the cached "working". One derivation now: the status
word derives from the same effective attention as the badge
(`AgentCard::status_label` takes the effective attention and is
crate-private; `Model::status_label_for` is the only entry). Locked by
`attention::stale_working_degrades_the_fleet_badge_and_label_together`.

[P2] **Exit on a truncated window fell to Unknown.** `observe_exit`
cleared activity but left `truncated_start`, so the phase degraded
instead of settling. Orderly termination is now a recorded FACT
(`exited`) that overrides truncation and staleness: phase settles to
`Idle{Fact}` (choice recorded — the vocabulary has no exited phase; the
card's `AgentPhase::Exited` carries the exit itself), attention to
Idle. Locked by the extended
`phase::agent_exit_closes_asks_and_settles_the_phase` (plus a
truncated-window variant).

[Hardening] **The capture harness can no longer let claude's installer
touch the owner's real launcher.** During Phase 0 captures the
auto-updater downloaded into the scratch XDG_DATA_HOME and REPOINTED
`~/.local/bin/claude` at the temp dir (the launcher path is
`join(homedir(), ".local/bin/claude")` in the 2.1.228 binary — not
env-overridable — and the harness must keep the real HOME for keychain
auth). The harness env now carries `DISABLE_AUTOUPDATER=1`,
`DISABLE_UPDATES=1` (the hard lock — `claude update` itself refuses),
and `DISABLE_INSTALLATION_CHECKS=1`, all three verified against the
2.1.228 binary's env registry; env construction moved to a pure map
with the guard asserted on every capture run (the capture binary has no
test harness; Phase 7's scheduled capture validates live). No new
captures were run.

### Changes
- crates/amux-ui/src/claude/mod.rs — `replay_complete` + `exited`
  facts, `observe_replay_complete`, `live()` gate in phase/attention.
- crates/amux-ui/src/update.rs — ReplayComplete reaches the layer.
- crates/amux-ui/src/model.rs — label derives from effective attention.
- crates/amux-ui/src/claude/fold.rs — the relink reset re-stamps the
  arrival clock the triggering row set.
- crates/amux-ui/tests/spec/{phase,attention,feed_replay,inventory,
  sessions}.rs — locking cases; the mid-replay and relink sequences
  restated so ReplayComplete sits where reality puts it (after replay
  batches).
- crates/amux/tests/capture/harness.rs — update guards + assertion.

### Verification
- fmt + workspace clippy `-D warnings` clean; `timeout 600 cargo test
  -p amux-ui` (11 + 1 + 104 spec), `-p amux --lib` (401), amux spec
  suite (44), `-p amux-tui` (3 + 19 goldens) all green.

---

## 2026-08-12: Chat V1 Phase 2 — one fold: the summarizer unification (E2)

### Summary
The duplicate interpretation dies. `summarizers/claude.rs` — the
separate attention fold with its KNOWN-FRAGILE notification-wording
split (which the plan-approval notification defeats: "needs your
approval", no "permission" substring) — is deleted; kernel attention is
now a pure projection of the SAME Claude-layer state the chat phase
derives from: `ClaudeLayer::attention()`, cached on the card by the one
`with_claude_layer` gate (the `with_claude_summarizer` duplication dies
with it). The kernel Attention vocabulary is unchanged; the
AttentionMismatch invariant now checks card-vs-layer. Fleet gains: the
badge routes asks on `hook.permission_request.tool_name` (plan review
is `!`, never `?`), `turn_duration` now closes Working → Finished (the
old fold left tool-denial turns stuck Working forever — it treated
system rows as weak and the deny turn has no hook.stop), an interrupt
settles to Idle instead of Working, an API error degrades to Unknown
instead of lying Working, and the E1 staleness cap applies to the badge
at read time (`effective_attention`), keeping fleet and chat in
agreement (E3). Chapter 5 is rewritten onto the chat fixtures — its
three authored summarizer-era fixture JSONs are deleted — and closes
with the unification property: on every fixture, folded row by row,
fleet needs-you equals chat-phase needs-you.

### Changes
- crates/amux-ui/src/summarizers/ — deleted (claude.rs + mod.rs).
- crates/amux-ui/src/update.rs — one gate; attention refreshed from the
  layer after every fold step.
- crates/amux-ui/src/model.rs — `AgentCard.summarizer` removed;
  AttentionMismatch retargeted (card vs layer-derived);
  `effective_attention` staleness degrade.
- crates/amux-ui/src/lib.rs — `SummarizerState` unexported.
- crates/amux-ui/tests/spec/attention.rs — rewritten for the unified
  fold; authored fixtures claude_permission_flow/stop_and_notification/
  truncated_tail.json deleted.
- crates/amux-tui/tests/golden.rs — fixture rows restated in the
  unified fold's facts (ready marker, human-prompt turn starts, the
  question as an AskUserQuestion permission_request). Every golden
  FRAME is byte-identical — the badges' meaning survived the fold swap,
  which is exactly the E3 guarantee.

### Decisions Made
- Attention stays TIME-FREE on the card (cache-vs-fold invariant holds
  between Ticks); read-time policy (host offline, working staleness)
  lives in `Model::effective_attention` — computed once for every
  renderer.
- Errored maps to Attention::Unknown: the kernel vocabulary cannot say
  "errored", retries run invisibly, and Working/Idle would be wrong
  badges. The chat phase carries the errored FACT.
- Interrupt-closed turns map to Idle (the user closed it deliberately);
  authority/presignal-closed turns keep NeedsYou(Finished).

### Verification
- `timeout 600 cargo test -p amux-ui`: 11 lib + 1 runtime + 103 spec
  green (equivalence property sweeps all 9 fixtures row-by-row);
  workspace clippy `-D warnings` clean; amux-tui, amux lib, and the
  amux spec suite green (phase gate).

---

## 2026-08-12: Chat V1 Phase 2 — the ask model and the phase derivation

### Summary
The Claude layer grows the ASK model and the E1 phase derivation
(docs/CHAT.md §Asks, §Phase and attention). An `Ask` is `{id, kind,
payload}` queued in arrival order with an honest head + count: the
pending signal is the `hook.permission_request` row routed on
`tool_name` (AskUserQuestion → question kind; ExitPlanMode → the
plan-review permission variant carrying the plan; anything else → plain
permission with the SAME typed per-tool payload tool entries use — one
extraction, both consumers), with the unpaired
AskUserQuestion/ExitPlanMode in a final message as the transcript-only
fallback. Hook payloads carry NO tool_use id (fixture-verified), so
correlation rides content identity — the hook's `tool_input` equals the
transcript `tool_use.input` byte-for-byte on every fixture. Resolution
is the B5 facts (non-error result / typed denial / answers / plan
approval), plus interrupt-closes-ask, turn-end closers for lagging
resolutions, and human-turn-start as the stale guard. The C5
answered-optimistic and send-failed states exist structurally; Phase 3
drives them with Commands. Phase: `ClaudeLayer::phase(now)` +
`Model::claude_phase` implement the E1 table row by row — replaying /
working / idle / needs-you(permission|question) / errored / unknown,
each value tagged fact vs inferred, working staleness-capped into
Unknown (600 s), idle decaying FACT→INFERRED (60 s), truncated blind
windows Unknown, transport loss Unknown-with-obligations-kept.

Hook rows now dedupe by bounded content hash in the fold (the same
window unknown rows use): historical streams and replays carry every
hook row twice (§18b), and without the dedupe a shrink re-replay would
resurrect a long-resolved ask as pending — the re-replay spec case locks
this on the permission fixture. Recorded trade-off: a byte-identical
genuine re-request within the window folds once (payloads carry
prompt_id and tool_input, so distinct events collide only when truly
identical).

### Changes
- crates/amux-ui/src/claude/mod.rs — Ask/AskKind/AskState/AskWhy,
  ChatPhase/PhaseTag, TurnState activity signals, asks queue +
  invariants (`claude-ask-order` + asks retention bound, firing tests).
- crates/amux-ui/src/claude/fold.rs — ask extraction, correlation,
  resolution and closers; hook-row content dedupe; error_live; turn
  open/close bookkeeping; `observe` gains the batch arrival time (the
  staleness clock enters through Msgs).
- crates/amux-ui/src/model.rs — `Model::claude_phase`, the
  `ClaudeAskOrder` violation class.
- crates/amux-ui/src/update.rs — arrival time threaded; stream close
  wires `observe_exit` (asks die with the process) and `invalidate`
  (stale → Unknown, obligations kept).
- crates/amux-ui/tests/spec/asks.rs (Chapter 11) and phase.rs (Chapter
  12); harness `chat_feed_prefix`; feed_replay's re-replay case
  extended to the hook-carrying permission fixture. 99 spec tests, all
  sequences registered into the differential.

### Decisions Made
- No new Msg kinds (recorder/flow-class coverage by construction).
- One extraction for ask payloads and tool entries: `AskKind` wraps
  `ToolInvocation`, so plan review is literally a permission whose
  invocation is the Plan variant (CHAT.md: not a third kind).
- Ask identity is per request content, not per row or per prompt_id —
  plan_reject proves a revised plan re-ask shares prompt_id.
- Turn-end signals (`turn_duration`, non-active `hook.stop`) close
  pending asks: an ended turn is incompatible with a blocking ask; this
  is the catch-up path when another client answered and the resolving
  rows lag.
- The transcript-only plain-permission inference (E1's read-only
  failure mode: unpaired non-question tool in a final message) is
  deliberately NOT implemented — unpaired = running; only
  question/plan tools are self-evident asks. Recorded for Phase 5.
- Staleness caps are policy constants (600 s working, 60 s idle-fact
  decay), applied at read time so the folded state stays time-free.

### Verification
- `timeout 600 cargo test -p amux-ui` — 10 lib + 1 runtime + 99 spec
  green; every new sequence wrapped by the wire_free differential.

---

## 2026-08-12: Dedupe duplicate Claude hook deliveries at the daemon seam

### Summary
Root cause of the Phase 1 "every hook row arrives twice" observation,
found in the transcript's own records rather than theorized: Claude Code
runs EVERY registered hook command per event, and this machine carries
two registrations of the amux hook — a legacy user-scope
`~/.claude/settings.json` entry (`amux-dev hooks claude`, from the
owner's pre-plugin dev setup; no current repo code writes settings.json)
beside the plugin's `amux hooks claude`. The capture transcripts'
`stop_hook_summary` rows record it verbatim: `hookCount: 2, hookInfos:
[{amux-dev hooks claude}, {amux hooks claude}]`. Both processes read the
same stdin JSON and deliver byte-identical payloads to the same daemon,
which wrote each — hence duplicate adjacent `hook.*` rows in every
fixture. Hook delivery is at-least-once by construction (user settings,
project settings, and plugins can all legitimately carry the
registration), so the fix is at the seam where deliveries become stream
facts: `ClaudeSession::handle_hook` now fingerprints each emitted
payload and drops a byte-identical re-delivery within a 2 s window. The
three structured-emission arms collapsed into one in passing. Client
folds still tolerate duplicates (historical streams and replays contain
them) — that lands with the Phase 2 layer work.

### Changes
- crates/amux/src/agents/claude/session/hooks.rs — dedupe window +
  fingerprint, consolidated emission arm, `tokio::time`-paused test
  proving collapse/distinct/past-window behavior.
- crates/amux/src/agents/claude/session/core.rs — `last_emitted_hook`
  suppression state on `ClaudeSession`.
- crates/amux/src/agents/hook.rs (+ two call sites) — boxed
  `ExternalHookBootstrap::Register` (the grown session tripped clippy's
  variant-size lint).

### Decisions Made
- Dedupe in core, not only in client folds: two identical deliveries of
  one event are one fact, and the daemon is the transport seam; a
  2-second window keeps a genuinely identical future event (same
  notification re-fired minutes later) honest. This is transport
  hygiene, not interpretation.
- The legacy settings.json registration itself is owner-machine state,
  not repo code; flagged in the phase report rather than auto-edited
  (amux does not rewrite user Claude config).

### Verification
- `timeout 600 cargo test -p amux --lib` (401), `timeout 600 cargo test
  -p amux --features testnet --test spec` (44), clippy
  `--all-targets --features testnet` clean, fmt clean.

---

## 2026-08-12: Phase 1 gate — spec corrections from the layer build

### Summary
docs/CHAT.md absorbs the Phase 1 fixture-grounded corrections: B3
(tool-denial interrupts ARE followed by `turn_duration`; the inferred
marker reconciles in place), B4 (Write-create carries an empty
`structuredPatch`; magnitude = created line count), B1 (bare
local-command rows render with unstated source, never start a turn),
B7 (task notifications are their own entry — no agent-id key to
correlate), and E2 (notification-wording heuristics are forbidden;
interpretation routes on `hook.permission_request.tool_name` — the
plan-approval notification says "needs your approval", no
"permission" substring). Gate context: codex review returned seven
P2 fold findings — every one an edge-path violation of a stated spec
rule — all fixed with a locking spec-chapter case each (`2df0e8b`).

### Changes
- docs/CHAT.md — five corrections, each evidence-tagged "Phase 1".

### Verification
- Prose only; wireframes remain exactly 80 columns.

---

## 2026-08-12: Chat V1 Phase 1 — codex review fixes

### Summary
All seven codex findings on the Phase 1 diff accepted and fixed in the
Claude layer fold — each one was the fold breaking a rule docs/CHAT.md
or its own module docs state, and each fix now carries a Tier-1 spec
case documenting the rule: (1) feed-producing rows WITHOUT uuids (the
retained unknown shapes) now dedupe by content hash in the same bounded
window, so a source-shrink re-replay stays idempotent (B10); (2) the
top-level `isMeta` discriminator is filtered before any interrupt/tool/
closure handling, so an array-form meta row can no longer abandon a
message or emit unrecognized entries — meta rows are fully inert in
either content form; (3) `origin.kind:"human"` is the definitive
turn-start discriminator — an unknown `promptSource` label degrades
gracefully instead of silently killing the turn clock; (4) an API-error
row closes any open null-stop message as abandoned before recording the
error (B2); (5) a `turn_duration` row without a readable `durationMs`
degrades to an unrecognized entry — zero is not a fact, and it can no
longer overwrite a better inferred marker; (6) a compaction boundary
invalidates the elapsed prompt base and pending marker reconciliation,
so no duration state crosses it (B3); (7) unknown uuid rows advance the
thinking-duration timestamp chain like every other row.

### Changes
- crates/amux-ui/src/claude/fold.rs — the seven fixes (`remember` /
  `content_key` dedupe helpers, hoisted isMeta filter, turn-start rule,
  API-error closure, malformed-authority degrade, boundary
  invalidation, chain touch in the unknown arm).
- crates/amux-ui/tests/spec/{feed_replay,feed_turns,feed_edges}.rs —
  one spec case per finding (7 new tests, 62 → 69), plus four new
  differential sequences.

### Decisions Made
- No-uuid dedupe strategy: content hash (FNV-1a over canonical JSON,
  domain-tagged) within the SAME bounded seen-window as row uuids —
  identical unknown rows fold once per window, which mirrors what a
  uuid would give them; a distinct-content repeat still folds.
- Malformed `turn_duration` degrades to unrecognized (uninterpreted —
  no turn-state changes) rather than "measured unknown": zero facts
  are never invented, and the row stays visible.
- The isMeta hoist also stops string-form meta rows from closing open
  messages (previously they did): machine-injected rows are not user
  actions, so B2's user-row closure does not apply to them.

### Verification
- fmt + CI clippy clean; `timeout 600 cargo test -p amux-ui` (10 lib +
  1 runtime + 69 spec) green; amux spec suite 44 green; differential
  wraps the new sequences.

---

## 2026-08-12: Chat V1 Phase 1 — the Claude feed-facts layer

### Summary
The typed Claude chat layer (docs/CHAT.md §The feed, B1–B10 + G1) landed
in amux-ui: `claude::ClaudeLayer`, a per-agent child model folding native
`claude_pty_transcript_v1` rows into typed feed entries — prompts,
messages (upsert by `message.id`, dedupe by row `uuid`, finality on
non-null `stop_reason`, abandoned/interrupted closure), thinking/turn/
compaction markers with the FACT-vs-INFERRED tags, tool entries paired by
`tool_use.id` with typed sidecar facts (Edit magnitude, question answers
keyed by question text, subagent launch/completion, plan approval),
status entries (API errors, interruptions), and explicit unrecognized
entries for unknown shapes. Retention is bounded and honest: the feed
evicts from the front counted (`history_truncated`), while the pairing
index, accepted plans, and session facts live outside the window —
the structure Phase 2's obligations-outlive-eviction rule builds on.
Replay/live rides `amux.transcript_ready`; a differing `sessionId` is the
relink fact and opens a fresh epoch; re-replay after source shrink is
idempotent by row uuid. Research-first against the Phase 0 fixtures
found real drift — recorded in transcript-semantics.md §18b (agent-name
rows returned, plan_mode attachments, bare `/compact` user rows,
Write-create's empty structuredPatch, same-message rows resuming after a
tool_result, turn_duration closing denial-interrupt turns, duplicated
hook rows, the plan-approval notification wording that defeats the
summarizer's substring split).

### Changes
- crates/amux-ui/src/claude/{mod.rs,fold.rs} — the layer: entry types,
  bounded state, the fold, `check_invariants` extension (+ firing tests).
- crates/amux-ui/src/model.rs — `AgentCard.claude` + accessors; four new
  Violation classes (retention overflow, feed-order arithmetic,
  index-ahead, dedupe incoherence).
- crates/amux-ui/src/update.rs — `with_claude_layer` routing beside the
  summarizer (unification is Phase 2); layer window resets on `Opened`.
- crates/amux-ui/tests/spec/{feed_replay,feed_turns,feed_tools,
  feed_edges}.rs — Chapters 6–9, fixture-driven against
  crates/amux/tests/fixtures/chat-v1 (referenced via `include_str!`, so
  the suite stays IO-free); sequences registered so the wire_free
  differential wraps them.
- crates/amux/tests/fixtures/chat-v1/README.md — corrected the pong row
  claim (the fixture holds no assistant rows; the capture closed at the
  arrival-ordered hook.stop).

### Decisions Made
- No new Msg kinds: the layer folds from the existing Stream Msgs, so
  the recorder, flow classes, and the differential property cover it by
  construction (fold-from-recording == live proven over the new
  sequences).
- The layer lives beside `ClaudeSummarizer`, not unified with it — E2's
  one-fold unification is Phase 2's brief; duplicating interpretation
  now was rejected in favor of leaving the summarizer untouched.
- Session-epoch detection keys on transcript `sessionId` only (never
  hook `session_id`, which is arrival-ordered and could false-trigger).
- Meta user rows, local-command records, attachments, file-history and
  queue rows fold to no entry (known bookkeeping); the bare `/compact`
  row renders as an unstated-source prompt.

### Verification
- fmt + CI clippy (`--workspace --all-targets --features amux/testnet
  -D warnings`) clean; `timeout 600 cargo test -p amux-ui` (10 lib + 1
  runtime + 62 spec, all green); amux spec suite 44 green.

---

## 2026-08-12: Phase 0 simplification pass

### Summary
Simplification pass over the Phase 0 diff (`41243f9..0b7f0d4`), scoped
to the capture harness. Main finding: `CaptureSession::close` returned
the keystroke log but every scenario discarded it, cloning the same
data out of the pub `keys_log` field via a `keys_json` helper one line
earlier — `close` now returns the keys JSON directly, the helper is
gone, and `agent_name`/`rows`/`raw_screen`/`keys_log` went private.
Also: dropped three `let index = …; let _ = index;` dances in
scenarios, made `CONFIG_ATTACHMENT_TYPES` private (redact.rs-internal),
and fixed the garbled `target_debug_dir` comment. Deliberately left
alone: `DaemonEnv` (named-field call-site clarity beats a bare bool),
`claude_spawn_env`/`apply_env` (exist so the unit tests exercise the
exact production seam), `wait_for_transcript_ready` (named domain
boundary), and the `Row` predicate duplication (merging `is_tool_use`
into `tool_use_id().is_some()` would change edge behavior on id-less
blocks). Altitude check: src/testnet is in-process only — no existing
utility covers a real subprocess daemon, which the env-inheritance
seam requires. No fixture row content touched; no recapture needed.

### Changes
- crates/amux/tests/capture/harness.rs, main.rs, redact.rs — above.

### Verification
- `cargo fmt`, clippy (`-p amux --all-targets --features testnet`),
  `cargo test -p amux --lib` (400 ok), spec suite (44 ok), and the
  capture binary's opt-in no-op path — all green.

---

## 2026-08-12: Phase 0 gate — spec corrections from fixtures

### Summary
docs/CHAT.md absorbs the Phase 0 fixture-grounded corrections: plan
review resolution rules (manual approval emits no `permission-mode`
change; approval fact is the canonical tool_result, rejection is
`is_error:true`; approve-auto still owed to H.5), lazy transcript
creation on fresh sessions (empty-chat state, `transcript_ready`
arrives with the first turn), and ask extraction routing on the hook
payload's `tool_name` (`hook.permission_request` also fires for
AskUserQuestion/ExitPlanMode). Gate context: codex review
(gpt-5.6-sol, high) of the phase diff returned five findings — one P1
fixture-privacy leak, four P2 harness defects — all five fixed and
the phase recommitted clean (`0b7f0d4`) before anything was pushed;
leak grep over committed fixtures independently verified zero.

### Changes
- docs/CHAT.md — three corrections, each evidence-tagged "Phase 0".

### Verification
- Prose only; wireframes remain exactly 80 columns (machine-checked).

---

## 2026-08-11: Chat V1 Phase 0 — transcript persistence fix + capture harness

### Summary
Fixed the transcript-persistence bug that starved the structured
stream, and built the real-Claude capture harness that produced the
first baseline fixture set (including the previously-UNOBSERVED
ExitPlanMode rows). Empirical root cause: an amux daemon whose ancestry
includes a Claude session inherits Claude Code's child-session marker
set (`CLAUDE_CODE_CHILD_SESSION=1`, `CLAUDECODE=1`, `CLAUDE_PID`,
`CLAUDE_CODE_SESSION_ID`, …) and leaks it to every claude it spawns;
the child sees itself as a nested session and turns transcript saving
off. Confirmed by `ps eww` on daemon + spawned claude, and by
disassembling the claude 2.1.228 binary's env-builder and suppression
check.

### Changes
- `crates/amux/src/agents/pty.rs`: `spawn_pty_agent` grew an
  `env_remove` param; new `apply_env` helper (env_remove then env).
- `crates/amux/src/agents/claude/session/core.rs`: at the Claude spawn
  seam, scrub `CLAUDE_CHILD_SESSION_ENV_SCRUB` (explicit list, not a
  `CLAUDE_*` wipe — `CLAUDE_CONFIG_DIR` must survive) and set
  `CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1`. Unit-tested.
- `crates/amux/src/agents/test_agent.rs`: pass `&[]` for env_remove.
- `crates/amux/tests/capture/`: opt-in real-Claude capture harness
  (`harness=false`), driving keystrokes through the structured input
  seq-guarded path; scenario redaction with a fail-loud verify pass.
- `crates/amux/tests/fixtures/chat-v1/`: 9 redacted, provenance-stamped
  baseline fixtures (pong, tools, permission, question single/multi,
  interrupt, plan approve/reject, compact) + README.

### Decisions Made
- Scrub AND force (not force alone): force fixes persistence, but the
  leaked SESSION_ID/PID/MESSAGING_SOCKET point the child at the parent
  session's identity/IPC — scrubbing is the correct hygiene regardless.
- Poisoned-daemon-by-default in the harness: every capture run doubles
  as a live regression test of the scrub (empty capture ⇒ scrub broke).
- Explicit scrub list, not a prefix wipe: user config like
  `CLAUDE_CONFIG_DIR` must survive.

### Verification
- Unit: env-scrub + force assertions (core.rs), apply_env (pty.rs).
- Live: `ps eww` of the spawned claude showed the markers gone and
  `FORCE=1` set; all 9 poisoned-daemon capture runs produced rows and
  `amux.transcript_ready`.
- Gate: fmt clean, clippy (`--all-targets --features testnet`) clean,
  `cargo test -p amux --lib` 400 passed, spec suite 44 passed.
- Codex review (5 findings, all accepted & fixed before this commit):
  redaction now drops config-bearing attachment rows
  (skills/agents/MCP-tool inventory) that had leaked the owner's Claude
  config — profile isolation breaks auth on macOS (keychain is config-dir
  gated, verified), so the fix strips at capture time + fails-loud verify;
  question_multi asserts every selection + the Other value (encoding solved:
  Space commits the Other checkbox); tools asserts recorded Edit+Bash rows;
  harness holds the daemon child in a kill-on-drop guard across startup;
  plan_approve stops at the ExitPlanMode resolution (264s→26s). All 9
  fixtures recaptured through the new redaction; leak sweep = 0.

### Spec drift recorded (notes/chat-v1/transcript-semantics.md §18a)
- Plan approve (manual) does NOT emit a `permission-mode` change —
  contradicts the docs-sourced C3/§18 rule; approval FACT is the
  tool_result success + canonical content. Rejection = `is_error:true`.
- AskUserQuestion `answers` keyed by question TEXT, not header;
  options are `{label,description}` objects.
- Transcript file is created lazily on the first turn, not at
  SessionStart. docs/CHAT.md deltas queued for the orchestrator in
  notes/chat-v1/phases/00-report.md.

### Next Steps
- Phase 1 (Claude layer fold) can read these fixtures raw.
- Phase 3 owns the multi-select joined-selection encoding (captured
  structurally here but the exact keystroke table is unconfirmed) and a
  mode-cycle capture for D4.

## 2026-08-11: Chat V1 spec (docs/CHAT.md)

### Summary
docs/CHAT.md lands: the normative V1 spec for the structured chat
view over `claude_pty_transcript_v1` — vocabulary (feed / ask /
composer / phase / reader), modes and entry, feed semantics grounded
in a transcript evidence survey with fact-vs-inferred discipline
throughout, the ask lifecycle, principle-derived keybindings (guarded
Ctrl+C, no Alt ever, readline in text fields), diff/artifact
rendering, seven 80-col wireframes, and the real-Claude E2E suite.
Designed from first principles with UX studies of Codex CLI and
opencode plus a transcript semantics survey (working material in
notes/chat-v1/, gitignored per convention). Implementation is phased
in notes/chat-v1/orchestration.md.

### Changes
- docs/CHAT.md — new; companion to docs/UI.md.

### Decisions Made
- The chat is a typed Claude layer over native transcript rows (no
  IR). Asks have two kinds — permission and question; plan review is
  a permission variant with a fullscreen reader.
- Shipped default open mode: raw attach; chat is one setting away in
  the standard config; mobile clients are chat-only.
- Keybindings derive from ten recorded principles: Esc never answers
  or interrupts; interrupt is Ctrl+X alone; Ctrl+C is the guarded
  abandon key (clears a non-empty field as a yankable kill; two-press
  rendered quit when empty), one rule chrome-wide.
- UI.md's deferred content-windowing decision is resolved for the
  chat milestone: window = bounded source tail, relink = epoch.
- Diffs render precomputed `structuredPatch` facts; the only diff
  ever computed client-side is the ask-time preview (`similar`, in
  the fold), numberless and estimated — ask-time hunks do not exist.

### Verification
- Prose spec only; wireframes machine-checked at exactly 80 columns;
  all 34 requirement IDs traceable to their sections. The executable
  half arrives with the phased implementation.

### Next Steps
- Phase 0 (notes/chat-v1/orchestration.md): transcript-persistence
  bug fix + baseline fixture capture; overnight autonomous run.

---

## 2026-08-10: Panic-hook recorder dump

### Summary
Panics now leave a Msg recording. The Runtime's recorder became
`Arc<StdMutex<Recorder>>` (single-threaded fold — contention nil; the
mutex exists for the hook, and a poisoned lock is entered via
`PoisonError::into_inner` so a panic mid-record cannot block the
hook). `Runtime::install_panic_dump()` registers (recorder, dump dir,
BUILD) in a process-global OnceLock; the free function
`amux_ui::write_panic_dump(detail)` reads it and writes a
`DumpReason::Panic` bundle, returning quietly on any error. The
amux-tui panic hook calls it AFTER `restore_now()` (a dump is
worthless if writing it destroys the panic report), and `amux ui`
installs it right after `Runtime::start`. This completes the
dump-trigger set from the spec: tripwire, overflow-reserved
(`ChannelOverflow`), panic, and user-requested (`C-g`).

### Changes
- `crates/amux-ui/src/runtime.rs` — shared recorder; `lock_recorder`
  (poison-tolerant) + `dump_stamp` helpers; `install_panic_dump`;
  `write_panic_dump`; unit test.
- `crates/amux-ui/src/lib.rs` — export `write_panic_dump`.
- `crates/amux-tui/src/terminal.rs` — panic hook: restore, then
  `write_panic_dump(&info.to_string())`, then the previous hook.
- `crates/amux-cli/src/ui.rs` — `runtime.install_panic_dump()`.

### Decisions Made
- A process-global OnceLock, not a hook re-registration: terminal.rs
  owns hook installation order (restore FIRST); amux-ui only exposes
  the write. One Runtime per client process makes the single global
  slot honest.
- The test exercises the hook's dump path without panicking (build a
  Runtime with a tempdir dump dir, record Msgs, install, call
  `write_panic_dump`, assert the file and its header) — actually
  panicking in tests buys nothing and poisons test output.

### Verification
- New unit test: `runtime::tests::write_panic_dump_writes_a_dump_after_install`.
- fmt + CI clippy clean; amux-ui/amux-tui/amux-cli suites, the 44-test
  spec suite, and e2e-runner 14/14.

### Next Steps
- `amux debug ui-dump` (IPC-triggered dump of a running TUI) remains
  deferred.

---

## 2026-08-10: Model invariants checked at the fold seam

### Summary
`Model::check_invariants()` reports structural incoherence as typed,
entity-addressed `Violation` values, and the Runtime enforces it after
every fold: panic in debug builds, dump-once-per-violation-kind
(`DumpReason::Tripwire`, detail `invariant: …`) plus `tracing::warn`
in release — never a release panic, folding continues. This is the
third assertion class, distinct from input tripwires (refuse + dump at
the receiving reducer arm) and renderer staleness (tolerance/clamps,
never assertions); docs/UI.md's Testing section now draws that
three-way line.

### Changes
- `crates/amux-ui/src/model.rs` — `Violation` (kind() throttle key +
  Display) and `check_invariants()`: streams ⊆ agents; card/host
  epochs never ahead of the model epoch; when synchronized, all card/
  host epochs equal it; `finished_ops` within retention; cached
  `card.attention` equals its summarizer's derived attention. Doc
  comment carries the discipline rule: invariants range over the
  structural index (ids, counts, phases, epochs), never content —
  O(entities) forever.
- `crates/amux-ui/src/runtime.rs` — `enforce_invariants()` runs in
  `process()` after `update` AND after the effects loop;
  `reported_violations: HashSet<&'static str>` throttles release
  dumps. Debug-only shell companion (post-effects, where a CloseStream
  decided by the same fold has already executed): every live stream
  task key exists in `model.streams`.
- `crates/amux-ui/src/lib.rs` — export `Violation`.
- `crates/amux-ui/tests/spec/wire_free.rs` — the differential wrapper
  additionally asserts `check_invariants().is_empty()` after every Msg
  of every registered sequence.
- `docs/UI.md` — Testing: the three-way assertion line.

### Decisions Made
- Checked at the seam (shell), not inside `update`: the reducer stays
  a pure fold; coherence enforcement is shell policy, like dumping.
- Reused `DumpReason::Tripwire` rather than adding a variant — the
  meaning ("state the protocol says cannot happen") is the same; the
  detail string distinguishes fold-seam invariants.
- Both check points matter: after `update` catches the fold that broke
  the Model; after the effects loop catches shell bookkeeping drift
  and is where the task-map companion is race-free.

### Verification
- New unit tests (model.rs): `detects_stream_without_agent`,
  `detects_epochs_ahead_of_the_model`,
  `detects_stale_epochs_while_synchronized`,
  `detects_finished_ops_over_retention`,
  `detects_attention_disagreeing_with_the_summarizer` — each corrupts
  a fold-built Model directly and asserts its class fires.
- fmt + CI clippy clean; amux-ui/amux-tui/amux-cli suites and the
  44-test spec suite pass.

### Next Steps
- None; the invariant list grows only with new structural state.

---

## 2026-08-10: Restore the terminal on SIGINT/SIGTERM/SIGHUP

### Summary
The chrome now restores the terminal when killed by SIGINT/SIGTERM/
SIGHUP, closing the gap `docs/UI.md` already scoped ("nothing survives
SIGKILL" — but the catchable signals should not leave a raw-mode alt
screen either). A `signal_restore` module in `amux-tui/src/terminal.rs`
installs an async-signal-safe low-level handler (via `signal-hook`)
that restores the cooked termios saved before raw mode first engaged
(`tcsetattr`), writes the leave-alt-screen/show-cursor bytes with a raw
`write`, then re-raises the default disposition so the process still
dies exactly as expected. Installed once from `install_panic_hook`, so
every chrome entry point gets it.

### Changes
- `Cargo.toml` — workspace dep `signal-hook = "0.3"`.
- `crates/amux-tui/Cargo.toml` — `[target.'cfg(unix)'.dependencies]`
  libc + signal-hook.
- `crates/amux-tui/src/terminal.rs` — `signal_restore` module
  (cfg(unix)); `install_panic_hook` calls its `install()`; unit test
  `signal_restore_bytes_match_write_restore` locks the handler's
  hardcoded `RESTORE_BYTES` to what `write_restore` (crossterm)
  actually emits.

### Decisions Made
- Not a tokio signal arm in the chrome loop: the handler must cover
  the mid-attach passthrough phase, where nothing polls the event
  stream — a stream-based handler would leave the process deaf to
  signals exactly when the terminal is rawest. The low-level handler
  works process-wide regardless of what the main thread is doing.
- The restore is deliberately unconditional (no CHROME_OWNS_TERMINAL
  gate): on a sane terminal it is a no-op, and that is what lets one
  handler cover both chrome mode and mid-attach.
- Signals covered: SIGINT, SIGTERM, SIGHUP. SIGQUIT is left at its
  default (core dump semantics stay untouched); SIGKILL is
  uncatchable by definition.

### Verification
- `signal_restore_bytes_match_write_restore` passes; fmt + CI clippy
  clean; full per-crate suites and the 44-test spec suite pass;
  e2e-runner 14/14.
- Automated signal-delivery testing is deliberately absent: kill-based
  tests of terminal state are flaky by construction. One manual
  `kill -TERM` verification on a real terminal is still pending.
- Known remaining gap (pre-existing, milder): CLI-only `amux attach`
  (never entered the TUI) does not install the handler, so a signal
  mid-passthrough there still leaves the terminal raw.

### Next Steps
- Manual `kill -TERM` check during chrome and mid-attach on a real
  terminal.

---

## 2026-08-10: TUI V1 review fixes — resize guard, stream lifecycle, scroll clamp

### Summary
Fixed the code-review findings on the V1 TUI. P1: the fleet renderer
underflowed `width - RIGHT_INFO_FROM_EDGE` below ~13 columns (debug
panic / huge `pad_to` allocation in release) — the below-minimum guard
now covers it and a sweep test renders every viewport 1..=200 ×
1..=60. P2s: late stream events for removed agents no longer
re-materialize `Model::streams` ghosts; `prune_if_synchronized` emits
`Effect::CloseStream` for every stream it drops so reconnect pruning
no longer orphans shell tasks; and render clamps stale scroll/selection
against the rows it is actually drawing, so a subscription-driven fleet
shrink cannot draw an empty list until the next keypress. Minors:
finished stream `JoinHandle`s are dropped when their `Closed` Msg is
observed, the dead `Effect::ScheduleTick` vocabulary is deleted, and
the Claude notification permission/question string-match is marked as
a known-fragile seam.

### Changes
- `crates/amux-tui/src/render.rs` — `MIN_FRAME_WIDTH` guard
  (too-small notice below 13 cols); render-time clamp of stale
  `view.scroll`/`view.selected` (formatting a stale ViewState against
  the Model, not deciding — render stays pure).
- `crates/amux-ui/src/update.rs` — `update_stream` discards events for
  agents absent from `model.agents` (all arms);
  `prune_if_synchronized` returns `CloseStream` effects, propagated
  from both synchronized arms (skipping already-Closed streams,
  matching the `AgentRemoved` arm).
- `crates/amux-ui/src/runtime.rs` — `process` drops an agent's stream
  `JoinHandle` on an observed `Closed` Msg when the task is finished
  (shell resource bookkeeping; `is_finished` guards against a stale
  Closed racing a newer OpenStream); `ScheduleTick` executor arm gone.
- `crates/amux-ui/src/effect.rs` — `Effect::ScheduleTick` deleted
  (nothing emitted it; the TUI drives ticks via its own interval +
  `observe_now`).
- `crates/amux-tui/src/run.rs` — ticker comment: the interval always
  fires, only the repaint is gated (deliberate V1 simplification).
- `crates/amux-ui/src/summarizers/claude.rs` — KNOWN-FRAGILE SEAM
  comment on the notification-text permission/question split.
- Tests: `sessions::late_stream_events_after_removal_leave_no_ghost_state`,
  `connection::epoch_prune_emits_close_stream_for_dropped_streams`
  (both sequences registered, so the wire_free differential wraps
  them), golden `fleet_too_narrow` (new frame; existing frames
  untouched), `rendering_never_panics_at_any_viewport_size`,
  `stale_scroll_after_fleet_shrink_clamps_at_render`.

### Decisions Made
- Below-minimum widths reuse the existing "amux: terminal too small"
  state rather than growing a second degraded layout; the new
  `fleet_too_narrow` golden locks it as a byte contract.
- Discarding late stream events keys on `model.agents` membership,
  which restores the invariant `streams ⊆ agents` everywhere (Opened
  inserts only for known agents; removal and prune delete both sides).
- Handle cleanup on `Closed` only removes finished tasks: best-effort
  cleanup that can never discard a live task's handle.

### Verification
- `cargo +nightly fmt --all`; `timeout 600 cargo clippy --workspace
  --all-targets --features amux/testnet -- -D warnings` clean.
- `timeout 600 cargo test -p amux-ui` (28 passed), `-p amux-tui`
  (19 passed), `-p amux-cli` (52 passed), `-p amux --features testnet
  --test spec` (44 passed).

### Next Steps
- The stale-Closed-vs-new-OpenStream reducer race (stream Msgs carry
  no generation id, so a queued Closed can overwrite a reopened
  stream's phase) is noted but out of scope here.

---

## 2026-08-09: TUI V1 M4 — attach round-trip (the spine)

### Summary
`enter` on a fleet row now runs the real passthrough: the chrome leaves
the alternate screen and restores termios (RAII guard from M3), the
existing `session_client` passthrough runs in-process on the real
terminal, and on detach the chrome re-enters and repaints from the
Model. `<leader>d` and `<leader>s` both detach (the fleet IS `s`'s
target in V1). The passthrough was reused, not rewritten: the leader-
scanning stdin reader and select loop moved intact into
`spawn_stdin_reader`/`attach_loop`, now parameterized over injected
input events and an output writer so the tier-2 suite drives them
without a terminal; the CLI `amux attach`/`amux new` paths keep their
exact print-and-exit behavior via `finish_cli_attach`.

### Changes
- `crates/amux-cli/src/session_client.rs` — `AttachOutcome`,
  `subscribe_raw`, `spawn_stdin_reader` (+`s` chord, +stdin reclaim),
  `attach_loop` (generic writer), `attach_terminal`, `attach_for_ui`;
  tier-2 `mod attach` test suite (embedded daemon + pty `cat` agent).
- `crates/amux-tui/src/run.rs` — attach callback now returns an optional
  status-line notice; outcomes surface in the fleet status line.
- `crates/amux-cli/src/ui.rs` — wires `attach_for_ui` as the handoff.

### Decisions Made
- Stdin reclaim: the blocking stdin reader cannot be killed portably
  mid-read, so when a session ends WITHOUT a detach chord (agent exited,
  killed, daemon shutdown) the TUI path prints
  `[<label> — press any key to return to the fleet]` and the next
  keypress is consumed to hand stdin back exclusively before the
  chrome's event stream reads again. Detach chords need no prompt (the
  reader exits as part of the chord). CLI paths never reclaim — the
  process exits, as today.
- Tier-2 tests use the embedded in-process daemon (same pattern as
  amux-ui's runtime test) rather than the multi-daemon testnet: the
  testnet's client accessors are pub(crate) and attach is single-daemon
  work; a real daemon plus a real pty test-agent is the tier-2 contract.
  Test names live under `session_client::attach::*` because amux-cli is
  a binary crate (no lib target for integration tests to import).
- Named tests: `attach::round_trip_repaints_fleet` (attach → echoed
  output → detach → fleet repaints from Model → attach again → 100
  scripted attach/detach cycles), `attach::detach_leaves_terminal_sane`
  (vt100 over the real enter/restore byte sequences: alternate screen
  entered and left, cursor re-shown),
  `attach::kill_during_attach_still_restores_the_terminal` (delete the
  agent mid-attach; the loop reports the close instead of hanging, and
  the restore sequence still yields a sane vt100 screen),
  `attach::offline_host_shows_dial_error_instead_of_attaching` (enter on
  an offline host surfaces `last_dial_error` in the status line, no
  attach).
- Late attach relies on the existing buffer replay unchanged
  (`replay_query: None`); how well real agent TUIs reconstruct was not
  visually verified in this environment — the SIGWINCH-wiggle fallback
  remains the known remedy, out of V1 scope.

### Verification
- `cargo +nightly fmt --all`; CI clippy invocation clean.
- `timeout 600 cargo test -p amux-cli` — 52 passed (incl. the four
  attach tests; the 100-cycle loop runs in ~0.1s against the embedded
  daemon). `timeout 600 cargo test -p amux-ui` — 26 passed;
  `-p amux-tui` — 16 passed; protocol spec suite 44 passed.
- Non-TTY smoke: `amux ui` without a terminal fails gracefully
  ("Device not configured"), no panic, no partial terminal state.
- Interactive feel ("the loop feels instant") not verifiable headless;
  the tier-2 round trip plus terminal-hygiene bytes are the executable
  stand-in. Real-terminal pass recommended at review.

### Next Steps
- M5 (naming translation cleanup) deliberately not picked up.
- Backlog: recorded claude-code fixture to supplement the authored M2
  ones; `amux debug ui-dump` IPC; Windows CI leg for amux-tui.

---

## 2026-08-09: TUI V1 M3 — amux-tui fleet screen + golden frames

### Summary
New library crate `crates/amux-tui` (ratatui + crossterm), consumed by
`amux-cli`: bare `amux` (and `amux ui`) opens the fleet. The renderer is
a pure function of (Model, ViewState, FrameContext) building the whole
chrome as styled text lines, so the tier-3 golden frames control every
cell; the checked-in frames reproduce the spec mockups verbatim
(byte-compared in this session). Event loop is event-driven — drain,
fold, draw once, dirty-flag gated, ticks only while relative ages are on
screen. Alt-screen only, RAII terminal guard with a chained panic hook
that restores before reporting.

### Changes
- `crates/amux-tui/src/{lib,render,view,keys,run,terminal}.rs` — new.
- `crates/amux-tui/tests/golden.rs` + `tests/golden/*.txt` — all named
  frames: fleet_ranked, fleet_attention_badges, fleet_offline_host_rows,
  fleet_cloud_auth_banner, fleet_empty_no_agents, fleet_daemon_starting,
  fleet_daemon_unreachable (extra), picker_filtered, row_rename_inline,
  delete_confirm_statusline, op_pending_and_failed, help_overlay,
  fleet_ranked_80col/60col (column-collapse rule), plus a run-stability
  test and style assertions (badge colors, offline dim) that text
  goldens cannot see.
- `crates/amux-cli/src/ui.rs` + `main.rs` dispatch — bare `amux` runs
  init first on an uninitialized machine (CLI-owned auth; TUI stays
  auth-passive), then opens the fleet; the connector wraps `get_client`
  so daemon spawn shows as the "Starting daemon…" state. `amux ui` is
  the explicit alias.
- `crates/amux/src/{setup,identity}.rs` — minimal read-only plumbing:
  `setup::local_host_id[_in]()` reads the stored identity's host id
  (the wire does not mark the local host); with test.
- `crates/amux-ui` — Model grew `effective_attention`/`status_label_for`
  (offline hosts degrade display to Unknown, computed once for every
  renderer) and fleet ranking now uses effective attention; summarizer
  routing keys on the advertised `claude_pty_transcript_v1` protocol
  fact instead of the `agent_type` string; op seq is 1-based; kernel
  entity re-exports widened (Capabilities, HostTrustStatus).

### Decisions Made
- Whole-frame text rendering (one Paragraph of spans) instead of nested
  ratatui widgets: the mockups are the contract, and byte-level control
  is what makes "verbatim" testable.
- Spec-mockup inconsistency flagged (NOT silently deviated): the rename
  and pending-row mockup lines place age/status at columns 46/52 while
  every fleet-frame row uses 48/54. The renderer uses the fleet grid
  consistently; those two golden lines differ from the mockup by that
  2-column shift only. All other mockup lines match byte-for-byte.
- The attach handoff seam is in place (`run_fleet` takes an async attach
  callback; chrome restores the terminal before calling it and resumes
  after) but amux-cli passes a no-op until M4 wires the passthrough.
- Rename mode hides the `▸` marker on the edited row (the draft cursor
  `▌` marks focus) — matches the mockup.
- Column collapse: the status-word column drops below 68 columns; key
  hints drop when they no longer fit. Locked by the 80/60col frames.
- Debug-dump keybinding is `C-g`; `amux debug ui-dump` (spec-listed)
  needs an IPC surface to reach a running TUI's ring and is deferred
  with a note — the keybinding plus tripwire/panic/overflow triggers
  cover V1.
- Layout math counts chars (fixtures are width-1 glyphs); wide-glyph
  names will mis-pad until a unicode-width pass — accepted for V1.

### Verification
- `cargo +nightly fmt --all`; CI clippy invocation clean.
- `timeout 600 cargo test -p amux-ui` — 26 passed; `timeout 600 cargo
  test -p amux-tui` — 16 passed (goldens stable across two runs);
  protocol spec suite 44 passed; `amux --help` lists `ui`.
- `fleet_ranked` golden byte-compared against the spec mockup: verbatim
  match; all other spec frame lines verified present verbatim except the
  two flagged 46/52-column lines.
- Windows leg NOT verifiable from this mac: `cargo check -p amux-tui
  --target x86_64-pc-windows-msvc` fails building `ring`'s C code (no
  Windows-target C toolchain locally). amux-tui itself has no
  platform-specific code; CI's Windows runner builds ring natively.

### Next Steps
- M4: attach round-trip — reuse `session_client` passthrough in-process,
  terminal hygiene tests over the vt100 harness.

---

## 2026-08-09: TUI V1 M2 — attention (pure amux-ui, zero core changes)

### Summary
Attention lands as a pure per-agent fold with zero changes under
`crates/amux` (verified: empty diff). `summarizers/claude.rs` folds the
`claude_pty_transcript_v1` stream — raw transcript rows interleaved with
the three hook rows the claude session emits — into the kernel
`Attention` vocabulary, with the entry/clear table written exhaustively
in the module docs. The kernel subscription policy is in the reducer:
every local agent advertising the structured stream is subscribed (tail
1000, one policy constant), remote agents join on a reified
`Msg::UserAttached`. The shell grew the stream executor: per-agent tasks
that coalesce entries into batched Msgs before the recorder sees them
and derive the truncation fact from the first replayed seq.

### Changes
- `crates/amux-ui/src/summarizers/{mod,claude}.rs` — new; typed
  `SummarizerState` carried on the `AgentCard`.
- `crates/amux-ui/src/{update,model,msg,runtime,lib}.rs` — subscription
  policy (`ensure_stream`), attention folding on stream Msgs,
  `Msg::UserAttached`, `Effect::OpenStream/CloseStream` executor.
- `crates/amux-ui/tests/spec/attention.rs` + `tests/spec/fixtures/
  claude_{permission_flow,stop_and_notification,truncated_tail}.json`.

### Decisions Made
- Clearing rules: a permission answered through raw passthrough is only
  visible as subsequent activity — and a blocked agent emits nothing, so
  any activity row IS the unblock evidence. Only hook rows leave
  `Working`, so streaming output cannot strobe (hysteresis by
  construction rather than by timer).
- `ClaudeHookKind` enumerated: SessionStart/SessionEnd are
  internal-only, Unknown is dropped at the core — the stream carries
  exactly permission_request/stop/notification plus transcript rows.
- Question-vs-permission: hooks alone do not distinguish them; both
  arrive as Notification differing in text. The fold inspects the
  message ("permission" → Permission, else Question) — interpretation at
  observation time, resolving M2's open question in the spec.
- Honest degrade: truncation = first replayed seq > 1 (the source buffer
  is bounded, so this also covers server-side eviction). A truncated
  window with no attention-bearing rows reports `Unknown`; the same
  window over complete history reports `Idle`. Weak rows (`summary`,
  `system`, unknown shapes) carry no signal either way.
- Stream-loss degrade: transport-ish closes invalidate the fold to
  `Unknown`; orderly agent exit resets it (phase carries the exit).
  Retryable closes reopen on the next inventory upsert — bounded, no
  timer loops.
- Fixtures are authored (shapes from `agents/claude/session/hooks.rs` +
  the Claude Code transcript/hook schema) and say so in `capturedWith`.
  No live claude-code capture was available in this environment; the
  spec's intent is recorded-first, so replacing/supplementing these with
  a real redacted capture stays on the backlog.

### Verification
- `cargo +nightly fmt --all`; CI clippy invocation clean.
- `timeout 600 cargo test -p amux-ui` — 26 passed (25 tier-1 incl. the
  six named attention tests wrapped by the differential property, 1
  tier-2).
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44
  passed; `git diff crates/amux` empty (zero core changes).

### Next Steps
- M3: `amux-tui` fleet screen + golden frames; M4: attach round-trip.

---

## 2026-08-09: TUI V1 M1 — amux-ui becomes the reducer

### Summary
Replaced the placeholder `amux-ui` crate (1.1k lines, zero consumers)
wholesale with the reducer core from `docs/UI.md`: serializable `Msg`s
in, pure `update(&mut Model, Msg) -> Vec<Effect>`, a `Runtime` shell
that owns `amux::Client` and funnels every stimulus through one ordered
bounded channel, and a `Recorder` ring whose dumps are self-contained
JSONL replay bundles. Tier-1 spec suite (`tests/spec/`) with the
differential fold≡live property wrapping every chapter's sequences, plus
a tier-2 embedded-server integration test replacing the old ignored one.

### Changes
- `crates/amux-ui/src/{msg,model,update,effect,recorder,runtime,lib}.rs`
  — new crate body; old `{agent_cache,cmd,error,inventory,notification,
  runtime,session,types}.rs` deleted, no compat wrapper.
- `crates/amux-ui/tests/spec/{main,harness,connection,inventory,ops,
  sessions,wire_free}.rs` — tier-1 chapters; `tests/runtime.rs` — tier-2.

### Decisions Made
- Reconnect snapshot semantics: entities are epoch-tagged; the swap to
  the new snapshot happens at the synchronized marker (both hosts and
  agents snapshots complete), so stale rows stay visible during catch-up
  and renderers distinguish "loading" from "empty".
- `local_host_id` enters the Model via `ServerMsg::Connected` — the wire
  does not mark the local host, so the embedding client reads the device
  identity and hands it to the shell (wired up in M3; `None` degrades to
  no local-subscription policy, honestly `Unknown` attention).
- OpIds are minted by the shell (`Uuid::new_v4`) and enter via
  `Msg::Command`; epochs are minted by the reducer (increment on
  `Connected`) — both deterministic under replay.
- The recorder checkpoint advances by folding evicted Msgs through the
  same pure `update`, so `checkpoint + ring` always reproduces the live
  Model; dumps are 0600, retention-bounded (20 files), never uploaded.
- `cloud_auth_required` is Model state set by auth-required op/stream
  failures (mapped from `ProtocolError::InvalidCredentials`), cleared on
  reconnect; a connection-level credential failure maps to
  `DisconnectReason::AuthenticationRequired`. Degraded banner, never a
  blocking screen.
- `Effect::OpenStream`/`CloseStream` are defined but the reducer does
  not emit them yet — the subscription policy is M2's first bullet; the
  shell executor lands with it.
- Fleet ranking: `NeedsYou` first (permission, question, finished), then
  recency, id as the deterministic tiebreak; pending creates render as
  optimistic rows at the bottom in dispatch order.
- `AgentPhase::Suspended` was not modeled: nothing on the wire reports
  it (suspended agents leave inventory), and inventing it would violate
  facts-only. `exited(N)` derives from stream-close facts.

### Verification
- `cargo +nightly fmt --all` (repo formats with nightly rustfmt).
- `cargo clippy --workspace --all-targets --features amux/testnet -- -D
  warnings` clean.
- `timeout 600 cargo test -p amux-ui` — 18 passed (17 tier-1 spec, 1
  tier-2 embedded-server integration).
- `timeout 600 cargo test -p amux --features testnet --test spec` — 44
  passed (protocol suite untouched).

### Next Steps
- M2: attention summarizer fold + subscription Effect policy.
- M3: `amux-tui` fleet screen + golden frames; M4: attach round-trip.

---

## 2026-08-10: `q quit` in the hint bar; Ctrl-C quits the chrome

### Summary
UX pass #3. The status-line hints gain `q quit` (HINTS_COL 31 → 25 so
the longer string still fits the 68-col frame), and Ctrl-C now quits
the chrome from ANY mode — the chrome is a stateless viewer and must
never feel like it traps the terminal (`esc` cancels modes). First
intentional run of the golden regeneration workflow: 11 frames
regenerated with UPDATE_GOLDENS=1 and diff-reviewed — every change is
the hint shift/addition plus two help-overlay lines (`q / C-c  quit`;
`C-a d` relabeled "detach to shell" post-chord-split). Verified the
passthrough is untouched: `handle_key`'s only production caller is the
chrome loop (unreachable while attached), the stdin reader intercepts
leader chords only (0x03 forwards to the agent as always), and Claude's
interrupt injection (`session/core.rs` StopPolicy) is server-side.

### Decisions Made
- Ctrl-C is global quit even mid-rename/filter; esc is the cancel key.
- Backlog (noted, not fixed): an external `kill -INT` terminates the
  chrome without unwinding (keyboard cannot produce SIGINT in raw
  mode), skipping terminal restore — a signal handler running the same
  restore would close it; parked, vanishingly rare. Also candidates,
  discussed but NOT committed to: a `DumpReason::Panic` recorder dump
  from the panic hook (cheap, fires exactly when a recording matters
  most — do opportunistically), and a `Model::check_invariants()`
  harness pass (streams ⊆ agents, epoch bounds, attention ==
  summarizer-derived). The invariant checker is an oracle without a
  generator today — hand-authored sequences only reach "normal" states
  — so it waits for whichever arrives first: fuzzed Msg sequences, the
  chat-milestone Model growth, or a second shipped state-lifecycle bug.

### Verification
- `timeout 600 cargo test -p amux-tui` 21 passed (19 + 2 new key
  tests: `ctrl_c_quits_from_any_mode` incl. filter/rename where `c` is
  text input, `q_quits_in_normal_mode`); goldens stable post-regen.
- `-p amux-ui` 29; `-p amux-cli` 53; e2e 14/14; CI clippy clean.

---

## 2026-08-10: Readonly agents hidden from the fleet

### Summary
UX pass #2: readonly agents (externally captured sessions the chrome
cannot drive) are hidden from the fleet until the structured chat view
can render them. Per "views format, never decide" the filter lives in
`Model::fleet()`; a new `fleet_agent_count()` keeps the header, empty
state, and ticker consistent with the visible list (`agent_count()`
stays the honest entity total); and the subscription policy skips
readonly agents — a badge nobody can see is not worth a stream.

### Verification
- `timeout 600 cargo test -p amux-ui` 29 passed incl.
  `inventory::readonly_agents_are_hidden_from_the_fleet` (visibility,
  counts, and no `OpenStream` in one sequence, wrapped by the
  differential property); `-p amux-tui` 19 (goldens untouched);
  `-p amux-cli` 53; e2e 14/14; CI clippy clean.

---

## 2026-08-10: `<leader>d` detaches to the shell; `<leader>s` is the fleet

### Summary
First UX-pass fix: V1 had collapsed both chords into return-to-fleet
(noted in the M4 entry); real usage immediately surfaced the tmux
muscle-memory expectation — `d` means the shell. The chords now split
all the way through: `StdinEvent::{Detach,SwitchToFleet}`,
`AttachOutcome::{Detached,SwitchedToFleet}`, and `attach_for_ui`
returns a typed `amux_tui::AttachReturn::{Fleet(notice),Exit}` that
`run_fleet` honors (`Exit` ends the TUI; the terminal was already
restored by the handoff). CLI `amux attach` treats `s` like detach
(no fleet to return to — opening the TUI from there is a future
nicety). The help overlay already documented these semantics, so no
golden changes: behavior caught up with the documentation.

### Verification
- `timeout 600 cargo test -p amux-cli` 53 passed (round-trip second leg
  now locks `s`→`SwitchedToFleet`; first leg and the 100-cycle loop
  still lock `d`→`Detached`; new `leader_chords_split_detach_from_fleet`).
- `-p amux-tui` 19, `-p amux-ui` 28, e2e 14/14, CI clippy clean.

---

## 2026-08-10: Bare `amux` keeps printing help off-TTY; e2e green again

### Summary
First TUI push turned the e2e leg red on both unix runners: `bare_help`
runs bare `amux` in a context without a usable TTY, where the new
open-the-fleet dispatch errored (ENXIO) instead of printing help. Bare
`amux` now checks `IsTerminal` on stdin+stdout: real terminal → fleet
TUI (init-first unchanged); scripts/pipes → top-level help, exactly the
pre-TUI behavior (`amux ui` still errors honestly off-TTY). The
`bare_help` expected output also needed the new `ui` command line.
Process lesson recorded: the local review battery covered every cargo
suite but not `cargo run -p e2e-runner -- run` — e2e belongs in the
pre-push battery for anything touching the CLI surface.

### Verification
- `timeout 600 cargo run -p e2e-runner -- run`: 14 passed, 0 failed.
- CI run on the fix watched to completion (see push).

---

## 2026-08-10: CI hardening for the TUI suites before first push

### Summary
CI's test job already runs `cargo test --workspace --all-targets` on all
three OSes, so the new amux-ui/amux-tui/amux-cli suites are covered with
zero workflow changes — but two Windows hazards needed closing first.
The three PTY-backed tier-2 tests (`attach::round_trip_repaints_fleet`,
`attach::kill_during_attach_still_restores_the_terminal`,
`runtime_reflects_daemon_state_in_the_model`) get the existing ConPTY
ignore gate (same attribute and reason as the embedded suite; the pure
byte-sequence and no-agent attach tests stay ungated everywhere). New
`.gitattributes` pins LF on the tier-3 golden frames so Windows
checkouts cannot CRLF-translate byte-contract fixtures.

### Verification
- `cargo +nightly fmt --all` clean; `timeout 600 cargo test -p amux-ui`
  28 passed, `-p amux-cli` 52 passed, `-p amux-tui` 19 passed (macOS —
  gates are windows-only, nothing ignored here).

---

## 2026-08-09: Client-layer design — docs/UI.md, external review, V1 TUI spec

### Summary
Designed the client/UI layer and committed it as `docs/UI.md` (fourth
committed doc; CLAUDE.md repointed). Core decisions: `amux-ui` is a
reducer core — serializable Msgs in, pure `update`, Effects out to a
shell — with CQRS edge vocabulary (`Command` in, entity-keyed idempotent
`Delta` out, `Effect` internal, `Ephemeral` reserved/uninhabited); a
kernel of protocol facts plus typed per-agent layers (no generic agent
IR, no capability flags); the core↔UI boundary as three verbs (core
transports and translates facts, never interprets — attention is a pure
per-agent fold at observation time, zero core changes); chrome-first TUI
(fleet around existing raw passthrough, alt-screen only, no scrollback
writes, no split panes ever); four test tiers with a differential
fold≡live property as the load-bearing test, plus a Msg recorder whose
dumps replay as tier-1 regressions.

The draft was reviewed externally before revision: a GPT 5.6 Sol xhigh
design review (which read core source) plus three Opus agents comparing
against openai/codex, sst/opencode, and pingdotgg/t3code. The two
central rejections turned out to be empirical — t3code and opencode
shipped the generic IR and per-client folds and the predicted failure
modes are visible in their trees; t3code independently converged on the
same reducer architecture. Revisions from the review: scoped and
enforced determinism guarantee, authority rule (subscriptions are the
sole entity writer; RPC results resolve ops only), per-Msg flow classes
with loud overflow, retention rule (never evict live obligations),
cloud-auth expiry as a degraded banner rather than a blocking screen,
unknown-agent attach reduced to card-only pending an agent-independent
raw terminal protocol. Full findings: notes/ui-review-findings.md
(gitignored); V1 build spec with milestones, named tier-1 tests, and
aligned golden-frame mockups (flat globally-ranked fleet):
notes/tui-v1-spec.md (gitignored).

### Changes
- `docs/UI.md`: new — normative client-layer design.
- `CLAUDE.md`: points at docs/UI.md.

### Decisions Made
- amux-tui will be a library crate invoked by amux-cli (one shipped
  binary); bare `amux` opens the TUI, running the init flow first on an
  uninitialized machine (CLI dispatch — the TUI stays auth-passive).
- V1 attention subscribes local agents only (+ interacted-with
  remotes); ClientService-side stream dedup/replica is a deferred,
  seam-protected optimization.
- Naming cleanup (`provider_label` translation, provenance deletion) is
  a lightly-held candidate chunk, decided at pickup.

### Verification
- Docs-only. Spec suite green on current toolchain (44 passed, see the
  catch-up entry below).

---

## 2026-08-09: Catch-up — two June commits landed without entries

### Summary
Work paused ~6 weeks after 2026-06-28; two commits predate the break and
have no entries. `2e5f9d8` (2026-06-20) added QR pairing deeplinks
(production `amux://pair` links from `pair --qr`, debug-only `--link`
helper for simulator pairing) and tucked in two behavior changes
documented only in the commit message: untrusted cloud pairing
candidates no longer trigger eager trusted-tunnel activation, and remote
agent-event subscriptions are gated on trusted hosts. `baaebc3`
(2026-06-28) serialized CLI refresh-token rotation
(`DeviceFlowProvider` refresh lock in `amux-cli/src/auth.rs`),
coordinated same-day with amuxapp ("Handle rotating runtime
credentials") and amuxcloud ("Test rotating refresh token clients").

### Verification
- Spec suite re-verified green on the current toolchain 2026-08-09:
  44 passed / 0 failed / 0 ignored, 32.3s (the 44th is the presence
  spec test added by `2e5f9d8`).

---

## 2026-06-17: Cleanup-review pass over the client-only/seam effort

### Summary
Reviewed the full client-only → seam effort (`f90a8de`..`1925197`) for
vestigial code and stale references left by the superseded passes (the
`LocalAgentHost` trait replaced the earlier "Design Y" gating). The
codebase came back clean: no dead code, every suspect (`local_agent_count`,
the dual agent-event subscribe paths, `is_cloud_server`, `DebugAgent`) is
still reached in at least one feature config, and the ~21 residual
`local-agents` cfgs are all module/re-export/construction/capability gates.

One stale reference fixed: the iOS `compile_error!` in `lib.rs` still told
callers to use the `client-only` feature, which was deleted in `40b426b`.
Corrected to "depend on amux with `default-features = false`."

### Verification
- `cargo build -p amux` (default): clean.

---

## 2026-06-17: Self-update poll is a desktop-daemon concern, not embedded

### Summary
Fixes a regression from the 2026-06-16 "decouple daemonish" change: that
commit gated the periodic self-update poll behind a `with_daemon_tasks`
flag and conflated it with reachability, which broke the
`embedded_server_runs_update_checker_when_reporter_is_configured` test and —
more importantly — encoded the wrong model. The active manifest poll checks
for a newer amux *binary* to install; a mobile client can't self-update (the
app store owns its binary), so it must never poll. A too-old mobile client is
instead told to update *over the cloud connection*: a relay rejects
under-version clients (`Config::minimum_client_versions`) and the client
surfaces `UpdateStatus::Required` to its `update_reporter`
(`services/startup/cloud.rs`) — that lib path is already wired; acting on the
signal (forcing an app-store update) is the host app's job and is not yet
implemented in the mobile app.

Split `spawn_local_background_tasks` (cloud connection — every local host)
from a new `spawn_daemon_background_tasks` (reachability links + update poll —
desktop daemon only). The `with_daemon_tasks` flag is gone; the embedded path
simply never calls the daemon-only function.

### Changes
- `server.rs`: split the two task groups; deleted the `with_daemon_tasks`
  flag; documented the poll-vs-enforcement distinction.
- `tests/embedded.rs`: replaced the "embedded polls" test with
  `embedded_server_does_not_poll_for_updates` (reaches a live manifest server,
  asserts the embedded client never hits it).
- `server.rs` unit tests: added `update_checker_is_spawned_with_reporter`.

### Verification
- `cargo build` / `clippy` (default + `--no-default-features`): clean.
- `cargo test --lib`: 394 / 300 passed. `--test embedded`: 10 passed.
- `cargo test --features testnet --test spec`: 43 passed.

---

## 2026-06-16: Make local-agents a real seam (LocalAgentHost trait)

### Summary
Replaced the pervasive, implicit `local-agents` boundary with one explicit
abstraction. The core no longer reaches the agent runtime by concrete type;
it depends on a `LocalAgentHost` trait and holds an
`Option<Arc<dyn LocalAgentHost>>` (`None` = the embedded client). The concrete
runtime — sessions, PTY, hooks, lifecycle, suspend/resume, session I/O,
the registry, and its three event sources — moved behind the seam into the
`PtyAgentHost` impl, gated at its module declaration. Every RPC and shutdown
path that used to carry a `#[cfg]` (and usually a hand-written client-only
twin) is now a uniform delegator whose `None` arm is ordinary control flow.

Derived the trait surface from the existing `#[cfg(not)]` stubs (they were the
de-facto "what the core needs without a runtime"); cross-checking surfaced
that the three `EventSource`s and the `SessionEvent` channel were structurally
"core" but only ever fed by runtime code, so they moved into the host too —
that is what makes the boundary clean rather than merely relocated. The
shutdown *orchestration* stays in `server.rs` (so `notify_routing_peers` keeps
interleaving between local-notify and commit); `prepare_suspend` owns the save
and folds prepare/save failures into `Err`, and `server.rs` bails before
notify/commit on `Err`.

Result: `cfg(feature = "local-agents")` sites 87 → 21, and the 21 are all
module declarations, re-exports, construction wiring, or the `routing/host.rs`
capability statement — **no boundary/business-logic gate and no cfg twin
remains**.

### Changes
- `services/agent/host.rs`: new — `PtyAgentHost` + `impl LocalAgentHost`
  (create/rename/delete/send_input/subscribe_session/agent-events/handle_hook/
  resume/stop_all/prepare_suspend/commit_suspend/notify_shutdown/debug_dump).
- `services/agent/state.rs`: new — `AgentServiceState`/`LocalAgentContext`
  registry (moved from `mod.rs`, gated at the `mod` site).
- `services/agent/mod.rs`: `LocalAgentHost` trait + `DebugAgent` (always
  compiled); `AgentServiceCtx` now `{ Option<host>, host_id, is_cloud }` and
  delegates; removed the create/rename/delete twins and moved helpers to host.
- `services/agent/session_rpc.rs`: retargeted `AgentServiceCtx` → `PtyAgentHost`,
  dropped all per-item gates (module gated) and the client-only stub.
- `services/{mod,client}.rs`, `server.rs`, `debug/server.rs`,
  `user_state.rs`, `services/startup/mod.rs`, `testnet/daemon.rs`: route
  through the host; `ServerState` holds `Option<Arc<dyn LocalAgentHost>>`,
  built lazily in `ensure_local_agent_host` (the host spawns a task, so it
  must be constructed inside the runtime, not in `ServerState::new`).
- `debug/server.rs`: consumes owned `Vec<DebugAgent>` from `host.debug_dump`
  (session detail rendered to JSON inside the host) instead of borrowing the
  registry guard.

### Decisions Made
- Host presence = the feature, not the role: `Some` whenever `local-agents`
  is compiled (cloud relays keep an empty registry); cloud-vs-device stays a
  runtime guard. So the host needs only `host_id` to construct.
- Keep suspend's pieces (`prepare`/`commit`/`notify`) as separate trait
  methods: the shutdown orchestration interleaves `notify_routing_peers`, so a
  merged `suspend()` could not preserve ordering.
- `routing/host.rs` capabilities (3 gates) stay: a compile-time capability
  fact consulted where no host handle exists.

### Verification
- `cargo build`/`clippy` (default + `--no-default-features` + `testnet`): clean
  (pre-existing `serve_*_tracked` / embedded-no-default warnings unrelated).
- `cargo test --lib`: 393 (default) / 299 (client-only) passed — including
  `attach_local_agent_events_populates_client_agent_model` (the event-delivery
  bridge through the host).
- `cargo test --features testnet --test spec`: 43 passed.
- `amux-core-bridge` `cargo check` (embedded, `default-features = false`): clean.

---

## 2026-06-16: Split agent data types from the runtime (seam prep)

### Summary
First chunk of the `local-agents` seam refactor. Separated the agent *data*
types from the *runtime*, so the runtime gates at one module declaration
instead of per item, and removed gates that `lib.rs`'s client-only
`allow(dead_code)` already makes unnecessary. `AgentRecord`, `SessionEvent`,
and `StopPolicy` (plus the `Agent: From<AgentRecord>` impls) moved to a new
ungated `agents/record.rs`; `agents/session.rs` is now pure runtime
(`AgentSession`/`StructuredInputTarget`) gated at `#[cfg] mod session`, with
its 8 internal `local-agents` gates removed. `suspend.rs` references only
ungated data, so its 7 gates were dropped outright — it simply compiles dead
in client-only builds.

### Changes
- `agents/record.rs`: new, holds `AgentRecord`/`SessionEvent`/`StopPolicy`.
- `agents/session.rs`: data types removed; per-item `local-agents` gates
  dropped (module gated at the `mod` site).
- `agents/mod.rs`: `mod record;`, `#[cfg] mod session;`, re-export data from
  `record`.
- `suspend.rs`: removed all 7 `local-agents` gates.

### Decisions Made
- Lean on the existing client-only `allow(dead_code)`: an item needs `#[cfg]`
  only if it would fail to *compile* (names a gated type), not merely if it is
  unused. This is why `suspend.rs` needs no gates.

### Verification
- `cargo build` / `--no-default-features`: clean, no warnings.
- `cargo test --lib`: 393 (default) / 299 (client-only) passed.

### Next Steps
- Introduce the `LocalAgentHost` trait + `PtyAgentHost`; route
  `AgentServiceCtx` and the core consumers through `Option<dyn LocalAgentHost>`.

---

## 2026-06-16: Gate the agent runtime at the AgentSession seam

### Summary
Collapsed the leaf-level `local-agents` gating into a single module seam.
Previously `AgentSession`, `StructuredInputTarget`, and `SuspendedAgent`
each carried a `Disabled` variant in client-only builds, forcing every
match arm into a three-way `#[cfg]` split ending in `unreachable!()`. Now
the agent-runtime types and their impls live entirely behind
`#[cfg(feature = "local-agents")]`: client-only builds don't compile them
at all, so the `Disabled` variants and ~20 `unreachable!()` arms are gone
and `session.rs` / `suspend.rs` read like the original. The data the
client still needs — `AgentRecord`, `SessionEvent`, `AgentType`,
`claude_io` — stays compiled. `AgentServiceCtx` / `AgentServiceState`
remain as the identity/event carrier; only their session-holding
internals (the `local_agents` map, the `lifecycle` module, the
create/rename/delete RPCs) are gated, with `FailedPrecondition` stubs in
the client build.

### Changes
- `agents/session.rs`, `suspend.rs`: gate `AgentSession`,
  `StructuredInputTarget`, `SuspendedAgent` + impls; remove `Disabled`.
- `services/agent/{mod,lifecycle}.rs`: gate the `lifecycle` module and the
  session-holding `AgentServiceState` methods; client-only `create` /
  `rename` / `delete` stubs; added a build-agnostic `local_agent_count()`.
- `services/mod.rs`, `agents/mod.rs`, `server.rs`, `debug/server.rs`:
  gate the suspend/shutdown re-exports and the debug-dump session details.

### Decisions Made
- Gate the type, not the arm: a `#[cfg]` belongs on a module/type/field,
  never inside a match. The `Disabled`/`unreachable!()` pattern was the
  symptom of gating at the wrong altitude.
- Keep `host_id` on `AgentServiceCtx` (Design Y): gating only the
  session-holding internals avoids rippling into the `ClientService` /
  startup / server construction for no functional gain.

### Verification
- `cargo build` / `clippy` (default + `--no-default-features`): clean.
- `cargo test --lib`: 393 (default) / 298 (client-only) passed.
- `cargo test --features testnet --test spec`: 43 passed.
- `amux-core-bridge` `cargo check`: clean against the refactored lib.

---

## 2026-06-16: Decouple daemonish behaviors from local-agents; drop client-only marker

### Summary
Reworked the embedded-build gating along the right axes. The "daemonish"
behaviors (peer reachability links, periodic self-update poll, direct
dispatcher TCP listener, local client unix listener, sleep inhibition,
LAN/direct pairing) had been coupled to the `local-agents` compile
feature via a `runtime_profile` shim. Those are *runtime* concerns — a
cloud relay is directly reachable yet hosts no agents — so they now live
on the builder/config axis instead. `spawn_local_background_tasks` takes
a `with_daemon_tasks` flag (desktop daemon `true`, embedded `false`); the
listener/sleep gates revert to the existing `tcp_port` /
`prevent_idle_sleep` config, which already sat in the daemon-only `run()`
path. Deleted the `runtime_profile` module and the redundant
`client-only` marker (an empty feature — `default-features = false` is
the real signal).

### Changes
- `Cargo.toml`: removed `client-only`; documented that `local-agents`
  gates only PTY hosting, not daemonish behavior.
- `server.rs`: `spawn_local_background_tasks(..., with_daemon_tasks)`;
  reverted the sleep / TCP / unix-listener runtime_profile gates.
- `client/mod.rs`, `services/client.rs`, `services/reachability.rs`:
  removed the `runtime_profile` pairing/reachability guards.
- Deleted `runtime_profile.rs`.

### Decisions Made
- Daemonish = runtime, not a feature: keeps cloud-relay/desktop role
  switching in one binary; only `local-agents` (native `portable-pty`)
  earns a compile-time feature.

### Verification
- `cargo build -p amux` and `--no-default-features`: clean.
- `cargo test -p amux --lib`: 393 (default) / 298 (client-only) passed.
- `cargo test -p amux --features testnet --test spec`: 43 passed.

### Next Steps
- Collapse the remaining leaf-level `local-agents` gating: gate the agent
  runtime at the `AgentSession` seam and remove the `Disabled` enum arms
  / `unreachable!()` noise in `session.rs` and `suspend.rs`.

---

## 2026-06-16: Client-only feature gating for embedded iOS builds (first cut)

### Summary
First cut of compile-time gating so the amux lib can build for iOS and
link into the React Native app without spawning local agents. Adds a
`local-agents` default feature (owns PTY-backed Claude/test-agent
spawning via `portable-pty`) plus a `client-only` marker; embedded
builds use `default-features = false`. iOS gets its own state/data/log
paths under the app container, and a `compile_error!` guard rejects iOS
builds that leave `local-agents` enabled. A `runtime_profile` module
centralizes the "is this a local-agent host" decision for listeners,
reachability, sleep inhibition, and periodic update checks.

### Changes
- `Cargo.toml`: `local-agents` (default) + `client-only` features;
  `portable-pty` now optional.
- Feature-gated the agent runtime (`agents::{session,pty,hook,...}`,
  `suspend`, `services::agent` hosting) with `Disabled` enum arms for
  the client build.
- `paths.rs`: iOS app-container state/data/log paths.
- `runtime_profile.rs`: centralized daemonish gates.
- `lib.rs`: `VERSION`/`PROTOCOL_VERSION` exports, iOS compile guard.
- `services/client.rs`: `CreateAgent` now checks the target host's
  supported agent types precisely (`FailedPrecondition` instead of a
  generic `Unreachable`).

### Verification
- `cargo build -p amux --no-default-features`: clean (client-only).
- The app bridge (`amuxapp/modules/amux-core`) builds against this with
  `default-features = false, features = ["client-only"]`.

### Next Steps
- Simplify: collapse the leaf-level gating to a single module seam, move
  the daemonish behaviors onto the builder/config runtime axis, and
  delete `runtime_profile` and the redundant `client-only` marker.

---

## 2026-06-11: Stop the pre-existing Windows test hang from eating 6-hour runners

### Summary
With the clap failure fixed, the Windows test leg ran further and hit a
hang that PREDATES the protocol rework (the 2026-06-07 run on old main
was killed at the same 6-hour limit): `embedded_shutdown_…` and
`embedded_suspend_…` never return on Windows — agent PTY teardown hangs
under ConPTY, the same class of issue that already keeps the Windows e2e
leg disabled. Both tests are now `#[cfg_attr(windows, ignore)]` with the
reason inline. And the "tests always run with a timeout" rule now applies
to CI itself: every job has `timeout-minutes` (10–30), so a future hang
fails in minutes instead of silently burning a 6-hour runner.

### Verification
- `cargo test -p amux --test embedded`: 10 passed (macOS).
- Windows leg verified by the subsequent CI run.

---

## 2026-06-11: Fix Windows CLI arg conflicts referencing the unix-only --via-ssh

### Summary
CI's windows-latest test leg caught five failing `amux pair` arg-parsing
tests: `qr`/`listen`/`connect` listed `via_ssh` in `conflicts_with_all`,
but `via_ssh` is `#[cfg(unix)]`-only, so on Windows clap's debug
assertion fired for a conflict against a nonexistent argument. The
conflict is now declared via `#[cfg_attr(unix, arg(conflicts_with =
"via_ssh"))]` on each of the three args. macOS-only local verification
could not have caught this; the CI matrix did its job.

### Verification
- `cargo test -p amux-cli`: 43 passed (macOS); Windows leg verified by
  the subsequent CI run.

---

## 2026-06-11: Protocol version resets to 1

### Summary
amux is unreleased and the wire owes nothing to history:
`PROTOCOL_VERSION` resets from the internal working number to **1** —
the protocol as designed is the first one that will ever ship. The
internal version labels scrubbed from code comments, docs, and this log
in favor of plain language ("the protocol rework", "the earlier
route-list routing").

### Verification
- Full local CI parity run (fmt --check, CI clippy, workspace build, lib
  tests, spec suite, e2e suite) before rebasing onto main.

---

## 2026-06-11: Docs housekeeping — three docs, repointed instructions, compacted log

### Summary
docs/ now holds exactly three files, one per audience: PROTOCOL.md (the
wire, for implementers), ARCHITECTURE.md (the system, for developers),
HOW_IT_WORKS.md (the mental model + trust story, for the documentation
website). The investigations and ops material moved to gitignored notes/
(deployment, transcript-redaction ×2, session-tracking research).
CLAUDE.md repointed at the three docs + the spec suite, and now carries
the working conventions (timeout-wrapped tests, DEVLOG-per-chunk, no
trailers, docs-vs-notes). The spec suite's main.rs module doc absorbed
the "how to add a test" guide, retiring notes/SPEC_TESTS_DESIGN.md;
notes/REFACTOR_PROGRESS.md (migration complete) deleted. DEVLOG
compacted: June 2026 entries (spec suite + protocol rework + docs) kept verbatim, the
~100 older entries summarized into era paragraphs — full text remains in
this file's git history.

### Changes
- docs/: transcript-redaction-spec/-feasibility, research-claude-session-
  tracking untracked and moved to notes/ (deployment.md was never
  tracked; moved too).
- CLAUDE.md rewritten; tests/spec/main.rs module doc expanded.
- notes/SPEC_TESTS_DESIGN.md, notes/REFACTOR_PROGRESS.md deleted.
- DEVLOG.md: 3,700 → ~840 lines.

### Verification
- Spec suite recompiled and green after the main.rs edit; CI clippy
  clean.

---

## 2026-06-11: Protocol spec grows its survivors; the website one-pager lands

### Summary
docs/PROTOCOL.md absorbed the three protocol-level survivors of the v5
spec — the SPAKE2 wire crypto (RFC 9382/edwards25519, transcript hash,
HKDF infos, sealed identities, opaque INVALID_PIN), the
versioning-is-equality rule, and the QR payload shape — plus a new "Why
it is shaped this way" section folding the load-bearing rationale from
the protocol-rework decision walkthrough (stateless relays, one proxy hop, only-Open-
allocates, no housekeeping acks, tunnels die with their link, deliberate
double encryption, PAKE over bearer tokens). With that folded, the
protocol-decisions working note is deleted; rejected-alternative history
lives in the June DEVLOG entries. New
docs/HOW_IT_WORKS.md: the human-readable one-pager for the documentation
website — mental model (devices not accounts, pairing as a one-time act,
links carry / tunnels authenticate) and the trust story (what a relay
structurally cannot do, local revocation, missing-by-design, executable
spec).

### Changes
- docs/PROTOCOL.md: pairing crypto paragraph, version rule, rationale
  section, header repointed (now ~150 lines).
- docs/HOW_IT_WORKS.md: new.
- the protocol-decisions working note deleted (gitignored; content folded).

### Verification
- Docs-only change; spec suite green on the prior commit re-verified
  (43 passed / 0 ignored).

---

## 2026-06-11: Replace the v5 networking spec with an architecture doc

### Summary
The tombstone chain is gone: `docs/NETWORKING.md` (the superseded 3,202-line
v5 spec), `docs/NETWORKING_PROGRESS.md` (its work ledger), and the three
pointer stubs (`NEW_ARCHITECTURE.md`, `architecture.md`,
`cloud_architecture.md`) are deleted; git history keeps them. In their
place, `docs/ARCHITECTURE.md` — a current-design system doc complementing
PROTOCOL.md: PROTOCOL.md owns the wire, ARCHITECTURE.md owns the system
(process/deployment shapes, identity & trust store, the two-server model,
the dispatcher's classification table, the service surface map, the
multi-tenant cloud deployment, and the LinkRegistry / RoutingCore /
TunnelPool / ConnectionManager layering). Disposition of NETWORKING.md's
material: §3–4 (threat model, identity, trust, two servers, dispatcher,
tenancy), §7 (CLI shape), §8.4/8.12/8.13, and the resource caps were
rewritten for the new design into ARCHITECTURE.md; §4.8, §5–6, §8.5–8.11 were
protocol-level and already superseded by PROTOCOL.md (the SPAKE2 crypto
detail of §5.2.1 survives only in `services/pairing.rs` for now); the
glossary, invariant catalog (§10), and reference implementation map (§12)
died with v5's vocabulary.

### Changes
- `docs/ARCHITECTURE.md` created; the five superseded docs `git rm`ed.
- `docs/PROTOCOL.md` status header now points at ARCHITECTURE.md instead
  of the deleted NETWORKING.md.
- Every `docs/NETWORKING.md §x.y` / `NETWORKING_REVIEW.md §6.x` citation in
  `crates/` re-pointed at PROTOCOL.md/ARCHITECTURE.md section names or
  rewritten as self-contained prose (spec chapter headers in
  `tests/spec/{identity,presence,routing,sessions,wire}.rs`; doc comments
  in `connection.rs`, `setup.rs`, `tunnel/pool.rs`, `testnet/daemon.rs`).
  The invariant catalog is not preserved as a numbered list; comments now
  say what they mean.
- `CLAUDE.md`/`AGENTS.md` still reference the deleted docs; they are
  re-pointed in a follow-up chunk.

### Verification
- Spec suite: 43 passed / 0 ignored. Lib tests: 394 passed.
- `cargo fmt --all`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `grep -rn "NETWORKING\|NEW_ARCHITECTURE\|cloud_architecture" --include="*.rs"
  --include="*.md" .` is clean outside `notes/`, DEVLOG history, and
  CLAUDE.md (deferred above).

### Next Steps
- Re-point CLAUDE.md and AGENTS.md at PROTOCOL.md + ARCHITECTURE.md.
- Ledger candidate: `RoutedStream::expect_stalled_open` in
  `testnet/daemon.rs` no longer has a caller — revocation now breaks
  streams instead of stalling them — and could be deleted.

---

## 2026-06-11: e2e runner catches up with the reworked config

### Summary
All 14 e2e tests failed after the protocol-rework chunks with one cause: the runner's
generated `local.yaml` still set `randomise_link_name`, deleted with wire
link names in chunk 2 (the config parser rejects unknown fields). Removed
the field from the template in `e2e-runner/src/executor.rs`.

### Verification
- `cargo run -p e2e-runner -- run`: 14 passed, 0 failed.

---

## 2026-06-11: protocol rework complete — docs graduate

### Summary
Closing docs pass for the protocol-rework implementation (chunks 1–5, commits
6735df6 → ccd7a25). `docs/PROTOCOL.md` graduates from "target design" to
the implemented spec, locked in by the prose suite in
`crates/amux/tests/spec/`. `docs/NETWORKING.md` is marked superseded
(v5, historical) with a banner saying exactly what the rework replaced and that
PROTOCOL.md + the spec suite win where they disagree.

### Changes
- `docs/PROTOCOL.md`: status header → implemented.
- `docs/NETWORKING.md`: supersession banner.

### Verification
- Final state on the rework branch: lib 394 passed; spec suite 43 passed /
  0 ignored; CI clippy clean; workspace build clean. The wire
  `Message.body` oneof matches PROTOCOL.md's vocabulary verbatim:
  Hello · HelloAck · NeighborUp · NeighborDown · TunnelOpen · TunnelData
  · TunnelClose · Reauth · LinkClose, plus `PairingService.Pair`;
  `PROTOCOL_VERSION = 6`.

### Next Steps
- Decide the fate of NETWORKING.md's still-accurate material (identity,
  trust store, two-server model, dispatcher): fold into PROTOCOL.md
  companions or rewrite as a current reference.
- §6 ledger follow-ups: move route activation's TLS handshake off the
  ConnectionManager events task (§6.12); `last_dial_error` changes push
  no event to subscription-only UIs (D15 note).

---

## 2026-06-11: protocol-rework chunk 5 — fire-and-forget Reauth; reachability shrinks to two fields

### Summary
The final protocol-rework code chunk: D12 (P5) and D15 (P7). Credential refresh is now
fire-and-forget — the daemon sends `Reauth { token }` on the cloud link at
expiry − 5 min and expects nothing back; the cloud's only answers are the
two things it can already say: silence (refresh accepted, the link
continues) or `LinkClose(AUTH_EXPIRED)` (the daemon reconnects with a
fresh token — the recovery path that exists anyway). `ReauthAck`, the 15s
ack-timeout state machine, and `ConnectorReauthState` are deleted; the
protocol never acknowledges housekeeping, it only signals state changes.
A healthy link, and every session riding its tunnels, is never disturbed
by refresh. Separately, the host-listing reachability surface shrinks to
two honest fields: `HostEntry` carries `online: bool` (routing-derived
presence) + `last_dial_error: optional string` (the last failed dial
attempt's outcome, cleared when a route comes up). The
`reachable/unreachable/unknown` enum, its three proto wrapper messages,
and the `StatusChanged` plumbing are deleted — nothing probes, so
"unknown" is `!online && last_dial_error.is_none()`, derived client-side
if anyone cares. This completes the reworked wire vocabulary: `Hello`/`HelloAck`
· `NeighborUp`/`NeighborDown` · `TunnelOpen`/`TunnelData`/`TunnelClose` ·
`LinkClose` · `Reauth` · `PairingService.Pair`.

### Changes
- `proto/amux/v1/amux.proto`: `ReauthAck` deleted from `Message.body`
  (`LinkClose` renumbered to 9); the three `HostReachability*` wrapper
  messages and `HostReachabilityStatus` deleted;
  `HostEntry.reachability_status` → `optional string last_dial_error`.
- `routing/connect/mod.rs`: `ConnectorReauthState` and
  `LINK_AUTH_REAUTH_RESPONSE_TIMEOUT` deleted; `LinkConnectorAuth` itself
  now holds the token in force and gains `refresh_deadline`/`send_refresh`
  (send the Reauth, adopt the refreshed token's expiry as the next
  deadline — no pending/awaiting state). The ack-timeout select arm and
  the `ReauthAck` body arm are gone; the refresh-send arm survives. On the
  acceptor, a good refresh extends auth silently; a bad token (validation
  failure or user mismatch) answers `LinkClose(AUTH_EXPIRED)` and closes.
- `routing/events.rs`: `HostReachabilityStatus` deleted;
  `HostReachabilityEvent::StatusChanged` deleted; `HostEntry` carries
  `last_dial_error`. `routing/core.rs::notify_host_status_changed`
  deleted; `connection.rs` record/clear_reachability_error keep the
  storage (it IS `last_dial_error`) but emit nothing.
- `services/client.rs`: both host-entry builders read the stored dial
  error straight off the connection manager (`stored_last_dial_error`);
  `host_reachability_status_to_wire` deleted; the pairing-candidate filter
  drops its `reachability_status.is_none()` clause (the trust-status check
  already said it); `publish_host_status_update` survives for trust
  transitions but returns nothing; `HostEventOutcome::Updated` deleted.
- `client/mod.rs`: `host_reachability_status_from_wire` and the
  status-presence validation rules deleted; `last_dial_error` decodes as a
  plain optional string. `HostReachabilityStatus` un-exported from
  `lib.rs`/`routing/mod.rs`; `amux-cli` test fixture updated (the CLI/UI
  never rendered the three-state beyond carrying the field).
- Spec: `cloud_peers_keep_communicating_across_a_jwt_expiry` strengthened
  per D12 — an echo session opened before the refresh point echoes again
  after `jwt.expired()` on the same stream/tunnel/link (tunnels die with
  links, so the surviving stream is proof of zero disturbance).

### Decisions Made
- Lib tests:
  `unauthenticated_acceptor_rejects_reauth_ack` deleted (message gone);
  `authenticated_acceptor_accepts_reauth_for_same_user` rewritten as
  `…_silently_extends_auth_on_reauth_for_same_user` (initial token expires
  inside the asserted quiet window, so silence also proves the extension);
  `…_rejects_reauth_for_different_user` rewritten to expect
  `LinkClose(AUTH_EXPIRED)`, plus a new invalid-token twin;
  `connector_sends_reauth_before_token_expiry` rewritten ack-free plus new
  `connector_schedules_the_next_refresh_from_the_refreshed_token` (locks
  the no-ack rescheduling rule); connection.rs
  `reachability_error_changes_emit_host_status_event` rewritten as
  `reachability_errors_are_stored_until_cleared` (storage, no events);
  client.rs status tests re-pointed at `last_dial_error`
  (`host_status_change_publishes_cached_reachability_error` folded into
  the list-hosts dial-error test — there is no status event to publish).
- `last_dial_error` changes do not push `HostUpdated` events (that was the
  deleted `StatusChanged` plumbing); listings re-read the storage on
  demand, which is what the harness verbs and UIs poll anyway.

### Verification
- `cargo build -p amux --features testnet` and `cargo build --workspace`
  clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `cargo test -p amux --lib`: 394 passed.
- Spec suite: 43 passed / 0 ignored, ×2 runs (32.25s / 32.26s), with the
  strengthened JWT-expiry test.

### Next Steps
- Protocol-rework implementation complete (chunks 1–5: D10/D11 → D2/D13/D14 → D3a/P8 →
  D9 → D12/D15). Graduate the remaining doc work: PROTOCOL.md is current;
  NETWORKING.md still describes v5 and is superseded where they overlap.
- Ledger note: the connector trusts the refresher's `expires_at`
  unconditionally — a refresher that keeps minting tokens already inside
  the 5-minute refresh window drives back-to-back refreshes (pre-existing
  shape, now without even an ack to pace it).

---

## 2026-06-11: protocol-rework chunk 4 — one pairing protocol (SPAKE2), two secret deliveries

### Summary
P2/D9 plus the D13 rename: pairing is now ONE wire protocol —
`PairingService.Pair(stream PairMessage)`, the SPAKE2 exchange — with two
out-of-band secret-delivery mechanisms: a typed 6-digit PIN (no camera) and
a QR-carried 256-bit secret (scan → paired; the user never sees it). The QR
payload shrinks to `{host_id, cloud_url, secret}` — the pubkey drops out
because SPAKE2 provides mutual authentication from possession of the
secret. This is stronger, not just simpler: the old `PairByToken` flow sent
a bearer token through a pubkey-pinned TLS channel, so the secret crossed
the wire; now it never does. One-shot consumption, ~5-minute TTL, and the
5-attempt cap are uniform across both deliveries (the cap is moot for a
256-bit secret but harmless). The cloud-only token-ingress restriction
(review item D5) is deleted with the token protocol — the Pair stream is
admitted on any pre-trust pairing transport. `PairBySpake2` → `Pair` and
`PairBySpake2Message` → `PairMessage`: the qualifier was a fossil of the
deleted second protocol.

### Changes
- `proto/amux/v1/amux.proto`: `PairByToken` RPC + request/response deleted;
  `PairBySpake2(stream PairBySpake2Message)` → `Pair(stream PairMessage)`;
  `PairingError.Reason::INVALID_TOKEN` deleted (reasons renumbered);
  `PairQrCloudPeerRequest` is `{host_id, secret}`;
  `StartPairingResponse.secret` oneof carries `qr_secret` not
  `one_shot_token`.
- `pairing/mod.rs`: `PairMode` collapses to one secret kind — bytes plus
  attempt accounting (`failed/in_flight/reserved`) and an audit label.
  `PairSecret::Token`, `PairModeTokenAttempt`, `begin/complete/abort`
  token methods, `token_matches`, and `InvalidToken`/`NotTokenMode`/
  `NotPinMode` errors deleted. The attempt machinery drops its PIN
  qualifier (`PairModeAttempt`/`PairModeCommit`, `begin_attempt`,
  `record_failure`, `begin_commit`, `complete_success`,
  `PAIR_ATTEMPT_LIMIT`); `attempt.secret()` returns the SPAKE2 password
  bytes. New `start_qr_secret[_for_duration]` mints the 256-bit secret.
- `services/pairing.rs`: `pair_by_token` server method,
  `pair_by_token_initiator`, `commit_pairing_token`, and the cloud-only
  `token_pairing_request_reachability` deleted; the SPAKE2 initiator
  (`pair_initiator`) takes `secret: &[u8]`; one `pair_mode_status` mapper.
- QR-pinned TLS verifier mode deleted: `transport/tls.rs` loses
  `QrServerVerification`, `qr_pairing_channel_from_io`, and the
  `expected_pubkey` threading (`tunnel/pool.rs::qr_pairing_channel_via`,
  `connection.rs::cloud_qr_pairing_channel_to`). The initiator verification
  matrix is {trust-pinned, none}; the surviving anonymous-TLS channel
  helpers drop their `pin_` prefix (`pairing_channel*`,
  `pairing_channel_via`, `cloud_pairing_channel_to`).
- `services/client.rs`: `PairPinCloudPeer`/`PairQrCloudPeer` now share one
  `pair_cloud_peer_with_secret` path (channel → SPAKE2 → host-id match →
  commit); the QR pubkey/duplicate-pubkey preflight went with the pubkey.
  `pairing/qr.rs` encodes/parses the new payload; `client/mod.rs` has
  `PairingSecret::QrSecret` and the two-argument `pair_qr_cloud_peer`;
  `amux-cli` renders/consumes the new payload.
- Harness: `testnet/pairing.rs::QrPayload` is `{host_id, secret}`;
  `with_qr` drives the same admin RPC as before. Spec chapter prose updated
  to D9 (no behavioral flips; all 43 specs unchanged and green).
- Kept deliberately: `TunnelTransport::has_cloud_pairing_reachability`
  (the responder still learns `Reachability::Cloud` from the pairing
  tunnel's arrival link) and `StartPairing`'s "QR requires cloud mode"
  check (the minted payload's `cloud_url` is what makes a QR consumable
  today; the wire protocol itself no longer cares).

### Decisions Made
- Wrong-secret on the QR path reports the same opaque `INVALID_PIN` as the
  PIN path: one protocol, one failure surface.
- Lib tests that exercised `PairByToken` were deleted where the behavior
  died with the token (cloud-only ingress, token race/reservation, token
  self-pairing preflights) and rewritten on the unified path where the
  behavior survives (trust-save failure releasing the reservation,
  duplicate-pubkey rejection at staging, pubkey replacement incl. the D10
  in-flight-tunnel teardown, QR-secret one-shot consumption).

### Verification
- `cargo build --workspace` and `cargo build -p amux --features testnet`
  clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.
- `cargo test -p amux --lib`: 394 passed (was 402: token tests deleted,
  several consolidated into unified-path equivalents).
- Spec suite: 43 passed / 0 ignored, ×2 runs (~32.3s each), plus a third
  green run after prose-only spec comment updates.

### Next Steps
- Chunk 5 (P5/D12 + P7/D15): delete `ReauthAck` and the reauth ack state
  machine; shrink `HostEntry` reachability to `online` +
  `last_dial_error`.
- Ledger note: `StartPairing` still refuses QR mode without cloud config —
  revisit if a non-cloud QR consumer path (e.g. LAN rendezvous by host_id)
  ever exists.

---

## 2026-06-11: protocol-rework chunk 3 — every call is a tunnel, with an explicit lifecycle

### Summary
P8 + D3a: the tunnel protocol is now the link lifecycle one layer up —
`TunnelOpen { tunnel_id, src, dst }` · `TunnelData { tunnel_id, dst,
payload }` · `TunnelClose { tunnel_id, dst }`, with `tunnel_id` a plain
16-byte UUID. Only an Open allocates endpoint state (the reply address
travels exactly once, in the Open; cloud rate limiting keys on Opens);
Data for an unknown id is a principled drop — zero allocation, link stays
up — which closes B1 structurally; normal teardown sends TunnelClose
proactively from either end (rejection too: there is no open-ack, the
inner mTLS handshake is the acknowledgement). Relays forward all three
frame types statelessly by `dst`. The dual-path channel discipline is
deleted: every peer call rides a tunnel, including to adjacent peers
(`Route::Direct(link)` materializes a tunnel pinned to that link with
`dst = peer`). Because tunnels are opened by sending frames, and frames
flow both ways, every live link is callable from both ends — both sides
now record a Direct route at link establishment (the multi-tenant cloud
acceptor excepted: the relay records no routes), which makes the
SSH-pairing responder's call-back real (D6).

### Changes
- `proto/amux/v1/amux.proto`: `TunnelId`/`TunnelFrame` deleted;
  `TunnelOpen`/`TunnelData`/`TunnelClose` added; `Message.body` renumbered.
- `tunnel/types.rs`: `TunnelId` is a UUID newtype (initiator/nonce gone —
  "initiated by me" is now an explicit `ActiveTunnel.initiated` flag).
- `tunnel/mod.rs`: initiator endpoints send the Open lazily, just ahead of
  their first Data frame (a never-used tunnel never allocates remotely);
  hosted endpoints never open. `TUNNEL_FRAME_PAYLOAD_MAX` →
  `TUNNEL_DATA_PAYLOAD_MAX` (cap unchanged).
- `tunnel/pool.rs`: one inbound handler per frame type; retired-tunnel
  tombstones (`BoundedTunnelIdSet`, `RETIRED_TUNNEL_CAP`) and the cloud
  tunnel-id cache deleted — with explicit Opens there is nothing for a
  stale Data frame to allocate. Endpoint drop/retirement sends a
  best-effort TunnelClose on the pinned link (`LinkRegistry::
  send_best_effort`); `InboundClosed`/`IncomingTunnelsClosed`/
  `MissingTunnelId` error variants gone. New `channel_on_link` for Direct
  routes.
- `connection.rs`: `ChannelKey::Direct`, `RouteRuntimeState`,
  `register_direct`/`remove_direct` deleted; the pool keys on
  `(peer, Route)` and materialization is one path — open a tunnel.
  NeighborUp gained the same never-eagerly-tunnel-into-the-cloud guard
  ClaimUp had (§6.7).
- `routing/connect/mod.rs`: the `direct_channel` threading is gone;
  `run_established_connect` records `apply_direct_up` on both roles
  (`auth_session.is_none()` — i.e. everyone but the cloud acceptor).
- Harness: `WirePeer` scripts Open/Data/Close; testnet tracks *dialed*
  direct-link TCP sockets (`trusted_device_channel_tracked` +
  `ReachabilityLinkConnector::track_dialed_tcp`) so an in-process restart
  severs them — with acceptors recording routes, a leaked dialer socket
  kept the acceptor seeing a stopped daemon online. `over_ssh` now stands
  the post-pairing SSH link in with the test TCP transport, as production
  dials its stored reachability at commit time.

### Decisions Made
- Pre-planned spec flips: the SSH-responder test asserts
  `server.can_call(&laptop)` over the inbound link; the B1 `#[ignore]`
  marker became a real passing test (open → close → late data: dropped,
  link up); wire scripts rewritten to the new grammar.
- Additional flips, all structural consequences of the rework, called out here:
  `direct_beats_cloud_when_both_are_available` — the acceptor now routes
  back over the link itself, not via the cloud;
  `revocation_evicts_routes_and_breaks_in_flight_streams` (was
  `…strands_in_flight_streams`) — streams ride tunnels, tunnels die with
  links/TunnelClose, so the §6.5 stall is gone and the spec-intended
  prompt break is real; the SSH-relay lib test gives the responder a
  pinned entry for the initiator (D4: tunnel mTLS is uniform, SSH links no
  longer inherit transport trust); startup lib tests poll for active
  routes (activation now performs a real tunnel TLS handshake).

### Verification
- `cargo test -p amux --lib`: 402 passed.
- Spec suite (`--features testnet --test spec`): 43 passed / 0 ignored,
  ×2 runs (~32.3s each).
- `cargo build --workspace` clean; `cargo fmt`; CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`)
  clean.

### Next Steps
- Chunk 4 (P2/D9): pairing collapse to one SPAKE2 protocol; delete
  `PairByToken`, `INVALID_TOKEN`, the QR-pinned verifier mode.
- Chunk 5 (P5/D12 + P7/D15): delete `ReauthAck` and the reauth ack state
  machine; shrink `HostEntry` reachability to `online` +
  `last_dial_error`.
- Ledger candidates from this chunk: eager route activation now runs a
  device-TLS handshake serially on the events task (the §6.7 "move
  activation off the event loop" smell got heavier); link-up now costs
  two tunnel TLS handshakes (both ends activate eagerly) even if no call
  is ever made.

---

## 2026-06-11: protocol-rework chunk 2 — route by host id with adjacency-only events

### Summary
The core routing rewrite (D2, D14, the routing-relevant D13 renames;
PROTOCOL_VERSION = 6). Routes are now `Direct(link) | Via(relay_host)` and
the wire carries host-id-addressed frames under two structural rules:
advertise only adjacency (`NeighborUp`/`NeighborDown` claim strictly "I
have/lost a direct link to H") and forward only to adjacency (a frame for
`dst` is forwarded iff a direct link to `dst` exists, else dropped). Route
lists, prepend-on-forward, split-horizon, hop caps, route dedup, wire link
names, and the snapshot/delta phase distinction (`SnapshotComplete`,
pre-activation event buffering) are deleted. The neighbor snapshot is a
field of `Hello`/`HelloAccepted`, reconciled atomically with link
registration. Presence is a two-hop derivation; replies leave on the
tunnel's arrival link, which removes the §6.6 direction-dependent
black-hole structurally. `RoutingService` → `LinkService`. The dual-path
channel discipline (direct channels eager, Via tunnels lazy) survives
until chunk 3 (P8).

### Changes
- `proto/amux/v1/amux.proto`: `Route` message deleted; `TunnelFrame.dst`
  is a host id; `HostUp/HostDown` → top-level `NeighborUp { host }` /
  `NeighborDown { host_id, reason }` (no route field); `RoutingEvent`
  envelope and `SnapshotComplete` deleted; `Hello`/`HelloAccepted` lose
  link names and gain `neighbors`; `service RoutingService` →
  `service LinkService`.
- `routing/route.rs` deleted (898 + 443 lines of route-list machinery with
  `routing/core.rs` rewritten around host entries + claims); registry-level
  adjacency discipline moved into `routing/link_registry.rs::register`
  (snapshot diff + `NeighborUp` fanout under one lock).
- `tunnel/pool.rs`: forwarding is the registry lookup
  (`forward_to_peer`), non-awaiting — a congested peer link gets the
  existing try_send-or-request-close policy instead of stalling the origin
  link's inbound processing (head-of-line guard).
- `connection.rs`: `ChannelKey::{Direct(link), Via{target, relay}}`
  replaces route-keyed pooling; §6.2/§6.7 guards reduced to what the new
  model still needs.
- Startup/cloud (`services/startup/*`, `user_state.rs`): per-user
  registries fan out scoped `NeighborUp`s; the cloud is adjacency, not a
  host — neither side records a host entry for the other.
- Harness/spec: `WirePeer` speaks the new handshake; chain tests rewritten —
  `endpoints_call_each_other_through_a_chain_regardless_of_dial_direction`
  (the §6.6 pair collapsed; dial direction no longer matters) and
  `presence_reaches_exactly_two_hops_along_a_chain` (catalog 28b inverted:
  three hops out is deliberately invisible).

### Decisions Made
- Pre-planned spec flips only: 28b inversion, §6.6 collapse, handshake-
  snapshot wire tests. Everything else green with mechanical updates.
- Lib tests that asserted old observables were re-pointed at current ones:
  rate-limit/forwarding tests count `TunnelFrame` bodies (links also carry
  adjacency events now); cloud-startup tests wait on live links in the
  registries instead of host entries (the cloud never appears as a host).

### Verification
- `cargo test -p amux --lib`: 394 passed.
- Spec suite (`--features testnet --test spec`): 42 passed / 1 ignored,
  ×3 runs (~32.3s each).
- `cargo fmt`; CI clippy (`--workspace --all-targets --features
  amux/testnet -- -D warnings`) clean; `cargo build --workspace` clean.

### Next Steps
- Chunk 3 (P8 + D3a): every call a tunnel, `TunnelOpen`/`TunnelData`/
  `TunnelClose` with UUID ids, delete the dual-path materialization and
  `ChannelKey::Direct`; SSH-responder spec test flips cannot_call →
  can_call.

---

## 2026-06-11: protocol-rework chunk 1 — delete preserve_tunnel_id and GoAway drain; rename GoAway → LinkClose

### Summary
First implementation chunk of the protocol rework: the two pure deletions (D10/P3,
D11/P6). `preserve_tunnel_id` — the `Option<TunnelId>` threaded from the
dispatcher through the pairing service into `ConnectionManager` so a
same-host_id/different-pubkey replacement could keep the in-flight pairing
tunnel alive — is gone; teardown now tears down everything, and an initiator
that misses `PairingComplete` re-pairs. The GoAway drain machinery
(`drain_timeout_ms`, draining flags, draining-mode frame filtering, the
`LinkDraining`/`Draining` error variants) is gone, and the wire message is
renamed `GoAway` → `LinkClose { reason, error }`: receiving it means "the
link is closed now, here's why" — no grace period.

### Changes
- `proto/amux/v1/amux.proto`: `GoAway` → `LinkClose` (drops
  `drain_timeout_ms`), `GoAwayReason` → `LinkCloseReason` (values unchanged).
  PROTOCOL_VERSION deliberately not bumped — that lands with the full wire
  rewrite next chunk.
- `connection.rs`, `tunnel/pool.rs`, `services/pairing.rs`, `dispatcher.rs`,
  `transport/io.rs`, `tunnel/transport.rs`: every `*_preserving_tunnel`
  doubled method collapsed to its plain form; `BoxedGrpcAuth::PreTrustPairing`
  and `TunnelTransport` lose their `tunnel_id` fields.
- `routing/connect/mod.rs`: drain deadline/flag/filtering deleted; inbound
  `LinkClose` breaks the loop immediately (`PostHandshakeAction::Drain` →
  `LinkClosed`); `goaway_drain_duration` deleted.
- `routing/link_registry.rs`: `draining` writer flag, `mark_draining`,
  `is_draining`, `LinkRegistryError::Draining` deleted;
  `send_goaway_to_*` → `send_link_close_to_*` (no drain arg).
- `server.rs`: shutdown notification renamed; the 200 ms pre-abort sleep is
  kept as a purely local flush grace (`SERVER_LINK_CLOSE_FLUSH_TIMEOUT`).
- Testnet harness mirror `GoAwayReason` → `LinkCloseReason`,
  `expect_goaway` → `expect_link_close`; spec tests renamed accordingly.
- Tests: drain-behavior tests rewritten to assert immediate-close (or
  link-removed) semantics; the cloud pin/QR pairing startup tests no longer
  preload a stale key (they locked in the preserved-tunnel success); new
  `pair_by_token_replacement_retires_the_in_flight_pairing_tunnel` locks the
  D10 behavior directly.

### Decisions Made
- Per D10, a cloud pairing that triggers key replacement now fails at the
  initiator (timeout today; prompt once D3's TunnelClose lands) and the
  initiator re-pairs. Known v5-interim consequence: after the replacement the
  responder holds no route back to the initiator until a link flap, so the
  immediate re-pair over the same relay can black-hole — the new reply rule
  (replies ride the arrival link) removes this class structurally.
- Internal `routing::LinkCloseReason` (writer close requests) keeps its name;
  the wire enum converges on the same name per D11.

### Verification
- `cargo build -p amux --features testnet` clean; lib tests 449 passed; spec
  suite green twice (43 passed, 1 ignored, ~32s each); fmt + CI clippy
  (`--workspace --all-targets --features amux/testnet -- -D warnings`) clean.

### Next Steps
- P1+P8 core rewrite: host-id routing, every-call-a-tunnel,
  TunnelOpen/TunnelData/TunnelClose, PROTOCOL_VERSION = 6.

---

## 2026-06-11: Protocol rework one-pager

### Summary
Concluded the protocol-simplification walkthrough (all proposals from the
networking review resolved) and wrote `docs/PROTOCOL.md` — the target
design as a one-pager. Core decisions: host-id routing with one proxy hop
through any relay (advertise-only-adjacency / forward-only-to-adjacency);
every peer call is a tunnel with pinned e2e mTLS inside (links grant
forwarding only, never call authority); QR pairing collapses into the
SPAKE2 flow as a QR-carried 256-bit secret; neighbor snapshot moves into
Hello/HelloAck (SnapshotComplete deleted); GoAway drain deleted and the
message renamed LinkClose; Reauth kept but ReauthAck deleted
(fire-and-forget refresh, so live sessions are never disturbed); TunnelClose
added; HostUp/Down → NeighborUp/Down, TunnelFrame → TunnelData,
RoutingService → LinkService, PairBySpake2 → Pair.

### Decisions Made
- Full rationale recorded in the protocol-decisions working note (D1–D15,
  local working notes). Notable rejections: link-scoped tunnel IDs (forces
  relay remap state), tunnels surviving link replacement (cross-link
  reordering), dropping Reauth entirely (hourly breaks of live cloud
  sessions).
- Amended same day (D3a): tunnel lifecycle made explicit —
  TunnelOpen/TunnelData/TunnelClose with plain UUID ids, no open-ack (inner
  TLS is the ack, TunnelClose the rejection); only Opens allocate endpoint
  state. Replaces implicit-open + `TunnelId{initiator, nonce}`; both
  lifecycle grammars (link/tunnel) are now symmetric.

### Verification
- Design-only change; no code. Spec suite unaffected (expected flips when
  the rework lands are pre-recorded in the decisions notes).

### Next Steps
- Implement in sequence: P3+P6 deletions → P1+P8 core rewrite → P2 →
  P5/P7, spec suite green throughout.

---

## 2026-06-10: Spec suite polish, harness docs, CI wiring

### Summary
Final pass over the spec-test suite: hoisted remaining plumbing out of test
bodies into harness verbs (`Pin::wrong_guess`, `WirePeer::flood_handshakes_
until_rate_limited`), documented the testnet harness at module level, ordered
chapter modules as documentation, and wired the suite into CI.

### Changes
- `crates/amux/src/testnet/` rustdoc on all public items; ~25-line module doc
  covering TestNet/Daemon/WirePeer and the eventually/restart/sever
  disciplines.
- `.github/workflows/ci.yml`: test job runs
  `cargo test -p amux --features testnet --test spec`; clippy covers the
  feature.

### Verification
- Spec suite green twice (43 passed, 1 ignored, ~32s); lib 451 passed; fmt +
  clippy (CI invocation) clean.

### Next Steps
- Decide whether `notes/SPEC_TESTS_DESIGN.md` and `notes/NETWORKING_REVIEW.md`
  (gitignored, referenced from committed code comments) should be force-added.

---

## 2026-06-10: Spec chapters 5–6 — remote sessions, authority, wire conformance

### Summary
Catalog items 30–39: remote agent attach with IO round-trips over direct and
cloud routes, session survival across topology churn, full runtime authority
for paired peers, local-admin RPC rejection over remote transports, and a
`WirePeer` scripted protocol actor exercising the real TCP listener +
dispatcher + TLS for the wire-conformance chapter.

### Changes
- `crates/amux/tests/spec/{sessions,wire}.rs`; `src/testnet/{session,wire}.rs`.
- Production: only `#[cfg(test)]` → `#[cfg(any(test, feature = "testnet"))]`
  widenings in `agents/` and `services/agent/session_rpc.rs`.

### Decisions Made
- Item 38's race half (review finding B1: a benign tunnel-close race can
  escalate to a link-level GoAway) is an `#[ignore]`d test documenting the
  bug; the deterministic unknown-tunnel-drop case asserts the spec'd behavior.

### Verification
- Spec suite 43 passed / 1 ignored across 3 consecutive runs; lib 451 passed;
  fmt + clippy clean.

---

## 2026-06-10: Spec chapters 3–4 — presence, routing & failover

### Summary
Catalog items 15–29 (+28b): presence lifecycle, per-user isolation at the
relay, pairing-candidate scoping, direct-beats-cloud selection, failover in
both directions, make-then-break swaps with in-flight stream breakage,
startup re-establishment, revocation teardown, 3- and 4-node relay chains,
and JWT-expiry continuity driven through the real Reauth path with
short-TTL testnet tokens.

### Changes
- `crates/amux/tests/spec/{presence,routing}.rs`; harness verbs `stop`,
  `sees_offline`, `cannot_call`, pairing-candidate observers,
  `open_event_stream_to` (`RoutedStream`), `.cloud_user(...)`,
  `.trusted(...)`, expiring-JWT support in the testnet relay.
- Product fixes found by the suite (NETWORKING_REVIEW.md §6.7/§6.9):
  `connection.rs` no longer eagerly materializes multi-hop routes to relay
  hosts (head-of-line blocked the routing-event loop for the 10s handshake
  timeout); `tunnel/pool.rs` route removal no longer retires healthy hosted
  inbound tunnels on make-then-break swaps.

### Decisions Made
- Tests lock current behavior where it diverges from docs/NETWORKING.md, with
  NOTE comments: dropped-route streams fail with `h2 protocol error` (not
  UNAVAILABLE); revocation strands in-flight streams both ways; chained
  relaying is dial-direction-dependent (acceptor-side HostUp suppression).

### Verification
- Spec suite 32 passed across 5 consecutive runs; lib 451 passed (incl. new
  regression tests for both product fixes); fmt + clippy clean.

---

## 2026-06-10: Spec chapters 1–2 — identity, trust, pairing; harness hardening

### Summary
Catalog items 1–14: identity persistence across restart, trust locality, all
three pairing flows (PIN direct/cloud, QR, SSH over an in-process stream
seam), wrong-PIN opacity, the 5-attempt cap, pair-mode TTL and one-shot
consumption, self-pairing rejection, and key rotation. Pairing verbs drive
the real local-admin RPCs, not trust-store pokes.

### Changes
- `crates/amux/tests/spec/{identity,pairing}.rs`; `src/testnet/pairing.rs`.
- Harness hardening after a hang hunt: `eventually()` now bounds both the
  condition poll and the failure dump; routed-call verbs are
  timeout-bounded (`can_call` for post-churn assertions); `restart()` severs
  tracked inbound and outbound sockets so peers observe a real process death
  (aborting the connector task alone leaks the established link on both
  ends — documented finding).
- Product fix (NETWORKING_REVIEW.md §6.2): a stale
  `ConnectionManager::activate_route` could demote a fresh direct route and
  permanently strand its pooled channel; activation now refuses
  longer-or-equal demotion.

### Verification
- Spec suite 16 passed, 3 consecutive runs, zero hangs; lib 450 passed.

---

## 2026-06-09: Testnet spec-test harness and smoke tests

### Summary
Built the `amux::testnet` harness (behind a `testnet` feature) and the
`tests/spec` integration target per `notes/SPEC_TESTS_DESIGN.md`: whole-daemon
in-process networks with real localhost TCP + mTLS and a real cloud relay,
declarative `TestNet` builder with trust pre-pairing, eventually-assertions
with topology failure dumps, and operator verbs (cloud outage, link sever,
restart). Two smoke tests cover the canonical example and the operator verbs.

### Changes
- `crates/amux/src/testnet/{mod,net,daemon,assertions}.rs`;
  `crates/amux/tests/spec/`; `testnet` feature + `[[test]] spec` target.
- Production: cfg-gate widenings plus feature-gated accessors and a tracked
  TCP-accept seam in `dispatcher.rs` (no behavior changes).

### Decisions Made
- Assembly generalizes the existing `services/startup/mod.rs` integration
  tests rather than inventing a new path; no skip-TLS anywhere.
- Daemon bring-up is sequenced (direct links, then cloud) to avoid a
  route-activation race discovered during construction — later root-caused
  and fixed as NETWORKING_REVIEW.md §6.2.

### Verification
- `cargo test -p amux --features testnet --test spec` — 2 passed, stable
  across 10 runs; lib 450 passed; clippy + fmt clean.

---

## Earlier history (compacted 2026-06-11)

Entries below this point were compacted into era summaries; the full
entries remain in this file's git history (any commit before the
compaction).

### 2026-05 → 2026-06-08: Networking & Security v1 (protocol v5)
The full v5 networking architecture landed directly as commits (devlog
gap): gRPC `RoutingService.Connect` bidi links, route-list stack routing,
`TunnelFrame` tunnels carrying end-to-end pinned mTLS, SPAKE2 + token
pairing, the trust store, the two-server model with dispatcher
classification, and the multi-tenant cloud relay (TLS+JWT). Anchor
commits: fd02e8e ("Implement v1 routing and service architecture"),
681ee49 ("Implement amux Networking & Security v1"), 707f620. All of it
was specified in docs/NETWORKING.md (since deleted) and superseded by
the protocol rework — see the June entries above.

### 2026-05-15: Embedded-library refactor
`amux` reshaped into a library for embedded clients plus `amux-ui` (a
reactive client runtime); public API surfaced deliberately; cleanup pass
behind it.

### 2026-04: Hardening and idiomatic-Rust era
Wire enums reshaped for non-Rust clients with forward-compatibility
`Unknown` variants; per-client minimum-version enforcement; CLI
ergonomics (`server` subcommand, positional attach, suspend/resume);
pre-release security hardening; a multi-pass idiomatic-Rust refactor and
domain reshuffle (opaque hook handling, facade cleanup, server-giant
splits); init flow + cloud-state reshape; negotiated idle timeouts for
heartbeats; subscription leases; `amux debug` rework.

### 2026-03: Agents and sessions era
Workspace split into crates with a public API; `AgentSession` enum +
`PtyHandle`; suspend/resume with persisted state; `amux update`;
graceful shutdown; Windows support behind a platform abstraction; the
Claude hook-event family (PreToolUse/PostToolUse/Notification/Stop) and
the AskUserQuestion keystroke work; structured I/O sequence numbers;
external session capture (fork-and-swap); protocol reset to v1 with
handshake extraction and per-user sockets.

### 2026-02: Custom-protocol era
Cloud mode over WebSocket + MessagePack; the protocol-v3 restructure
(command enum, opaque payloads, request_id, route management,
SubscriptionClosed); agent/host discovery (AnnounceAgent/AnnounceHost);
user multi-tenancy (`ServerUserState`); the Claude Code plugin with
auto-install hooks; the tracing migration and the zero-clippy policy;
TCP keepalives; protocol version checking on Connect.

### 2026-01: Prototype era
The initial PTY-multiplexer prototype; milestone 1 with the e2e testing
framework; TCP transport and remote subscriptions; hooks, structured
logs, and the dashboard; link-based stack routing — the design whose
relay statefulness the rework finally resolved by routing on host ids.
2026-08-23 — **Recorded the Claude MCP registration probe.** The opt-in A2A
capture now starts Claude 2.1.240 in default permission mode with a strict
inline MCP configuration and an isolated stdio stub exposing the five amux
tool schemas. The live model called `send` with the requested arguments,
completed without a permission hook, and the stub retained the exact
JSON-RPC request only inside the disposable project. Claude did not persist
the strict-MCP transcript on either live run, including after the observed
Stop metadata was replayed through the normal hook seam, so the committed
tool-use/result fixture is explicitly synthetic and `fidelity_risk: true`;
the metadata preserves the live observation and the structural waiter pins
the expected paired row shape.
2026-08-23 — **Pinned Claude’s session-name and version probe.** The isolated
registry capture starts a named, socket-enabled Claude 2.1.240 session and
records the name in its terminal presentation, `claude --version`, the
hook-reported transcript path pattern, and a full sweep of the scratch and
Claude-project search roots. No durable Claude registry file or
`peerProtocol` field was found in that live corpus, so the graduated metadata
records both facts as `false`/`null`; the test-local gate parser consumes that
captured version string and requires the socket-capable minimum.
2026-08-23 — **Opened the authenticated messaging service boundary.** Both
client and peer services now expose message delivery and current-work updates,
with strict wire decoding and local target resolution ahead of intentionally
unimplemented backend behavior. Create requests carry parent and initial-prompt
metadata, delete responses reserve explicit removed/unreachable child lists,
and client-authored agent provenance is accepted only for a live local sender.
The whole-daemon specification proves an unknown sender UUID is refused before
delivery, while wire tests cover envelope, relationship, prompt, and status
round trips.
2026-08-23 — **Made remote agent messaging honestly fire-and-forget.** Local
delivery now records the envelope id and accepted backend carrier at info, and
records failed carrier attempts with the same correlation fields. When a
selected remote host becomes unreachable, a human send still receives an
Unavailable response; a live local agent send is logged and dropped while
returning its daemon-issued envelope id. The whole-daemon specification
reproduces the route-loss window from a last-known remote agent observation and
locks in both caller outcomes.
2026-08-23 — **Routed Claude lifecycle results back to parent agents.** Claude
sessions now retain their daemon-owned parent edge. An accepted Stop hook with
`last_assistant_message` emits one authenticated `completed` envelope, while
process end emits the distinct `exited` envelope before withdrawal. A local
outbound bridge dispatches both through the same local or peer message carrier
as ordinary agent sends. Whole-daemon specifications use an echo parent and a
process-free scripted Claude session to prove local and direct-TCP delivery of
both lifecycle signals.
2026-08-23 — **Pinned Claude carrier rows at the transcript-ingest seam.** The
graduated socket and PTY captures now replay through the daemon's opaque
transcript ingest with every row preserved and only the documented readiness
marker appended. The transcript reference records Claude 2.1.240's distinct
busy-input shapes: PTY delivery yields a human-origin `queued_command`
attachment, while socket delivery yields a peer-origin meta user row and no
such attachment. This keeps later carrier confirmation logic grounded in the
captured schema instead of assuming both paths share one row shape.
2026-08-23 — **Forwarded Claude inbox credentials through every hook.** The
hook CLI now copies only Claude Code's messaging socket and token variables
into the hook RPC. Managed and externally discovered Claude sessions retain a
complete credential pair on first observation and refresh it on every later
hook, including duplicate deliveries, so token or socket rotation cannot leave
the daemon using stale readiness state. Captured Stop-hook coverage pins the
wire-to-session behavior without exposing the token through debug output.
2026-08-23 — **Delivered Claude agent messages through the authenticated inbox.**
Claude sessions now snapshot their socket credentials, transcript source, and
PTY fallback before delivery, keeping the agent registry available while a
message waits for confirmation. Agent-authored envelopes use Claude's native
cross-session JSONL protocol and are accepted only after the envelope id
appears in a peer-origin user row or queued-command attachment within five
seconds. A failed or unconfirmed post permanently moves that session to the
safe bracketed-paste carrier and resends the message once.
2026-08-23 — **Folded agent families into one fleet row.** A card now states
its parent edge and current work directly, and the ranked fleet groups every
agent under the ancestor nobody claims: one row per family, carrying the whole
subtree in family rank order with each member's depth, the count of agents the
collapsed row stands for, and the loudest effective attention anywhere beneath
it. A family ranks as a unit on that attention and on its most recent activity,
so a working child never sinks under the idle parent hiding it. Attention
summaries prefer honest ignorance to a wrong badge: one member on an offline
host makes the family Unknown rather than idle. Edges that name an agent this
inventory cannot see leave the child a row of its own, and edges that loop
strand nobody — each agent still gets a row and the Model reports the broken
topology. The chrome keeps drawing its flat list until the family chrome lands.
2026-08-23 — **Read inbound agent messages out of the recipient's own rows.**
The Claude layer now recognizes a message another agent sent it in either
carrier — the generic tag the bracketed paste delivers, and Claude's native
cross-session envelope with amux's header line that the inbox socket posts —
and folds it to its own feed entry carrying the envelope id, context, sender
and kind. The row never becomes a prompt: the paste carrier arrives wearing
the human discriminators only a terminal can produce, and rendering it as
something the human said would let a peer borrow their voice. Turn
bookkeeping still follows the row's own discriminators, so a recipient the
harness set working is not reported idle. The reader is deliberately lenient
about everything but the sender address, and its agreement with the daemon's
formatter is asserted over both carriers rather than assumed. Claude peer
messages that amux did not send, and humans quoting a tag, are left alone.
2026-08-23 — **Gave an outbound agent message its own chat row.** A
`mcp__amux__send` tool call now folds to a typed invocation carrying the
recipient and the message, and renders as one directional glyph, the agent
it went to, and a summary — the outbound half of a conversation rather than
an MCP tool name beside a JSON blob. It stays an ordinary tool row in every
other respect, so a send that failed still reads as a failure, and the other
amux tools deliberately keep the generic shape.
2026-08-23 — **Read Codex agent messages and amux's own tool calls.** The
Codex layer now folds the synthesized message row the daemon writes when a
carrier accepts a delivery — keeping the sender, envelope id, context, kind
and which of the three carriers took it — because the native thread shows
nothing for an injected item. A message raises no attention of its own: a
queued delivery is not the human being needed. Dynamic tool calls that amux
itself registered fold to their own work kind, discriminated against the
registrar's list rather than a copy of it, so the fleet's work on itself
reads in the fleet's words while anyone else's dynamic tools stay theirs.
The envelope-kind vocabulary moved to the kernel, where amux's own wire
facts belong; each layer keeps its own entry, since what a Claude transcript
can recover and what the Codex daemon authored are different facts.
2026-08-23 — **Folded families into one fleet row.** An agent that spawned
others now occupies a single row wearing the loudest badge anywhere inside
its family and a `⋯N` marker for what it stands in for, so a blocked
grandchild pulls the whole family to the top of the list rather than hiding
under an idle parent. `z` opens and shuts the family under the cursor;
descendants indent one step per generation, and shutting from inside leaves
the cursor on the row that swallowed it. A typed filter opens every family,
because a name the human typed must never miss an agent behind a fold. The
fleet also states what each agent says it is working on, clipped to the room
left over and stamped with how long ago it said so — and left plainly empty
for an agent that has said nothing. That column is the first to collapse on
a narrow terminal, ahead of the status word: it elaborates where every other
column answers.
2026-08-23 — **Named a family in the chat header, and gave it a key.** A
parent's chat now says `⋯ N subagents` beside its name — the whole subtree,
because that is what is out of sight — and says nothing at all when it
started nobody. `<leader> n` walks the family in the order the fleet ranks
it and wraps past the last member back to the top, so one repeated key goes
into the children and comes back out, from a child as readily as from the
parent. It joins the two chords that already leave a chat, which is why it
works from a read-only chat and from under an open panel and never leaks a
keystroke into a draft. Members this build cannot open — an unrenderable
protocol, a host that is not answering — are stepped over rather than
shown, because a frame that can say nothing is a worse answer than staying
put.
2026-08-23 — **Raised a child's ask in its parent's chat.** A parent's chat
now carries one warning row under its header naming which agent below it is
waiting and for what — the act itself, spoken by the child's own layer: the
command a Codex child wants to run, the tool a Claude child wants to use,
the question it is asking. The parent's chat decides where the ask is drawn;
the child's layer decides what it says. Nothing is written into the parent's
stream and nothing is stored, so answering the ask anywhere — in the child's
own chat, on another device — empties the row on the next frame with nothing
to clear, and a second child's ask appearing does not disturb the first. Only
the loudest is named and the rest are counted, because a chat that spends
four rows on other agents' business has stopped being this agent's chat. The
row costs the feed a line rather than floating over it: covering a message to
announce an ask trades one thing the human needs to read for another. The
sticky diagnostic banner moved with the chrome rule it replaces, so a
consistency warning can no longer hide a child who is waiting on a person.
2026-08-23 — **Gave agent messages their shape in both chats.** An inbound
message shows everything it said, because somebody is talking to this agent.
A completion closes to its first line over a marker saying how many lines are
behind the fold and which chord opens them — `<leader> m` opens and closes
every completion in the chat, a display state rather than a per-row
affordance, because the feed has no cursor to point at one row with. An exit
offers nothing to open, because the envelope carries nothing. The sender
marker names the agent, and its host only when the message came from another
machine this inventory knows; an address nobody here can place stays exactly
as it arrived rather than being shortened into a claim. Codex's outbound
`send` now reads as the target and a summary, the way Claude's already did,
while amux's other tools keep the generic tool shape: spawning and stopping
are work, not talk.

2026-08-23 — **Made the command-line fleet family-aware.** `amux list` now
shows one row per family with a count of the hidden descendants, while
`amux list --all` expands the same stable tree and indents every generation.
Each visible row also carries the agent's first-line work claim, bounded to a
small column and stamped with its age; an unset claim stays absent. Missing
parents and malformed loops remain visible instead of losing inventory, and
duplicate names and multi-host labels keep their previous disambiguation.
The CLI now declares its timestamp formatter as a shipped dependency as well
as exercising it in tests, so release builds and test builds use the same
dependency surface.

2026-08-23 — **Put an explicit guard around command-line family deletion.**
`amux rm` now refuses to cascade while any descendant reports active work and
names each blocking child and task; `--force` is the deliberate override for
scripts and attended cleanup. A successful cascade reports every child the
daemon removed and visibly marks those that had a work claim, while unreachable
children are named as still running. The interactive fleet keeps its existing
confirmation-only behavior because the person there is already reading the
full cascade before choosing.

2026-08-23 — **Kept the cross-kind round trip reproducible in both live
harnesses.** The opt-in Claude suite now has an H.10 scenario in which a
Claude parent uses amux's shipped `spawn` tool to start a Codex child, observes
the child's automatic completion, and acknowledges it. The Codex suite's new
C.15 scenario exercises the mirror image with a Claude child and structurally
matches the synthesized completion row before accepting the parent's reply.
Both scenarios verify the parent edge in live inventory and remain inert unless
their scenario name, or the suite-wide selector, is explicitly requested.

2026-08-23 — **Kept release and harness-only helpers out of one another's
warning surface.** The legacy injectable delete helper now compiles only with
the CLI tests that exercise it. The Codex round-trip scenario uses the shared
row-type matcher and validates completion fields immediately afterward, so the
same structure module remains warning-free when compiled by its offline waiter
tests, where live-only matchers are intentionally never constructed.
