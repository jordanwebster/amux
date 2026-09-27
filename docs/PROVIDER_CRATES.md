# Provider crate boundaries

The provider crates carry transport only: how to start Claude Code and Codex,
the bytes and frames they read and write, and the sockets beside them. Hosting
a provider session belongs to the agent process, and deciding what the
provider's traffic means belongs to the per-kind interpreter in
`crates/interpret`, which reads the same facts from a live provider and from a
recording.

## Crates

| Crate | Owns |
|---|---|
| `claude` | Launch arguments and merged settings for both ways of running Claude (`launch`); starting it under a PTY with its hooks pointed at a socket (`pty::spawn`); the semantic PTY input and the keymaps that turn it into bytes, resolved against the observed Claude version (`pty::input`, `pty::keymap`); the hook payloads and the `claude-hook` forwarder (`hooks`); the messaging-socket client (`messaging`); the transcript tailer (`transcript`); Claude's own session files (`history`); version probing (`version`); and the stream-JSON frames, control requests and option types (`sdk`). |
| `codex` | The app-server JSON-RPC protocol: spawning `codex app-server` over stdio, the initialize handshake, typed requests, notifications and approvals, and one handle per thread. |
| `claude-specs`, `codex-specs` | The recorded provider corpora and the executable specifications that produced them. Each specification drives a live provider through a recording driver and asserts what must hold; the same function replays the recording strictly. `claude-specs` keeps its drivers (`driver::sdk`, a stream-JSON client shaped after the published SDK, and `driver::pty`, a terminal session that reads hooks and the transcript) because only specifications host a session outside the agent process. |
| `pty-host` | Provider-neutral PTY spawn, one owned output stream, input, resize, process-group signalling and termination. |
| `redaction` | Removes secrets, local paths and personal identifiers for reports and captured traffic. |
| `replay-support` | Recordings, strict replay, registries, verification ledgers and drift reports shared by both spec crates. |

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
| Permission or approval decisions | Semantic answers to named asks | Typed permission, question and plan decisions | Typed app-server approvals |
| Elicitation and dialog decisions | Answered in the provider terminal; an open tool-server form or link shows as an unanswerable ask | Pending requests exposed to the chat; typed answers return through the control handle | Native obligations, with unsupported inputs visibly blocked |
| Session details | Transcript model/usage and hook permission mode | Streaming, tasks, model/mode controls, passive context meter, requested breakdown and MCP status | Native app-server facts and controls |
| Suspend and resume | Claude session id and transcript relink | Claude session id, with a gap row before resumed ready | Codex thread id across server restart |
| A2A delivery | Supported by Claude socket with PTY fallback | Supported by stream input | Supported by item injection with turn fallback |
| Recipient-owned A2A record | Transcript confirmation | `amux.claude_sdk.message` with `delivery: "stream"` | `amux.codex_message` with the accepted carrier |
| Executable-specification corpus | Claude PTY recordings | Claude SDK recordings | Codex recordings |
| Opt-in live backend suite | `claude_pty_live` | `claude_sdk_live` | `codex_live` |
| Current gaps | No terminal screen model; unforeseen dialogs require raw attach | No raw terminal; nested elicitation schemas are blocked; the dialog recognizer has no live frame behind it (see `CLAUDE_SDK.md`). | No provider-specific gap introduced by this boundary |

SDK-driven Claude is a full A2A recipient, not a reduced messaging mode. The
daemon formats the ordinary amux envelope, sends it as a stream user message,
and writes `amux.claude_sdk.message` only after Claude accepts it.

The SDK daemon keeps permission, elicitation and user-dialog requests pending
until a client answers or the session exits. It never auto-declines an
elicitation or auto-cancels a dialog. Each request and resolution becomes a
typed row; an answer to an unknown request returns an input error.

Managed SDK launches enable partial messages and register no amux hook
callbacks. Ordinary user hooks remain owned by Claude settings, with no
setting-source restriction; an unexpected callback receives the neutral continue
response. The SDK chat renders the native rows through its own client layer.
`CLAUDE_SDK.md` describes its controls and the source-known but unrecorded dialog
paths in Claude Code 2.1.261.

New Claude agents resolve `--driver` first, then `claude.driver`, then the
shipped `pty` default. CLI creation, TUI creation and MCP spawn use the same
resolver. Changing configuration never converts an existing agent.

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
   `crates/node/tests/fixtures/rows/` byte for byte. Its `claude-pty`,
   `claude-sdk`, and `codex` directories derive from
   `crates/claude-specs/fixtures/pty`, `crates/claude-specs/fixtures/sdk`, and
   `crates/codex-specs/fixtures/runtime`, respectively.
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
