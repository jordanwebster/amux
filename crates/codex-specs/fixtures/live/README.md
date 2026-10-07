# Codex live-run captures

Recordings of Codex CLI 0.160.0 taken by amux's live qualification run
(`just live codex`), on the account's default model. Unlike
`fixtures/runtime`, no specification produced them: amux's own interpreter
drove each session, over the app-server socket, so no spec can replay or
regenerate them, and they are not in the registry. A later live run
replaces them.

| capture | what it holds |
|---|---|
| `plan` | a plan for a one-line change, approved from amux, and the change made |
| `questions` | two questions asked together while planning, the first skipped and the second answered, then a third replied to instead |
| `usage` | one turn, then the usage windows read |
| `permission` | the agent made read-only from amux |
| `mode` | the agent put in plan mode from amux |

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Codex version and model the capture shows.
`codex-probe join <capture dir> <name>` writes one from the live run's
`target/live/recordings/codex/<scenario>/`. The strict decode tests, the
fakes' conformance run and the interpreter's `recorded_live_*` goldens read
them.
