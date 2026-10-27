# AUR packages

Two Arch User Repository packages are maintained from the files in this
directory:

| Package | Directory | What it does |
| --- | --- | --- |
| `ferramp` | `ferramp/` | Builds from source with `cargo build --release` |
| `ferramp-bin` | `ferramp-bin/` | Installs the prebuilt binary from the GitHub release tarball |

Install with an AUR helper:

```sh
yay -S ferramp        # build from source
yay -S ferramp-bin    # prebuilt binary (faster)
```

or with `paru`:

```sh
paru -S ferramp-bin
```

Manual install:

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

Replace the `SKIP` checksums with the real `sha256sum` of the `v<version>`
source tarball (for `ferramp`) or the per-architecture release tarballs
(for `ferramp-bin`) before pushing:

```sh
makepkg -g   # prints sha256sums to paste into PKGBUILD
makepkg --printsrcinfo > .SRCINFO
```

Remember to update `pkgver` in both `PKGBUILD` files when tagging a release.
