#!/usr/bin/env bash
set -euo pipefail

# Usage: ./make_release.sh [version]
# If version is not provided, bumps the minor version of the current release.

# Get current version from amux-cli Cargo.toml
current=$(grep '^version = ' crates/amux-cli/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

if [ -n "${1:-}" ]; then
    new_version="$1"
else
    # Bump minor version
    IFS='.' read -r major minor patch <<< "$current"
    new_version="${major}.$((minor + 1)).0"
fi

echo "Releasing v${new_version} (current: v${current})"

# Update version in all crate Cargo.toml files that track the release version
sed -i '' "s/^version = \"${current}\"/version = \"${new_version}\"/" \
    crates/amux/Cargo.toml \
    crates/amux-cli/Cargo.toml

# Update Cargo.lock
cargo check --quiet

# Commit, tag, push
git add crates/amux/Cargo.toml crates/amux-cli/Cargo.toml Cargo.lock
git commit -m "v${new_version}"
git tag "v${new_version}"
git push
git push origin "v${new_version}"

echo "Released v${new_version}"
