# Provider crate boundaries

amux owns the provider boundary for Claude Code and Codex. Provider-specific
processes, wire formats, and recordings live in canonical provider crates;
the daemon consumes those crates through thin adapters. The common boundary is
one owned, ordered event stream paired with a cloneable control handle. No
provider callback trait crosses into amux.

The daemon remains responsible for agent identity, sequencing and fan-out,
typed protocol exposure, outstanding obligations, agent-to-agent delivery,
suspend records, and client layers. The provider crates own the interaction
with the provider and expose provider-native facts without making daemon or UI
policy.

## Crates and session types

| Crate | Boundary | Responsibility |
|---|---|---|
| `claude` | `claude::pty::Session { events: EventStream, control: Control }` | Hosts interactive Claude Code, combines PTY output, hooks and transcript rows, derives asks, resolves a keymap, and accepts semantic input intents. Raw terminal access is available only through the control handle for the typed terminal plane. |
| `claude` | `claude::sdk::Session { events: EventStream, control: Control }` | Hosts Claude Code in stream-JSON mode. The ordered stream contains verbatim messages and typed permission, hook, elicitation, user-dialog and exit events; the control handle sends prompts and answers requests. It does not tail transcript files. |
| `codex` | `codex::Session { events: ThreadEventStream, control: ThreadControl }` | Owns one Codex app-server thread event stream. The control handle starts and steers turns, interrupts, answers approvals, injects items, and exposes the durable thread id. |
| `pty-host` | `PtyProcess { handle: PtyHandle, exit: ExitMonitor }` | Provides provider-neutral PTY spawn, one owned output stream, input, resize, process-group signalling and termination. Claude PTY, the Codex raw plane and the test agent use it. |
| `replay-support` | `Recording`, `StrictReplay`, registries and probes | Supplies the shared, provider-neutral executable-specification corpus, sanitizing, inventory validation, strict replay, verification ledgers and additive drift reports. |

`crates/amux/src/agents/claude` and `crates/amux/src/agents/codex` are adapters.
They translate crate events into the protocol rows owned by amux, route typed
input to the control handle, implement delivery carriers, and save the minimum
provider identity needed to resume. Provider behavior does not belong there.

## Claude PTY source bundle

`claude::pty::Sources` is the complete input to the PTY session state machine:

- `PtySource` supplies the single raw output stream, an input writer, optional
  live PTY handle, and exit future.
- `HookSource` supplies typed Claude hook payloads.
- `TranscriptSource` supplies rows tagged with their transcript path and a
  relink operation for compact and clear transitions.
- `ClaudeVersion` is the observed provider version used for keymap resolution.
- `DelaySource` sleeps with the bounded live clock or advances replay's virtual
  clock, so keymap timing remains semantic without slowing corpus derivation.

Live construction obtains those sources from `pty-host`, the hook receiver,
the transcript tailer and a version probe. Recorded construction obtains the
same sources from strict-replay transports and the recording manifest.
`from_sources` is therefore the shared behavioral path; replay does not use a
separate imitation of the provider session.

The SDK and Codex boundaries use the same injection principle at their native
transport boundary: a live process and a strict replay both feed the same
session API. That lets amux backend tests instantiate the real adapters with a
recorded provider session.

## Typed kinds and protocols

The closed agent kind is Claude with a `Pty` or `Sdk` driver, Codex, or the
test agent. Each kind derives its protocol set. Claude PTY exposes
`terminal_v1` and `claude_pty_transcript_v1`; Claude SDK exposes only
`claude_sdk_v1`; Codex exposes `terminal_v1` and `codex_sdk_v1`. A request for
a protocol the kind does not expose returns a typed `NotExposed` error.

This keeps provider differences visible at the boundary. Claude PTY structured
input contains semantic intents, Claude SDK structured input contains SDK
commands, and Codex structured input contains Codex commands. Raw bytes are
confined to a terminal protocol.

## Driver capabilities and gaps

