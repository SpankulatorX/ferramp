#!/bin/sh
# Builds a .deb package from an already-compiled release binary.
# Usage: packaging/linux/build-deb.sh [target/release/ferramp] [output-dir]
# Needs: dpkg-deb. Version is read from Cargo.toml.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
bin=${1:-"$root/target/release/ferramp"}
outdir=${2:-"$root/dist"}

if [ ! -x "$bin" ]; then
    echo "error: binary not found or not executable: $bin" >&2
    echo "hint: run 'cargo build --release' first" >&2
    exit 1
fi

version=$(grep -m1 '^version' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
arch=$(uname -m)
case "$arch" in
    x86_64) debarch=amd64 ;;
    aarch64|arm64) debarch=arm64 ;;
    *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
esac

stagedir=$(mktemp -d)
trap 'rm -rf "$stagedir"' EXIT INT TERM

pkgdir="$stagedir/ferramp_${version}_${debarch}"
mkdir -p "$pkgdir/DEBIAN" \
    "$pkgdir/usr/bin" \
    "$pkgdir/usr/share/applications" \
    "$pkgdir/usr/share/doc/ferramp"
for size in 16 32 48 64 128 256; do
    mkdir -p "$pkgdir/usr/share/icons/hicolor/${size}x${size}/apps"
done

install -m755 "$bin" "$pkgdir/usr/bin/ferramp"
install -m644 "$root/assets/ferramp.desktop" "$pkgdir/usr/share/applications/ferramp.desktop"
for size in 16 32 48 64 128 256; do
    install -m644 "$root/assets/icons/ferramp-$size.png" \
        "$pkgdir/usr/share/icons/hicolor/${size}x${size}/apps/ferramp.png"
done
install -m644 "$root/README.md" "$pkgdir/usr/share/doc/ferramp/README.md"
for f in LICENSE-MIT LICENSE-APACHE NOTICE; do
    [ -f "$root/$f" ] && install -m644 "$root/$f" "$pkgdir/usr/share/doc/ferramp/$f"
done
cat > "$pkgdir/usr/share/doc/ferramp/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Source: https://github.com/SpankulatorX/ferramp

Files: *
Copyright: Ferramp contributors
License: MIT or Apache-2.0
 See /usr/share/common-licenses/MIT and
 /usr/share/common-licenses/Apache-2.0,
 or the files LICENSE-MIT and LICENSE-APACHE shipped with this package.
EOF

installed_size=$(du -sk "$pkgdir/usr" | cut -f1)
cat > "$pkgdir/DEBIAN/control" <<EOF
Package: ferramp
Version: $version
Section: sound
Priority: optional
Architecture: $debarch
Maintainer: Ferramp contributors <https://github.com/SpankulatorX/ferramp>
Depends: libasound2 | libasound2t64, libc6 (>= 2.35)
Description: Classic skinnable audio player
 Ferramp is a cross-platform audio player in Rust that recreates the
 look and feel of the classic Winamp 2 player: main window, graphical
 equalizer and playlist editor, docked together. It plays MP3, Ogg
 Vorbis, FLAC, WAV, AIFF, AAC/M4A, ALAC and internet radio, and loads
 classic .wsz skins.
EOF
echo "$installed_size" > /dev/null

mkdir -p "$outdir"
deb="$outdir/ferramp-${version}-linux-${debarch}.deb"
# dpkg-deb names the file itself; build then rename to release convention.
dpkg-deb -Zgzip --root-owner-group --build "$pkgdir" "$stagedir/package.deb"
mv "$stagedir/package.deb" "$deb"
echo "Built $deb"
