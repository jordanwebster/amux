#!/bin/sh
set -eu

reserve=$1
label=$2
shift 2

exec python3 scripts/output-budget.py run \
    --target target \
    --reserve "$reserve" \
    --label "$label" \
    --prune \
    -- "$@"
