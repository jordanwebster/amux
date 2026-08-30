# Claude PTY derived transcript fixtures

These 18 fixtures are the daemon-visible `claude_pty_transcript_v1` rows
derived from the canonical Claude provider recordings in
`crates/claude/fixtures/pty/`. The derivation test strictly replays each
recording through `claude::pty::from_recording`, drives the provider with typed
semantic intents, and captures the real amux Claude PTY backend log. It then
compares that output byte for byte with the checked-in rows.

The recording-to-fixture names are:

| provider recording | row fixture |
|---|---|
| `prompt` | `pong` |
| `prompt_multiline` | `prompt_multiline` |
| `tools` | `tools` |
| `permission_allow_once` | `permission` |
| `permission_allow_scoped` | `permission_session` |
| `permission_deny_feedback` | `permission_deny_feedback` |
| `plan_approve` | `plan_approve` |
| `plan_auto` | `plan_auto` |
| `plan_request_changes` | `plan_reject` |
| `question_single` | `question_single` |
| `question_multi_other` | `question_multi` |
| `question_mixed` | `question_mixed` |
| `question_tabs` | `question_tabs` |
| `question_other_single` | `question_other_single` |
| `interrupt` | `interrupt` |
| `mode_cycle` | `mode_cycle` |
| `compact_relink` | `compact` |
| `clear_relink` | `clear` |

Each sidecar records `derived: true`, the provider recording name, and the
recorded Claude version. The current corpus was recorded with Claude Code
2.1.251. `external_readonly`, `stale_seq`, and `subscriptions` are process-only
live-suite scenarios and are deliberately not synthetic row fixtures.

Regenerate the corpus after an intentional daemon-boundary change with:

```sh
UPDATE_DERIVED_ROWS=1 timeout 900 \
  cargo test -p amux --test derived_rows claude_pty_recordings
```

Without `UPDATE_DERIVED_ROWS`, the same test is the executable byte-for-byte
specification. The tracked transcript semantics are documented in
[`docs/CLAUDE_TRANSCRIPT.md`](../../../../../../docs/CLAUDE_TRANSCRIPT.md).
