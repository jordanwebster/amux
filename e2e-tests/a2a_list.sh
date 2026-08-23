#!/bin/sh
set -eu

# Offline, deterministic coverage for the family view; the real CLI uses the
# same renderer after fetching the daemon inventory.
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"
timeout 900 cargo test -p amux-cli -- a2a_list

