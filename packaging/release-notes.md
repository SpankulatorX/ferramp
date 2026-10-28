Ferramp is a from-scratch recreation of the classic 2.x-era skinnable
desktop audio player, written in Rust. Main window, equalizer and
playlist editor, docked together. Load any classic `.wsz` skin and it
looks the way it always did, pixel for pixel.

## New in 0.2.0

- Classic plug-ins: input (`in_*.dll`), output (`out_*.dll`), DSP
  (`dsp_*.dll`), visualization (`vis_*.dll`) and general purpose
  (`gen_*.dll`) plug-ins from the 2.x and 5.x players load from the
  `Plugins` folder in the configuration folder. *Options > Plug-ins*
  (Ctrl+K) lists them, with Configure, About, Start / Stop and
  Enable / Disable.
- Plug-ins run in a separate helper, `ferramp-plughost`, which ships in
  every download. Windows runs it directly; Linux and macOS run it under
  Wine, so install Wine to use plug-ins. A plug-in that crashes or hangs
  only takes the helper down, and one that fails while loading is left
  out and marked in the list together with the reason.
- Input plug-ins play formats Ferramp has no decoder for (Opus, tracker
  modules, SID, VGM, AdLib and more) and supply playlist titles and
  durations. Items general plug-ins add to the player menu, such as Big
  Clock's show and hide, appear in Ferramp's main menu.
- Tested under Wine on Linux: `in_opus`, `in_openmpt`, `in_vgm`,
  `in_sidplay2`, `in_adlib`, `dsp_bs2b`, `dsp_qat` and `gen_bigclock`,
  plus several more that load. Plug-ins built only for WACUP do not
  load. Real plug-ins have not been tried on Windows or macOS yet; the
  helper itself is tested on Windows and under Wine.
- First start offers the original base skin from the Internet Archive,
  shown next to the built-in skin and checked against a fixed SHA-256.
- *Skins > Get more skins!* searches the Winamp Skin Museum and
  installs a skin with one click.
- Sharp text in dialogs, menus and tooltips on high-DPI screens.

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
  and internet radio with stream titles, plus whatever your input
  plug-ins play.
- Classic skins with `region.txt` shapes, skin cursors, `viscolor.txt`
  and `pledit.txt`.

## Known limitations

- Without plug-ins, no Opus, WavPack, tracker modules or MIDI.
- Plug-ins that need the later player's media library or other parts of
  its internal service layer do not start.
- The equalizer graph is close to the original but not exact.
- Native Wayland mode (`FERRAMP_WAYLAND=1`) cannot dock windows.
- No media library, minibrowser or video window.
