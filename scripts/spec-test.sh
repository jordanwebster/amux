#!/bin/sh

set -eu

timeout 900 cargo test --locked -p ui-state --test spec "$@"
exec timeout 900 cargo test --locked -p node --lib spec:: "$@"
