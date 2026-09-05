#!/bin/sh

set -eu

exec timeout 900 python3 "$(dirname "$0")/checked-cargo-test.py" --workspace --test spec "$@"
