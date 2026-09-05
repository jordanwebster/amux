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
timeout 120 git push origin HEAD:nativeapp
exec timeout 3300 wt run ci-status -- --wait 3000
