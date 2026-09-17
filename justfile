set shell := ["sh", "-cu"]
set windows-shell := ["sh", "-cu"]
set positional-arguments

export AMUX_GIT_SHA := `git rev-parse HEAD`

# Wall-clock bound for every recipe; portable where GNU timeout is absent.
bounded := "scripts/bounded"
# Desktop roots keep naming the feature explicitly so an invocation that
# disables defaults cannot silently switch the SQLite linkage policy.
desktop_features := "--features bundled"

# List the available repository tasks.
default:
    @just --list

# The iPhone app's recipes: `just ios build`, `just ios verify`, ... (`just --list ios`).
mod ios 'apps/apple/justfile'

# Build the desktop product binaries.
build:
    {{bounded}} 900 cargo build --locked -p amux --bins {{desktop_features}}

# Check every workspace library and binary.
check:
    {{bounded}} 900 cargo check --locked --workspace --lib --bins {{desktop_features}}

# Run workspace tests, preserving explicit Cargo target selections.
test *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; scripts/workspace-test.sh {{desktop_features}} "$@"

# Run tests for one named workspace crate.
test-crate CRATE *ARGS:
    crate=$1; shift; if [ "${1-}" = -- ]; then shift; fi; feature=; if cargo tree --locked -p "$crate" -e normal --prefix none --format '{p}' | grep -q '^store v'; then feature='{{desktop_features}}'; fi; {{bounded}} 900 cargo test --locked -p "$crate" $feature "$@"

# Exercise the shared merge algebra and provider folds.
test-fold *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; if [ "${2-}" = --nocapture ]; then filter=$1; shift 2; {{bounded}} 1200 cargo test --locked -p fold "$filter" -- --nocapture "$@"; else {{bounded}} 1200 cargo test --locked -p fold "$@"; fi

# Exercise the shared SQLite lifecycle and materialisation contract.
test-store *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo test --locked -p store --features bundled "$@"

# Cross-compile every store test and run it against bundled SQLite on the
# leased, booted iOS simulator, including linkage and runtime identity proof.
test-store-ios:
    scripts/with iphone -- {{bounded}} 2300 scripts/python -B scripts/ios-store-test.py

# Exercise the pure store-backed reducer lifecycle and recorded UI specs.
test-ui *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo test --locked -p ui-state "$@"

# Compile every ordinary workspace test target without running it.
test-build:
    {{bounded}} 1200 cargo test --locked --workspace --lib --tests --no-run {{desktop_features}}

# Run all workspace documentation tests.
doctest:
    {{bounded}} 600 cargo test --locked --workspace --doc {{desktop_features}}

# Run the whole-daemon and UI-state specification suites.
spec *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; scripts/spec-test.sh "$@"

# Exercise the structured daemon replay, ring and semantic-reset contract.
test-daemon-protocol *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; filter=daemon_protocol; if [ "$#" -gt 0 ]; then filter="daemon_protocol_$1"; shift; fi; {{bounded}} 1200 cargo test --locked -p agent-runtime "$filter" "$@"

# Exercise daemon-owned structured-agent summaries, progress and health recovery.
test-daemon-summarizer *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo test --locked -p agent-runtime daemon_summarizer "$@" && {{bounded}} 1200 cargo test --locked -p node daemon_summarizer "$@"

# Exercise Claude SDK transcript-tail resume publication and fold semantics.
test-daemon-sdk *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo test --locked -p claude history "$@" && {{bounded}} 1200 cargo test --locked -p agent-runtime daemon_sdk "$@" && {{bounded}} 1200 cargo test --locked -p fold claude_sdk "$@"

# Exercise TUI behavior; `standing` runs the two-terminal daemon-summary proof.
test-tui *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; if [ "${1-}" = standing ]; then shift; {{bounded}} 900 cargo build --locked -p amux -p testnet --bins {{desktop_features}}; {{bounded}} 900 scripts/tui-standing-test.py "$@"; else {{bounded}} 1200 cargo test --locked -p tui --features fixtures "$@"; fi

# Lint every workspace target with warnings denied.
lint:
    {{bounded}} 1200 cargo clippy --locked --workspace --all-targets {{desktop_features}} -- -D warnings

