#!/usr/bin/env bash
set -euo pipefail

# Usage: ./make_release.sh [version]
# If version is not provided, bumps the minor version of the current release.

# Get the current product version.
current=$(grep '^version = ' crates/amux/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

if [ -n "${1:-}" ]; then
    new_version="$1"
else
    # Bump minor version
    IFS='.' read -r major minor patch <<< "$current"
    new_version="${major}.$((minor + 1)).0"
fi

echo "Releasing v${new_version} (current: v${current})"

# Update version in all crate Cargo.toml files that track the release version.
# The daemon announces its own crate's version to peers and writes it into the
# update marker, so a release that moved only the CLI would have `amux
# --version` and the machine a person sees in their fleet disagree.
sed -i '' "s/^version = \"${current}\"/version = \"${new_version}\"/" \
    crates/amux/Cargo.toml crates/node/Cargo.toml

# Update Cargo.lock
cargo update --offline -p amux -p node
just release-check

# Commit, tag, push
git add crates/amux/Cargo.toml crates/node/Cargo.toml Cargo.lock
git commit -m "v${new_version}"
git tag "v${new_version}"
git push
git push origin "v${new_version}"

echo "Released v${new_version}"
