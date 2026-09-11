#!/bin/sh
set -eu

reserve=$1
label=$2
shift 2

target=${AMUX_OUTPUT_TARGET:-${CARGO_TARGET_DIR:-target}}

exec python3 scripts/output-budget.py run \
    --target "$target" \
    --reserve "$reserve" \
    --label "$label" \
    --prune \
    -- "$@"