# Format all Rust sources with the pinned nightly toolchain.
fmt:
    {{bounded}} 600 cargo +nightly-2026-08-30 fmt --all

# Check Rust formatting with the pinned nightly toolchain.
fmt-check:
    {{bounded}} 600 cargo +nightly-2026-08-30 fmt --all -- --check

# Regenerate the committed protobuf bindings.
protobuf:
    {{bounded}} 600 cargo run --locked -p xtask -- codegen

# Check that committed protobuf bindings match their sources.
codegen-check:
    {{bounded}} 600 scripts/codegen-check.sh

# Build test binaries and run the end-to-end scenarios.
e2e *ARGS: e2e-build
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 900 target/debug/e2e-runner run --amux-binary target/debug/amux --test-agent-binary target/debug/test-agent "$@"

# Build the binaries used by end-to-end scenarios.
e2e-build:
    {{bounded}} 900 cargo build --locked -p amux -p e2e-runner -p test-agent --bins {{desktop_features}}

# Build the shipping binary and enforce the release dependency policy.
release-check *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1200 cargo build --locked --release -p amux --bins --no-default-features {{desktop_features}} "$@"
    if [ "${1-}" = -- ]; then shift; fi; scripts/release-policy-check.sh "$@"

# Check the provider-free graph used by embedded clients.
embedded-check:
    {{bounded}} 900 cargo check --locked -p node -p client -p ui-state -p ui-runtime {{desktop_features}}

# Exercise the public embedded owner and client boundary.
embedded-test:
    {{bounded}} 900 cargo test --locked -p testnet --test embedding {{desktop_features}}

# Check the provider-free graph for iOS devices and simulators.
mobile-check:
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime --target aarch64-apple-ios
    {{bounded}} 1200 cargo check --locked -p node -p client -p ui-state -p ui-runtime --target aarch64-apple-ios-sim

# Run workspace tests with isolated user configuration and no external network.
offline-test:
    {{bounded}} 1200 scripts/offline-check.sh cargo test --locked --workspace --lib --tests {{desktop_features}}

# Build the product with the full-debug profile.
full-debug:
    {{bounded}} 900 cargo build --locked -p amux --bins --profile full-debug {{desktop_features}}

# Run selected live Codex scenarios.
codex-live *ARGS: build
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 900 cargo test --locked -p testnet --test codex_live {{desktop_features}} -- "$@"

# Run selected live Claude PTY scenarios.
claude-pty-live *ARGS: build
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 900 cargo test --locked -p testnet --test claude_pty_live {{desktop_features}} -- "$@"

# Run selected live Claude SDK scenarios.
claude-sdk-live *ARGS: build
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 900 cargo test --locked -p testnet --test claude_sdk_live {{desktop_features}} -- "$@"

# Render or inspect deterministic TUI evidence.
shot *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 600 cargo run --locked --quiet -p shot --bin amux-shot {{desktop_features}} -- "$@"

# Generate and verify the complete TUI evidence bundle.
tui-evidence *ARGS:
    if [ "${1-}" = -- ]; then shift; fi; {{bounded}} 1800 scripts/tui-evidence "$@"

# Enforce production and test-infrastructure dependency boundaries.
dependency-policy:
    {{bounded}} 60 python3 scripts/check-dependency-policy.py

# Warm the product and ordinary test build graphs used by new worktrees.
warm: build test-build

# Run the same task sequence exercised across continuous-integration jobs.
ci: check lint fmt-check codegen-check dependency-policy test doctest release-check e2e embedded-check embedded-test mobile-check

# Qualify desktop performance on an enrolled machine and include the phone
# suite whenever Xcode is available. The phone recipe takes its own simulator
# lease and prepares the pinned device. Pass --baseline to record the current
# release medians after every absolute budget passes.
perf *ARGS:
    set -e; if [ "${1-}" = -- ]; then shift; fi; mode=${1-}; {{bounded}} 1200 cargo build --locked --release -p amux --bin amux --features bundled,perf; {{bounded}} 1200 cargo build --locked --release -p testnet --bin perf --features bundled,perf; {{bounded}} 1800 target/release/perf "$@"; if [ "$mode" != soak ] && command -v xcrun >/dev/null 2>&1; then {{bounded}} 3600 just ios perf; fi
