# Codex backend fixture provenance

## Session-scoped MCP substrate

`mcp_substrate.jsonl` is a minimal structural projection of a no-turn capture
made on 2026-08-24 with installed `codex-cli 0.149.0`, local Codex source commit
`aec653daa9873bf44517a623fd033722737817a8`, and amux commit
`c4b69f4b9d21114b559c152643cf9e72d7176af6`. The bounded source command was
`timeout 180 python3 -B capture.py <capture-dir>`.

The source run used an isolated `CODEX_HOME`, project, and stdio MCP stub. It
copied no credentials and started no model turn. It exercised a fresh
`thread/start`, a cold `thread/resume` with synthetic nonempty history, and a
required-server startup failure. The stub advertised one extra tool so the
capture could prove Codex applied the five-name allowlist. The synthetic resume
history exercises request-scoped resume configuration; it does not claim that
an unmaterialized no-turn rollout can be resumed by ID.

The projection preserves request order, the complete MCP configuration,
absolute-command and environment placeholders, startup status spelling,
filtered inventory, and the required-server error. It omits volatile IDs,
timestamps, platform details, and duplicate protocol envelopes. The source
normalizer replaced the isolated root with `<ISOLATED_ROOT>` and the
machine-specific Python executable with `<ABSOLUTE_STUB_PYTHON>`, and rejected
secret-like material or surviving machine paths. `mcp_substrate.meta.json`
records the source and derived artifact identities and SHA-256 lineage.

The earlier dynamic-tool fixture is not retained as a compatibility oracle.
amux is unreleased, and that registration path is the behavior this work is
replacing.

## Structured backend scenarios

This fixture was captured and reduced on 2026-08-13 from the P5b live capture
rig against `codex-cli 0.147.0`, then redacted and normalized for deterministic
replay. User and assistant content is synthetic (`Reply exactly PONG.` / `PONG`),
thread, turn, item, approval, and request IDs are stable placeholders, timestamps
are relative fixture time, and local paths are replaced with `/work` and
`/Users/test/.codex`.

The reduced stream combines the six captured scenario families: pong, command
approval allow, command approval deny, file-change approval, interrupt, and
resume with history. `rows.jsonl` is a structural oracle: fields omitted there
are intentionally prose- or schema-detail-insensitive. The full raw params are
still asserted separately by the replay test.

The final `amux.codex_message` projection is daemon-synthesized rather than an
upstream capture. Its expected row is generated from the same authenticated
envelope fixture used by the Codex delivery tests, then checked alongside the
captured stream so the closed row vocabulary cannot reduce it to a type-only
unknown row.

The older `crates/codex-sdk/fixtures` remain SDK parser/transport anchors; this
fixture supersedes them only for amux backend semantics.
