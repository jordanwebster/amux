# Performance

*For developers measuring amux, changing a budget, or recording a baseline.*

Performance is a test of a specified workload on a reference machine. Each metric has an absolute budget and, for
some, a drift limit against a reviewed baseline. Because the numbers depend on the machine, performance runs in
the qualification lane on enrolled machines, never in an ordinary push. Claims that do not depend on the machine
(rows and bytes retained, message counts, cache misses) belong in ordinary tests; elapsed time and memory belong
here. How this lane sits beside the others is on [Testing](TESTING.md).

The harness is the `qualification` crate (`crates/qualification/src/perf`). The phone is measured through it:
the flood's viewer stands in for it (see [The phone](#the-phone)).

## Running it

```sh
just perf                        # every workload
just perf -- --only flood        # the flood alone
just perf --baseline             # record this machine's baseline after every budget passes
just perf -- --only flood --baseline
```

`just perf` builds the `perf` binary in release with the `bundled,perf` features and runs it; the binary refuses to
run from a debug build. The binary accepts nothing but `--baseline` and `--only flood`.

For the whole run a child process keeps one core busy. An otherwise idle Apple Silicon machine lets its cores drop
into a low-power state between bursts and measures several times slower with wider spread; one busy core is the
repeatable reference state baselines are recorded in (`one busy core (cluster warmer)` in the report and the
baseline). It changes no workload, statistic or budget.

Wait for unrelated builds, simulator runs and other harnesses to finish before a qualifying run: the machine's load
is part of what is measured.

## The flood

The flood is the desktop workload (`crates/qualification/src/perf/flood.rs`). Twenty agents on one host (`desk`)
stream at full rate, and a second runtime (`phone`), linked to it and holding nothing yet, opens the fleet and one
chat. Full rate is the fastest a real provider streams, not the fastest a fake can write: every recording in the
Claude and Codex corpora peaks at 47 provider frames in one second, so each agent sends a message every 20 ms,
about a thousand frames a second on the host. The hosts run with a replica tail and catch-up cap K of 200, and
every agent's history is well past K before the viewer connects.

One run measures, in order:

| Metric | Statistic | Budget | Gate |
| --- | --- | --- | --- |
| `flood fleet caught up`: the viewer's fleet reaching CaughtUp | worst | 1,000 ms | budget |
| `flood chat caught up`: one chat's tail of K rows and snapshot while the rest catch up beside it | worst | 2,000 ms | budget |
| `flood ingest lag`: a journal write to its commit in the store | p99 | 250 ms | budget |
| `flood agent process memory`: each agent process's footprint | peak | 48 MiB | budget and drift |
| `flood catch-up under K`: the viewer cut off for K/2 rows, then restored, to CaughtUp | worst | 2,000 ms | budget |
| `flood catch-up over K`: the same for 5K rows, answered with a reset and a tail | worst | 2,000 ms | budget |
| `flood backlog growth with the daemon killed`: journal growth while the host's daemon is dead | worst | 1,200 MiB/min | budget |
| `flood backlog drain after restart`: every journal read to its end after the restart | worst | 5,000 ms | budget |
| `flood ingest cost per frame`: ingest draining a held backlog alone, per committed frame | median | 100 µs | budget and drift |

The last metric comes from a second phase: the agents write unthrottled while the host's store is held, so ingest
commits nothing until the journals hold a fixed backlog; then the writers pause and ingest drains the backlog alone,
priced over seven equal shares of the frames it commits. That isolates ingest's own cost from how busy writers share
the cores with it.

Memory is the platform's own measure: physical footprint on macOS, resident set size on Linux. The two are not
interchangeable and the report names which it used.

### The smoke

The same workload with three agents, a small K and short phases runs in the ordinary test lane so it keeps
working between qualifying runs. It checks that every metric is measured, not the budgets, which hold only for
release builds on an enrolled machine.

```sh
just test-crate qualification -- --test flood
cargo test -p qualification --features perf flood
```

The same test file holds the served flood to the measured one: `journeys/topologies/flood.json` must equal the
topology the perf lane builds, so `testnet serve journeys/topologies/flood.json` (see [TestNet](TESTNET.md)) hands a
real client the flood the lane measures. `FLOOD_TOPOLOGY_UPDATE=1` rewrites the file after the workload changes.

## Budgets and baselines

