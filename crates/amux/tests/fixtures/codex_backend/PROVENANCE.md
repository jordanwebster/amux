# Codex backend fixture provenance

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
