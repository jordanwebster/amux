# iPhone performance: what is measured, on what, and against what

Every number the iPhone app is held to is defined here: the machine it is
measured on, the workload it is measured over, the budget it must meet, and
what the number stands for when it cannot stand for itself. `wt run ios-perf`
reads the two tables at the bottom of this document, so the numbers a person
reads here and the numbers the suite enforces are the same numbers.

Every number printed by a run was written by that run. The previous run's
verdict is deleted before anything is launched, and a run whose suite never
wrote one stops with that as its reason rather than reporting the file it
found on disk.

## Running it periodically

Run `timeout 3000 wt run ios-perf` when checking for regressions. It prints a
line per metric, with its median, budget and baseline comparison, and exits
non-zero if any measured metric exceeds its budget or regression tolerance.
Read `target/ios/perf/report.md` for the table, proxy labels and wall time;
`verdict.json`, `samples.json`, `cadence.json`, `lifecycle.json` and `size.md`
retain the underlying evidence. Per-run artifacts are ignored, so copy a run
somewhere durable when comparing it later.

This suite is intended for periodic use, not as a required gate on every pull
request. The branch's complete `ios-verify` recipe and its current push workflow
include it; that full verification is also available when qualifying a release.
Allow about seven and a half minutes on the pinned Mac with warm build outputs,
or about twenty minutes from a cold tree. Each run records its actual wall time.

Before spending that time, run `timeout 60 python3 -B scripts/ios-perf.py
--describe`. It prints the machine row and whether its baseline exists without
building or launching anything. `--machine` is the older spelling of the same
query. The wt recipe also accepts `--describe`, but runs its build prerequisites
first. An unknown Mac is refused. To enroll one, add its `sysctl -n hw.model`
value and a unique name to the Machines table below, record the OS, Xcode and
simulator configuration, then deliberately record and review its baseline.
`AMUX_PERF_MACHINE` selects an existing row explicitly, as CI does; it is not a
way to claim one Mac's measurements describe another.

## Baselines and drift

Baselines under `ios/Perf/baselines/` are tracked in git, one file per machine.
Record one with `timeout 3000 wt run ios-perf -- --baseline`, after checking
that the workload and measurement are still appropriate. Re-baselining is a
deliberate reviewed act with a reason in the commit message, never a way to
make a failing run pass. An invisible baseline lets performance ratchet
downward unnoticed: replacing yesterday's number becomes cheaper than fixing
the regression. Per-run output is disposable; the comparison history is not.

Budgets always apply on the pinned Mac, and so does the drift check: the median
may grow by at most 15% for timing, hitches and CPU, or 10% for memory over the
number recorded in `ios/Perf/baselines/pinned-mac.json`. That file is the
machine's history and an ordinary run needs it — a run that cannot find it stops
and says so rather than quietly falling back to the budgets alone, which would
leave a slow bleed unwatched for as long as nobody noticed the file was gone.
`--describe` reports whether it is there without measuring anything.
On the CI runner a missing baseline is reported as `no baseline for this runner`;
verification still runs the hard budgets and never records a baseline on its
own. A deliberate first baseline run also has to meet the hard budgets.

A refused drift is reported where the numbers are: the line for the measurement
says by how much it is over the recorded figure, and the run fails even where
the metric is comfortably inside its budget — which is the whole point of
recording one.

Drift catches a slow bleed that a budget alone misses. Cold first frame grew
from about 310 ms to about 439 ms across roughly 250 commits. Each incremental
change looked small, and the runs stayed inside the then-400 ms budget until
the last few. Comparing with the original 310 ms baseline would have flagged
the drift at about 357 ms. Moving the baseline with every run would have erased
that signal. The current simulator gate is 460 ms for the reason below; the
physical-phone target remains 400 ms.

Telling two machines apart is a different thing, and cold start needs it. Every
number here is taken in a simulator, and for cold start the simulator's own
cost is most of the number: an empty SwiftUI app that links StoreKit and
AuthenticationServices — as this app does, for subscriptions and web sign-in —
already draws its first frame at about 414 ms there, before a line of this
app's code runs. A simulator budget below that figure measures the simulator
and not the app. So cold start has two numbers, for two machines: a simulator
gate of 460 ms, derived below, and the 400 ms requirement on a phone, which no
recipe measures and which the physical-phone checklist holds.

## Measurement definitions

