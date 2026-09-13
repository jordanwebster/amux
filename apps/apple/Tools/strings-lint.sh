#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
python3 -B apps/apple/Tools/strings_lint_test.py
exec python3 -B apps/apple/Tools/strings_lint.py "$@"
