#!/bin/sh

set -eu

report=$(mktemp "${TMPDIR:-/tmp}/amux-debug-policy.XXXXXX")
cleanup() {
    rm -f -- "$report"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

if RUST_BACKTRACE=1 cargo test --locked -p model --test debug_policy -- \
    --ignored --exact backtrace_probe >"$report" 2>&1
then
    echo "debug-policy-check: the intentional panic unexpectedly passed" >&2
    exit 1
fi

for expected in \
    "debug policy backtrace probe" \
    "backtrace_probe" \
    "stack backtrace"
do
    if ! grep -F "$expected" "$report" >/dev/null; then
        cat "$report" >&2
        echo "debug-policy-check: backtrace omitted: $expected" >&2
        exit 1
    fi
done

if [ "$(uname -s)" = Darwin ]; then
    bundle=target/debug/amux.dSYM
    if [ ! -d "$bundle" ]; then
        echo "debug-policy-check: packed Apple debug bundle is missing: $bundle" >&2
        exit 1
    fi
    dwarfdump --uuid "$bundle" >/dev/null
fi

echo "debug policy preserves named panic backtraces; Apple product debug data is packed"
