# Running and designing tests

`just --list` is the test command catalogue. The recipes below share the
workspace lockfile and carry outer timeouts; a timeout is a hang to diagnose,
not a reason to silently lengthen a deadline.

## 1. Crate unit tests

Unit tests live beside the code whose value or state transition they exercise.
Run one crate, optionally with Cargo and harness arguments:

```sh
just test-crate model
just test-crate claude -- sdk::query::tests
just test-crate model -- envelope::tests -- --exact
```

Use values and explicit signals for parsing, reducer and concurrency tests.
When an assertion concerns command arguments, environment or working
directory, inspect the prepared command rather than launching a provider.

## 2. Prose specifications

Executable specs state whole behaviors in domain language. The daemon specs
under `crates/testnet/tests/spec` use the public `TestNet` harness; the reducer
specs under `crates/ui-state/tests/spec` use the same messages clients see.

```sh
just spec
just spec -- a2a_cross_device
```

The two spec targets are part of `just test`; `just spec` is the focused
way to read or diagnose them.

## 3. Cross-crate integration tests

Integration tests live under `crates/testnet/tests`,
`crates/ui-runtime/tests`, and `crates/amux/tests`. Run the full workspace or
select a Cargo target:

```sh
just test
just test-build
just test -- --test embedding
just test-crate testnet -- --test embedding
just doctest
just offline-test
```

A name alone filters functions inside every selected harness; it does not stop
unrelated harnesses from starting. Select `--lib` or `--test NAME` when the
target matters. One `--` separates Cargo arguments from harness arguments.
For example:

```sh
just test -- --test spec_replay pty_replays::plan_approve -- --exact
just test -- --test spec_replay pty_replays::plan_approve -- --exact --nocapture
```

Recorded PTY scenarios own their streams and virtual clocks, so the Rust
harness may run them concurrently. Add `--test-threads=1` after the second
`--` only when ordered diagnostic output matters.

The live-provider entry points also live in `testnet`. Ordinary workspace
tests invoke each custom main with no scenario; it prints usage and exits
before opening an account or provider process. Real provider access is always
explicit:

```sh
just codex-live -- SCENARIO
just claude-pty-live -- SCENARIO
just claude-sdk-live -- SCENARIO
```

## Clocks, readiness and independent cases

Tests do not sleep in real time. Backoff, cooldowns and keyboard delays use a
clock the test drives. A simulated timeout is a duration the test advances,
not a quiet interval it waits out. Keep the full duration and all boundary
assertions when moving a scenario onto a controlled clock.

Readiness waits on notifications, never polling. Register the subscription
before inspecting the current state so a change between inspection and waiting
cannot be lost. Keep a bounded failure deadline and report the observed state
when it expires. Completing recorded output closes the stream before the
simulated process exits; no live settling delay belongs in a replay.

Real sockets and child processes still need to make progress. Run their IO on
real time, and advance controlled time only for the simulated behavior under
test. An assertion that a real peer sends nothing must observe its complete
declared absence window; a scheduling yield does not prove absence.

Each independent scenario is one test, so the harness can run it concurrently.
Share immutable fixture construction once per harness when it is expensive;
each test owns its mutable session, output and observation state. Partition
large viewport or replay sweeps without dropping any size, frame, prefix or
assertion. A serial loop over the whole corpus conceals the work from the
harness and prevents it from scheduling the cases independently.

## 4. PTY end-to-end tests

`e2e-runner` launches real CLI, daemon and test-agent processes and compares
their terminal conversations with `e2e-tests/*.test`.

```sh
just e2e
just e2e -- profile
```

Use this tier for OS behavior such as pipe backpressure, exit status, signals
and process shutdown. A fixture must report readiness before a behavior
deadline starts, and shutdown tests must prove that the child exited rather
than merely that a signal was sent.

## 5. Phone journeys

Phone journeys drive the iPhone app the way a person does, against a served
test network: real daemon and relay processes started from a committed
topology, scripted providers, and the app's own UI on a pinned simulator.

```sh
just ios journey
just ios journey -- hosts
```

The served network is `target/debug/testnet serve`; every control verb it
accepts is also a method of the in-process harness with the same name, so a
journey and a Rust spec describe the same behaviour. [TESTNET.md](TESTNET.md)
owns the topology format and the control protocol; [IOS.md](IOS.md) owns the
journey manifest, goldens and simulator pins.

## 6. Performance qualification

`just perf` builds the qualified harness in release mode and measures the
desktop frame loop, store-backed cold start and chat attachment, fold bounds,
SQLite commit and maintenance work, summarizer cost, and reconnect wire size.
The report names the enrolled hardware and OS, profile and features, workload
seed, identity-growth mode, warm-up, sample count, timestamps, statistic,
budget, committed baseline and drift. It fails on an absolute budget miss, on
time drift above 15%, or on memory drift above 10%. On a Mac with Xcode, the
same invocation also runs `just ios perf` under its own simulator lease; that
recipe prepares the pinned simulator, and a failure there fails the combined
recipe. A desktop build or measurement failure stops the recipe before the
phone suite starts.

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
new expectation. Baseline recording is qualification work, so do it only on
an otherwise idle reference machine and review the complete reports before
committing the files.

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

## Failure evidence

Rust reports captured output for a failed test, but an outer timeout may kill a
harness before it can do so. Rerun the narrow target with `--nocapture` and
record whether its executable reached readiness. On macOS, a new executable
can pause under host security assessment before its first instruction; match
the exact process with system logs before calling that a protocol or shutdown
failure. A passing rerun alone does not explain the original failure.