Budgets are constants beside the workload, in the `budgets` module of `crates/qualification/src/perf/flood.rs`,
each with the reasoning behind its number. The values they rest on (K, the tail N, the facts ring and journal
segment sizes) are on [Parameters](PARAMETERS.md). Changing a budget is a code change reviewed like any other.

A metric either is held to its budget alone ("ceiling only") or also to drift from the recorded baseline median.
Most flood timings are one observation, or sub-millisecond medians that vary several-fold between runs on the same
machine, so drift on them would fail runs for noise; their budget is what they must meet. Agent memory and ingest
cost per frame are stable enough to drift-check. Drift limits come from the unit: 15% for times and percentages,
10% for bytes, MiB and ratios.

Baselines are committed per enrolled machine:

```text
perf/baselines/
  desktop/<hw.model>.json      e.g. desktop/Mac14,6.json
```

A desktop baseline records its schema version, machine model, profile (`release`), features (`bundled,perf`),
reference state, and the median of every metric, `null` for ceiling-only ones. The enrolled machines are listed in
`crates/qualification/src/perf/report.rs`: at present one, `pinned-mac`, model `Mac14,6`. A run on a machine not in
the list is refused, and so is a baseline recorded on another model, profile, feature set or reference state, or
one whose metrics differ from the run's. With no baseline on disk, drift is reported as unavailable and the run is
judged on budgets alone.

`--baseline` records the whole workload it ran. It still enforces every absolute budget, and a run outside a budget
cannot become a baseline: a miss is a defect to explain, never a new value to adopt. Review the complete report
before committing the file.

A baseline is only comparable with a run that measures the same way. Whenever a metric's measurement changes,
re-record that metric's baseline in the same commit as the change. `--baseline` rewrites every median, so when only
one measurement changed, keep the other metrics' committed values and commit only the changed one.

A baseline, or a run that qualifies a change, counts only when the machine is shown to be in the reference state,
never assumed. Show it in the same session, with the machine state (`uptime`, `ps -Ao pcpu,comm -r | head`,
`df -h`) recorded beside every run:

- **Control.** Build the commit that recorded the current baseline (`git archive <sha>` into a scratch tree with its
  own target directory) and run its `perf --only flood` three times before and three times after the runs that
  count. Both medians must reproduce its recorded value within 5%. One reading is not a control: runs of one build
  differ by several percent.
- **Quiet.** Before each run, no process holds more than a fifth of a core for a minute. After every flood run the
  file-events daemon works through the run's writes for minutes; wait it out.
- **Free space.** Ample free space on the volume the run writes to (the temporary directory's). Ingest cost is file
  writes, and a nearly full APFS volume inflates it: with 5 GiB free on a 926 GiB disk the baseline commit read 8
  to 25% above its own recorded value; with 176 GiB free it reproduced it.
- **Agreement.** Run the build being judged at least three times; the recorded run must fall within 5% of the
  median of that session's runs of the same build. A reading outside that is noise, not a baseline.

## Reading a report

The report names the machine and OS, profile and features and the reference state, then one line per metric:

```text
metric | median | measured | budget | baseline | drift | verdict
flood ingest cost per frame | 21.002 µs | median 21.002 µs | 100.000 µs | 21.002 µs | +0.0% | PASS
  workload=… · seed=0 · identities=… · samples=7 · warm-up=… · start=… · end=…
```

`measured` is the metric's named statistic over its samples; `baseline` and `drift` read `ceiling only` for metrics
without a drift gate. The run fails if any verdict is `FAIL`.

## The phone

The flood's viewer holds nothing and reads the fleet and one chat through the same subscriptions the iPhone app
reads, from the same node the app runs inside itself, so its fleet and chat budgets are the phone's catching up under
load, without a simulator's timing in the number. The phone's own screen under the
flood is seen, not timed: serve `journeys/topologies/flood.json` with `testnet serve` and pair the app with its
`desk` host as [the iPhone app](IOS.md) describes for any served topology. Every flood message carries its number
(`message 1234: ...`), so the newest row the phone draws can be read against the newest row `desk` holds (the served
net's `Chat` verb), and a page of history shows by its numbers where it starts. On the app's debug door, scrolling up
leaves the newest row as a person's drag does, and the `conversation` reading of the chat on screen is the rows its
page holds.
