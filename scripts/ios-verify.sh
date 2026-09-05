#!/bin/sh
set -eu
exec timeout 9000 cargo run -q -p xtask -- ios-verify
