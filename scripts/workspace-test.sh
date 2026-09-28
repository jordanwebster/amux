#!/bin/sh

set -eu

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
for arg do
    case "$arg" in
        --) break ;;
        --lib|--bins|--bin|--bin=*|--examples|--example|--example=*|--tests|--test|--test=*|--benches|--bench|--bench=*|--all-targets|--doc)
            exec "$(dirname "$0")/bounded" 900 cargo test --locked $scope "$@"
            ;;
    esac
done

exec "$(dirname "$0")/bounded" 900 cargo test --locked $scope --all-targets "$@"
