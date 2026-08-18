# Documentation Map

Where each part of the system is documented. Each document owns its
subject; when two overlap, the owner named here wins.

- `HOW_IT_WORKS.md` — the user-facing model: what amux lets your devices
  do and why it is trustworthy.
- `PROTOCOL.md` — the wire protocol: links, routing, tunnels, pairing,
  and the design rationale.
- `ARCHITECTURE.md` — the system: processes, the two-server model, the
  dispatcher, trust storage, service surfaces, internal layering.
- `UI.md` — the client layer: the amux-ui reducer core, the
  kernel/per-agent-layer split, edge contracts, and the TUI.
- `CHAT.md` — the chat TUI view; companion to `UI.md`, which owns the
  client layer it stands on.
- `CODEX.md` — the OpenAI Codex integration: process ownership, the two
  planes a codex agent exposes, the structured row vocabulary, and the
  client-side layer that folds it.
- `CLAUDE_TRANSCRIPT.md` — the grounded Claude Code transcript taxonomy
  consumed by the capture drift tooling and its committed fixtures.
- `../crates/amux/tests/spec/` — the executable spec. The suite reads as
  documentation and locks the protocol's guarantees; run it with
  `timeout 600 cargo test -p amux --features testnet --test spec`.
- `../DEVLOG.md` — recent work history and decisions.
