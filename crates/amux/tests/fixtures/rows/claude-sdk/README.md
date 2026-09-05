# Claude SDK derived rows

These `claude_sdk_v1` row files are derived from the corresponding
recordings under `crates/claude/fixtures/sdk/`. The `derived_rows` integration
test opens each recording as a `claude::sdk::Session`, drives it through the
real amux Claude SDK adapter, and compares the complete emitted JSONL bytes
with the checked-in file.

The corpus covers text and streamed turns, permissions, interruption, resume,
multiple turns, subagent activity, compaction, clearing, and the turn limit.
Subagent rows include completion notifications after the first turn result.
Stream events are also compared directly with the recording's inbound JSON to
prove that every field and event survives the adapter in order.

Regenerate with `UPDATE_DERIVED_ROWS=1 timeout 900 wt test --
claude_sdk_derived_rows`, then verify with
`timeout 900 wt test -- derived_rows` without the update flag.
