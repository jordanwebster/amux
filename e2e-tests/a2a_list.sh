#!/bin/sh
set -eu

# Offline, deterministic coverage for the family view; the real CLI uses the
# same renderer after fetching the daemon inventory.
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"
wt test -- a2a_list
