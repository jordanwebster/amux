#!/bin/sh
set -eu
# Refuse an accidental invocation from another worktree before any remote write.
if [ "$(git branch --show-current)" != nativeapp ]; then
    echo '{"error":"WrongBranch","expected":"nativeapp"}' >&2
    exit 1
fi
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo '{"error":"DirtyTree"}' >&2
    exit 1
fi
"$(dirname "$0")/bounded" 120 git push origin HEAD:nativeapp
exec just ios ci-status --wait 3000
