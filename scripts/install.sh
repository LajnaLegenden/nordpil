#!/bin/sh
# Installs nordpil from this folder: the binary, a launcher entry and the icon.
#
#   ./install.sh               into /usr/local, with sudo if needed
#   ./install.sh --user        into ~/.local, no sudo
#   ./install.sh /opt/nordpil  into another prefix
#   ./install.sh --uninstall [--user | prefix]   removes them again
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
prefix=/usr/local
uninstall=false
for arg in "$@"; do
    case "$arg" in
        --user) prefix="$HOME/.local" ;;
        --uninstall) uninstall=true ;;
        -h | --help)
            sed -n '2,8s/^# \{0,1\}//p' "$0"
            exit 0
            ;;
        -*)
            echo "Unknown option $arg. See ./install.sh --help." >&2
            exit 1
            ;;
        *) prefix="$arg" ;;
    esac
done

bin="$prefix/bin/nordpil"
desktop="$prefix/share/applications/nordpil.desktop"
icon="$prefix/share/icons/hicolor/scalable/apps/nordpil.svg"

# sudo only when the prefix isn't ours to write.
run() {
    probe="$prefix"
    while [ ! -e "$probe" ]; do probe="$(dirname "$probe")"; done
    if [ -w "$probe" ]; then "$@"; else sudo "$@"; fi
}

refresh_launchers() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        run update-desktop-database -q "$prefix/share/applications" || true
    fi
}

if [ "$uninstall" = true ]; then
    run rm -f "$bin" "$desktop" "$icon"
    refresh_launchers
    echo "Removed nordpil from $prefix."
    exit 0
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
sed "s|^Exec=.*|Exec=$bin|" "$here/nordpil.desktop" >"$tmp"
run mkdir -p "$prefix/bin" "$prefix/share/applications" "$prefix/share/icons/hicolor/scalable/apps"
run cp "$here/nordpil" "$bin"
run chmod 755 "$bin"
run cp "$tmp" "$desktop"
run chmod 644 "$desktop"
run cp "$here/nordpil.svg" "$icon"
run chmod 644 "$icon"
refresh_launchers
echo "Installed nordpil to $bin."
