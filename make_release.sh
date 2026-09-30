#!/usr/bin/env bash
set -euo pipefail

# Usage: ./make_release.sh [version] [--channel stable|preview] [--rollout N]
# If version is not provided, bumps the minor version of the current release.
# After the tag's Release workflow has published the binaries, signs them
# with this Mac's release key and uploads the channel manifest (stable unless
# --channel says otherwise); docs/RELEASE.md, "The daemon's release feed".

# Get the current product version.
current=$(grep '^version = ' crates/amux/Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

new_version=""
manifest_args=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --channel|--rollout) manifest_args+=("$1" "$2"); shift 2 ;;
        --*) echo "unknown option $1" >&2; exit 2 ;;
        *) new_version="$1"; shift ;;
    esac
done

# The manifest is signed with the key in this Mac's login keychain; find out
# now that it is there and matches the workflow, before anything is tagged.
just release-key public >/dev/null

if [ -n "$new_version" ]; then
    :
else
    # Bump minor version
    IFS='.' read -r major minor patch <<< "$current"
    new_version="${major}.$((minor + 1)).0"
fi

echo "Releasing v${new_version} (current: v${current})"

# Update version in all crate Cargo.toml files that track the release version.
# The daemon announces its own crate's version to peers, so a release that moved only the CLI would have `amux
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

# The pushed tag starts the Release workflow; wait for it, then sign what it
# published and upload the channel manifest to the same release.
echo "Waiting for the Release workflow of v${new_version}"
run_id=""
for _ in $(seq 1 60); do
    run_id=$(gh run list --workflow=release.yml --branch "v${new_version}" --json databaseId --jq '.[0].databaseId // empty')
    [ -n "$run_id" ] && break
    sleep 5
done
[ -n "$run_id" ] || { echo "no Release workflow run for v${new_version} after five minutes" >&2; exit 1; }
gh run watch "$run_id" --exit-status
just release-manifest "${new_version}" "${manifest_args[@]}" --publish

echo "Released v${new_version}"
