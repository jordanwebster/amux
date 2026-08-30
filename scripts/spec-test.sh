#!/bin/sh

set -eu

exec timeout 900 cargo test -p amux --features testnet --test spec
