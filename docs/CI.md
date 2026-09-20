# Native iOS verification

Run `just ios verify` from anywhere in the checkout. It runs the Rust and
native iOS recipes in dependency order, stops at the first failure, and
refuses a recipe list that a justfile no longer declares. Golden baseline
updates and deliberate perturbation runs remain explicit commands. `just
--list ios` shows every phone recipe with its purpose; each carries its own
wall-clock bound, so a firing bound is a hang to diagnose.

The gate runs native component snapshots after building the app, so visual
changes to those components are checked on pull requests rather than waiting
for the nightly display suite. The examples are also available in Xcode's
component gallery. These in-process pictures check native layout and styling;
they do not replace full-screen compositor or real-input journey coverage.

`just ios goldens` captures the routine full-screen suite in both appearances.
States whose variations have moved to native component snapshots name those
examples in the manifest and are excluded from this default selection. Use
`--all` to run the historical full catalogue, or name a state explicitly.
`--built` reports unopened states instead of failing on them; missing baselines
still fail. See [native visual testing](IOS.md#component-snapshots-and-full-screen-goldens)
for coverage responsibilities and deliberate baseline updates. The
measured run happens only where a number from it would mean something: on a
machine whose budgets are written down in `docs/IOS_PERFORMANCE.md`, or on one
judged against its own recorded run once that baseline file exists. Otherwise
it is skipped with one line naming the machine and the file, and recording
the baseline enrols it with no further edit. `AMUX_PERF_MACHINE` names the
row deliberately; the GitHub runner sets it because no hardware row identifies
it.

The nightly `iOS captures` workflow compares components and the routine
full-screen suite on a GitHub runner with both pinned devices booted;
dispatching it by hand also runs the journeys, accessibility and performance
suites. Component comparison artifacts and timings are uploaded alongside the
full-screen evidence. Exact pixels still require the pinned native environment;
the display comparisons exclude the declared system-chrome rectangles.

No capture is quarantined: every selected capture gates on its pixel difference. Three of
them — `strip.light`, `strip.dark` and `ax-composer.dark` — were, until the
transcript that drew them was fixed on 2026-09-14, and
`apps/apple/Goldens/BASELINE.md` says what was wrong with it. The manifest can
still mark a capture flaky, which keeps it in every run and prints its verdict
while making only the pixel difference non-gating; a failed capture, a missing
baseline or a size change fails either way. Marking one is an argument to be
made in the open and nothing carries the mark today.

## The bridge

`just ios rust` builds the one bridge slice a development build links: the
simulator architecture, with the driving tools, under the ordinary `release`
profile, into a Cargo target directory owned by that triple under
`target/ios/rust-cargo`. It packages `target/ios/AmuxAppDebugTools.xcframework`
only when the static library or its generated C header changed, and it runs
no cargo at all when no Rust input changed, so a Swift-only edit followed by
`just ios build` or `just ios unit` compiles nothing in Rust and repackages
nothing. The Swift package's binary target names the shipping framework, so a
tree that has never packaged for shipping receives the same development slice
there as a stand-in; the debug configurations force-load the driving library
first, so which archive sits at that path does not change what they link.

`just ios package` builds every shipping slice — simulator and device — under
the workspace `mobile` profile (release inheritance, fat LTO, one codegen
unit, size optimisation, abort on panic), assembles
`target/ios/AmuxApp.xcframework`, and links it from a bare Swift executable
run on the pinned simulator. `just ios scope-audit` and `just ios release`
depend on it. `target/ios/size.txt` records the archive sizes of whichever
recipe ran last; these are not linked application sizes. Cargo's JSON build
output beside the staged slices identifies the exact source artifacts,
including on cached builds. `SDKROOT` is never exported for a whole recipe:
cargo fingerprints host-side build scripts with it, so alternating simulator
and device SDKs would rebuild the shared host graph every time.

`just mobile-check` checks the provider-free client graph for ARM iOS devices
and simulators; `just ios graph-check` proves the bridge's own dependency
graph reaches no provider, agent host or test crate, with the driving tools
as the one deliberate exception. Desktop host executables compose the agent
runtime explicitly.

## GitHub

The `iOS verification` job (`ios-verify` in `.github/workflows/ci.yml`) runs
on `macos-26`, selects Xcode 26.6, and checks that the iOS 26.5 simulator
runtime and iPhone 17 Pro device type are available. It installs XcodeGen,
`just`, the pinned stable toolchain with both ARM iOS targets and the nightly
formatter, then runs `just ios verify` and pairs the preserved design
references. The other jobs retain their platform matrix.

Use `just ios ci-observe` for intermediate milestone checks and `just ios
ci-gate` for final verification. Both require a clean `nativeapp` checkout and
push `HEAD` to `origin/nativeapp` without force. Neither runs from a different
branch or with tracked changes or normal untracked files present.

`just ios ci-observe --record /tmp/amux-ci-observations.jsonl` allows up to
180 seconds for this exact commit's push run of `ci.yml` to appear. An already
successful run exits zero only under the same whole-workflow and iOS
verification rules as `ci-status`. Any completed non-success job or workflow
exits nonzero immediately. For a queued or running run, the command inspects
the newest push run for a different commit. If that prior run completed
unsuccessfully, it waits up to 3,000 seconds for the current commit to
succeed, failing on a CI failure or the deadline. Otherwise it exits zero with
`status: "pending"`. That exit permits intermediate work to continue; it does
not establish passing CI. A missing run at the settle deadline always fails
with `NoRunForHead`.

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

`just ios ci-gate` requires a clean `nativeapp` checkout, pushes `HEAD` to
`origin/nativeapp` without force, and waits up to 3,000 seconds for that
commit's CI run. It fails unless the whole workflow succeeds and the `iOS
verification` job successfully executes its `Run iOS verification` step.

For a read-only check, use `just ios ci-status`, optionally adding
`--wait 3000`. The command prints one JSON result to stdout; waiting updates
go to stderr. A successful result includes `run_id`, `url`, `head` and
`ios_job_duration_secs`. Failures exit nonzero with an `error` field:

| Error | Meaning |
| --- | --- |
| `NotPushed` | Local HEAD differs from the remote nativeapp head. |
| `NoRunForHead` | No push run of ci.yml exists for that exact commit. |
| `StillRunning` | CI has not completed within the requested wait. |
| `Failed` | A job, the workflow or the required verification step failed or was skipped. |
| `JobAbsent` | A completed run has no iOS verification job. |
| `ToolFailure` | Git, GitHub access or the response format failed. |

A newer failed run cannot be masked by an older successful run for the same
commit. Read the returned run URL to inspect job logs and failure artifacts.
