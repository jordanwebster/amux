#!/bin/sh
set -eu

export IPHONEOS_DEPLOYMENT_TARGET=26.0

mkdir -p target/ios
for triple in aarch64-apple-ios aarch64-apple-ios-sim; do
    case "$triple" in
        *-sim) sdk=iphonesimulator ;;
        *) sdk=iphoneos ;;
    esac
    SDKROOT=$(timeout 30 xcrun --sdk "$sdk" --show-sdk-path)
    export SDKROOT
    cargo tree --locked -p amux -p amux-ui -p amux-mobile --no-default-features \
        --target "$triple" --edges normal --prefix none --format '{p}' \
        > "target/ios/$triple-dependencies.txt"
    if grep -E '^(pty-host|codex) v' "target/ios/$triple-dependencies.txt"; then
        echo "Local agent dependency in the mobile graph for $triple" >&2
        exit 1
    fi
    cargo check --locked -p amux -p amux-ui -p amux-mobile \
        --no-default-features --lib --target "$triple"
done
