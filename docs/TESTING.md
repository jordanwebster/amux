# Running and designing tests

Use the root `justfile` so builds share one workspace dependency graph and
tests have an outer timeout:

```sh
just build                 # desktop product binaries only
just check                 # workspace libraries and binaries
just test-build            # compile ordinary test harnesses
just test                  # execute the full workspace selection
just doctest
just lint
just spec
just offline-test          # isolated HOME/config and denied external network
```

`just test` runs every workspace target by default. To select test functions
inside every library, or one named integration-test target:

```sh
just test -- --lib sdk::query::tests
just test -- --test spec
just test -- --test spec some_test_name -- --exact
```

Cargo target arguments and filters follow the recipe name. A `--` separates
Cargo's arguments from the test harness's arguments. A name alone filters functions
inside every selected harness; it does not prevent unrelated harnesses from
starting. Select a target when investigating one component. Target selection
keeps `--workspace`. For routine component work, use the declared focused
recipes; their smaller dependency closures may compile a different feature
variant than full verification.

Use `just test-crate model` for a focused package. `just test-build` compiles
every workspace library and integration test without executing it.

The opt-in provider harnesses and their argument, depfile, and redaction tests
live under `crates/testnet/tests`. `just test` compiles them and executes each
custom entry point with no scenario; it prints usage and exits before opening
an account or provider process. Real live scenarios still require an explicit
`just codex-live`, `just claude-pty-live`, or `just claude-sdk-live`
command.

Build output is wt's to keep bounded, not a recipe's. After task execution in a
wt-managed tree, and in `wt prune`, wt deletes superseded units,
unreachable object files and excess incremental state by following Cargo's
workspace units and dependency fingerprints. `wt ls --disk` sizes each tree's build output;
`wt prune amux` shows what a sweep of every tree would reclaim before
applying it. A tree that is no longer needed is removed with `wt rm`, which
is what reclaims its output entirely. Wt 0.4.0 can discard a valid narrow test
graph after a workspace-wide test graph supersedes its root; the consequence
and required correction are recorded in
[wt output requirements](WT_OUTPUT_REQUIREMENTS.md).

Wt 0.4.0 runs on POSIX systems, so the declared CI matrix covers Linux and
macOS. WSL follows the Linux path. Native Windows compilation and debugger
behavior remain an evidence gap until wt can execute the same declared tasks
there; the Cargo profile itself keeps Apple-only split-debug flags target
scoped.

## Recorded PTY tests

Each recorded Claude PTY scenario is a separate test. The standard Rust test
harness runs them concurrently; each owns its replay streams and session state.
Run the corpus or one scenario with:

```sh
just test -- --test spec_replay pty_replays
just test -- --test spec_replay pty_replays::plan_approve -- --exact
```

Recorded readiness waits for output notifications, and keyboard delays advance
the replay clock. Completing a replay closes its recorded output streams before waiting
for the simulated process to exit. Live terminal settling waits do not apply
to recorded sessions; shutdown timeouts are failures.

## Output when diagnosing failures

Rust normally captures test output and reports it for failed tests. If an outer
timeout kills the harness, it may never report that captured output. Stream
output during a focused hang investigation with:

```sh
just test -- --test spec_replay pty_replays::plan_approve -- --exact --nocapture
```

Parallel tests can interleave streamed output; add `--test-threads=1` after the
second `--` when ordering matters. A binary stalled before its first instruction
has no test output to display, even with capture disabled.

## Choose the boundary the assertion needs

Test parsing and state transitions with values, and concurrency with explicit
signals. Inspect the prepared command when asserting CLI arguments,
environment, or working directory. Use the provider's in-memory stream
transport when asserting protocol messages, session identity, or row order.

Use real child processes for OS behavior: pipe backpressure, exit status,
stderr, signals, and waiting for a child to exit. On Unix, simple fixtures can
run script text through an existing `/bin/sh -c` invocation. Keep scenario
state local to that child; do not change the test process's global environment
or create a fresh executable script for each scenario.

An executable's launch is subject to host security assessment. On macOS, a
tiny new script can queue behind another worktree's large test executable for
seconds before running its first instruction. That is not a protocol or
shutdown failure. Process fixtures should report readiness before measuring
the behavior under test, with a separate bounded startup check. A test of
startup itself must retain the startup deadline.

Shutdown tests should prove that completion waits for exit, not merely that a
signal was sent. For example, hold the child inside its signal handler until
the test releases it, assert that shutdown remains pending, then release and
await completion. Arrange cleanup even if setup or an assertion fails.

A timeout is a failure to investigate. A passing rerun alone does not identify
the cause, and increasing deadlines or rerunning until green is not a fix.
Capture the executable, process state, and whether it reached readiness. If
macOS shows a verification dialog, correlate its exact executable with
`syspolicyd` logs before attributing a test failure to it.
