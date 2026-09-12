#!/bin/sh

set -eu

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <command> [args...]" >&2
    exit 64
fi

# Cargo snapshots build-script output from the canonical checkout. Supplying
# the revision as a value lets an identical snapshot reuse that output while a
# new commit still invalidates the product stamp deliberately.
if [ -z "${AMUX_GIT_SHA:-}" ]; then
    AMUX_GIT_SHA=$(git rev-parse --verify HEAD)
    export AMUX_GIT_SHA
fi

exec "$@"
