#!/bin/sh
set -eu

# Offline, deterministic coverage for the CLI's cascade guard and report; the
# daemon's local/remote deletion path is covered by the whole-daemon spec.
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"
wt test -- a2a_rm_cascade
