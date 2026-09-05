#!/bin/sh
set -eu
exec timeout 3600 cargo run -q -p xtask -- ci-observe "$@"
