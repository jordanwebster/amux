#!/bin/sh

set -eu

# Two phases, each under its own bound: compiling the test targets, then
# running them. A bound is a hang detector sized above the honest duration of
# its one phase on the slowest CI runner (docs/CI.md records the measurements);
# it is never raised to hide a failing or slow test. The compile bound is the
# one `just test-build` carries.
compile_bound=1200
run_bound=1000

# The whole workspace unless the arguments name packages. The workspace run
# names the desktop SQLite linkage explicitly; a named package that lacks the
# feature would refuse it, and its store dependency links SQLite by default.
scope="--workspace --features bundled"
for arg do
    case "$arg" in
        --) break ;;
        -p|-p*|--package|--package=*) scope= ;;
    esac
done

# A test-name filter is applied inside each harness. Only a Cargo target
# selection avoids starting unrelated harnesses (and their OS launch checks).
# Doctests cannot be compiled without running them, so a doctest run has no
# separate compile phase; a --no-run is the compile phase alone.
targets=--all-targets
compile=yes
for arg do
    case "$arg" in
        --) break ;;
        --doc) targets= compile= ;;
        --no-run) compile= run_bound=$compile_bound ;;
        --lib|--bins|--bin|--bin=*|--examples|--example|--example=*|--tests|--test|--test=*|--benches|--bench|--bench=*|--all-targets)
            targets=
            ;;
    esac
done

bounded="$(dirname "$0")/bounded"

if [ -n "$compile" ]; then
    # The same Cargo arguments without the harness's: everything before `--`,
    # less --no-fail-fast, which only matters once tests run.
    (
        harness=
        for arg do
            shift
            [ "$arg" = -- ] && harness=yes
            [ -n "$harness" ] && continue
            [ "$arg" = --no-fail-fast ] && continue
            set -- "$@" "$arg"
        done
        "$bounded" "$compile_bound" cargo test --locked $scope $targets --no-run "$@"
    )
fi

exec "$bounded" "$run_bound" cargo test --locked $scope $targets "$@"
