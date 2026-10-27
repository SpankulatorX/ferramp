# Ferramp

A cross-platform audio player in Rust that recreates the look and feel
of the classic Winamp 2 player: main window, graphical equalizer and
playlist editor, docked together, pixel for pixel. It
runs on Linux, Windows and macOS. Every line of code and every pixel of
the built-in skin was written from scratch, and it is free software
(MIT or Apache-2.0).

Load any classic Winamp 2 `.wsz` skin, including the original base skin
if you have it, and the windows look exactly as they did in the original.

![Ferramp with its built-in skin: main window, equalizer and playlist editor](docs/screenshot.png)

## What it does

- Main window with time display, scrolling song title, bitrate and
  sample rate, mono/stereo, spectrum analyser and oscilloscope, volume,
  balance, seek bar, transport buttons, shuffle, repeat and the clutter
  bar (options, always on top, file info, double size, visualisation).
- Window-shade mode for all three windows, double size, EasyMove,
  always on top.
- 10-band equalizer with preamp, the response graph, presets
  (built-in library, auto-load presets per file, EQF import and export).
- Playlist editor: resizable in steps, the ADD, REM, SEL, MISC and LIST
  button menus, multiple selection, drag to reorder, sorting, M3U, M3U8
  and PLS, jump to file, bookmarks, HTML export.
- Windows dock to each other and snap to screen edges; moving the main
  window moves the docked group.
- MP3, MP2, Ogg Vorbis, FLAC, WAV, AIFF, AAC/M4A, ALAC, CAF and Matroska
  audio (Symphonia), internet radio over HTTP(S) with stream titles.
- Classic skins: `.wsz`/`.zip` or folders, `viscolor.txt`,
  `pledit.txt`, `region.txt` (shaped windows) and skin cursors.
- The classic menus, keyboard shortcuts and tooltips.

## Download

Ready-made builds for Windows, macOS (universal) and Linux (x86_64 and
ARM64) are on the [releases page](https://github.com/SpankulatorX/ferramp/releases/latest).
None of them is code-signed: on Windows SmartScreen may ask you to
confirm, and on macOS you right-click the app and choose Open the first
time.

One-liner (Linux: picks the .deb, .rpm, AUR or tarball for you):

```sh
curl -fsSL https://raw.githubusercontent.com/SpankulatorX/ferramp/main/install.sh | sh
```

| Distro | Install |
| --- | --- |
| Debian / Ubuntu | Download `ferramp-<version>-linux-amd64.deb` (ARM64: `-arm64.deb`) and `sudo apt install ./ferramp-*.deb` |
| Fedora / RHEL / openSUSE | Download `ferramp-<version>-linux-x86_64.rpm` (ARM64: `-aarch64.rpm`) and `sudo dnf install ./ferramp-*.rpm` |
| Arch | `yay -S ferramp-bin` (or `ferramp` to build from source) — see `packaging/aur/` |
| Anything else | Download `Ferramp-<version>-linux-<arch>.AppImage`, `chmod +x` it and run it — or unpack the `.tar.gz` and run `./install.sh` for a `~/.local` install with a menu entry |

Linux needs ALSA (or PipeWire/Pulse with the ALSA plug-in), an X11 or
XWayland display and glibc 2.35 or newer (Ubuntu 22.04, Debian 12,
Fedora 36 or later).

## Building

```sh
cargo build --release
./target/release/ferramp [files, folders, playlists or URLs]
```

Linux needs the ALSA development files (`alsa-lib`) and an X11 or
XWayland display. On Wayland desktops Ferramp uses XWayland, because the
docked borderless windows must be positioned freely; `FERRAMP_WAYLAND=1`
selects native Wayland, where the window manager places the windows.

To cross-check a build without a C compiler for the target, disable the
TLS feature: `cargo check --no-default-features --target x86_64-pc-windows-gnu`.

## Skins

Put `.wsz` files in the skins folder (`~/.config/ferramp/Skins` on Linux,
`%APPDATA%\ferramp\Skins` on Windows, `~/Library/Application Support/ferramp/Skins`
on macOS) or use *Skins > Skin Browser* (Alt+S). The browser also lists
the Skins folder of an existing classic player installation (Windows or
Wine). Missing bitmaps fall back to the built-in skin, the way classic
skins always worked.

## Keyboard

| Key | Action |
| --- | --- |
| Z X C V B | Previous, Play, Pause, Stop, Next |
| Shift+V, Ctrl+V | Stop with fade-out, stop after current |
| Left, Right | Back / forward 5 seconds |
| Up, Down | Volume |
| L, Ctrl+L, Shift+L | Open file, location, folder |
| J, Ctrl+J | Jump to file, jump to time |
| R, S | Repeat, shuffle |
| Ctrl+T, Ctrl+D, Ctrl+A, Ctrl+E | Time mode, double size, always on top, EasyMove |
| Alt+W, Alt+E, Alt+G | Main window, playlist editor, equalizer |
| Alt+3, Alt+S, Ctrl+P | File info, skin browser, preferences |
| Alt+I, Ctrl+Alt+I | Add bookmark, edit bookmarks |
| Numpad 0-9 | The classic numpad bindings |

## Options

`ferramp --render DIR [--skin FILE] [files]` renders every window to PNG
files without opening a window. `FERRAMP_SCALE=2` doubles everything on
very high-resolution screens. Settings are kept in `ferramp.ini` in the
configuration folder (`FERRAMP_CONFIG_DIR` overrides it).

## Limitations

- Opus, WavPack, tracker modules and MIDI cannot be played (Symphonia has
  no decoder for them).
- The equalizer graph follows the original curve closely but not to the
  last pixel.
- Native Wayland mode cannot dock or snap windows, since Wayland does not
  let applications position their own windows.
- The media library, minibrowser, video window and input/output plug-ins
  are not part of this recreation.

## Licence

MIT or Apache-2.0, at your option. See `NOTICE` for third-party credits.

Ferramp is an independent project. It is not affiliated with or endorsed
by Winamp or its owners, and it contains none of their code or artwork.
"Winamp" is a trademark of its respective owner and is named here only
to say which skins Ferramp can load.
