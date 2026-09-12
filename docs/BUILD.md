# Building amux

## Toolchains

`rust-toolchain.toml` pins Rust 1.98.0, the components used by repository
tasks, and the supported cross-compilation targets. Formatting uses
nightly-2026-08-30 explicitly so a stable-toolchain update cannot rewrite the
tree unexpectedly.

The workspace has one lockfile. Build and verification commands use
`--locked`, and protobuf output is committed, so the same revision selects the
same Rust dependencies and wire code on every machine.

## Profiles

Three profiles are defined here; the native app adds a fourth:

| Profile | Purpose |
| --- | --- |
| `dev` | Routine builds and tests: incremental, line-table debug information, and unwinding panics |
| `release` | Optimized shipping CLI builds with incremental compilation disabled and aborting panics |
| `full-debug` | A `dev` build with complete debug information for debugger sessions |
| `mobile` | The native app's shipping library: size optimization, fat LTO, one codegen unit, and aborting panics |

`mobile` arrives with the app library and its packaging recipes; it is not an
alternate profile for ordinary Rust tests. There is no separate `test` or CI
profile, so one development configuration of each workspace crate serves
product builds, focused tests, and the full test build. CI disables
incremental compilation through its environment.

On Apple targets, development builds use packed split debug information. That
keeps debugger symbols in dSYM bundles instead of leaving every incremental
object beside its Cargo unit. Dependencies omit debug information; workspace
crates retain line tables. Use `just full-debug` when a debugger needs complete
file and line information.

## Repository tasks

The root `justfile` is the task contract. `just --list` shows every supported
build, test, lint, format, code-generation, release, E2E, embedded and mobile
check with its outer timeout. A firing timeout is a hang to diagnose.
`AMUX_GIT_SHA` is exported from the current commit for product builds.

Wt has a narrower job: it owns worktrees, sessions, each tree's rendered
environment and daemon resource. Its retained build, test, lint, format and
warm tasks are one-line delegations to `just`, allowing wt to coordinate a
tree without maintaining a second repository task catalogue.

## Protobuf

Schemas live under `crates/wire/proto`; generated Rust and the descriptor set
are committed under `crates/wire/src/generated`. After changing a schema, run:

```sh
just protobuf
just codegen-check
```

The freshness check regenerates the output and fails if the committed result
differs.

## Warm worktrees

`just warm` builds the desktop product and compiles the ordinary workspace
test targets without running them. Wt warms the canonical checkout with that
recipe, then snapshots its private `target` directory into a new worktree.
Unchanged Cargo output starts as copy-on-write data while each tree owns every
later write. Removing the worktree removes its build output; the repository
does not use a shared writable Cargo target.
