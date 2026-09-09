#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."
python3 -B ios/Tools/strings_lint_test.py
exec python3 -B ios/Tools/strings_lint.py "$@"
