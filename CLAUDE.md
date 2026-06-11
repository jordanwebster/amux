# Claude Code Instructions

amux is in active development and is not released. Do not preserve backwards
compatibility for old local protocols, config keys, or public APIs.

Use these sources as the current guidance:

- `AGENTS.md` for repo-level agent instructions.
- `docs/PROTOCOL.md` for the wire protocol: links, routing, tunnels,
  pairing, and the design rationale.
- `docs/ARCHITECTURE.md` for the system: processes, the two-server model,
  the dispatcher, trust storage, service surfaces, internal layering.
- `crates/amux/tests/spec/` — the executable spec. The suite reads as
  documentation and locks the protocol's guarantees; run it with
  `timeout 600 cargo test -p amux --features testnet --test spec`.
- `DEVLOG.md` for recent work history and decisions.

Wrap every test invocation in `timeout` (a firing timeout is a hang to
diagnose, not a slow suite). Update DEVLOG.md in the same commit as each
chunk of work. Never add Co-Authored-By trailers.

Committed documentation lives in `docs/`; `notes/` is gitignored working
material.
