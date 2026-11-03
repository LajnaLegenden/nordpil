#!/usr/bin/env bash
# Packages a release build for macOS into <out-dir> as a .dmg holding
# nordpil.app, ad-hoc signed (not notarized), and as an .app.tar.gz that the
# app's updater unpacks in place of the old bundle. Run on macOS after
# `cargo build --release --locked -p nordpil --target <target>`.
#
#   scripts/package-macos.sh 0.2.0 aarch64-apple-darwin dist
set -euo pipefail

usage="usage: scripts/package-macos.sh <version> <target> <out-dir>"
version="${1:?$usage}"
target="${2:?$usage}"
out="${3:?$usage}"
root="$(cd "$(dirname "$0")/.." && pwd)"
binary="$root/target/$target/release/nordpil"
assets="$root/crates/app/assets"
case "$target" in
    aarch64-*) arch=arm64 ;;
    x86_64-*) arch=x86_64 ;;
    *)
        echo "Unknown macOS target $target." >&2
        exit 1
        ;;
esac
name="nordpil-$version-macos-$arch"

if [[ ! -x $binary ]]; then
    echo "No release build at $binary." >&2
    exit 1
fi
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

app="$work/dmg/nordpil.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed "s/@VERSION@/$version/g" "$assets/macos/Info.plist" >"$app/Contents/Info.plist"
cp "$binary" "$app/Contents/MacOS/nordpil"
strip -x "$app/Contents/MacOS/nordpil"

# The icon at every size macOS asks for.
iconset="$work/nordpil.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$assets/nordpil.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$assets/nordpil.png" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/nordpil.icns"

# Apple Silicon only runs signed code; an ad-hoc signature is enough for that.
codesign --force --deep --sign - "$app"
# Without COPYFILE_DISABLE, tar adds ._ files that would break the signature.
COPYFILE_DISABLE=1 tar -C "$work/dmg" -czf "$out/$name.app.tar.gz" nordpil.app
ln -s /Applications "$work/dmg/Applications"
hdiutil create -volname "nordpil $version" -srcfolder "$work/dmg" -ov -format UDZO \
    "$out/$name.dmg" >/dev/null

ls -l "$out"
