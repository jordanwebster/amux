# Native iOS verification

Run `timeout 9000 wt run ios-verify` from the repository root. It runs the
available Rust and native iOS recipes in dependency order, stops at the first
failure, and refuses a configuration with no Rust checks. As native build,
unit, golden, journey and performance recipes are added, they join this check.
Golden baseline updates and deliberate perturbation runs remain explicit
commands.

While the native app is being built, this check verifies what exists rather
than what is planned. The goldens run over the states the app can open
(`wt run ios-goldens -- --built`): every one of them is captured in both
appearances and must still draw what it was locked as, and a state nobody has
built yet is reported and counted rather than failed. Built is a property of a
state, not of a screen: one screen draws several of them — the conversation, the
conversation whose host was lost, the same conversation at an accessibility
type size — and each is written, captured and locked on its own, so the first
one landing does not make the rest openable. A bare `wt run ios-goldens` is
unchanged — it asks the whole catalogue and fails naming every state still
owed. A state that opens and has no baseline fails either way; that is what
catches one built and never locked. The measured run happens only where a number
from it would mean something: on a machine whose budgets are written down in
`docs/IOS_PERFORMANCE.md`, or on one judged against its own recorded run once
that baseline file exists. Otherwise it is skipped with one line naming the
machine and the file, and recording the baseline enrols it with no further
edit. `AMUX_PERF_MACHINE` names the row deliberately; the GitHub runner sets
it because no hardware row identifies it. When the app is finished, the closing
work makes all of it required: the whole catalogue, every journey and a
measured run against the recorded runner baseline.

`timeout 900 wt run mobile-check` checks `amux`, `amux-ui` and `amux-mobile`
for ARM iOS devices and simulators with default features disabled. It also
rejects local-agent dependencies in that graph. Desktop host executables
explicitly enable local agents.

`timeout 1800 wt run ios-rust` builds both ARM iOS static libraries using the
workspace `mobile` profile: release inheritance, fat LTO, one codegen unit,
size optimization (`s`) and abort on panic. Each archive and its C header,
generated from Rust by cbindgen, are staged under
`target/ios/<target-triple>/`. `target/ios/size.txt` records both archive sizes
in bytes; these are not linked application sizes. Cargo's JSON build output
alongside the staged directories identifies the exact source artifacts,
including on cached builds.

The `ios` GitHub Actions job runs on `macos-26`, selects Xcode 26.6, and checks
that the iOS 26.5 simulator runtime and iPhone 17 Pro device type are available.
It installs XcodeGen, both ARM iOS Rust targets and the checksum-verified wt
0.3.0 release. Pushes to `main` and `nativeapp`, and pull requests targeting
`main`, run CI. The other jobs retain their platform matrix.

Use `ci-observe` for intermediate milestone checks and `ci-gate` for final
verification. Both require a clean `nativeapp` checkout and push `HEAD` to
`origin/nativeapp` without force. Neither runs from a different branch or with
tracked changes or normal untracked files present.

`timeout 3600 wt run ci-observe -- --record /tmp/amux-ci-observations.jsonl`
allows up to 180 seconds for this exact commit's push run of `ci.yml` to appear.
An already successful run exits zero only under the same whole-workflow and iOS
verification rules as `ci-status`. Any completed non-success job or workflow
exits nonzero immediately. For a queued or running run, the command inspects the
newest push run for a different commit. If that prior run completed unsuccessfully,
it waits up to 3,000 seconds for the current commit to succeed, failing on a CI
failure or the deadline. Otherwise it exits zero with `status: "pending"`.
That exit permits intermediate work to continue; it does not establish passing
CI. A missing run at the settle deadline always fails with `NoRunForHead`.

Override the windows with `--settle SECS` and `--wait SECS`; zero performs an
immediate observation. Each outcome prints one JSON object containing `head`,
`run_id`, `url`, `status` (`pending`, `success`, `failed` or `missing`), `prior`
and `observed_at`. `prior` is null when not consulted or absent; otherwise it
contains `head`, `run_id`, `url` and `conclusion`. Failures include a typed `error`
object. A deadline with a failed prior preserves both run records and remains
`pending` with `StillRunning`, exiting nonzero. Waiting updates go to stderr.
The optional `--record PATH` appends the stdout line, creating parent directories;
recording errors fail the command. Keep the record outside tracked source files
or in an ignored results directory so it does not dirty the next check.

`timeout 3600 wt run ci-gate` requires a clean `nativeapp` checkout, pushes
`HEAD` to `origin/nativeapp` without force, and waits up to 3,000 seconds for
that commit's CI run. It fails unless the
whole workflow succeeds and the `ios` job successfully executes its
`Run iOS verification` step.

For a read-only check, use `timeout 3300 wt run ci-status`, optionally adding
`-- --wait 3000`. The command prints one JSON result to stdout; waiting updates
go to stderr. A successful result includes `run_id`, `url`, `head` and
`ios_job_duration_secs`. Failures exit nonzero with an `error` field:

| Error | Meaning |
| --- | --- |
| `NotPushed` | Local HEAD differs from the remote nativeapp head. |
| `NoRunForHead` | No push run of ci.yml exists for that exact commit. |
| `StillRunning` | CI has not completed within the requested wait. |
| `Failed` | A job, the workflow or the required verification step failed or was skipped. |
| `JobAbsent` | A completed run has no ios job. |
| `ToolFailure` | Git, GitHub access or the response format failed. |

A newer failed run cannot be masked by an older successful run for the same
commit. Read the returned run URL to inspect job logs and failure artifacts.
