# Continuous integration

*For developers who need to know what CI runs on a push, how to run the same checks locally, and how to check Windows from a Mac.*

CI is GitHub Actions. Every job installs the pinned toolchain and `just`, then
runs `just` recipes — the same recipes a developer runs locally, so a job's
log names the command to reproduce it. [Build](BUILD.md) describes the
recipes; [Testing](TESTING.md) describes the suites they run.

## Workflows

| Workflow | File | When | What |
| --- | --- | --- | --- |
| CI | [`ci.yml`](../.github/workflows/ci.yml) | Pushes to `main`, `testing` and `rearchitect`; pull requests into `main` | Every check a change is held to, on Linux, macOS and Windows, plus the iOS gate |
| Weekly offline tests | [`offline.yml`](../.github/workflows/offline.yml) | Sundays 04:00 UTC, and by hand | The workspace tests with no external network |
| iOS captures | [`ios-captures.yml`](../.github/workflows/ios-captures.yml) | Nightly 03:00 UTC, and by hand | The phone's photographed suites |
| Release | [`release.yml`](../.github/workflows/release.yml) | A pushed `v*` tag | The `amux` release binaries; see [Release](RELEASE.md) |

Every workflow sets `CARGO_INCREMENTAL=0`, and every job has a
`timeout-minutes` bound.

### CI jobs

| Job | Runner | Runs |
| --- | --- | --- |
| Check and Clippy | `ubuntu-latest` | `just check`, `just lint` |
| Format | `ubuntu-latest` | `just fmt-check` with `nightly-2026-08-30` |
| Codegen and dependency policy | `ubuntu-latest` | `just codegen-check`, `just proto-check`, `just dependency-policy`, `just tests-check`, `just docs-check`, `just deletion-ledger-check` |
| Test | `ubuntu-latest`, `macos-latest`, `windows-latest` | `scripts/no-update-flags.sh`, `just test -- --no-fail-fast`, `just doctest`; on macOS also `just contracts-check` |
| Build | `ubuntu-latest`, `macos-latest`, `windows-latest` | `just release-check` |
| Terminal journeys | `ubuntu-latest`, `macos-latest` | Installs `tmux`, then eight `just journey terminal <name>` runs |
| Embedded client | `ubuntu-latest` | `just embedded-check`, `just embedded-test` |
| iOS target check | `macos-latest` | `just mobile-check` |
| iOS gate | `macos-26` | `just ios gate` on Xcode 26.6 |

The test job runs with `--no-fail-fast`, so a platform reports every failing
test binary, not only the first. Its matrix does not fail fast either: a
failure on one operating system does not cancel the others. The Windows
runner compiles and runs every test target; suites that need Unix processes,
PTYs or sockets are compiled out there. On Linux and macOS the same run is the
system lane as well: the built `amux` binary's suites (`process`,
`supervise_cli`, `overlap`, `attach` and the rest), node's `supervisor` suite
and the `survive-daemon` system journey are ordinary test targets.
`contracts-check` runs on the macOS test runner only, after the tests have
built: the contracts name Unix-only tests and one macOS-only test, so only
there does every named test exist.

The terminal journeys are `reach-host`,
`conversation-decision-claude-pty`, `conversation-decision-claude-sdk`,
`conversation-decision-codex`, `leave-and-recover`, `manage-agent`,
`attachment-or-review` and `keep-authority`.

### Bounds

Two kinds of bound stop a run, and neither is ever raised to hide a failing
or slow test.

- **A recipe bound** is a hang detector for one phase, sized above that
  phase's honest duration. `just test` runs two phases, each under its own
  bound ([`scripts/workspace-test.sh`](../scripts/workspace-test.sh)): compiling
  every test target (`cargo test --no-run`, 1200 s, the bound `just test-build`
  carries), then running them (1000 s). The run's bound is the slowest
  platform's measured clean run — one where no failing test waits out its own
  timeout — times 1.5, rounded up to the hundred.
- **The job guard** (`timeout-minutes`) bounds the honest total of a job:
  setup, cache restore, every step. The test job's is 30 minutes.

What the bounds were sized from, on a warm cache:

