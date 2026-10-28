#!/bin/sh
# Ferramp one-liner installer.
#
#   curl -fsSL https://raw.githubusercontent.com/SpankulatorX/ferramp/main/install.sh | sh
#
# Picks the right artifact for your system:
#   Debian/Ubuntu -> .deb    Fedora/RHEL/openSUSE -> .rpm
#   anything else -> tarball installed into ~/.local
# Every download is checked against the release's SHA256SUMS.txt.
#
# Options: --appimage or --tarball to force that format.
# Environment: TAG (default: latest release), PREFIX (tarball/AppImage
# install dir, default ~/.local).
set -eu

REPO=SpankulatorX/ferramp
TAG=${TAG:-latest}
PREFIX=${PREFIX:-"$HOME/.local"}

force=
for a in "$@"; do
    case "$a" in
        --appimage) force=appimage ;;
        --tarball) force=tarball ;;
        -h|--help) echo "Usage: install.sh [--appimage | --tarball]"; exit 0 ;;
        *) echo "error: unknown option: $a" >&2; exit 1 ;;
    esac
done

arch=$(uname -m)
case "$arch" in
    x86_64) debarch=amd64 ;;
    aarch64|arm64) arch=aarch64; debarch=arm64 ;;
    *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
esac

have() { command -v "$1" >/dev/null 2>&1; }

if have curl; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif have wget; then
    fetch() { wget -qO "$2" "$1"; }
else
    echo "error: need curl or wget" >&2; exit 1
fi

if have sha256sum; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif have shasum; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    echo "error: need sha256sum or shasum to verify the download" >&2; exit 1
fi

as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi
}

# Resolve "latest" to a tag like v0.1.0 by following the release redirect.
tag=$TAG
if [ "$tag" = latest ]; then
    if have curl; then
        tag=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest")
        tag=${tag##*/}
    else
        tag=$(wget -qS --max-redirect=0 -O /dev/null \
            "https://github.com/$REPO/releases/latest" 2>&1 |
            sed -n 's#^ *[Ll]ocation: .*/tag/##p' | tr -d '\r' | tail -n1)
    fi
fi
case "$tag" in
    v[0-9]*) ;;
    *) echo "error: could not find the latest release; set TAG=vX.Y.Z" >&2; exit 1 ;;
esac
version=${tag#v}
base="https://github.com/$REPO/releases/download/$tag"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
fetch "$base/SHA256SUMS.txt" "$tmp/SHA256SUMS.txt"

# Downloads a release file into $tmp and checks it against SHA256SUMS.txt.
get() {
    expected=
    while read -r line_sum line_file; do
        if [ "$line_file" = "$1" ] || [ "$line_file" = "*$1" ]; then expected=$line_sum; fi
    done < "$tmp/SHA256SUMS.txt"
    if [ -z "$expected" ]; then
        echo "error: $1 is not part of release $tag" >&2; exit 1
    fi
    echo "Downloading $1 ..."
    fetch "$base/$1" "$tmp/$1"
    if [ "$(sha256 "$tmp/$1")" != "$expected" ]; then
        echo "error: checksum mismatch for $1" >&2; exit 1
    fi
}

install_tarball() {
    name="ferramp-$tag-linux-$arch"
    get "$name.tar.gz"
    tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
    # The tarball's own install.sh does the ~/.local install.
    PREFIX="$PREFIX" sh "$tmp/$name/install.sh"
}

install_appimage() {
    file="Ferramp-$version-linux-$arch.AppImage"
    get "$file"
    install -Dm755 "$tmp/$file" "$PREFIX/bin/Ferramp.AppImage"
    echo "Installed $PREFIX/bin/Ferramp.AppImage"
}

case "$force" in
    appimage) install_appimage; exit 0 ;;
    tarball) install_tarball; exit 0 ;;
esac

if [ -f /etc/debian_version ] && have apt-get; then
    file="ferramp-$version-linux-$debarch.deb"
    get "$file"
    as_root apt-get install -y "$tmp/$file"
elif have dnf || have yum || have zypper; then
    file="ferramp-$version-linux-$arch.rpm"
    get "$file"
    if have dnf; then as_root dnf install -y "$tmp/$file"
    elif have zypper; then as_root zypper --non-interactive install --allow-unsigned-rpm "$tmp/$file"
    else as_root yum install -y "$tmp/$file"; fi
else
    install_tarball
fi
