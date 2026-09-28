# Building amux

*For developers building amux from source: the toolchain, the build profiles, the task recipes and the generated code that is committed.*

## Toolchains

[`rust-toolchain.toml`](../rust-toolchain.toml) pins Rust 1.98.0 with the
`minimal` profile, the components repository tasks use (`clippy`,
`llvm-tools`, `rust-src`, `rustfmt`) and the cross-compilation targets:

| Target | Used for |
| --- | --- |
| `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios` | The iPhone app's Rust bridge and `just mobile-check` |
| `x86_64-pc-windows-msvc` | The Windows release build and the [Windows cross-check](CI.md) |
| `x86_64-unknown-linux-gnu` | The Linux release build |

Formatting uses `nightly-2026-08-30` explicitly (`just fmt`, `just
fmt-check`), so a stable-toolchain update cannot rewrite the tree
unexpectedly. Install that nightly with `rustfmt` once.

Other tools a full checkout uses:

- [`just`](https://github.com/casey/just) runs every task.
- Python 3.11 or newer runs the repository scripts. `scripts/python` and
  `scripts/py` find one even when `python3` on the PATH is the macOS system
  3.9.
- Xcode and XcodeGen build the iPhone app (see [the iPhone app](IOS.md)).
- `tmux` drives the terminal journeys.

The workspace has one lockfile. Build and verification recipes pass
`--locked`, and protobuf output is committed, so the same revision selects
the same Rust dependencies and wire code on every machine. Protobuf
generation uses a vendored `protoc`, so none needs installing.

## Profiles

Four profiles are defined in the workspace [`Cargo.toml`](../Cargo.toml):

| Profile | Purpose |
| --- | --- |
| `dev` | Routine builds and tests: incremental, line-table debug information, and unwinding panics |
| `release` | Optimized shipping builds with incremental compilation disabled and aborting panics |
| `full-debug` | A `dev` build with complete debug information for debugger sessions |
| `mobile` | The iPhone app's shipping library: size optimization, fat LTO, one codegen unit, and aborting panics |

`mobile` is used only by `just ios package` and the app release; it is not an
alternate profile for ordinary Rust tests. There is no separate `test` or CI
profile, so one development configuration of each workspace crate serves
product builds, focused tests, and the full test build. CI disables
incremental compilation through its environment (`CARGO_INCREMENTAL=0`).

In `dev`, dependencies omit debug information and workspace crates keep line
tables. On Apple targets, [`.cargo/config.toml`](../.cargo/config.toml) sets
`split-debuginfo=packed`, which keeps debugger symbols in dSYM bundles
instead of leaving every incremental object beside its Cargo unit. Use `just
full-debug` when a debugger needs complete file and line information.

Desktop builds name the `bundled` feature explicitly, which compiles SQLite
into the binary; the recipes pass `--features bundled` so a command that
disables default features cannot silently switch SQLite linkage.

## Recipes

The root [`justfile`](../justfile) is the task contract, and `just --list`
shows every recipe. The iPhone app's recipes are a module:
`just --list ios`, `just ios build`, `just ios verify`. Every recipe runs its
command under `scripts/bounded`, a wall-clock bound that uses GNU `timeout`
where present and a small fallback elsewhere; a firing bound is a hang to
diagnose, not a slow machine. `AMUX_GIT_SHA` is exported from the current
commit for product builds.

The recipes a build or change usually needs:

| Recipe | What it runs |
| --- | --- |
| `just build` | `cargo build -p amux --bins`: the desktop binary |
| `just check` | `cargo check` of every workspace library and binary |
| `just full-debug` | The desktop binary under the `full-debug` profile |
| `just release-check` | The shipping binary under `release`, then the release policy check (see [Release](RELEASE.md)) |
| `just test` | Every workspace test target; arguments after `--` go to Cargo |
| `just test-crate <crate>` | One crate's tests |
| `just test-build` | Compiles every ordinary test target without running it |
| `just doctest` | Documentation tests |
| `just lint` | Clippy on every target, warnings denied |
| `just fmt`, `just fmt-check` | Format, or check formatting, with the pinned nightly |
| `just protobuf` | Regenerates the committed protobuf output |
| `just codegen-check` | Fails if the committed protobuf output is stale |
| `just proto-check` | Fails if the protos drop or change anything the committed baseline has |
| `just embedded-check`, `just mobile-check` | Check the provider-free client graph on the desktop and for iOS |
| `just warm` | `just build` and `just test-build` |
| `just ci` | The whole check sequence; see [CI](CI.md) |

[Testing](TESTING.md) covers the test recipes (`test-store`, `test-ui`,
`test-tui`, `spec`, `journey`, `live`, `perf`, `tests-list`) in detail.

## Generated code

Generated code is committed, and a check fails when it is stale.

**Protobuf.** The schemas live under
[`crates/wire/proto/amux/v1`](../crates/wire/proto/amux/v1). The generated
Rust (`amux.v1.rs`) and descriptor set (`amux.v1.bin`) are committed under
[`crates/wire/src/generated`](../crates/wire/src/generated). After changing a
schema:

```sh
just protobuf
just codegen-check
just proto-check
```

`just protobuf` runs `cargo run -p xtask -- codegen`. `just codegen-check`
regenerates and fails if the committed result differs. `just proto-check`
compiles the protos and compares them with the committed baseline,
`crates/wire/proto/baseline.binpb`: anything the baseline has that the
current schema removed, renumbered, retyped or made required fails with its
full name, because binaries of other versions read every amux protocol
surface. Additions pass. `just proto-check --update` rewrites the baseline
once a breaking change is deliberate. [The wire](WIRE.md) explains the rule.

**Swift value types.** The Swift mirrors of the view values the phone reads,
`apps/apple/Packages/AmuxCore/Sources/AmuxValues/Values.swift`, are generated
from the Rust definitions by `cargo run -p xtask -- swift-types`. A unit test
in the `xtask` crate, run by `just test`, fails when the committed file
differs from what the generator writes.

## Worktrees

Development happens in git worktrees managed by `wt`, which owns worktrees,
sessions, each tree's rendered environment and its daemon; `just` owns every
build and test task. [`.wt.toml`](../.wt.toml) holds the tree's side:

- A new worktree runs `just build`, and `just warm` is the warm hook. `just
  warm` builds the desktop product and compiles the ordinary workspace test
  targets without running them. `wt` warms the canonical checkout with it,
  then gives a new worktree a copy-on-write snapshot of that `target`
  directory, so unchanged Cargo output is already there while each tree owns
  every later write. Removing the worktree removes its build output; there is
  no shared writable Cargo target.
- Each tree gets its own amux installation under `.wt/amux/`:
  `AMUX_CONFIG` names its installation file, `AMUX_LOG` its daemon log, and
  `target/debug` is on the tree's PATH. The installation sets its own
  discovery scope, so a worktree's daemon never meets real machines or other
  trees, and turns `keep_awake` off. `amux server start` and `amux server
  stop` create and destroy it.
- The pinned iPhone simulators are machine-wide resources that the iOS
  recipes lease through `scripts/with`.
