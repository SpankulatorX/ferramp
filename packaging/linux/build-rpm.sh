#!/bin/sh
# Builds an .rpm package from an already-compiled release binary.
# Usage: packaging/linux/build-rpm.sh [target/release/ferramp] [output-dir]
# Needs: rpmbuild. Version is read from Cargo.toml.
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
command -v rpmbuild >/dev/null 2>&1 || {
    echo "error: rpmbuild not found" >&2; exit 1
}

version=$(grep -m1 '^version' "$root/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
arch=$(uname -m)
case "$arch" in
    x86_64) rpmarch=x86_64 ;;
    aarch64|arm64) rpmarch=aarch64 ;;
    *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
esac

topdir=$(mktemp -d)
trap 'rm -rf "$topdir"' EXIT INT TERM
mkdir -p "$topdir/BUILD" "$topdir/RPMS" "$topdir/SOURCES" "$topdir/SPECS" "$topdir/SRPMS"

# Stage the payload as SOURCES; the spec installs from %{_sourcedir}.
cp "$bin" "$topdir/SOURCES/ferramp"
cp "$root/assets/ferramp.desktop" "$topdir/SOURCES/ferramp.desktop"
mkdir -p "$topdir/SOURCES/icons"
cp "$root"/assets/icons/ferramp-*.png "$topdir/SOURCES/icons/"
cp "$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/NOTICE" "$topdir/SOURCES/"
sed "s/@VERSION@/$version/g" "$root/packaging/linux/ferramp.spec" > "$topdir/SPECS/ferramp.spec"

rpmbuild --define "_topdir $topdir" -bb "$topdir/SPECS/ferramp.spec"

mkdir -p "$outdir"
rpm=$(find "$topdir/RPMS" -name '*.rpm' | head -n1)
mv "$rpm" "$outdir/ferramp-${version}-linux-${arch}.rpm"
echo "Built $outdir/ferramp-${version}-linux-${arch}.rpm"
