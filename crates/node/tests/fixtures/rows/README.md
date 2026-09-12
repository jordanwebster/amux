# Derived provider rows

Each directory contains daemon-visible rows derived through the real amux
adapter from recordings owned by the named provider crate:

| Directory | Provider recording directory | Plane |
|---|---|---|
| `claude-pty/` | `crates/claude/fixtures/pty/` | `claude_pty_transcript_v1` |
| `claude-sdk/` | `crates/claude/fixtures/sdk/` | `claude_sdk_v1` |
| `codex/` | `crates/codex/fixtures/` | `codex_sdk_v1` |

`crates/amux/tests/derived_rows.rs` opens the provider recording, constructs
the canonical crate session, drives it through the real daemon adapter, and
compares the complete emitted JSONL bytes with these files. Set
`UPDATE_DERIVED_ROWS=1` only when an intentional boundary change requires the
derived output to be regenerated.
