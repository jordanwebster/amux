# Claude SDK derived rows

These five `claude_sdk_v1` row files are derived from the corresponding
recordings under `crates/claude/fixtures/sdk/`. The `derived_rows` integration
test opens each recording as a `claude::sdk::Session`, drives it through the
real amux Claude SDK adapter, and compares the complete emitted JSONL bytes
with the checked-in file.
