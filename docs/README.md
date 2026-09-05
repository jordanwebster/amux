# Documentation Map

Where each part of the system is documented. Each document owns its
subject; when two overlap, the owner named here wins.

- `HOW_IT_WORKS.md` — the user-facing model: installations, account profiles,
  what amux lets your devices do and why it is trustworthy.
- `PROTOCOL.md` — the wire protocol: links, routing, tunnels, pairing,
  and the design rationale.
- `ARCHITECTURE.md` — the system: installations and profiles, configuration
  ownership, front-door discovery, servers, dispatcher, trust storage, service
  surfaces, isolation scope and internal layering.
- `A2A.md` — agent-to-agent messaging and families: envelopes, provider
  carriers, model-facing tools, parent/child lifecycle, and client behavior.
- `UI.md` — the client layer: the amux-ui reducer core, the
  kernel/per-agent-layer split, edge contracts, and the TUI.
- `CHAT.md` — the chat TUI view; companion to `UI.md`, which owns the
  client layer it stands on; includes the full-screen frame, interaction
  bindings, theme-file format, and `amux-shot` screenshot workflow.
- `ATTACHMENTS.md` — chat attachments and diff reviews: the canonical element
  syntax, artifact lifetime and cache, RPC and stream delivery, agent tool,
  and deferred client surfaces.
- `DEBUGGING.md` — agent workflow for profile debug reports: report locations,
  installation log tails, bundle layout, replay, marked tweaks, redaction,
  graduation and committed fixtures.
- `CODEX.md` — the OpenAI Codex integration: process ownership, the two
  planes a codex agent exposes, the structured row vocabulary, and the
  client-side layer that folds it.
- `CLAUDE_TRANSCRIPT.md` — the grounded Claude Code transcript taxonomy
  consumed by the capture drift tooling and its committed fixtures.
- `../crates/amux-shot/README.md` — the committed 120×40 PNG and wheel-recording
  tool for named TUI states.
- [Profile screenshots](screenshots/profiles/README.md) — the switcher and
  both account fleets, with hashes and reproducible capture commands.
- `PROVIDER_CRATES.md` — the canonical Claude, Codex, PTY-hosting and replay
  crate boundaries; session shapes, capabilities, gaps, corpora and drift
  ledgers.
- `KEYMAPS.md` — semantic Claude PTY input; keymap data, resolution,
  interpretation, provenance, management and screen-detection limits.
- `../crates/amux/tests/spec/` — the executable spec. The suite reads as
  documentation and locks the protocol's guarantees; run it with
  `timeout 600 wt run spec`.
- `../DEVLOG.md` — recent work history and decisions.
