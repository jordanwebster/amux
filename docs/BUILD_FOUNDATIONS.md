# Build and codebase foundations

amux uses one Cargo workspace and lockfile, deliberately small production
packages, declared `wt` tasks, and private build output in each worktree. This
document describes the implemented foundation and the measured limits of its
storage strategy.

## Ownership boundaries

The production graph separates values from effects and composition:

| Package | Responsibility |
| --- | --- |
| `model` | Shared identifiers, requests, events, errors, artifacts and provider-neutral values; no I/O |
| `wire` | Protobuf schemas, committed generated code, codecs and domain conversions |
| `settings` | Persisted configuration values and validation |
| `artifacts` | Content-addressed storage and retention |
| `client` | Typed RPC clients over an explicitly supplied channel or endpoint |
| `ui-state` | Pure state, messages, effects and update logic |
| `ui-runtime` | Per-view connections, subscriptions, effects, reports and fetched resources |
| `host-api` | Owned asynchronous contract between node services and an agent host |
| `node` | Identity, trust, routing, admission, RPC services and installation lifecycle |
| `agent-runtime` | Provider sessions, persistence, attachments, diffs and host implementation |
| `tui` | Terminal interaction and rendering |
| `amux` | Desktop CLI composition |

`testnet`, `claude-specs`, `codex-specs` and `tui-fixtures` own reusable test
scenarios and runners. They depend on production APIs. Product and build
dependencies cannot reach them. Provider packages have no profile-sensitive
build scripts; optimized tests therefore expose the same production API as
ordinary builds.

Node retains a private white-box harness under `src/testnet` for its own unit
tests. It reaches internal service state by design and is not exported as a
reusable scenario API. Cross-package scenarios, recordings and embedded
lifecycle tests belong to the support packages. TUI's unit-test crate includes
the fixture implementation owned by `tui-fixtures`; this avoids a Cargo package
cycle while keeping the fixture code out of normal product builds.
Provider adapters that require private backend state remain beside that state
behind an explicit test-support feature; the executable scenario and runner
logic that consumes those adapters lives in `testnet`.

The dependency policy checks the direct local edges and the test-support
exclusions. Package root exports are reviewed APIs; moving an implementation
does not justify a compatibility package or an old package-name facade.

The future native packages are extracted from working nativeapp code during
integration:

- `app-runtime` will own reusable rich-client sessions, projection/cache and
  typed presentation state without depending on `node`.
- `embedded-client` will own an embedded node and provide explicit client
  channels.
- `client-ffi` will own serialization, callbacks and foreign handle lifetimes.

They are not empty placeholders in this workspace. See
[native integration](NATIVE_INTEGRATION.md) for the required composition and
acceptance evidence.

## Reproducible task contract

`rust-toolchain.toml` pins Rust 1.98.0, components and supported cross targets.
Formatting names nightly 2026-08-30 explicitly. The supported task runner is
wt 0.4.0. CI and release jobs install that exact released version with Cargo's
locked resolution.

All normal work enters through `.wt.toml`. Cargo-producing tasks use
`Cargo.lock`, and `scripts/with-git-revision.sh` supplies the current commit to
the CLI build script. The environment value avoids checkout-specific Git watch
paths, so moving identical sources at the same revision does not invalidate the
binary. A changed commit still changes the embedded revision and relinks the
product.

The ordinary build selects only the desktop product binary; the screenshot
generator has its own task. Focused component tests select
one package. Full test compilation, doctests, live provider harnesses, code
generation, release builds and iOS target checks have explicit tasks. The
release task executes the resulting product and verifies that its debug command
and development test-agent entry point are unavailable. The
provider-free embedded graph selects `node`, `client`, `ui-state` and
`ui-runtime` directly; it does not rely on disabling a product default feature.
The optimized test-build task compiles every library and integration harness
under the release profile, proving that test support does not depend on a
profile-selected production API.

