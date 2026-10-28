#!/bin/sh
# Installs Ferramp for the current user (or system-wide with PREFIX=/usr/local, run as root).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-"$HOME/.local"}

install -Dm755 "$here/ferramp" "$prefix/bin/ferramp"
# Absolute Exec path: desktop menus often run without ~/.local/bin on PATH.
mkdir -p "$prefix/share/applications"
sed "s|^Exec=ferramp |Exec=\"$prefix/bin/ferramp\" |" "$here/ferramp.desktop" \
    > "$prefix/share/applications/ferramp.desktop"
chmod 644 "$prefix/share/applications/ferramp.desktop"
for size in 16 32 48 64 128 256; do
    install -Dm644 "$here/icons/ferramp-$size.png" \
        "$prefix/share/icons/hicolor/${size}x${size}/apps/ferramp.png"
done
command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$prefix/share/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -q "$prefix/share/icons/hicolor" 2>/dev/null || true

echo "Ferramp installed to $prefix/bin/ferramp"
case ":$PATH:" in
    *":$prefix/bin:"*) ;;
    *) echo "Note: $prefix/bin is not on your PATH." ;;
esac
