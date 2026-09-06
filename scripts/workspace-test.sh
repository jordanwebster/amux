#!/bin/sh

set -eu

# A test-name filter is applied inside each harness. Only a Cargo target
# selection avoids starting unrelated harnesses (and their OS launch checks).
for arg do
    case "$arg" in
        --) break ;;
        --lib|--bins|--bin|--bin=*|--examples|--example|--example=*|--tests|--test|--test=*|--benches|--bench|--bench=*|--all-targets|--doc)
            exec timeout 900 cargo test --workspace "$@"
            ;;
    esac
done

exec timeout 900 cargo test --workspace --all-targets "$@"
