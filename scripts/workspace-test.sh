#!/bin/sh

set -eu

exec timeout 900 cargo test --workspace --all-targets "$@"
