# Claude Code Instructions

amux is in active development and is not released. Do not preserve backwards
compatibility for old local protocols, config keys, or public APIs.

Use these sources as the current guidance:

- `AGENTS.md` for repo-level agent instructions.
- `docs/NEW_ARCHITECTURE.md` for the active architecture and wire-protocol spec.
- `notes/REFACTOR_PROGRESS.md` for migration status, review findings, and
  verification history.

The old custom RPC/framing/WebSocket architecture has been removed. Current work
should use generated gRPC services, `RoutingService.Connect` for host links,
in-band routing events, tunnel-backed routed `AgentService` calls, and the
generated local `ClientService` surface.