| Item | Pinned value |
| --- | --- |
| Mac | MacBook Pro Mac14,6, Apple M2 Max, 32 GB, macOS 26.5.2, Xcode 26.6 (17F113); the perf recipe refuses an unknown machine |
| Simulator | amux-golden: iPhone 17 Pro, iOS 26.5, 3× scale, en_US, 9:41 status bar, full battery; reports 60 Hz, so every frame-rate figure from it is a proxy |
| CI runner | GitHub-hosted `macos-26` (Xcode 26.6 default, iOS 26.5 simulator runtime, iPhone 17 Pro device type), Xcode selected explicitly in the workflow |
| Build | The `Measured` configuration: optimised the way a shipped build is, with the driving door, the fixtures and the workload generator still compiled in and testability on, and coverage and sanitizers off. One image: the packages are linked statically into the app, as they are in a shipped build, and the bridge inside it is the single copy built with the driving tools — the recipe asks the running app which bridge it has and refuses a build that answers with the shipping one. Every verdict names the configuration and says whether the code that took the numbers was optimised |
| Fleet workload | 40 cached agents over 3 hosts: 6 needing you, 4 finished, 3 unknown, 5 day-old, the rest running or idle; seed 1 |
| Conversation workload | 1,000 rows: 55% prose with markdown, 20% tool rows, 10% folded reads, 5% command output over 200 lines, 5% edits, 5% rules and unknown rows; seed 1 |
| Stream | 50 rows per second for 20 s appended to the conversation workload while the list auto-scrolls to the tail; the arriving rows carry identities that continue the transcript's, as a real feed's do |
| Network | Runner latency 0 ms and 100 ms; reconciliation measured at both, budget applies at both |
| Cold first frame | Kernel process start to the first presented frame containing the cached fleet rows themselves, shimmer running — not a launch image and not an empty list; 5 cold launches of a `Measured` build with the app terminated between; on the pinned simulator median ≤ 460 ms, worst ≤ 600 ms; the 400 ms this stands for on a phone is on the physical-phone checklist |
| Reconciliation | `streamConnected` to the last row's shimmer ending; median ≤ 1,000 ms at either latency |
| Optimistic echo | `sendTapped` to the first presented frame containing the row, taken over the conversation workload on the shipped page with the composer there; ≤ 1 frame interval, measured on the simulator as ≤ 17 ms and labelled a proxy for 8.3 ms on ProMotion |
| Streaming scroll | Hitch time ratio ≤ 5 ms per second (display-link missed-frame accounting, labelled a proxy for `XCTHitchMetric` on a device); main-thread CPU ≤ 60% of one core averaged over the stream; footprint ≤ 250 MB |
| Idle | After a 2 s settle with no stream, zero transcript commits and zero display-link ticks requested over 5 s |
| Cadence readiness | `capped` false, `disableMinimumFrameDurationOnPhone` true, preferred range upper bound equal to the display maximum; the simulator's 60 is recorded as a proxy |
| Lifecycle | Foreground: exactly one relay connection per host and no request while idle for 60 s; background 30 s: zero connections; foreground again: one connection within 2 s, and a fresh confirmation of the fleet within 1,000 ms — an arrival counted after the pickup, not the app's `reconciled` flag, which was already true when the phone was put away |
| Samples and tolerance | 5 samples per metric, simulator state reset between samples, one suite at a time; the median must meet the budget and must not exceed the recorded baseline by more than 15% (time, hitch, CPU) or 10% (footprint) |
| Not measured here | Cold start on a phone, presented-frame rates on ProMotion, thermal and battery behaviour on the oldest supported phone; these are the physical-phone checklist, which is satisfied by a recorded measurement and not by a tick |

## How a number is taken

The app marks named moments — `processStart`, `firstCachedFrame`,
`streamConnected`, `reconciled`, `sendTapped`, `echoCommitted`, `streamRow`,
`transcriptCommit`, `idleTick` — in every build, debug and release alike, so a
timing is never a property of the build it was taken from. Instruments shows
the same names on a timeline.

Workloads are generated from seed 1 rather than recorded, so two machines
measure the same bytes without shipping a fixture, and they are delivered
through the runtime's own callback: the same decoding, the same ordering and
the same main-thread application a relay-fed run would do.

`processStart` comes from the process table, not from the first line of
`main()`, so the dynamic linker's work is inside the cold-start measurement
rather than hidden by it. A run reports a cold launch in three parts — loading
the app, starting it, drawing the first frame — so the next regression can be
placed in one of them. The boundary between the first two is marked by an image
initialiser written in C, which the dynamic linker calls when it has finished
its work; that is the earliest moment a program can observe itself, and no
Swift declaration can reach it.

