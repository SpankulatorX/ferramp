Name:           ferramp
Version:        @VERSION@
Release:        1%{?dist}
Summary:        Classic skinnable audio player
License:        MIT OR Apache-2.0
URL:            https://github.com/SpankulatorX/ferramp
Source0:        %{name}-%{version}.tar.gz
# Built from an already-compiled release binary; no BuildRequires.

BuildArch:      %{_target_cpu}
Requires:       alsa-lib
Requires:       glibc >= 2.35

%description
Ferramp is a cross-platform audio player in Rust that recreates the
look and feel of the classic Winamp 2 player: main window, graphical
equalizer and playlist editor, docked together. It plays MP3, Ogg
Vorbis, FLAC, WAV, AIFF, AAC/M4A, ALAC and internet radio, and loads
classic .wsz skins.

%install
install -Dm755 %{_sourcedir}/ferramp %{buildroot}%{_bindir}/ferramp
install -Dm644 %{_sourcedir}/ferramp.desktop %{buildroot}%{_datadir}/applications/ferramp.desktop
for size in 16 32 48 64 128 256; do
    install -Dm644 %{_sourcedir}/icons/ferramp-$size.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/ferramp.png
done

%files
%{_bindir}/ferramp
%{_datadir}/applications/ferramp.desktop
%{_datadir}/icons/hicolor/*/apps/ferramp.png

%changelog
* Tue Oct 27 2026 Ferramp contributors - @VERSION@-1
- RPM packaging.
