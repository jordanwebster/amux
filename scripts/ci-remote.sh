#!/bin/sh
# Push the rearchitect branch head to origin and wait for its CI run.
#
# Exits zero and prints the run's URL only when the run for this exact commit
# succeeds; on anything else it prints the failed jobs' logs and exits non-zero.
# The push is never forced: a remote that has moved on is a divergence for a
# person to resolve, not something to overwrite.
set -eu

branch=rearchitect
bounded="$(dirname "$0")/bounded"

current=$(git branch --show-current)
if [ "$current" != "$branch" ]; then
    echo "ci-remote: runs only on the $branch branch, not ${current:-a detached head}" >&2
    exit 1
fi
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo "ci-remote: the working tree has uncommitted changes; commit them first" >&2
    exit 1
fi
head=$(git rev-parse HEAD)

if ! "$bounded" 300 git push origin "HEAD:refs/heads/$branch"; then
    echo "ci-remote: pushing $head to origin/$branch failed; if origin has moved on, reconcile by hand (never force)" >&2
    exit 1
fi

# GitHub registers the push's run a few seconds after the push lands.
run=""
for _ in $(seq 60); do
    run=$("$bounded" 60 gh run list --commit "$head" --branch "$branch" --event push \
        --workflow ci.yml --limit 1 --json databaseId --jq '.[0].databaseId // empty')
    [ -n "$run" ] && break
    sleep 5
done
if [ -z "$run" ]; then
    echo "ci-remote: no CI run appeared for $head within five minutes" >&2
    exit 1
fi
url=$("$bounded" 60 gh run view "$run" --json url --jq .url)
echo "ci-remote: watching $url"

watched=0
"$bounded" 6600 gh run watch "$run" --exit-status --interval 30 >/dev/null || watched=$?
conclusion=$("$bounded" 60 gh run view "$run" --json conclusion --jq .conclusion)
if [ "$watched" -ne 0 ] || [ "$conclusion" != success ]; then
    "$bounded" 300 gh run view "$run" --log-failed || true
    echo "ci-remote: run $url concluded ${conclusion:-unfinished}" >&2
    exit 1
fi
echo "$url"
