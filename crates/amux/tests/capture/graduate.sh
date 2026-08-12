#!/bin/sh
set -eu

if [ "$#" -lt 2 ]; then
  echo "usage: $0 RUN_DIR SCENARIO..." >&2
  exit 2
fi

run_dir=$1
shift

timeout 600 cargo test -p amux --test capture -- tooling verify "$run_dir" "$@"
timeout 600 cargo test -p amux --test capture -- tooling drift "$run_dir"
timeout 600 cargo test -p amux --test capture -- tooling graduate "$run_dir" "$@"
