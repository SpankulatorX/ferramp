#!/bin/sh
# Ferramp one-liner installer.
#
#   curl -fsSL https://raw.githubusercontent.com/SpankulatorX/ferramp/main/install.sh | sh
#
# Picks the right artifact for your system:
#   Debian/Ubuntu  -> .deb       Fedora/RHEL/openSUSE -> .rpm
#   Arch (yay/paru)-> AUR        anything else       -> tarball (~/.local) or AppImage
#
# Env overrides: TAG (default: latest), PREFIX (tarball/AppImage install dir),
# DESTDIR for AppImage file name, --appimage to force the AppImage.
set -eu

REPO=SpankulatorX/ferramp
TAG=${TAG:-latest}
PREFIX=${PREFIX:-"$HOME/.local"}

force_appimage=0
for a in "$@"; do
    case "$a" in
        --appimage) force_appimage=1 ;;
        -h|--help) echo "Usage: install.sh [--appimage]"; exit 0 ;;
    esac
done

arch=$(uname -m)
case "$arch" in
    x86_64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) echo "error: unsupported architecture: $arch" >&2; exit 1 ;;
esac
debarch=$arch; [ "$arch" = "x86_64" ] && debarch=amd64; [ "$arch" = "aarch64" ] && debarch=arm64

have() { command -v "$1" >/dev/null 2>&1; }

# Resolve "latest" to a tag like v0.1.0 via the GitHub API (no jq needed).
resolve_tag() {
    if [ "$TAG" != "latest" ]; then echo "$TAG"; return; fi
    if have curl; then
        tag=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/$REPO/releases/latest" 2>/dev/null | sed 's#.*/##')
        [ -n "$tag" ] && { echo "$tag"; return; }
    fi
    echo "latest"
}

download() {
    url=$1; out=$2
    if have curl; then curl -fsSL -o "$out" "$url"
    elif have wget; then wget -qO "$out" "$url"
    else echo "error: need curl or wget" >&2; exit 1; fi
}

tag=$(resolve_tag)
if [ "$tag" = "latest" ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/$tag"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

install_tarball() {
    echo "Installing Ferramp $tag ($arch) to $PREFIX ..."
    download "$base/ferramp-${tag}-linux-${arch}.tar.gz" "$tmp/ferramp.tar.gz"
    tar -xzf "$tmp/ferramp.tar.gz" -C "$tmp"
    # The tarball has a top-level dir with install.sh; reuse it.
    PREFIX="$PREFIX" sh "$tmp"/ferramp-*/install.sh
}

install_appimage() {
    echo "Installing Ferramp AppImage $tag ($arch) ..."
    mkdir -p "$PREFIX/bin"
    download "$base/Ferramp-${tag#v}-linux-${arch}.AppImage" "$PREFIX/bin/Ferramp.AppImage"
    chmod +x "$PREFIX/bin/Ferramp.AppImage"
    echo "Installed $PREFIX/bin/Ferramp.AppImage"
    echo "Run it with: $PREFIX/bin/Ferramp.AppImage"
}

# Explicit opt-out to the portable AppImage.
if [ "$force_appimage" = "1" ]; then install_appimage; exit 0; fi

# Native system packages where we can use them.
if [ -f /etc/debian_version ] && have apt-get; then
    echo "Detected Debian/Ubuntu: installing the .deb ..."
    download "$base/ferramp-${tag#v}-linux-${debarch}.deb" "$tmp/ferramp.deb"
    if [ "$(id -u)" = "0" ]; then apt-get install -y "$tmp/ferramp.deb"
    else sudo apt-get install -y "$tmp/ferramp.deb"; fi
    exit 0
fi
if have dnf || have yum || have zypper; then
    echo "Detected RPM distro: installing the .rpm ..."
    download "$base/ferramp-${tag#v}-linux-${arch}.rpm" "$tmp/ferramp.rpm"
    if have dnf; then
        if [ "$(id -u)" = "0" ]; then dnf install -y "$tmp/ferramp.rpm"
        else sudo dnf install -y "$tmp/ferramp.rpm"; fi
    elif have yum; then
        if [ "$(id -u)" = "0" ]; then yum install -y "$tmp/ferramp.rpm"
        else sudo yum install -y "$tmp/ferramp.rpm"; fi
    else
        if [ "$(id -u)" = "0" ]; then zypper --non-interactive install "$tmp/ferramp.rpm"
        else sudo zypper --non-interactive install "$tmp/ferramp.rpm"; fi
    fi
    exit 0
fi
if have pacman && (have yay || have paru); then
    echo "Detected Arch: installing from the AUR ..."
    if have yay; then yay -S --noconfirm ferramp-bin
    else paru -S --noconfirm ferramp-bin; fi
    exit 0
fi

# Fallback: user-space tarball install (works everywhere with glibc >= 2.35).
install_tarball
