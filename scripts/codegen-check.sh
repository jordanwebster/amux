#!/bin/sh
set -eu

cargo run --locked -p xtask -- codegen
if ! git diff --quiet -- crates/wire/src/generated; then
    git --no-pager diff -- crates/wire/src/generated
    echo "committed wire output is stale; run 'wt run protobuf' and commit it" >&2
    exit 1
fi
