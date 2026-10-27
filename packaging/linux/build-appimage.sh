#!/bin/sh
# Builds a Ferramp .AppImage from an already-compiled release binary.
# Usage: packaging/linux/build-appimage.sh [target/release/ferramp] [output-dir]
# Needs: appimagetool (https://github.com/AppImage/appimagetool).
# The AppImage runs on systems with ALSA and glibc >= 2.35.
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
command -v appimagetool >/dev/null 2>&1 || {
    echo "error: appimagetool not found (https://github.com/AppImage/appimagetool)" >&2
    exit 1
}

version=$(grep -m1 '^version' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
arch=$(uname -m)
case "$arch" in
    x86_64) appimage_arch=x86_64 ;;
    aarch64|arm64) appimage_arch=aarch64 ;;
    *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
esac

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT INT TERM
appdir="$workdir/Ferramp.AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/applications" "$appdir/usr/share/icons/hicolor/scalable/apps"

install -m755 "$bin" "$appdir/usr/bin/ferramp"
install -m644 "$root/assets/ferramp.desktop" "$appdir/usr/share/applications/ferramp.desktop"
install -m644 "$root/assets/ferramp.desktop" "$appdir/ferramp.desktop"
install -m644 "$root/assets/icons/ferramp-256.png" "$appdir/ferramp.png"
install -m644 "$root/assets/icons/ferramp-256.png" \
    "$appdir/usr/share/icons/hicolor/scalable/apps/ferramp.png"
for size in 16 32 48 64 128 256; do
    dir="$appdir/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    install -m644 "$root/assets/icons/ferramp-$size.png" "$dir/ferramp.png"
done

# AppRun launches the bundled binary. Using an absolute path inside the
# AppDir keeps it working when the AppImage is executed directly.
cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/ferramp" "$@"
EOF
chmod +x "$appdir/AppRun"

mkdir -p "$outdir"
# appimagetool derives the file name from $VERSION / $ARCH when set.
VERSION="$version" ARCH="$appimage_arch" appimagetool "$appdir" \
    "$outdir/Ferramp-${version}-linux-${arch}.AppImage"
echo "Built $outdir/Ferramp-${version}-linux-${arch}.AppImage"
