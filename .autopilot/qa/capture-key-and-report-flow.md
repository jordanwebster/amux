# Capture key and report flow

## Result

Pass for the chrome-wide C-g capture flow in this worktree's debug build. The
chat source was the local `test-agent` fallback, not real Claude; the daemon
was this worktree's own instance. No OS screenshot was created.

## Reproduction

1. Started the daemon with `AMUX_CONFIG=$PWD/.wt/amux/config.yaml target/debug/amux server start`.
2. Created `test-agent` session `live-qa`, opened `target/debug/amux ui` in tmux,
   and pressed C-g.
3. Selected `t` (tweak), entered `streaming status looked stale`, and pressed
   Enter.
4. Used the keyboard marking fallback to create two rectangles and entered
   notes `stale header` and `footer detail`.
5. Pressed Enter to finish and observed the live fleet chrome return with the
   written-report notice.

The captured report is:

`/Users/jlw/.wt/trees/amux/debugging/.wt/amux/data/reports/1788392061530-37988-tweak`

Its header records kind `tweak`, status `open`, two marks, all five parts
present, and replay verdict `reproduces`.

## Evidence

The tmux frame sequence covers freeze, kind, note, both marking attempts and
notes, finish, and resumed chrome:

![freeze and capture prompt](../evidence/live-capture/frames/01-freeze.txt)

![tweak selection](../evidence/live-capture/frames/02-kind-tweak.txt)

![operator note](../evidence/live-capture/frames/03-note.txt)

![first mark](../evidence/live-capture/frames/04-drag-one.txt)

![second mark](../evidence/live-capture/frames/09-drag-two-closed.txt)

![finished report and resumed chrome](../evidence/live-capture/frames/11-finish-resume.txt)

`report-listing.txt` shows `report.json`, `frame.txt`, `frame.styles`,
`trace.jsonl`, `msgs.jsonl`, `daemon.json`, and `log.txt`, with no image files.
`header.json` is the copied report header. `no-network.txt` records the TUI
PID and no TCP or UDP sockets. `README.md` records the fallback chat and
worktree daemon provenance.

## Gaps

The installed Claude shim was not launched: in this unattended environment it
may authenticate and transmit workspace-derived data. The local test-agent's
fleet `o` action did not enter structured chat, so this evidence exercises the
implemented chrome-wide report flow from the fleet screen rather than a real
Claude mid-stream conversation. No product defect was filed because the
observed behavior is a backend/tooling limitation and the report itself was
written and self-verified.
