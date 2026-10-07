# Claude live-run captures

Recordings of Claude Code 2.1.292 taken by amux's live qualification run
(`just live claude_sdk`, `just live claude_pty`), on the account's own
sign-in. Unlike `fixtures/sdk` and `fixtures/pty`, no specification
produced them: amux's own interpreter drove each session, so no spec can
replay or regenerate them, and they are not in either registry. A later
live run replaces them.

| capture | what it holds |
|---|---|
| `sdk/plan` | a plan for a one-line change, approved from amux, and the change made |
| `sdk/questions` | two questions asked together, the first skipped and the second answered, then a third replied to instead |
| `sdk/usage` | one turn, then the usage windows read |
| `pty/plan` | the same plan, through terminal Claude's plan menu |
| `pty/questions` | the same questions, the skip moved through terminal Claude's form to its review screen |

Each folder is sanitized like a recording: `io.jsonl`, `spawn.jsonl` and a
`manifest.json` naming the Claude version and model the capture shows.
`claude-probe join <capture dir> <name>` writes one from the live run's
`target/live/recordings/<kind>/<scenario>/`. The strict decode tests, the
fakes' conformance run and the interpreter's `recorded_live_*` goldens read
them.
