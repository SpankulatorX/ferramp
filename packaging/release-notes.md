The first release of Ferramp, a from-scratch recreation of the classic
2.x-era skinnable desktop audio player, written in Rust. Main window,
equalizer and playlist editor, docked together, pixel for pixel. Load
any classic `.wsz` skin and it looks the way it always did.

## Downloads

| Platform | File |
| --- | --- |
| Windows 10/11 (x64) | `ferramp-*-windows-x86_64.zip` |
| macOS 11 or later (Apple silicon and Intel) | `ferramp-*-macos-universal.dmg` |
| Linux x86_64 | `ferramp-*-linux-x86_64.tar.gz`, `ferramp-*-linux-amd64.deb`, `ferramp-*-linux-x86_64.rpm`, `Ferramp-*-linux-x86_64.AppImage` |
| Linux ARM64 | `ferramp-*-linux-aarch64.tar.gz`, `ferramp-*-linux-arm64.deb`, `ferramp-*-linux-aarch64.rpm`, `Ferramp-*-linux-aarch64.AppImage` |

One-liner (picks the right Linux package for you and checks it against
SHA256SUMS.txt):

```sh
curl -fsSL https://raw.githubusercontent.com/SpankulatorX/ferramp/main/install.sh | sh
```

On Arch Linux, `makepkg -si` in `packaging/aur/ferramp-bin` of the
source tree installs the prebuilt binary.

**Windows:** unzip and run `ferramp.exe`. The binary is not code-signed,
so SmartScreen may ask you to confirm (More info, Run anyway).

**macOS:** open the disk image and drag Ferramp to Applications. The app
is not notarized: the first time, right-click it and choose Open, or
allow it under System Settings, Privacy & Security. From a terminal,
`xattr -dr com.apple.quarantine /Applications/Ferramp.app` does the same.

**Linux:** install the `.deb` (`sudo apt install ./ferramp-*.deb`) or
`.rpm` (`sudo dnf install ./ferramp-*.rpm`, or on openSUSE
`sudo zypper install --allow-unsigned-rpm ./ferramp-*.rpm`), run the
`.AppImage`, or
unpack the `.tar.gz` and run `./ferramp`; `./install.sh` installs it
with a menu entry into `~/.local`. It needs ALSA (or PipeWire/Pulse
with the ALSA plug-in), an X11 or XWayland display and glibc 2.35 or
newer (Ubuntu 22.04, Debian 12, Fedora 36 or later).

`SHA256SUMS.txt` lists the checksum of every file.

## What works

- Main window, 10-band equalizer with presets and EQF files, playlist
  editor with the ADD, REM, SEL, MISC and LIST menus, window-shade mode,
  double size, docking and snapping.
- MP3, Ogg Vorbis, FLAC, WAV, AIFF, AAC/M4A, ALAC, CAF, Matroska audio
  and internet radio with stream titles.
- Classic skins with `region.txt` shapes, skin cursors, `viscolor.txt`
  and `pledit.txt`.

## Known limitations

- No Opus, WavPack, tracker modules or MIDI.
- The equalizer graph is close to the original but not exact.
- Native Wayland mode (`FERRAMP_WAYLAND=1`) cannot dock windows.
- No media library, minibrowser, video window or plug-ins.