| Platform | CI run | Compile | Test run | Whole job |
| --- | --- | --- | --- | --- |
| `ubuntu-latest` | [36448433638](https://github.com/jordanwebster/amux/actions/runs/36448433638) | 145 s | 601 s | 13 min 6 s |
| `macos-latest` | [36448433638](https://github.com/jordanwebster/amux/actions/runs/36448433638) | 95 s | 449 s | 13 min 13 s |
| `windows-latest` | [36448433638](https://github.com/jordanwebster/amux/actions/runs/36448433638) | 296 s | not clean | not clean |

Linux is the slowest clean test run: 601 s × 1.5 is 902 s, so the run's
bound is 1000 s. The macOS job spends a further 168 s in `contracts-check`
and still ends near 13 minutes, well inside the 30-minute guard. That Windows
run is not a measurement: its terminal-Claude tests, which Windows no longer
runs, waited out their own timeouts until the run's bound cut it. Windows compiles out most of the slow suites (the embedded client, the
supervisor, the system journeys), so its clean run is expected well under
Linux's; the first clean Windows run replaces that row, and resizes the bound
by the same rule if it is the slowest. A cold cache, after a `Cargo.lock`
change, compiles the whole dependency graph and can take longer than these
numbers.

### The iOS gate

The iOS gate job selects Xcode 26.6, asserts that the iOS 26.5 simulator
runtime and the iPhone 17 Pro device type are available, installs XcodeGen,
the stable toolchain with both ARM iOS targets, and runs `just ios gate`: the
half of the phone's verification that building the app can settle.

`just ios gate` runs, in order and stopping at the first failure:
`just mobile-check`, then `just ios` `lint`, `script-tests`, `graph-check`,
`rust`, `simulator golden`, `build`, `component-snapshots`, `loopback-smoke`
and `unit`. None of it compares a photograph of the whole display, so it
answers the same on any machine. The job uploads the component snapshot
comparisons and the shipped-scope audit directory to the run, whether it
passed or not.

`just ios verify` is the whole sequence a developer runs before pushing a
phone change: the workspace's `fmt-check`, `lint`, `test` and `spec`, then the
gate, then the captures (`ios goldens`, `ios goldens-perturb`,
`test-store-ios`, `ios journey`, `ios accessibility`, `ios perf`), then the
shipping stages (`ios package`, `ios scope-audit`). It refuses to start if
any stage names a recipe the justfiles no longer declare. `just ios captures`
and `just ios shipping` run those halves alone.

### iOS captures

The nightly workflow boots both pinned simulators on `macos-26`, compares the
native component snapshots, and compares the whole-screen goldens. Run by
hand, it instead runs `just ios captures` — goldens, journeys, the
accessibility sweep and the measured performance run — with
`AMUX_PERF_MACHINE=macos-26` naming the runner's baseline row. It uploads the
performance results, the golden comparisons, the journey evidence and the
component snapshots. [The iPhone app](IOS.md) and
[iPhone performance](IOS_PERFORMANCE.md) explain what those compare.

## Lanes

Each suite in [`tests/catalog.toml`](../tests/catalog.toml) names a lane:
when and where it runs. `just tests-list` prints the catalogue, and `just
tests-check` (in CI) fails if a suite names a recipe, Cargo target, feature,
journey or baseline that does not exist.

| Lane | What it holds | Where it runs |
| --- | --- | --- |
| `fast` | Offline, credential-free tests with no wall-time settling: unit tests, spec suites, adapter replays, store and effect integration, renderer goldens | `just test` on every push, on all three platforms |
| `system` | Real processes and transports with bounded deadlines: the built `amux` binary and its supervisor, the phone bridge on a simulator, the store on iOS | The desktop suites in `just test` on every push (Unix runners); `ios loopback-smoke` in the iOS gate; `test-store-ios` in `just ios captures` |
| `tool` | The repository's own machinery: script contracts, source and dependency policy, the CI tooling, the shipping scope audit | The Rust side in `just test`; `ios script-tests` and `ios lint` in the iOS gate; `ios scope-audit` before every app release |
| `journey` | Complete client stories through the shared manifest, and the phone's whole-screen goldens | The terminal journeys job and the system journey inside `just test` on every push; phone goldens nightly; phone journeys and the accessibility sweep in iOS captures run by hand |
| `qualification` | Real providers, real cloud accounts and measured performance | Never on an ordinary push: `just live`, `just perf` and the `just ios qa-*` recipes by hand, on machines that have the credentials or hardware; the phone's measured run in iOS captures run by hand |

[Testing](TESTING.md) defines the lanes and boundaries fully.

## `just ci`

`just ci` runs the check sequence the CI jobs are built from, in one process,
stopping at the first failure:

| Recipe | Checks |
| --- | --- |
| `no-update-flags` | No environment variable starting with `UPDATE_` is set, so no fixture is rewritten during an asserted run |
| `check` | The workspace compiles |
| `lint` | Clippy, warnings denied |
| `fmt-check` | Formatting, with the pinned nightly |
| `codegen-check` | The committed protobuf output matches the schemas |
| `proto-check` | The protos keep everything the committed baseline has |
| `dependency-policy` | Production and test-infrastructure dependency boundaries ([`scripts/check-dependency-policy.py`](../scripts/check-dependency-policy.py)) |
| `deletion-ledger-check` | No code, config, proto, recipe, script, workflow or doc names a mechanism amux removed |
| `docs-check` | Every link, image and heading anchor under `docs/` resolves, every figure is referenced and standalone, and `docs/README.md` lists every page once ([`scripts/docs-check.py`](../scripts/docs-check.py)) |
| `tests-check` | The suite catalogue names only things that exist |
| `test` | Every workspace test target |
| `contracts-check` | Every design contract in [`tests/contracts.toml`](../tests/contracts.toml) names at least one test, and every test it names exists |
| `doctest` | Documentation tests |
| `release-check` | The shipping binary builds and passes the release policy check |
| `embedded-check`, `embedded-test` | The provider-free client graph and the embedded owner and client boundary |
| `mobile-check` | The provider-free graph for iOS devices and simulators |

The terminal journeys and the iOS gate are in `ci.yml` but not in `just ci`.

## `just ci-remote`

`just ci-remote` runs the whole CI workflow on GitHub for the checked-out
commit ([`scripts/ci-remote.sh`](../scripts/ci-remote.sh)). It refuses to
start on any branch but `rearchitect` or with uncommitted changes, then:

1. pushes `HEAD` to `rearchitect` on `origin`, never forced: if the remote
   branch has moved on, the push fails and the divergence is left for a
   person to reconcile;
2. finds the push run of `ci.yml` for that exact commit with
   `gh run list --commit`, waiting up to five minutes for GitHub to register
   it;
3. waits for the run with `gh run watch --exit-status`;
4. on success prints the run's URL; on any other conclusion prints
   `gh run view --log-failed` and exits non-zero.

It needs the `gh` CLI signed in to an account that can push to the
repository and read its Actions runs.

## Checking Windows from a Mac

The Windows test job compiles every workspace target for
`x86_64-pc-windows-msvc`, and a Windows-only compile error otherwise shows up
only there. Type-checking for Windows from a Mac needs no Windows toolchain,
but it does need help: `ring` and `libsqlite3-sys` compile C in their build
scripts, and there is no MSVC compiler to run. Type-checking Rust needs no
real object code, so two stub tools that create the files they are asked for
are enough. There is no recipe for this; set it up once:

```sh
mkdir -p /tmp/winstub

cat > /tmp/winstub/cc <<'EOF'
#!/bin/sh
# Stand-in C compiler: create the object file named by -o or -Fo.
out=""
prev=""
for a in "$@"; do
  case "$prev" in -o) out="$a";; esac
  case "$a" in -Fo*) out="${a#-Fo}";; esac
  prev="$a"
done
[ -n "$out" ] && : > "$out"
exit 0
EOF

cat > /tmp/winstub/ar <<'EOF'
#!/bin/sh
# Stand-in archiver: create the library named by -out:.
for a in "$@"; do
  case "$a" in -out:*) : > "${a#-out:}";; esac
done
exit 0
EOF

chmod +x /tmp/winstub/cc /tmp/winstub/ar
```

Then check one crate:

```sh
CC_x86_64_pc_windows_msvc=/tmp/winstub/cc \
CXX_x86_64_pc_windows_msvc=/tmp/winstub/cc \
AR_x86_64_pc_windows_msvc=/tmp/winstub/ar \
cargo check --locked -p <crate> --all-targets --target x86_64-pc-windows-msvc
```

or the whole workspace, as the Windows test job compiles it:

```sh
CC_x86_64_pc_windows_msvc=/tmp/winstub/cc \
CXX_x86_64_pc_windows_msvc=/tmp/winstub/cc \
AR_x86_64_pc_windows_msvc=/tmp/winstub/ar \
cargo check --locked --workspace --all-targets --features bundled \
  --target x86_64-pc-windows-msvc --keep-going
```

`--keep-going` reports every crate's errors instead of stopping at the first.
The target is already installed by `rust-toolchain.toml`. This finds compile
errors only; Windows runtime behaviour still needs the Windows runner.
