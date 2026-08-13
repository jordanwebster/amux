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
