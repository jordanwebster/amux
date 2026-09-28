# Debugging from a report

A debug report is a frozen frame plus the state needed to reproduce it. The
frame is either the cells a terminal drew, in `frame.txt` and `frame.styles`,
or the picture a phone composited, in `frame.png` with its point size and scale
recorded in `report.json`. Marked rectangles are measured in whatever the frame
is measured in: terminal cells, or points on the phone. Start with the report
directory instead of asking the person who captured it to reconstruct the
session from memory.

Nothing is uploaded in the background. A report captured here stays here. A
report captured on the phone also stays on the phone until the person who
captured it presses Send on the report screen: that sends one bundle — the
picture, the recordings, the rectangles they drew and the notes they wrote — to
their own amux account, and nothing else leaves the phone. Sending is always
theirs to do, once, per report.

If your prompt contains only this document and a report directory, begin by
running `amux debug report replay <report-directory>`. Then read `report.json`
and `frame.txt` and describe where every marked rectangle lands before
inspecting source code.

The terminal capture and inspection commands in this document exist only in
debug builds. A release binary still writes bounded tripwire and panic reports,
but those reports declare the debug-only frame and trace parts absent and cannot
be replayed as screens. The phone is different: reporting a problem is in every
build of the app, described below.

In the debug TUI, `C-g` freezes the last drawn frame before either the fleet or
structured-chat key handler sees it. The flow asks for bug or tweak, a
top-level note, then zero or more marked rectangles with their own notes. Mouse
dragging and a keyboard fallback are available. Finishing writes and
self-replays the bundle, then repaints live state; the flow's own inputs never
enter the captured trace. The chrome is suspended during raw attach, so detach
before capturing an agent's screen.

On the phone the same flow starts from a screenshot. iOS notifies the app after
taking the system screenshot. The app immediately freezes its own composited
frame, the runtime's recording and the view-state trace before showing report
controls. The app frame may differ from the system screenshot; there is no
guaranteed maximum delay between the two captures.

An app-owned Report pill opens the report on that already frozen frame. A tap
elsewhere in the app dismisses the offer and releases the capture. The system
owns its screenshot preview, which may be a thumbnail or cover the app; the
app cannot position its prompt relative to that preview. Returning to the app
finds the same offer and frozen frame, without a Share step or Photos access.
Report a Problem under Help on the You tab also freezes the screen before
opening the report. There is no shake gesture: shaking is undo on iOS, and a
phone in a pocket shakes all the time.

Both entry points exist in every build of the app, Release included. A Release
report carries the picture, the rectangles and notes, and the session and host
records the runtime keeps (the profile's dump). It declares `trace.jsonl`
absent, because only a build with the driving tools records the view-state
trace. The log part is absent in every build: the app logs through
the system, which gives no app its records back. A report is filed under the
signed-in account on screen; with nobody signed in, the report screen says so
and Send is unavailable.

## Find the report

Reports belong to the selected profile. By default they live at that profile's
`<data_dir>/reports`, allocated beneath the installation root as
`profiles/<UUID>/data/reports`. Renaming a profile does not move its reports.
Switching profiles in the TUI changes the destination for subsequent captures
and automatic runtime reports, including panic reports, to the profile now on
screen.
The **installation configuration** can override this with one shared location
for all profiles:

```yaml
reports_dir: /absolute/path/to/amux-reports
```

With this override, all profiles share the report directory without UUID
subdirectories; listing or pruning it sees reports from every profile.

Use `amux profiles` to find the profile, then select it when inspecting reports,
or set `AMUX_CONFIG` to its profile configuration (which points to the
installation configuration):

```console
$ amux --profile Work debug report list
$ AMUX_CONFIG=/path/to/root/profiles/UUID/config.yaml amux debug report list
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
| `daemon.json` | Selected profile's hosts, routes, links, channels and session diagnostics |
| `log.txt` | Installation-wide log tail, line-aligned and capped at 64 KiB |

The text and style map are the screenshot. A report contains no OS screenshot
or image file.

The log tail is **installation-wide**, not filtered to the selected profile.
It can include activity from other profiles and local clients even when the
report lives in one profile's directory. Logging uses `AMUX_LOG` when set,
otherwise `$XDG_STATE_HOME/amux/amux.log` (fallback
`~/.local/state/amux/amux.log`). Use the same `AMUX_LOG` for daemon startup and
the capturing client; `amux init` also starts the installation. Distinct
worktree installations need distinct log paths if their tails should stay
separate.

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
Graduated report to crates/tui/tests/reports/chat_agent_activity
```

The name must match `surface_subject`: lowercase ASCII letters and digits for
the surface, an underscore, then lowercase letters, digits or underscores for
the subject. Pass `--into <directory>` outside the repository or before
`crates/tui/tests/reports` exists. Graduation refuses an existing name;
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
$ just test -- every_committed_report_fixture_reproduces
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