The no-network live-harness smoke runs each custom executable with no scenario
arguments. Each must reach its own entry point, print usage and exit before it
opens an account or provider process. This catches a custom main accidentally
being replaced by libtest while keeping real provider access opt-in.

The offline test task preserves the installed Cargo and Rust toolchains while
using a new HOME, Claude configuration directory and Codex home. Cargo is
forced offline, and the macOS sandbox denies external networking while allowing
the local sockets used by integration tests. It probes the denial before
running the suite.

## Development and verification profiles

Routine development and tests remain incremental. Development builds use line
tables for workspace crates and omit dependency debug information. On Apple
targets, `split-debuginfo=packed` produces dSYM bundles rather than keeping the
debugger's loose codegen objects beside every Cargo unit. The setting is target
specific because Windows MSVC does not accept every split-debuginfo value.

The test profile sets `debug=0` portably. Panic messages keep their assertion
location, and named frames remain available in backtraces, but arbitrary test
frames do not have complete debugger file/line information. Use the
`full-debug` task when that information is required. CI profiles disable
incremental state so ephemeral runners do not pay to create reusable sessions.
Release remains a separate optimized, nonincremental configuration.

`wt run debug-policy-check` verifies a named backtrace from an intentional test
panic. On macOS it also verifies the product dSYM bundles emitted by the packed
development configuration.

Wt 0.4.0 is a POSIX tool. Declared CI and release tasks therefore run on Linux
and macOS; WSL can use the Linux path, but this revision cannot execute the wt
contract on native Windows. The target-specific Cargo setting avoids sending
Apple's packed split-debuginfo option to MSVC, but no native Windows compile or
debugger result is claimed. Restoring a native Windows artifact requires a wt
release with Windows task support and platform CI evidence.

## Worktree reuse and retention

Every worktree owns its mutable Cargo output. On APFS, wt creates a copy-on-write
snapshot of the warm canonical checkout, including `target/`, and preserves
source modification times. Unchanged artifacts and incremental state initially
share physical extents with the canonical; subsequent writes diverge only the
changed extents. The canonical `warm` task builds the product and ordinary test
harnesses before new snapshots are made.

Wt sweeps after tasks that changed Cargo output. It starts at current workspace
units, follows Cargo fingerprint dependencies, and removes superseded units,
unreachable objects and excess incremental sessions. It holds Cargo's output
lock and leaves active builds alone. Removing a worktree removes all output that
tree owns.

This gives bounded retention of stale Cargo objects. It does not impose a byte
cap on live worktrees, legitimate target triples, profiles or feature graphs.
Volume free space is the capacity signal. Per-tree `du` and logical byte counts
remain useful inventory, but their sum over APFS clones is not exclusive disk
consumption. Native Cargo roots must be declared in a shape wt can discover;
being nested somewhere below `target` is not sufficient.

The repository does not maintain a shared writable target directory, output
pool, lease layer or sccache wrapper. A shared writable target would make Cargo
locks and incremental state cross-worktree concerns. The retired 60 GiB pool
summed clone-shared `du` values and reclaimed whole targets rather than stale
objects. A controlled sccache trial produced no additional Rust hits across
snapshots, so snapshot reuse plus private incremental compilation is the chosen
local strategy.

## Measurements

The pre-extraction baseline on the same Apple Silicon machine recorded a 60.93
second clean product build, a 76.21 second focused monolith test, a 7.89 second
private edit rebuild and 4.39 GB of logical product output. A first post-split
measurement recorded 41.52 seconds, 33.73 seconds, 4.06 seconds and 3.22 GB
respectively. Those runs predated wt 0.4.0 and establish only the effect of the
crate and recipe changes. A later three-cycle run at `e85265e1` used
comment-only edits; its ranges are historical diagnostics and are superseded
by the controlled run below.

