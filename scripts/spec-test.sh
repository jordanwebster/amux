#!/bin/sh

set -eu

if [ "${AMUX_OFFLINE:-0}" = 1 ]; then
    echo "spec-test: offline sandbox; compiling but not running the TCP-backed TestNet suite"
    exec timeout 900 cargo test -p amux --features testnet --test spec --no-run
fi

exec timeout 900 cargo test -p amux --features testnet --test spec
