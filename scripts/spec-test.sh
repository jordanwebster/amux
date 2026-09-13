#!/bin/sh

set -eu

"$(dirname "$0")/bounded" 900 cargo test --locked -p ui-state --test spec "$@"
exec "$(dirname "$0")/bounded" 900 cargo test --locked -p testnet --test spec "$@"
