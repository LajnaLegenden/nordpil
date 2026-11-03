#!/usr/bin/env bash
# Prints the version after the one in Cargo.toml, e.g.
# `scripts/next-version.sh minor` or `scripts/next-version.sh patch beta`.
#
#   patch, minor, major  bump that number: 0.2.1 -> 0.2.2, 0.3.0, 1.0.0. A
#                        pre-release becomes its own release when the bump
#                        allows it: 0.3.0-beta.2 minor -> 0.3.0.
#   prerelease           the next pre-release of the same version:
#                        0.3.0-beta.1 -> 0.3.0-beta.2, or with another tag
#                        0.3.0-beta.2 rc -> 0.3.0-rc.1.
#
# With a tag, patch, minor and major start a pre-release of the bumped
# version: 0.2.1 minor beta -> 0.3.0-beta.1.
set -euo pipefail

bump="${1:?usage: scripts/next-version.sh <patch|minor|major|prerelease> [tag]}"
tag="${2:-}"
if [[ -n $tag && ! $tag =~ ^[0-9A-Za-z-]+$ ]]; then
    echo "The pre-release tag must be letters, digits and hyphens, like beta or rc, not '$tag'." >&2
    exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/Cargo.toml"
current="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' "$manifest")"
if ! [[ $current =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-([0-9A-Za-z.-]+))?$ ]]; then
    echo "No semver version found under [workspace.package] in $manifest." >&2
    exit 1
fi
major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"
pre="${BASH_REMATCH[5]}"

case "$bump" in
patch)
    if [[ -z $pre || -n $tag ]]; then patch=$((patch + 1)); fi
    ;;
minor)
    if [[ -z $pre || -n $tag || $patch != 0 ]]; then
        minor=$((minor + 1))
        patch=0
    fi
    ;;
major)
    if [[ -z $pre || -n $tag || $minor != 0 || $patch != 0 ]]; then
        major=$((major + 1))
        minor=0
        patch=0
    fi
    ;;
prerelease)
    if [[ -z $pre ]]; then
        if [[ -z $tag ]]; then
            echo "$current isn't a pre-release; give a tag, like beta, to start one of the next patch." >&2
            exit 1
        fi
        echo "$major.$minor.$((patch + 1))-$tag.1"
        exit 0
    fi
    if [[ -n $tag && ${pre%%.*} != "$tag" ]]; then
        echo "$major.$minor.$patch-$tag.1"
    elif [[ $pre =~ ^(.*)\.([0-9]+)$ ]]; then
        echo "$major.$minor.$patch-${BASH_REMATCH[1]}.$((BASH_REMATCH[2] + 1))"
    else
        echo "$major.$minor.$patch-$pre.1"
    fi
    exit 0
    ;;
*)
    echo "The bump must be patch, minor, major or prerelease, not '$bump'." >&2
    exit 1
    ;;
esac

version="$major.$minor.$patch"
if [[ -n $tag ]]; then version="$version-$tag.1"; fi
echo "$version"
