# AUR packages

These are the build files for two Arch User Repository packages. They
are not published on the AUR yet; until they are, build them locally
with `makepkg -si` as shown below.

| Package | Directory | What it does |
| --- | --- | --- |
| `ferramp` | `ferramp/` | Builds from source with `cargo build --release` |
| `ferramp-bin` | `ferramp-bin/` | Installs the prebuilt binary from the GitHub release tarball |

Install from this directory:

```sh
cd packaging/aur/ferramp-bin
makepkg -si
```

## Publishing to the AUR

Each package lives in its own AUR repository (`aur.archlinux.org/ferramp.git`
and `aur.archlinux.org/ferramp-bin.git`). To publish a new release:

```sh
cd packaging/aur/ferramp
# bump pkgver, then refresh checksums with the release tarballs:
makepkg --printsrcinfo > .SRCINFO
git -C /path/to/aur-ferramp-clone commit -a -m "v0.1.0" && git -C /path/to/aur-ferramp-clone push
```

For each release, update `pkgver` in both `PKGBUILD` files, then refresh
the checksums of the `v<version>` source tarball (for `ferramp`) and the
per-architecture release tarballs (for `ferramp-bin`, also listed in the
release's SHA256SUMS.txt) before pushing:

```sh
makepkg -g   # prints sha256sums to paste into PKGBUILD
makepkg --printsrcinfo > .SRCINFO
```
