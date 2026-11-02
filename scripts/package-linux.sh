#!/usr/bin/env bash
# Packages a release build for Linux into <out-dir>: a tar.gz with
# install.sh, a .deb and an AppImage, named for this machine's architecture.
# Build first with `cargo build --release --locked -p nordpil`; needs
# cargo-deb. Downloads appimagetool unless $APPIMAGETOOL points at one.
#
#   scripts/package-linux.sh 0.2.0 dist
set -euo pipefail

version="${1:?usage: scripts/package-linux.sh <version> <out-dir>}"
out="${2:?usage: scripts/package-linux.sh <version> <out-dir>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
arch="$(uname -m)"
binary="$root/target/release/nordpil"
assets="$root/crates/app/assets"
name="nordpil-$version-linux-$arch"

if [[ ! -x $binary ]]; then
    echo "No release build at $binary." >&2
    exit 1
fi
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# tar.gz: the binary, launcher entry, icon, licenses and install.sh.
folder="$work/$name"
mkdir -p "$folder"
strip -o "$folder/nordpil" "$binary"
cp "$assets/nordpil.desktop" "$assets/nordpil.svg" "$root/scripts/install.sh" \
    "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/README.md" "$folder/"
tar -C "$work" -czf "$out/$name.tar.gz" "$name"

# .deb, from the metadata in crates/app/Cargo.toml.
deb="$(cd "$root" && cargo deb -p nordpil --no-build --locked --output "$work/deb" | tail -n 1)"
cp "$deb" "$out/$name.deb"

# AppImage.
appdir="$work/AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" \
    "$appdir/usr/share/icons/hicolor/scalable/apps"
cp "$folder/nordpil" "$appdir/usr/bin/"
cp "$assets/nordpil.desktop" "$appdir/usr/share/applications/"
cp "$assets/nordpil.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/"
cp "$assets/nordpil.desktop" "$assets/nordpil.svg" "$appdir/"
ln -s usr/bin/nordpil "$appdir/AppRun"
ln -s nordpil.svg "$appdir/.DirIcon"
tool="${APPIMAGETOOL:-}"
if [[ -z $tool ]]; then
    tool="$work/appimagetool"
    curl -fsSL -o "$tool" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$arch.AppImage"
    chmod +x "$tool"
fi
# Extracting instead of mounting works without FUSE, e.g. on CI.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH="$arch" "$tool" --no-appstream "$appdir" "$out/$name.AppImage" >/dev/null

ls -l "$out"
