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

Every `*.rows.jsonl` file is derived from the same-named recording under
`crates/codex/fixtures/`. The `derived_rows` integration test opens that
recording with strict replay, injects its `codex::Session` through
`CodexBackend::with_session`, drives the recorded scenario through the backend
boundary, and compares the complete emitted JSONL bytes with the committed
file. Set `UPDATE_DERIVED_ROWS=1` on that test to regenerate the files; there is
no separate hand-authored structural projection.

The recordings were captured with codex-cli 0.150.1 and explicit model
`gpt-5.6-luna`. Their sanitizer replaces machine paths and credentials while
preserving provider IDs, timestamps, notification payloads, and response
ordering, so the derived rows are deterministic offline but retain the full
backend-visible event shapes.

`amux.codex_message` remains a daemon-authored row and therefore is not present
in a provider-derived file. The UI A2A specification appends its explicit
carrier-row fixture after folding this corpus, keeping provider provenance and
daemon synthesis separate.