Two of those marks are left when a frame reaches the display rather than when
the state behind it changed, and they are not left at quite the same moment.
The cold first frame is marked one display refresh after the render server
committed the frame, which is a frame of slack nobody can see inside four
hundred milliseconds. The echo is marked at that commit instead: its whole
budget is one frame, so the same slack would double the number and report two
frames for work that took one.

The streaming and idle numbers are taken over the page the app pushes when
somebody opens an agent, whole: the fleet's drawer over the conversation, and
inside it the chrome, the transcript, the facts strip and the composer, with a
session in the store so the box is really there. Nothing is a stand-in and
nothing is left out. The container is what makes the stream a stream — it rests
at its tail and follows it while rows arrive, and a row appended below the fold
of a lazy stack is never built, so a list resting anywhere else would measure
nothing — but the strip, the foot and the composer are laid out on every frame
those arrivals cause, and a number taken with the feed alone would be a number
about a screen nobody uses.

The lifecycle numbers are the only ones in a run not taken inside the app, and
they could not be: how many connections a machine is holding is a fact about
the far end of the network, and being put away is something done to an app
rather than by it. So the recipe starts a relay and two machines and runs them
for real, points the app at them, pairs it, and reads the inventory the relay
itself keeps — while the app is in front, after a minute of nobody touching it,
and over five rounds of putting the phone behind another app for thirty seconds
and bringing it back. Bringing it back is checked to be the same process it put
away, because a phone switched on is not a phone picked up and the recovery it
would time is a cold start. Those samples land beside the app's own and are
judged against the same table.

Every run installs the app over a container it has erased first, so it starts
having never been signed in or paired. That is not tidiness: a machine this
phone has already been through is not offered for pairing again, and a run that
inherited the last one's trust would sit waiting for an offer of a machine the
runner had only just started.

A run also records what a shipped build weighs — a `Release` build for a phone,
unsigned, laid out on disk, with the bridge's own archives and the profile they
were built under beside it — and what the app asks the display for. Neither is
a budget: the size requirement is a policy about where size comes from, and the
cadence facts are about the app capping nothing, which on a simulator reporting
60 Hz cannot be the claim about 120.

A run can be asked for one group of measurements — `wt run ios-perf -- --only
streaming`, or `cold`, `reconciliation`, `echo` or `lifecycle` — which is for
working on that group rather than for reporting. The verdict then carries only the rows this
run measured, so a partial run cannot report a pass on a metric it never took;
recording a baseline needs a whole run, and asking for both is refused.

Every measurement is taken five times with the app's state reset between
samples, and the median is what a budget is applied to. One suite runs at a
time: two measurements sharing a machine measure each other.

What it costs to run, because a person deciding whether to start one should
not have to find out by starting one: on the pinned Mac, about seven and a half
minutes once the app is built — five cold launches, a suite of about two and a
half minutes, a release build for a phone to weigh, and a lifecycle audit whose
waits alone are three and a half — and about twenty from a cold tree, where
building the Rust bridge is the longest part and `wt run ios-rust` does it
before this recipe is reached. Every run prints its own figure and `report.md` carries it. The
recipe's own timeout is a hang guard and says nothing about how long a run
takes.

## Where the two cold-start numbers come from

The 400 ms was always a claim about a phone. It was checked on a simulator
because a simulator is the machine a recipe can drive, and for a while nothing
in this document told the two apart. These are the parts of one cold launch of
the probe home over the 40-agent cached fleet, Debug, on the pinned simulator,
median of five launches with the app terminated and its state reset between:

| Part of the launch | ms |
| --- | --- |
| Loading the app: process start to the end of the dynamic linker's work | 287 |
| Starting it: the system reaching this app's first line | 2 |
| Drawing the first frame, cached rows and all | 151 |
| Cold first frame | 439 |

Two thirds of that is loading, and almost all of the loading is frameworks.
Measured the same way, a hello-world SwiftUI app reaches its own first line in
206 ms; linking StoreKit takes it to 291 ms, AuthenticationServices to 296 ms,
and both together to 302 ms — they share dependencies, so the pair costs about
96 ms rather than 175. This app reaches its first line at 288 ms. The two
frameworks are here because the subscription screen and web sign-in need them,
and they are loaded whether or not a launch reaches either screen.

That fixes a floor. An empty SwiftUI app linking those two frameworks draws its
first frame at about 414 ms on this simulator. This app's own code accounts for
roughly 25 ms of its 439: from `App.init` to the first view body an empty app
spends 87 ms and this one spends 94, and building the forty cached rows the
first frame carries takes 3 ms.

