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
crate and recipe changes.

The wt 0.4.0 acceptance workload used a clean canonical at the tested revision
and two disposable snapshot worktrees. Warming the canonical took 81.803
seconds of wall time, including 80.48 seconds reported by Cargo and 398 compiled
units. In both new trees, the initial product build took 1.098–1.234 seconds and
the initial full test build took 0.438 seconds; Cargo compiled zero units in all
four tasks. Each tree reported about 7.995 GB of logical target data, while
creating both snapshots reduced volume free space by about 15.2 MB. The logical
sizes therefore cannot be added to estimate physical use.

Three concurrent edit/test/lint/build cycles reached about 13.5 GB logical
output per tree after introducing the needed configurations. File and
incremental-session counts levelled off in the last two cycles. Immediate
repeated product builds took 0.815–0.986 seconds with zero compiled units.
After snapshot creation, the two live trees' added focused, workspace-test,
Clippy and product configurations consumed about 21.2 GB of volume free space.
The figure includes legitimate new configurations and any other writers on the
same volume during the run; it is not an exclusive accounting system.

The run also found a wt 0.4.0 retention limitation. A package-focused test and
a workspace-wide test can compile a workspace root with the same visible unit
identity but different resolved dependency fingerprints. Wt keeps only the
newest root in that slot. The full test build consequently reclaimed 31 units,
and the next edited focused model test rebuilt 19 unchanged dependencies before
rebuilding `model`. An immediate no-edit repeat compiled nothing. The product
configuration did not oscillate. The required wt correction is documented in
[wt output requirements](WT_OUTPUT_REQUIREMENTS.md). Until it is released, the
sweep prevents monotonic stale-object growth but does not preserve every valid
focused/full-test variant.

The acceptance runner writes task logs, timing fields, inventories, sweep
notices, prune plans and volume samples under the ignored
`notes/build-foundations/measurements/` directory. It requires a clean commit,
wt 0.4.0 and at least two cycles, creates only uniquely marked disposable
resources, and prints rather than executes its cleanup commands.

## Deferred work

Bazel is deferred. It is not a foundation completion criterion and is not a
nativeapp merge requirement. Reconsider a second build system only after the
integrated Rust and native graph has measured needs that Cargo, wt and the
native build tools cannot meet.
