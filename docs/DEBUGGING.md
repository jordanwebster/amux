# Debugging from a report

A debug report is a frozen terminal frame plus the state needed to reproduce
it. Start with the report directory instead of asking the person who captured
it to reconstruct the session from memory. Reports never leave the machine;
amux does not upload or share them.

The capture and inspection commands in this document exist only in debug
builds. A release binary still writes bounded tripwire and panic reports, but
those reports declare the debug-only frame and trace parts absent and cannot be
replayed as screens.

In the debug TUI, `C-g` freezes the last drawn frame before either the fleet or
structured-chat key handler sees it. The flow asks for bug or tweak, a
top-level note, then zero or more marked rectangles with their own notes. Mouse
dragging and a keyboard fallback are available. Finishing writes and
self-replays the bundle, then repaints live state; the flow's own inputs never
enter the captured trace. The chrome is suspended during raw attach, so detach
before capturing an agent's screen.

## Find the report

By default reports live at `<data_dir>/reports`. A config can select one
canonical location instead:

```yaml
reports_dir: /absolute/path/to/amux-reports
```

Use the same config as the daemon when inspecting the default location:

```console
$ AMUX_CONFIG=/path/to/config.yaml amux debug report list
```

The list is newest first and gives the stamp, kind, status, replay verdict and
path. A report argument can be that path or a directory name beneath the
configured reports directory.

## Read the bundle

Open `report.json` first, or have the CLI validate and print it:

```console
$ amux debug report show 1788395144348-47628-tweak
```

Its fields are:

- `schema_version`: the directory format version. Readers reject versions they
  do not understand.
- `build`, `git_sha`, `created_at` and `stamp`: the build and capture identity.
- `kind`: `bug`, `tweak`, `tripwire`, `channel_overflow` or `panic`.
- `status`: `open` or `done`.
- `detail`: optional runtime detail for automatic reports.
- `note`: what the operator saw.
- `marks`: zero or more cell rectangles. Each has `x`, `y`, `width`, `height`
  and its own `note`; the origin is inclusive and the extent is exclusive.
- `viewport`: terminal width and height when a frame was captured.
- `parts`: whether each of `frame`, `trace`, `msgs`, `daemon` and `log` is
  present. An absent part carries the reason instead of silently disappearing.
- `replay`: `unchecked`, `reproduces`, or `diverges` with the first difference.

A full user capture contains these files:

| File | Contents |
| --- | --- |
| `report.json` | Header, notes, marks, part declarations and replay verdict |
| `frame.txt` | One row of frozen terminal cell text per line |
| `frame.styles` | One theme-class character per captured cell |
| `trace.jsonl` | Starting Model/view/theme snapshot, then ordered chrome events |
| `msgs.jsonl` | Recorder checkpoint and retained daemon messages |
| `daemon.json` | Hosts, routes, links, tunnels and session diagnostics |
| `log.txt` | A line-aligned tail of the local log, capped at 64 KiB |

The text and style map are the screenshot. A report contains no OS screenshot
or image file.

## Replay before changing code

Replay the final frame through the current build:

```console
$ amux debug report replay /path/to/report
Reproduces
Differing cells: none
Bounding rectangle: none
```

Replay always compares the final rendered frame with `frame.txt` and
`frame.styles`, writes the current verdict back to `report.json`, and exits
with status 1 on divergence. A divergence prints every differing cell and the
smallest rectangle containing them.

To inspect history, first note the available draw indices from an invalid
`--at` request if necessary, then render one of them:

```console
$ amux debug report replay /path/to/report --at 12 --frame
$ amux debug report replay /path/to/report --at 12 --styles
```

`--at` accepts draw-event indices, not arbitrary event counts. Replay is local
and deterministic: it contacts no daemon, opens no terminal, dispatches no
agent command and uses the times and viewport recorded in the trace.

## Work a tweak inside its marks

For a tweak, treat the marked rectangles as the requested change boundary.

1. Read the top-level note and every mark note in `report.json`.
2. Confirm the untouched build reproduces the report.
3. Change the smallest renderer rule that explains the marked issue.
4. Replay again. A deliberate visual fix normally diverges from the old frozen
   frame; check that every printed cell lies inside the relevant mark, where
   `x <= column < x + width` and `y <= row < y + height`.
5. Turn the intended rendering into an ordinary golden or focused test, then
   run the report fixture suite described below.

Unmarked differences mean the change has a wider visual effect than the report
asked for. Inspect or narrow it before calling the tweak fixed.

## Graduate a useful report

Graduation copies and redacts a report into the committed fixture root:

```console
$ amux debug report graduate /path/to/report chat_agent_activity
Graduated report to crates/amux-tui/tests/reports/chat_agent_activity
```

The name must match `surface_subject`: lowercase ASCII letters and digits for
the surface, an underscore, then lowercase letters, digits or underscores for
the subject. Pass `--into <directory>` outside the repository or before
`crates/amux-tui/tests/reports` exists. Graduation refuses an existing name;
it never overwrites a fixture.

Every source file is redacted. JSON is handled structurally, JSONL one value at
a time, and the frame, style map and log as text. The rules remove the local
home path, user and hostname along with known machine paths, email addresses,
secret fields and token forms. Inspect the result: redaction is intentionally
conservative and is not proof that arbitrary report content is safe to commit.

`manifest.json` records the fixture `name`, report `kind`, `original_stamp`,
redacted top-level `note`, redacted `marks`, `graduated_at`, and counts of
redacted secrets, machine paths and personal identifiers. The fixture keeps
the redacted report files beside it.

Run every committed fixture through the current renderer and privacy checks:

```console
$ timeout 600 cargo test -p amux-tui --test reports
```

## Retention and build gating

Bug and tweak reports are user-requested and are never removed by automatic
retention. `amux debug report prune` keeps the newest 20 reports of each
automatic kind and never touches user reports.

`C-g`, the frozen report flow, trace collection and the entire `amux debug`
command tree are debug-build surfaces. They do not appear in release help or
the release key table. The report bundle writer remains in every build so a
release tripwire or panic still leaves a local, self-describing degraded
report rather than a flat dump.
