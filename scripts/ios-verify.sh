#!/bin/sh
set -eu
exec timeout 12600 cargo run -q -p xtask -- ios-verify
