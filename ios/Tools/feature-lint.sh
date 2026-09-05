#!/bin/sh
# Refuses what the screens must not contain.
#
# The screens are meant to be functions of their state: SwiftUI, no platform to
# ask about, no view whose behaviour lives outside the state it was handed.
# That is what lets one be captured, diffed, replayed and — when there is a Mac
# app — drawn again without being rewritten. Three things break it quietly, and
# all three are refused here:
#
#   * UIKit anywhere but a registered leaf. A leaf is a deliberate exception,
#     one file under Leaves/, named in RegisteredLeaves.swift and justified by a
#     written measurement. UIKit reached for anywhere else spreads until the
#     package is a UIKit app with SwiftUI on top.
#   * A platform conditional. A screen that draws differently on iOS and macOS
#     is two screens, and the second one is never the one anybody looked at.
#   * A spinner. Nothing these screens wait for is worth covering readable
#     content with a symbol that says only "wait": a wait under 300 ms is over
#     before a spinner would have been noticed, and a longer one has something
#     true to show meanwhile — the remembered fleet, shimmering until it is
#     confirmed. If a screen ever earns one, it earns a delayed wrapper with a
#     measured threshold, and this rule changes with it.
#
# It checks itself first: the rules are run against files written to be wrong,
# so a lint that has stopped being able to fail says so instead of passing.

set -eu

package=${AMUX_FEATURES_SOURCES:-ios/Packages/AmuxFeatures/Sources/AmuxFeatures}

# Every complaint about one tree of sources, one per line. Says nothing and
# answers 0 when the tree is clean.
scan() {
    root=$1
    leaves=$root/Leaves
    registry=$root/RegisteredLeaves.swift
    complaints=0

    for file in $(find "$root" -name '*.swift' | sort); do
        case $file in
            "$leaves"/*) is_leaf=yes ;;
            *) is_leaf=no ;;
        esac

        if grep -qE '^[[:space:]]*(@[A-Za-z_]+[[:space:]]+)?import[[:space:]]+UIKit\b' "$file"; then
            if [ "$is_leaf" = no ]; then
                echo "$file: imports UIKit outside Leaves/; a UIKit view is a registered leaf or it is not written"
                complaints=$((complaints + 1))
            fi
        fi

        spinner=$(grep -nE '(ProgressView[[:space:]]*\(|UIActivityIndicatorView)' "$file" || true)
        if [ -n "$spinner" ]; then
            echo "$file: draws a spinner; a wait under 300 ms needs none and a longer one shows what it already knows"
            echo "$spinner" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi

        conditional=$(grep -nE '^[[:space:]]*#(if|elseif)\b.*(os\(|canImport\(|targetEnvironment\()' "$file" || true)
        if [ -n "$conditional" ]; then
            echo "$file: branches on the platform; a screen draws from its state, not from where it is running"
            echo "$conditional" | sed 's/^/    /'
            complaints=$((complaints + 1))
        fi
    done

    # A leaf nobody registered is a leaf nobody measured.
    if [ -d "$leaves" ]; then
        for file in $(find "$leaves" -name '*.swift' | sort); do
            name=$(basename "$file" .swift)
            case_name=$(printf '%s' "$name" | cut -c1 | tr '[:upper:]' '[:lower:]')$(printf '%s' "$name" | cut -c2-)
            if [ ! -f "$registry" ] || ! grep -qE "^[[:space:]]*case[[:space:]]+$case_name\b" "$registry"; then
                echo "$file: is a UIKit leaf that RegisteredLeaves does not name; add \`case $case_name\` and the measurement that justifies it"
                complaints=$((complaints + 1))
            fi
        done
    fi

    [ "$complaints" -eq 0 ]
}

# Runs the rules against sources written to break each of them, so a rule that
# has stopped catching anything is caught here rather than in review.
self_test() {
    fixtures=$(mktemp -d)
    trap 'rm -rf "$fixtures"' EXIT
    mkdir -p "$fixtures/Leaves"

    printf 'import SwiftUI\nimport UIKit\n' > "$fixtures/Reaching.swift"
    printf '#if os(macOS)\nimport SwiftUI\n#endif\n' > "$fixtures/Branching.swift"
    printf 'import SwiftUI\nstruct Waiting: View { var body: some View { ProgressView() } }\n' \
        > "$fixtures/Waiting.swift"
    printf 'import UIKit\nstruct Unnamed {}\n' > "$fixtures/Leaves/Unnamed.swift"
    printf 'public enum RegisteredLeaves { case transcriptList }\n' > "$fixtures/RegisteredLeaves.swift"

    found=$(scan "$fixtures" || true)
    for expected in Reaching.swift Branching.swift Waiting.swift Unnamed.swift; do
        case $found in
            *"$expected"*) ;;
            *)
                echo "feature-lint cannot catch $expected any more; the rule it breaks is not working"
                exit 1
                ;;
        esac
    done

    printf 'import SwiftUI\nstruct Fine: View { var body: some View { Text("fine") } }\n' \
        > "$fixtures/Fine.swift"
    rm "$fixtures/Reaching.swift" "$fixtures/Branching.swift" "$fixtures/Waiting.swift" \
        "$fixtures/Leaves/Unnamed.swift"
    if ! scan "$fixtures" > /dev/null; then
        echo "feature-lint complains about sources that break none of its rules"
        exit 1
    fi

    rm -rf "$fixtures"
    trap - EXIT
}

self_test

if [ ! -d "$package" ]; then
    echo "no sources at $package; run this from the checkout root"
    exit 1
fi

if scan "$package"; then
    echo "$package: SwiftUI only, no platform conditionals, no spinners, every leaf registered"
else
    exit 1
fi
