#!/bin/sh

set -eu

exec timeout 900 cargo test --workspace --test spec "$@"
