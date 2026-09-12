set shell := ["sh", "-cu"]
set windows-shell := ["sh", "-cu"]

export AMUX_GIT_SHA := `git rev-parse HEAD`

# List the available repository tasks.
default:
    @just --list

# Build the desktop product binaries.
build:
    timeout 900 cargo build --locked -p amux --bins

# Check every workspace library and binary.
check:
    timeout 900 cargo check --locked --workspace --lib --bins

# Run workspace tests, preserving explicit Cargo target selections.
test *ARGS:
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; scripts/workspace-test.sh "$@"

# Run tests for one named workspace crate.
test-crate CRATE *ARGS:
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 900 cargo test --locked -p {{CRATE}} "$@"

# Compile every ordinary workspace test target without running it.
test-build:
    timeout 1200 cargo test --locked --workspace --lib --tests --no-run

# Run all workspace documentation tests.
doctest:
    timeout 600 cargo test --locked --workspace --doc

# Run the whole-daemon and UI-state specification suites.
spec *ARGS:
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; scripts/spec-test.sh "$@"

# Lint every workspace target with warnings denied.
lint:
    timeout 1200 cargo clippy --locked --workspace --all-targets -- -D warnings

# Format all Rust sources with the pinned nightly toolchain.
fmt:
    timeout 600 cargo +nightly-2026-08-30 fmt --all

# Check Rust formatting with the pinned nightly toolchain.
fmt-check:
    timeout 600 cargo +nightly-2026-08-30 fmt --all -- --check

# Regenerate the committed protobuf bindings.
protobuf:
    timeout 600 cargo run --locked -p xtask -- codegen

# Check that committed protobuf bindings match their sources.
codegen-check:
    timeout 600 scripts/codegen-check.sh

# Build test binaries and run the end-to-end scenarios.
e2e *ARGS: e2e-build
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 900 target/debug/e2e-runner run --amux-binary target/debug/amux --test-agent-binary target/debug/test-agent "$@"

# Build the binaries used by end-to-end scenarios.
e2e-build:
    timeout 900 cargo build --locked -p amux -p e2e-runner -p test-agent --bins

# Build the shipping binary and enforce the release dependency policy.
release-check *ARGS:
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 1200 cargo build --locked --release -p amux --bins --no-default-features "$@"
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; scripts/release-policy-check.sh "$@"

# Check the provider-free graph used by embedded clients.
embedded-check:
    timeout 900 cargo check --locked -p node -p client -p ui-state -p ui-runtime

# Exercise the public embedded owner and client boundary.
embedded-test:
    timeout 900 cargo test --locked -p testnet --test embedding

# Check the provider-free graph for iOS devices and simulators.
mobile-check:
    timeout 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime --target aarch64-apple-ios
    timeout 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime --target aarch64-apple-ios-sim

# Run workspace tests with isolated user configuration and no external network.
offline-test:
    timeout 1200 scripts/offline-check.sh cargo test --locked --workspace --lib --tests

# Build the product with the full-debug profile.
full-debug:
    timeout 900 cargo build --locked -p amux --bins --profile full-debug

# Run selected live Codex scenarios.
codex-live *ARGS: build
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 900 cargo test --locked -p testnet --test codex_live -- "$@"

# Run selected live Claude PTY scenarios.
claude-pty-live *ARGS: build
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 900 cargo test --locked -p testnet --test claude_pty_live -- "$@"

# Run selected live Claude SDK scenarios.
claude-sdk-live *ARGS: build
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 900 cargo test --locked -p testnet --test claude_sdk_live -- "$@"

# Render or inspect deterministic TUI evidence.
shot *ARGS:
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 600 cargo run --locked --quiet -p shot --bin amux-shot -- "$@"

# Generate and verify the complete TUI evidence bundle.
tui-evidence *ARGS:
    set -- {{ARGS}}; if [ "${1-}" = -- ]; then shift; fi; timeout 1800 scripts/tui-evidence "$@"

# Enforce production and test-support dependency boundaries.
dependency-policy:
    timeout 60 python3 scripts/check-dependency-policy.py

# Warm the product and ordinary test build graphs used by new worktrees.
warm: build test-build

# Run the same task sequence exercised across continuous-integration jobs.
ci: check lint fmt-check codegen-check dependency-policy test doctest release-check e2e embedded-check embedded-test mobile-check
