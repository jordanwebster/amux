#!/bin/sh

set -eu

target=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)
            [ "$#" -ge 2 ] || {
                echo "release-policy-check: --target needs a value" >&2
                exit 2
            }
            target=$2
            shift 2
            ;;
        --target=*)
            target=${1#--target=}
            shift
            ;;
        *)
            shift
            ;;
    esac
done

if [ -n "$target" ]; then
    binary="target/$target/release/amux"
else
    binary="target/release/amux"
fi
case "$target" in
    *windows*) binary="$binary.exe" ;;
esac

[ -x "$binary" ] || {
    echo "release-policy-check: release product is missing: $binary" >&2
    exit 1
}

help=$("$binary" --help)
case "$help" in
    *debug*)
        echo "release-policy-check: release help exposes the debug command" >&2
        exit 1
        ;;
esac

new_help=$("$binary" new --help)
case "$new_help" in
    *test-agent*)
        echo "release-policy-check: release help exposes the test agent" >&2
        exit 1
        ;;
esac

if "$binary" debug --help >/dev/null 2>&1; then
    echo "release-policy-check: release binary accepts the debug command" >&2
    exit 1
fi
if "$binary" new test-agent >/dev/null 2>&1; then
    echo "release-policy-check: release binary accepts the test agent" >&2
    exit 1
fi

echo "release policy excludes diagnostics and development agents"