| Capability | Claude PTY | Claude SDK | Codex |
|---|---|---|---|
| Owned ordered event stream plus control handle | Supported | Supported | Supported |
| Structured prompt and interrupt | Semantic PTY intents | Stream-JSON controls | App-server turn controls |
| Raw terminal plane | Supported | Not exposed | Supported through `codex resume` |
| Permission or approval decisions | Semantic answers to named asks | Typed permission rows and input decisions | Typed app-server approvals |
| Suspend and resume | Claude session id and transcript relink | Claude session id, with a gap row before resumed ready | Codex thread id across server restart |
| A2A delivery | Supported by Claude socket with PTY fallback | Supported by stream input | Supported by item injection with turn fallback |
| Recipient-owned A2A record | Transcript confirmation | `amux.claude_sdk.message` with `delivery: "stream"` | `amux.codex_message` with the accepted carrier |
| Executable-specification corpus | Claude PTY recordings | Claude SDK recordings | Codex recordings |
| Opt-in live backend suite | `claude_pty_live` | `claude_sdk_live` | `codex_live` |
| Current gaps | No terminal screen model; unforeseen dialogs require raw attach | No chat UI; streaming partials, model/mode switching, context usage and MCP status are parked UI controls. Hook callbacks receive the neutral continue default; elicitation is declined and user dialogs are cancelled by default. | No provider-specific gap introduced by this boundary |

SDK-driven Claude is a full A2A recipient, not a reduced messaging mode. The
daemon formats the ordinary amux envelope, sends it as a stream user message,
and writes `amux.claude_sdk.message` only after Claude accepts it.

The SDK event boundary nevertheless preserves hook, elicitation and user-dialog
requests as typed crate events. The current daemon driver deliberately applies
the defaults in the table rather than exposing those controls in a UI. Those
defaults and the parked SDK UI controls are product gaps, not missing provider
crate capabilities.

## Executable specifications and derived rows

Every provider driver follows the same test story:

1. Crate unit tests cover deterministic behavior that a live capture cannot
   reliably induce.
2. A crate registry pairs each executable specification with one recording,
   its allowed models and the crate's minimum supported provider version.
3. The same specification function records against the real binary or verifies
   against `StrictReplay`. Replay matches writes byte for byte, delivers reads
   in causal order, and fails unless every transport and frame is accounted
   for.
4. Each recording manifest inventories every replay-relevant file by SHA-256,
   records its original provider version and model, and rejects orphaned,
   uninventoried, changed or below-minimum data.
5. amux's `derived_rows` test replays the crate recordings through the real
   daemon adapters and reproduces the committed structured row fixtures under
   `crates/amux/tests/fixtures/rows/` byte for byte. Its `claude-pty`,
   `claude-sdk`, and `codex` directories derive from
   `crates/claude/fixtures/pty`, `crates/claude/fixtures/sdk`, and
   `crates/codex/fixtures`, respectively.
6. Provider live suites remain opt-in and cover process-level behavior that a
   transport recording cannot prove.

The Claude registry contains separate SDK and PTY corpora. The Codex registry
uses codex-cli recordings. Both provider probes can list the registry and run
its specifications against the installed binary.

## Drift probes and verification ledgers

Recordings age; they do not expire at each provider release. A recording's
manifest preserves the version and model used to capture it and carries an
append-only `verified` ledger of later live versions. The crate declares a
minimum supported version, while replay itself remains version-independent and
must continue to pass for every recording in the mixed-version corpus.

`claude-probe probe` and `codex-probe probe` run the registered specifications
against the installed provider. A passing claim appends that provider version
and probe run id to the recording ledger without changing recorded traffic. A
failure re-records only the affected specification. The probe also writes an
additive drift report for newly observed frames, nested fields, discriminants,
and raw payload counts. Drift is evidence for review, not a reason by itself to
fail a behavior that still satisfies its claim.

For Claude PTY, a passing probe is also the only authority allowed to append a
verified version to the baked keymap. The keymap entry must name matching
recording evidence; provenance tests reject hand-authored verification.

## Compatibility boundary

The mobile `amux` library still builds without default features and therefore
without the PTY host. The separate amuxapp runtime bridge is not updated on this
branch and is expected to be broken until it adopts the typed agent kinds and
per-protocol payloads.
