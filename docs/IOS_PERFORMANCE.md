# iPhone performance: what is measured, on what, and against what

Every number the iPhone app is held to is defined here: the machine it is
measured on, the workload it is measured over, the budget it must meet, and
what the number stands for when it cannot stand for itself. `wt run ios-perf`
reads the two tables at the bottom of this document, so the numbers a person
reads here and the numbers the suite enforces are the same numbers.

Budgets are hard on the pinned Mac. Any other machine records its own baseline
row once and from then on is judged against that row with the same tolerances.
A budget is never loosened to fit a machine.

## Measurement definitions

| Item | Pinned value |
| --- | --- |
| Mac | MacBook Pro Mac14,6, Apple M2 Max, 32 GB, macOS 26.5.2, Xcode 26.6 (17F113); the perf recipe refuses an unknown machine |
| Simulator | amux-golden: iPhone 17 Pro, iOS 26.5, 3× scale, en_US, 9:41 status bar, full battery; reports 60 Hz, so every frame-rate figure from it is a proxy |
| CI runner | GitHub-hosted `macos-26` (Xcode 26.6 default, iOS 26.5 simulator runtime, iPhone 17 Pro device type), Xcode selected explicitly in the workflow |
| Fleet workload | 40 cached agents over 3 hosts: 6 needing you, 4 finished, 3 unknown, 5 day-old, the rest running or idle; seed 1 |
| Conversation workload | 1,000 rows: 55% prose with markdown, 20% tool rows, 10% folded reads, 5% command output over 200 lines, 5% edits, 5% rules and unknown rows; seed 1 |
| Stream | 50 rows per second for 20 s appended to the conversation workload while the list auto-scrolls to the tail |
| Network | Runner latency 0 ms and 100 ms; reconciliation measured at both, budget applies at both |
| Cold first frame | Kernel process start to the first presented frame containing the cached fleet rows; 5 cold launches with the app terminated between; median ≤ 400 ms, worst ≤ 600 ms |
| Reconciliation | `streamConnected` to the last row's shimmer ending; median ≤ 1,000 ms at either latency |
| Optimistic echo | `sendTapped` to the first presented frame containing the row; ≤ 1 frame interval, measured on the simulator as ≤ 17 ms and labelled a proxy for 8.3 ms on ProMotion |
| Streaming scroll | Hitch time ratio ≤ 5 ms per second (display-link missed-frame accounting, labelled a proxy for `XCTHitchMetric` on a device); main-thread CPU ≤ 60% of one core averaged over the stream; footprint ≤ 250 MB |
| Idle | After a 2 s settle with no stream, zero transcript commits and zero display-link ticks requested over 5 s |
| Cadence readiness | `capped` false, `disableMinimumFrameDurationOnPhone` true, preferred range upper bound equal to the display maximum; the simulator's 60 is recorded as a proxy |
| Lifecycle | Foreground: exactly one relay connection per host and no request while idle for 60 s; background 30 s: zero connections; foreground again: one connection within 2 s and `reconciled` within 1,000 ms |
| Samples and tolerance | 5 samples per metric, simulator state reset between samples, one suite at a time; the median must meet the budget and must not exceed the recorded baseline by more than 15% (time, hitch, CPU) or 10% (footprint) |
| Not measured here | Presented-frame rates on ProMotion, thermal and battery behaviour on the oldest supported phone; these are the physical-phone checklist, recorded as not done until a person runs it |

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
rather than hidden by it.

Every measurement is taken five times with the app's state reset between
samples, and the median is what a budget is applied to. One suite runs at a
time: two measurements sharing a machine measure each other.

## Proxies, stated plainly

- The simulator reports 60 Hz and composites through the Mac's display. Every
  frame-rate figure taken there — hitch time, echo frames, cadence readiness —
  stands in for a phone's number rather than being one.
- Hitch time is display-link missed-frame accounting, which is a proxy for
  `XCTHitchMetric` on a device.
- The optimistic echo budget of 17 ms is one simulator frame; on a ProMotion
  phone the same claim is 8.3 ms.

## The physical-phone checklist

Nothing here is measured by any recipe, and none of it is ticked until a person
has run it on real hardware. It is recorded as not done.

- [ ] Cold start and reconciliation on the oldest supported iPhone, on a
      household Wi-Fi network rather than loopback.
- [ ] Streaming scroll of the 1,000-row conversation on a ProMotion iPhone,
      with `XCTHitchMetric` rather than the display-link proxy, confirming the
      presented frame rate reaches 120 Hz.
- [ ] Thermal state after ten minutes of streaming, and battery drain over the
      same period, on the oldest supported iPhone.
- [ ] The optimistic echo, judged by eye at 120 Hz: the row must appear in the
      frame after the tap.

## Machines

The `Model` column is the machine's `hw.model`. A machine that is not listed is
refused: an unrecognised Mac has no budget row and no baseline, and a number
from it would mean nothing.

| Machine | Model | Budgets | Baseline |
| --- | --- | --- | --- |
| `pinned-mac` | `Mac14,6` | hard | optional |
| `macos-26` | `—` | recorded | required |

## Budgets

`Budget` is what the median must meet, `Worst` what the slowest sample must
meet, and `Tolerance` how far past a recorded baseline the median may drift.

| Metric | Unit | Budget | Worst | Tolerance |
| --- | --- | --- | --- | --- |
| `coldFirstFrameMs` | ms | 400 | 600 | 15% |
| `reconciliationMs` | ms | 1000 | | 15% |
| `echoFrames` | ms | 17 | | 15% |
| `hitchTimeRatioMsPerS` | ms/s | 5 | | 15% |
| `mainThreadCpuPercent` | % | 60 | | 15% |
| `footprintMB` | MB | 250 | | 10% |
| `idleCommits` | count | 0 | 0 | 0% |
| `connectionsPerHost` | count | 1 | 1 | 0% |
| `backgroundConnections` | count | 0 | 0 | 0% |
| `foregroundRecoveryMs` | ms | 2000 | | 15% |
