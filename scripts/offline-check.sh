#!/bin/sh

set -eu

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <command> [args...]" >&2
    exit 64
fi

if [ ! -x /usr/bin/sandbox-exec ]; then
    echo "offline-check: /usr/bin/sandbox-exec is unavailable" >&2
    exit 69
fi

original_home=${HOME:?HOME must be set}
offline_cargo_home=${CARGO_HOME:-"$original_home/.cargo"}
offline_rustup_home=${RUSTUP_HOME:-"$original_home/.rustup"}
offline_path=${PATH:?PATH must be set}

offline_root=$(mktemp -d "${TMPDIR:-/tmp}/amux-offline.XXXXXX")
case "$offline_root" in
    "${TMPDIR:-/tmp}"/amux-offline.*) ;;
    *)
        echo "offline-check: mktemp returned an unexpected path: $offline_root" >&2
        exit 70
        ;;
esac

cleanup() {
    rm -rf -- "$offline_root"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

offline_home="$offline_root/home"
offline_claude_config="$offline_root/claude"
offline_codex_home="$offline_root/codex"
offline_profile="$offline_root/network.sb"
mkdir -p "$offline_home" "$offline_claude_config" "$offline_codex_home"

cat >"$offline_profile" <<'PROFILE'
(version 1)
(allow default)
(deny network*)
(allow network-outbound (remote unix-socket))
(allow network-inbound (local unix-socket))
PROFILE

printf '%s\n' \
    "HOME=$offline_home" \
    "CLAUDE_CONFIG_DIR=$offline_claude_config" \
    "CODEX_HOME=$offline_codex_home" \
    "CARGO_HOME=$offline_cargo_home" \
    "RUSTUP_HOME=$offline_rustup_home" \
    "PATH=$offline_path" \
    "CARGO_NET_OFFLINE=true"

run_sandboxed() {
    /usr/bin/sandbox-exec -f "$offline_profile" \
        /usr/bin/env \
        HOME="$offline_home" \
        CLAUDE_CONFIG_DIR="$offline_claude_config" \
        CODEX_HOME="$offline_codex_home" \
        CARGO_HOME="$offline_cargo_home" \
        RUSTUP_HOME="$offline_rustup_home" \
        PATH="$offline_path" \
        CARGO_NET_OFFLINE=true \
        "$@"
}

if run_sandboxed /usr/bin/nc -z -w 1 1.1.1.1 443 >/dev/null 2>&1; then
    echo "offline-check: refusing to run because the sandbox permits outbound TCP" >&2
    exit 70
fi

run_sandboxed "$@"
