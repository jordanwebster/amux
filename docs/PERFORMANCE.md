# Performance qualification

Performance is a test of a specified workload on a reference environment. It
has an oracle, an absolute budget and a drift bound against a reviewed
baseline, and it is environment-dependent, so it runs in a qualification lane
on enrolled machines, never as part of an ordinary push. Deterministic work
bounds such as retained rows and bytes, cache misses and message counts belong
in ordinary tests; elapsed-time claims belong here.

The desktop harness lives with the network harness today and the phone
harness lives with the app. The convention to converge on is one manifest
format for every platform (workload, metric, unit, statistic, budget, drift,
reference environment), one baseline layout keyed by enrolled machine and one
report format, with harness code staying with its platform. Measure user
outcomes: store-painted startup, input to echo, streaming while typing and
scrolling, reconnect volume and latency, attachment and review opening, bridge
rendering, idle resources and sustained memory.

## Running it

`just perf` builds the qualified harness in release mode and measures the
desktop frame loop, store-backed cold start and chat attachment, fold bounds,
SQLite commit and maintenance work, summarizer cost, and reconnect wire size.
The report names the enrolled hardware and OS, profile and features, workload
seed, identity-growth mode, warm-up, sample count, timestamps, statistic,
budget, committed baseline and drift. The desktop report also names its
reference state: one child process keeps one core busy for the qualification,
so the CPU cluster remains active without adding its CPU time or memory to the
measured process. It fails on an absolute budget miss, on time drift above 15%,
or on memory drift above 10%. On a Mac with Xcode, the same invocation also
runs `just ios perf` under its own simulator lease; that recipe prepares the
pinned simulator, and a failure there fails the combined recipe. A desktop
build or measurement failure stops the recipe before the phone suite starts.

```sh
just perf
just perf --baseline
just perf soak
just perf soak --baseline
```

Desktop baselines live at `crates/testnet/perf/baselines/<hw.model>.json`, and
soak baselines live separately at
`crates/testnet/perf/baselines/<hw.model>-soak.json`. Phone baselines live at
`apps/apple/Perf/baselines/<machine>.json`. They are valid only for the
recorded machine model, release profile and feature set. An unknown hardware
model is refused. `--baseline` records the complete workload it accompanies
while still enforcing every absolute budget; it never turns a miss into the
new expectation. A baseline also records the reference state, and a report is
comparable only with a baseline recorded in that same state; a mismatch is
refused just like a different machine model.

An otherwise idle Apple Silicon machine is not a stable latency reference for
these bursty workloads. When no core is active, the cluster can remain in a
low-power state between wakeups and produce wall-clock results several times
slower and with wider spread, even though the work itself has not changed.
The absolute budgets hold both when the machine is idle and when the cluster
warmer is active. The warmer defines the repeatable state used for relative
drift; it does not change any workload, statistic, budget or drift limit.

The two `TUI cold start` rows measure an exec through the store-painted first
fleet frame with `AMUX_TUI_DIRECT_PROFILE=1`: the fixture profile socket is
absent and no installation daemon is spawned. Before this capture state was
fixed, the production connector started a fixture daemon beside some samples.
Six focused runs of the unchanged binary measured 14.200/14.254,
14.290/14.449, 14.843/14.914, 14.117/13.896, 15.556/14.989 and
14.159/15.349 ms for 40/200 agents, while three full qualifications shifted
both rows to roughly 30 ms. One observed fixture daemon failed with
`installation root is already in use: /Users/jlw/.local/share/amux`; that
message means the sample included a failing daemon start against the
operator's real installation lock, not store-painted client startup. The
workload now fails if a front-door socket, installation lock file or matching
`amux server start` process appears. The unchanged 100 ms median and 200 ms
worst budgets and the 15% drift gate remain the cold-start promise.

The `summarizer idle core` row is ceiling-only under its unchanged 1.0%
absolute budget. Five identical-code runs measured 0.055%, 0.071%, 0.057%,
0.078% and 0.059%, a 42% spread caused by macOS park and unpark cost for the
roughly 2,000 one-second health-tick wakeups in each window. Over those same
runs, `summarizer CPU per row` stayed between 4.243 and 4.397 microseconds, a
3.6% spread. The idle row therefore cannot carry a useful percentage drift
gate; other percentage rows, including `growth after sweep`, retain the 15%
limit. A miss of either an absolute budget or any remaining drift gate is
still a defect to explain, not a value to adopt.
The `scroll-back memory return` row is also ceiling-only under its unchanged
1.10x absolute budget. Five warmed runs of identical code measured 0.632x,
0.591x, 1.004x, 0.595x and 1.006x. Its denominator is the physical-footprint
snapshot taken after seeding 50,000 rows and the preceding commit workload. It
reads near 0.6 when allocator and kernel memory is returned during scrolling,
and near 1.0 when that memory was already returned before the first snapshot.
A lower ratio is therefore an inflated denominator, not a better product, and
a relative gate cannot usefully distinguish the two modes. The absolute budget
still guards the row. `growth after sweep`, `growth sweep duration` and
`growth longest statement upper bound` keep their relative gates, and any
budget miss remains a defect to explain.
Baseline recording is qualification work, so wait for unrelated builds,
simulator runs and performance harnesses to finish, then review the complete
reports before committing the files. `just perf soak` remains a memory-only
qualification and does not start the cluster warmer.

The soak holds ten chat windows and the daemon state for 200 idle plus 20
active structured agents for ten minutes. It samples the platform's named
memory measure every five seconds, excludes the first two minutes from linear
growth, and fails above 1 MiB/minute, 300 MiB client peak, 2 MiB per idle
daemon agent, or 40 MiB per active daemon agent. Fresh identities, an
oversized row, one hundred unresolved asks, a five-second persistence stall
and a semantic reset prevent deduplication or a quiet happy path from hiding
growth. Client peak and daemon memory per idle and active agent enforce the
10% memory drift limit. The two fitted MiB/minute rates are ceiling-only: a
percentage change near zero is not meaningful. Four passing ten-minute runs
measured client slopes from 0.068 to 0.255 MiB/min while the daemon slope
rounded to 0.001 MiB/min. A shortened `AMUX_PERF_SOAK_SECONDS` diagnostic run
therefore applies neither committed baselines nor drift, and it cannot record
a baseline. As with the fast report, a budget or drift miss is a defect to
explain, not a new value to adopt.
