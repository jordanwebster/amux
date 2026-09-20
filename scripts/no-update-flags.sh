#!/bin/sh
set -eu

flags=$(env | sed -n 's/^\(UPDATE_[A-Za-z0-9_]*\)=.*/\1/p' | LC_ALL=C sort -u)
if [ -n "$flags" ]; then
    echo "fixture update flags are forbidden in asserted test runs:" >&2
    printf '%s\n' "$flags" >&2
    exit 1
fi
