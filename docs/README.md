# Documentation Map

Where each part of the system is documented. Each document owns its
subject; when two overlap, the owner named here wins.

- `HOW_IT_WORKS.md` — the user-facing model: installations, account profiles,
  what amux lets your devices do and why it is trustworthy.
- `PROTOCOL.md` — the wire protocol: carriers, links, streams, channels,
  routing, pairing,
  and the design rationale.
- `ARCHITECTURE.md` — the system: installations and profiles, configuration
  ownership, front-door discovery, servers, dispatcher, trust storage, service
  surfaces, isolation scope and internal layering.
- `A2A.md` — agent-to-agent messaging and families: envelopes, provider
  carriers, model-facing tools, parent/child lifecycle, and client behavior.
- [iPhone app](IOS.md) — packages, bridge, build pins, fixture driving, goldens,
  journeys, replay and device qualification; [performance](IOS_PERFORMANCE.md)
  owns measurement definitions, budgets and reviewed baselines.
- `UI.md` — the client layer: the ui-state reducer core, the
  kernel/per-agent-layer split, edge contracts, and the TUI.
- `CHAT.md` — the chat TUI view; companion to `UI.md`, which owns the
  client layer it stands on; includes the full-screen frame, interaction
  bindings, theme-file format, and `amux-shot` screenshot workflow.
- `CLAUDE_SDK.md` — the chat for a Claude agent driven over its stream-JSON
  interface: session facts in the header, streaming replies, tasks, context,
  ask panels and their live-validation gaps, and the surfaces shared with the
  other two chats.
- `ATTACHMENTS.md` — chat attachments and diff reviews: the canonical element
  syntax, artifact lifetime and cache, RPC and stream delivery, agent tool,
  and deferred client surfaces.
- `DEBUGGING.md` — agent workflow for profile debug reports: report locations,
  installation log tails, bundle layout, replay, marked tweaks, redaction,
  graduation and committed fixtures.
- [Build](BUILD.md) — toolchain and profile choices, reproducible tasks,
  committed protobuf output, and warm worktree snapshots.
- `TESTING.md` — what each kind of test proves: boundaries and suites, the
  three clocks, fixtures, journeys, lanes and the suite catalogue.
- `PERFORMANCE.md` — the qualified performance harness, budgets, baselines
  and the measurement decisions behind them.
- `CODEX.md` — the OpenAI Codex integration: process ownership, the two
  planes a codex agent exposes, the structured row vocabulary, and the
  client-side layer that folds it.
- `CLAUDE_TRANSCRIPT.md` — the grounded Claude Code transcript taxonomy
  consumed by the capture drift tooling and its committed fixtures.
- `../crates/shot/README.md` — the committed 120×40 PNG and wheel-recording
  tool for named TUI states.
- [Profile screenshots](screenshots/profiles/README.md) — the switcher and
  both account fleets, with hashes and reproducible capture commands.
- `PROVIDER_CRATES.md` — the canonical Claude, Codex, PTY-hosting and replay
  crate boundaries; session shapes, capabilities, gaps, corpora and drift
  ledgers.
- `KEYMAPS.md` — semantic Claude PTY input; keymap data, resolution,
  interpretation, provenance, management and screen-detection limits.
- `../crates/testnet/tests/spec/` and `../crates/ui-state/tests/spec/` — the executable specs. The suites read as
  documentation and locks the protocol's guarantees; run it with
  `just spec`.
- [App layer](NATIVE_INTEGRATION.md) — the three crates any rich client
  reuses, their one dependency rule, the harness seams, the bridge build
  recipes and what the tests hold.
- [Native runtime bridge](../crates/app-ffi/README.md) — C lifecycle, routing
  tokens, callback ownership and the debug loopback boundary.
- `CI.md` — native iOS verification recipes, pinned runner and commit-specific
  CI status.
- [RELEASE.md](RELEASE.md) — shipping the iPhone app without Expo: the
  marketing version and build number, the release tag, the signing
  contract, archive, export and validation, and the one-time App Store
  Connect setup only a person can do.
- [TESTNET.md](TESTNET.md) — isolated relay and daemon topologies, readiness and the
  served control door's loopback protocol, provider scripts and the offline smoke
  recipe.
- [CLOUD.md](CLOUD.md) — what the iPhone app asks of amux.sh: every endpoint it
  uses, the entitlement read, how a signed purchase reaches the cloud, and
  where each configuration value and secret lives.
- `../DEVLOG.md` — recent work history and decisions.
