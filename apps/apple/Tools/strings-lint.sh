#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
scripts/python -B apps/apple/Tools/strings_lint_test.py
exec scripts/python -B apps/apple/Tools/strings_lint.py "$@"
