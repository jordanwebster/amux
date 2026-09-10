#!/bin/sh

set -eu

# Local runs measured 72 s, but GitHub's macos-26 iOS job was still making
# progress when 150 s killed it in the UI spec harness. Restore the 900 s
# ceiling: six times that observed CI lower bound, allowing slower hosted
# scheduling and the remaining harnesses, while bounding a hang to 15 minutes.
# This is a conservative runner allowance, not a measured CI completion time.
# The prerequisite workspace build stays outside this deadline.
deadline_seconds=900

# A test-name filter is applied inside each harness. Only a Cargo target
# selection avoids starting unrelated harnesses (and their OS launch checks).
for arg do
    case "$arg" in
        --) break ;;
        --lib|--bins|--bin|--bin=*|--examples|--example|--example=*|--tests|--test|--test=*|--benches|--bench|--bench=*|--all-targets|--doc)
            exec timeout "$deadline_seconds" cargo test --workspace "$@"
            ;;
    esac
done

exec timeout "$deadline_seconds" cargo test --workspace --all-targets "$@"
