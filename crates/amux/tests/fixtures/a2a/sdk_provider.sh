#!/bin/sh
# Synthetic stream-JSON peer. The real SDK owns this subprocess and its exit.
set -eu
IFS= read -r initialize
cat "$A2A_FIXTURE_DIR/sdk_initialize.jsonl"
while IFS= read -r prompt; do
    if [ "$A2A_ROLE" = child ]; then
        printf '%s\n' "$prompt"
        cat "$A2A_FIXTURE_DIR/sdk_result.jsonl"
        IFS= read -r finish
        exit 7
    fi
    printf '%s\n' "$prompt"
done
