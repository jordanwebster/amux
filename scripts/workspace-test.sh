#!/bin/sh

set -eu

# The store integrity suite keeps this separately compiled v2 family owner
# alive while the ordinary workspace test binary opens the same database.
"$(dirname "$0")/bounded" 1200 cargo build --locked -p store \
    --bin store-family-v2-fixture \
    --features bundled,family-definition-v2-fixture

# A test-name filter is applied inside each harness. Only a Cargo target
# selection avoids starting unrelated harnesses (and their OS launch checks).
for arg do
    case "$arg" in
        --) break ;;
        --lib|--bins|--bin|--bin=*|--examples|--example|--example=*|--tests|--test|--test=*|--benches|--bench|--bench=*|--all-targets|--doc)
            exec "$(dirname "$0")/bounded" 900 cargo test --locked --workspace "$@"
            ;;
    esac
done

exec "$(dirname "$0")/bounded" 900 cargo test --locked --workspace --all-targets "$@"
