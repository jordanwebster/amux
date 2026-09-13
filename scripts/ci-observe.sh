#!/bin/sh
set -eu
exec "$(dirname "$0")/bounded" 3600 cargo run --locked -q -p xtask -- ci-observe "$@"