The final controlled workload ran at clean revision `df4fa836` with Cargo and
Rust 1.98.0, wt 0.4.0, sccache disabled, a separate warm canonical and two
concurrent disposable worktrees. Warming product and test output took 81.228
seconds wall and 80.18 seconds of Cargo compile/link wall time. Timed linker
processes contributed 24.102 seconds in aggregate; linker durations can overlap
and therefore are not subtracted from Cargo wall time.

The two new snapshots initially reported 8.192 GB of logical target data each,
but creating both changed volume free space by only 18,296,832 bytes (17.45
MiB). Their product builds took 1.098–1.248 seconds and complete test builds
took 0.471–0.472 seconds with zero compiled or linked units. This is the reuse
expected when identical sources and the embedded revision move to another
checkout.

Each of five cycles made a real private function-body edit, ran its focused
test, compiled all workspace tests, linted the workspace and returned to the
product. After the first new configurations were established, cycles two
through five had these medians and ranges across both trees:

| Task | Median | Range | Work observed |
| --- | ---: | ---: | --- |
| Focused model test | 5.697 s | 5.504–6.650 s | 20 compiled units, 0.350–0.697 s harness launch, at most 0.01 s test execution |
| Complete test build | 24.251 s | 23.551–24.869 s | 16 compiled/linked workspace test units; no execution |
| Workspace lint | 7.019 s | 6.862–7.105 s | 16 checked workspace units, no linking |
| Product rebuild | 10.645 s | 10.091–10.720 s | 12 source-dependent units; about 0.58–0.67 s summed linker time in the last cycle |
| Immediate product repeat | 0.835 s | 0.776–0.891 s | zero compiled or linked units |

Five final no-edit samples per tree gave focused-test medians of 0.353 and
0.350 seconds and product-build medians of 0.798 and 0.858 seconds. The first
focused sample after the complete test graph still took 5.699–5.830 seconds and
compiled 20 units; the remaining four took 0.336–0.358 seconds and compiled
nothing. Harness launch on those no-op focused tests was 5–6 ms and the tests
reported 0.00 seconds of execution.

Per-tree output reached a stable range after the second cycle: 13.834–13.886 GB
logical, 35,013–35,026 files and exactly 352 incremental sessions at the
product-repeat boundaries. The final inventories reported 13.903 GB logical
and 35,218 files per tree. Every observed Cargo output root was `target`.
Adding the two trees' legitimate focused, workspace-test, Clippy and product
configurations changed volume free space by 21,993,431,040 bytes (20.48 GiB)
after snapshot creation. No other Cargo workload from this checkout ran during
the measurement, although the volume metric can still include operating-system
activity. After the inspected cleanup removed both disposable trees and their
marked root, free space returned slightly above its pre-run value.

Wt repeatedly removed superseded roots and excess incremental sessions; the
inventory remained bounded and the final dry-run prune was empty. It also
reproduced wt 0.4.0's dependency-distinct-root limitation. A focused test and a
workspace-wide test can produce useful roots with the same visible identity
but different dependency fingerprints. The workspace test sweep removed 32
units, so each next focused edit rebuilt 19 unchanged dependencies plus
`model`. This is predictable at the graph transition rather than an
every-other-run oscillation, but it is avoidable recompilation and does not
satisfy the stronger goal of preserving both live configurations. The required
wt correction is documented in [wt output requirements](WT_OUTPUT_REQUIREMENTS.md).

The acceptance runner stores task logs, separate Cargo compile/link,
linker-process, harness-launch and test-execution timing fields, inventories,
sweep notices, prune plans and volume samples under the ignored
`notes/build-foundations/measurements/` directory. It requires a clean commit,
wt 0.4.0, at least five edit cycles and five steady-state samples, creates only
uniquely marked disposable resources, and prints rather than executes its
cleanup commands.

## Deferred work

Bazel is deferred. It is not a foundation completion criterion and is not a
nativeapp merge requirement. Reconsider a second build system only after the
integrated Rust and native graph has measured needs that Cargo, wt and the
native build tools cannot meet.
