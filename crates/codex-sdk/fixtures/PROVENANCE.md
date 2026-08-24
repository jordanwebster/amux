# Replay fixture provenance

These provisional smoke fixtures were derived on 2026-08-13 from recorded
`io.jsonl` sessions in `claude-sdk` commit
`f935f6233e143524f9965fb730c956e00fdff5c9`:

- `initialize/io.jsonl` ← `crates/codex-ui/fixtures/create_thread/io.jsonl`
- `thread_list/io.jsonl` ← `crates/codex-ui/fixtures/list_threads/io.jsonl`
- `turn_notifications/io.jsonl` ←
  `crates/codex-ui/fixtures/item_notification_shape/io.jsonl`

The source sessions were recorded against `codex-cli 0.118.0` on 2026-04-02.
They were reduced to SDK-level smoke exchanges, request IDs were normalized,
and paths/content were scrubbed. The added 0.147 notification messages in the
third fixture come from the schema generated locally from `codex-cli 0.147.0`
on 2026-08-13. These anchors are intentionally small and will be superseded by
amux-recorded fixtures in P5b.

`a2a_dynamic_tools/io.jsonl` is the retained SDK replay anchor for the generic
upstream dynamic-tool API. It is a reduced projection of a 2026-08-22 capture
from `codex-cli 0.148.0`, produced by the bounded command `timeout 600 cargo
test -p amux --test codex_capture -- c11_dynamic_tools` with an isolated Codex
home and synthetic prompt. Volatile notifications were omitted, identifiers
and paths were normalized, and request order, the `send` arguments
`to=probe`/`text=C11_SENT`, successful response, and turn completion were
preserved. amux no longer uses this transport for its own tools; the fixture
remains here because the SDK still supports and tests the generic protocol.
