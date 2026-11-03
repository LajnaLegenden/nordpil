#!/usr/bin/env bash
# Writes nordpil-update.json into <dist-dir>, listing the version and the
# SHA-256 of every file there, and signs it as nordpil-update.json.sig with
# the Ed25519 key in $UPDATE_SIGNING_KEY (PEM). The app only installs files
# from a release whose manifest verifies against crates/app/assets/update-key.pem.
#
#   UPDATE_SIGNING_KEY="$(cat key.pem)" scripts/update-manifest.sh 0.2.0 dist
set -euo pipefail

version="${1:?usage: scripts/update-manifest.sh <version> <dist-dir>}"
dist="${2:?usage: scripts/update-manifest.sh <version> <dist-dir>}"
if [[ -z ${UPDATE_SIGNING_KEY:-} ]]; then
    echo "UPDATE_SIGNING_KEY is not set." >&2
    exit 1
fi
manifest="$dist/nordpil-update.json"
rm -f "$manifest" "$manifest.sig"

files="$(cd "$dist" && sha256sum -- * |
    jq -R 'capture("^(?<sha256>[0-9a-f]{64})  (?<name>.+)$") | {name, sha256}' | jq -s .)"
jq -n --arg version "$version" --argjson files "$files" \
    '{version: $version, files: $files}' >"$manifest"

key="$(mktemp)"
trap 'rm -f "$key"' EXIT
printf '%s\n' "$UPDATE_SIGNING_KEY" >"$key"
# -rawin signs the file itself, as Ed25519 requires (OpenSSL 3).
openssl pkeyutl -sign -inkey "$key" -rawin -in "$manifest" -out "$manifest.sig"
echo "Signed $manifest for $(jq '.files | length' "$manifest") files."