So the simulator gate is 460 ms, which is the floor plus about double the app
code there is today — a real constraint, and one this app would fail if launch
work grew the way it has. The worst sample stays at 600 ms; the slowest of the
five measured launches was 491 ms. The 15% tolerance against a recorded
baseline is untouched, and it, rather than the budget, is what catches a
regression: it fires at about 505 ms on today's numbers.

Those parts were taken in Debug, before the suite was moved onto the optimised
`Measured` configuration, and moving it changed nothing here: the same five
launches read a median of 446 ms optimised against 447 ms unoptimised. Two
thirds of a launch is the dynamic linker, and optimisation has no opinion about
that.

The 400 ms stays where it belongs, on the physical-phone checklist, and stays
unmeasured until somebody runs the app on a phone.

## Proxies, stated plainly

- The simulator reports 60 Hz and composites through the Mac's display. Every
  frame-rate figure taken there — hitch time, echo frames, cadence readiness —
  stands in for a phone's number rather than being one.
- Hitch time is display-link missed-frame accounting, which is a proxy for
  `XCTHitchMetric` on a device.
- The optimistic echo budget of 17 ms is one simulator frame; on a ProMotion
  phone the same claim is 8.3 ms.

## The physical-phone checklist

Nothing here is measured by any recipe. A line is done when its `Measured`
cell holds a number somebody took on real hardware, and never before: a box a
person can tick proves nothing about a phone the app was never launched on, and
the cold-start line is the one that finally says whether 400 ms was the right
figure to ask a phone for.

| What a person must take | On what | Requirement | Measured |
| --- | --- | --- | --- |
| Cold first frame, median and worst of five cold launches | Oldest supported iPhone, household Wi-Fi rather than loopback | median ≤ 400 ms, worst ≤ 600 ms | not measured |
| Reconciliation after a cold start | Oldest supported iPhone, household Wi-Fi | median ≤ 1,000 ms | not measured |
| Hitch time over the 1,000-row streaming conversation, with `XCTHitchMetric` rather than the display-link proxy | ProMotion iPhone | ≤ 5 ms/s, and the presented frame rate reaches 120 Hz | not measured |
| Presented-frame cadence under the same streaming workload | Standard 60 Hz iPhone | reaches 60 Hz when the system permits it | not measured |
| Cadence with Low Power Mode, thermal constraints and accessibility settings | ProMotion and standard iPhones | adapts to the system-selected rate without imposing a fixed 60 Hz ceiling | not measured |
| Thermal state and battery drain after ten minutes of streaming | Oldest supported iPhone | nominal or fair, no serious drain | not measured |
| The optimistic echo, judged by eye | ProMotion iPhone | the row is in the frame after the tap | not measured |

Use the same seed, row count and stream rate described above in a signed
Measured build. Record the device model, OS/build, refresh-rate capability,
power and thermal state alongside each result. Use Instruments signposts for
launch/reconciliation and a device presentation or hitch trace for cadence;
retain the trace and the five individual samples, not just their average.
Repeat cold launches with the app terminated, and test return from background
with the same process still alive. Run VoiceOver, Dynamic Type, dictation and
picker checks separately from the timing run so that their results remain
identifiable. Distribution signing and live-service qualification are separate
release checks.

## Machines

The `Model` column is the machine’s `hw.model`. A machine that is not listed is
refused. Add a reviewed row before measuring on another Mac; `--describe`
checks this without running the suite.

| Machine | Model | Budgets | Baseline |
| --- | --- | --- | --- |
| `pinned-mac` | `Mac14,6` | hard | required |
| `macos-26` | `—` | recorded | required |

## Budgets

`Budget` is what the median must meet, `Worst` what the slowest sample must
meet, and `Tolerance` how far past a recorded baseline the median may drift.

| Metric | Unit | Budget | Worst | Tolerance |
| --- | --- | --- | --- | --- |
| `coldFirstFrameMs` | ms | 460 | 600 | 15% |
| `reconciliationMs` | ms | 1000 | | 15% |
| `echoFrames` | ms | 17 | | 15% |
| `hitchTimeRatioMsPerS` | ms/s | 5 | | 15% |
| `mainThreadCpuPercent` | % | 60 | | 15% |
| `footprintMB` | MB | 250 | | 10% |
| `idleCommits` | count | 0 | 0 | 0% |
| `connectionsPerHost` | count | 1 | 1 | 0% |
| `backgroundConnections` | count | 0 | 0 | 0% |
| `foregroundRecoveryMs` | ms | 2000 | | 15% |
