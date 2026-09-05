# Running and designing tests

Use the checkout's `wt` recipes so builds share one workspace dependency
graph and tests have an outer timeout:

```sh
wt build
wt test
wt lint
wt run spec
```

`wt test` runs every workspace target by default. To select test functions
inside every library, or one named integration-test target:

```sh
wt test -- --lib sdk::query::tests
wt test -- --test spec
wt test -- --test spec some_test_name -- --exact
```

Arguments after the first `--` go to Cargo. A second `--` separates Cargo's
arguments from the test harness's arguments. A name alone filters functions
inside every selected harness; it does not prevent unrelated harnesses from
starting. Select a target when investigating one component. Target selection
keeps `--workspace`; selecting a package with `-p` can change feature
unification and compile a second dependency graph.

Both test recipes require at least one executed Rust test across the whole
run. A filter that matches nothing, or selects only ignored tests, fails;
unrelated harnesses may still report zero tests. Cargo failures retain their
exit status. Help, `--no-run`, and `-- --list` remain informational commands.
The guard reads standard Rust test summaries; custom live suites use their
own entry points.

Run `wt run test-recipes` to check argument forwarding and the result guard
without compiling. These checks also run automatically before `wt test` and
`wt run spec`.

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
