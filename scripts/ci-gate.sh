#!/bin/sh
set -eu
# Push the checked-out branch under its own name; a detached head has none.
branch=$(git branch --show-current)
if [ -z "$branch" ]; then
    echo '{"error":"DetachedHead"}' >&2
    exit 1
fi
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo '{"error":"DirtyTree"}' >&2
    exit 1
fi
"$(dirname "$0")/bounded" 120 git push origin "HEAD:$branch"
exec just ios ci-status --wait 3000
