#!/usr/bin/env bash
# Sets the workspace version in Cargo.toml and Cargo.lock, e.g.
# `scripts/bump-version.sh 0.2.0`. Refuses versions that aren't semver or
# aren't newer than the current one.
set -euo pipefail

version="${1:?usage: scripts/bump-version.sh <version>}"
semver='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'
if ! [[ $version =~ $semver ]]; then
    echo "The version must look like 1.2.3 or 1.2.3-beta.1, not '$version'." >&2
    exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/Cargo.toml"
current="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$manifest")"
if [[ -z $current ]]; then
    echo "No version found under [workspace.package] in $manifest." >&2
    exit 1
fi

# 1.2.3-beta.1 may become 1.2.3 or 1.2.3-beta.2; otherwise the numbers must grow.
base="${version%%-*}"
current_base="${current%%-*}"
newer_base="$(printf '%s\n%s\n' "$current_base" "$base" | sort -V | tail -n 1)"
if [[ $version == "$current" ]]; then
    echo "Cargo.toml is already at $version." >&2
    exit 1
elif [[ $base == "$current_base" && $current != *-* ]]; then
    echo "$version comes before $current, which is already released." >&2
    exit 1
elif [[ $base != "$current_base" && $newer_base != "$base" ]]; then
    echo "$version is older than $current." >&2
    exit 1
elif [[ $base == "$current_base" && $version == *-* ]] &&
    [[ "$(printf '%s\n%s\n' "$current" "$version" | sort -V | tail -n 1)" != "$version" ]]; then
    echo "$version is older than $current." >&2
    exit 1
fi

sed -i.bak "/^\[workspace\.package\]/,/^\[/s/^version = \".*\"$/version = \"$version\"/" "$manifest"
rm "$manifest.bak"
# Any resolve writes the workspace crates' new version into Cargo.lock.
cargo metadata --format-version 1 --manifest-path "$manifest" >/dev/null
echo "Version $current -> $version"
