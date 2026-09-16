#!/bin/sh
# Replay the store-backed desktop acceptance journeys against real TUI and
# daemon process boundaries. Each scenario owns its testnet daemon and tmux
# server; artifacts are written below the requested evidence directory.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
scenario=${1:-}
evidence=${2:-/tmp/amux-store-scenarios}

case "$scenario" in
  warm-start|chat|two-terminal|gap|sdk-resume|all) ;;
  *)
    echo "usage: $0 <warm-start|chat|two-terminal|gap|sdk-resume|all> [evidence-dir]" >&2
    exit 2
    ;;
esac

for dependency in python3 tmux script sqlite3; do
  command -v "$dependency" >/dev/null 2>&1 || {
    echo "store scenarios require $dependency" >&2
    exit 1
  }
done

cd "$repo_root"
scripts/bounded 900 cargo build --locked -p amux -p testnet --bins --features bundled
exec python3 -B scripts/store-scenarios.py "$scenario" "$evidence"
