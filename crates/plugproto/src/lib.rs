//! Wire protocol between Ferramp and `ferramp-plughost`, the process that
//! loads classic Windows player plugins (in_, out_, dsp_, vis_ and gen_
//! DLLs). Frames go over the host's stdin/stdout:
//!
//! `u32 length | u32 id | payload`, little-endian, where `length` counts the
//! id and the payload. Requests carry a non-zero id that the reply repeats;
//! events and fire-and-forget messages use id 0. Every integer has a fixed
//! width, so a 64-bit player and a 32-bit host agree on the layout.

use std::io::{self, Read, Write};

pub const PROTOCOL: u32 = 1;
/// Largest frame either side accepts.
pub const MAX_FRAME: u32 = 32 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Input,
    Output,
    Dsp,
    Vis,
    General,
}

impl Kind {
    /// The kind a plugin file name announces (`in_x.dll`, `vis_y.dll`...).
    pub fn from_file_name(name: &str) -> Option<Kind> {
        let n = name.to_ascii_lowercase();
        if !n.ends_with(".dll") {
            return None;
        }
        [
            ("in_", Kind::Input),
            ("out_", Kind::Output),
            ("dsp_", Kind::Dsp),
            ("vis_", Kind::Vis),
            ("gen_", Kind::General),
        ]
        .into_iter()
        .find(|(p, _)| n.starts_with(p))
        .map(|(_, k)| k)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Input => "Input",
            Kind::Output => "Output",
            Kind::Dsp => "DSP/Effect",
            Kind::Vis => "Visualization",
            Kind::General => "General Purpose",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PluginInfo {
    pub id: u32,
    /// File name, e.g. `in_vgmstream.dll`.
    pub file: String,
    pub kind: Option<Kind>,
    pub description: String,
    /// DSP and visualization plugins hold several modules.
    pub modules: Vec<String>,
    /// Input plugins: (`"vgm;vgz"`, `"VGM files"`) pairs.
    pub extensions: Vec<(String, String)>,
    pub seekable: bool,
    /// Input plugin exports the pull decoder (`winampGetExtendedRead_*`).
    pub extended_read: bool,
    pub unicode: bool,
    /// Why the plugin could not be loaded, if it failed.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct FileInfo {
    pub title: String,
    /// -1 when unknown.
    pub length_ms: i32,
    /// Extended fields (`artist`, `album`, `year`...) the plugin reported.
    pub tags: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct StreamFormat {
    pub rate: u32,
    pub channels: u32,
    /// 8, 16, 24 or 32 bits per sample, signed integer PCM.
    pub bits: u32,
    /// -1 when unknown.
    pub length_ms: i32,
    pub seekable: bool,
    pub kbps: u32,
}

/// What general-purpose plugins can ask the player about.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PlayerState {
    /// 0 stopped, 1 playing, 3 paused (the classic codes).
    pub status: u32,
    pub position_ms: u32,
    pub length_ms: i32,
    /// 0..=255.
    pub volume: u32,
    /// -127..=127.
    pub pan: i32,
    pub playlist_pos: i32,
    pub shuffle: bool,
    pub repeat: bool,
    pub rate: u32,
    pub kbps: u32,
    pub channels: u32,
    /// 10 bands then preamp, 0..=63 with 31 = 0 dB, then enabled (0/1).
    pub eq: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PlEntry {
    pub file: String,
    pub title: String,
    /// Seconds, -1 when unknown.
    pub length_s: i32,
}

/// Things plugins ask the player to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    FadeStop,
    SeekMs(u32),
    Forward5,
    Rewind5,
    Volume(u32),
    VolumeUp,
    VolumeDown,
    Pan(i32),
    PlaylistPos(i32),
    StartPlay,
    ClearPlaylist,
    Enqueue(String),
    PlayFile(String),
    Shuffle(bool),
    Repeat(bool),
    Eq(Vec<u8>),
    OpenFiles,
    Preferences,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToHost {
    Hello {
        protocol: u32,
        /// Where plugins keep their settings (a host-side path).
        ini_dir: String,
        /// Folders to load plugins from, host-side paths.
        plugin_dirs: Vec<String>,
    },
    Scan,
    Configure {
        plugin: u32,
        module: u32,
    },
    About {
        plugin: u32,
        module: u32,
    },
    IsOurFile {
        path: String,
    },
    FileInfo {
        plugin: u32,
        path: String,
    },
    InfoBox {
        plugin: u32,
        path: String,
    },
    /// Start decoding; `credit` bytes may be sent before the next Credit.
    Open {
        stream: u32,
        plugin: u32,
        path: String,
        credit: u32,
    },
    Credit {
        stream: u32,
        bytes: u32,
    },
    Seek {
        stream: u32,
        ms: u32,
    },
    Close {
        stream: u32,
    },
    /// How far the listener has got, for the plugin's own clock.
    Played {
        stream: u32,
        ms: u32,
    },
    DspStart {
        plugin: u32,
        module: u32,
    },
    DspProcess {
        rate: u32,
        channels: u32,
        samples: Vec<i16>,
    },
    DspStop,
    VisStart {
        plugin: u32,
        module: u32,
    },
    VisStop,
    VisPcm {
        rate: u32,
        channels: u32,
        samples: Vec<i16>,
    },
    State(PlayerState),
    Playlist(Vec<PlEntry>),
    OutOpen {
        plugin: u32,
        rate: u32,
        channels: u32,
        bits: u32,
    },
    OutWrite(Vec<u8>),
    OutQuery,
    OutPause(bool),
    OutFlush(u32),
    OutVolume(i32),
    OutClose,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FromHost {
    HelloOk {
        protocol: u32,
        arch: String,
        version: String,
    },
    Plugins(Vec<PluginInfo>),
    Done,
    Failed(String),
    Plugin(Option<u32>),
    Info(FileInfo),
    DspOut(Vec<i16>),
    OutOpened {
        latency_ms: i32,
    },
    OutStatus {
        can_write: u32,
        output_ms: u32,
        written_ms: u32,
        playing: bool,
    },
    Opened {
        stream: u32,
        format: StreamFormat,
    },
    Pcm {
        stream: u32,
        data: Vec<u8>,
    },
    Flushed {
        stream: u32,
        ms: u32,
    },
    Eof {
        stream: u32,
    },
    StreamFailed {
        stream: u32,
        msg: String,
    },
    StreamInfo {
        stream: u32,
        kbps: u32,
        rate: u32,
        channels: u32,
    },
    VisClosed,
    Command(Command),
    Log(String),
}

// ----------------------------------------------------------------------
// Encoding

#[derive(Debug)]
pub struct DecodeError(pub &'static str);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad message: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.buf.len() < n {
            return Err(DecodeError("truncated"));
        }
        let (a, b) = self.buf.split_at(n);
        self.buf = b;
        Ok(a)
    }
}

pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);
    fn get(r: &mut Reader) -> Result<Self, DecodeError>;
}

macro_rules! wire_int {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn get(r: &mut Reader) -> Result<Self, DecodeError> {
                let b = r.take(std::mem::size_of::<$t>())?;
                Ok(<$t>::from_le_bytes(b.try_into().unwrap()))
            }
        }
    )*};
}
wire_int!(u8, u16, u32, i16, i32, u64);

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn get(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(u8::get(r)? != 0)
    }
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u32).put(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn get(r: &mut Reader) -> Result<Self, DecodeError> {
        let n = u32::get(r)? as usize;
        let b = r.take(n)?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u32).put(out);
        for v in self {
            v.put(out);
        }
    }
    fn get(r: &mut Reader) -> Result<Self, DecodeError> {
        let n = u32::get(r)? as usize;
        // Every element takes at least one byte; refuse impossible counts.
        if n > r.buf.len() {
            return Err(DecodeError("bad length"));
        }
        (0..n).map(|_| T::get(r)).collect()
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Some(v) => {
                out.push(1);
                v.put(out);
            }
            None => out.push(0),
        }
    }
    fn get(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(match u8::get(r)? {
            0 => None,
            _ => Some(T::get(r)?),
        })
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }
    fn get(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

/// Implements `Wire` for a struct by listing its fields in order.
macro_rules! wire_struct {
    ($t:ident { $($f:ident),* $(,)? }) => {
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                $(self.$f.put(out);)*
            }
            fn get(r: &mut Reader) -> Result<Self, DecodeError> {
                Ok($t { $($f: Wire::get(r)?),* })
            }
        }
    };
}

impl Wire for Kind {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(*self as u8);
    }
    fn get(r: &mut Reader) -> Result<Self, DecodeError> {
        Ok(match u8::get(r)? {
            0 => Kind::Input,
            1 => Kind::Output,
            2 => Kind::Dsp,
            3 => Kind::Vis,
            4 => Kind::General,
            _ => return Err(DecodeError("bad kind")),
        })
    }
}

wire_struct!(PluginInfo {
    id,
    file,
    kind,
    description,
    modules,
    extensions,
    seekable,
    extended_read,
    unicode,
    error,
});
wire_struct!(FileInfo {
    title,
    length_ms,
    tags
});
wire_struct!(StreamFormat {
    rate,
    channels,
    bits,
    length_ms,
    seekable,
    kbps,
});
wire_struct!(PlayerState {
    status,
    position_ms,
    length_ms,
    volume,
    pan,
    playlist_pos,
    shuffle,
    repeat,
    rate,
    kbps,
    channels,
    eq,
});
wire_struct!(PlEntry {
    file,
    title,
    length_s
});

/// Implements `Wire` for an enum: one tag byte, then the variant's fields.
macro_rules! wire_enum {
    ($t:ident { $($tag:literal => $v:ident $({ $($f:ident),* })? $(( $($p:ident),* ))?),* $(,)? }) => {
        impl Wire for $t {
            fn put(&self, out: &mut Vec<u8>) {
                match self {
                    $(
                        $t::$v $({ $($f),* })? $(( $($p),* ))? => {
                            out.push($tag);
                            $($($f.put(out);)*)?
                            $($($p.put(out);)*)?
                        }
                    )*
                }
            }
            fn get(r: &mut Reader) -> Result<Self, DecodeError> {
                Ok(match u8::get(r)? {
                    $(
                        $tag => $t::$v $({ $($f: Wire::get(r)?),* })? $(( $({ let $p = Wire::get(r)?; $p }),* ))?,
                    )*
                    _ => return Err(DecodeError(concat!("bad ", stringify!($t), " tag"))),
                })
            }
        }
    };
}

wire_enum!(Command {
    0 => Play,
    1 => Pause,
    2 => Stop,
    3 => Next,
    4 => Prev,
    5 => FadeStop,
    6 => SeekMs(a),
    7 => Forward5,
    8 => Rewind5,
    9 => Volume(a),
    10 => VolumeUp,
    11 => VolumeDown,
    12 => Pan(a),
    13 => PlaylistPos(a),
    14 => StartPlay,
    15 => ClearPlaylist,
    16 => Enqueue(a),
    17 => PlayFile(a),
    18 => Shuffle(a),
    19 => Repeat(a),
    20 => Eq(a),
    21 => OpenFiles,
    22 => Preferences,
    23 => Quit,
});

wire_enum!(ToHost {
    0 => Hello { protocol, ini_dir, plugin_dirs },
    1 => Scan,
    2 => Configure { plugin, module },
    3 => About { plugin, module },
    4 => IsOurFile { path },
    5 => FileInfo { plugin, path },
    6 => InfoBox { plugin, path },
    7 => Open { stream, plugin, path, credit },
    8 => Credit { stream, bytes },
    9 => Seek { stream, ms },
    10 => Close { stream },
    11 => Played { stream, ms },
    12 => DspStart { plugin, module },
    13 => DspProcess { rate, channels, samples },
    14 => DspStop,
    15 => VisStart { plugin, module },
    16 => VisStop,
    17 => VisPcm { rate, channels, samples },
    18 => State(a),
    19 => Playlist(a),
    20 => OutOpen { plugin, rate, channels, bits },
    21 => OutWrite(a),
    22 => OutQuery,
    23 => OutPause(a),
    24 => OutFlush(a),
    25 => OutVolume(a),
    26 => OutClose,
    27 => Shutdown,
});

wire_enum!(FromHost {
    0 => HelloOk { protocol, arch, version },
    1 => Plugins(a),
    2 => Done,
    3 => Failed(a),
    4 => Plugin(a),
    5 => Info(a),
    6 => DspOut(a),
    7 => OutOpened { latency_ms },
    8 => OutStatus { can_write, output_ms, written_ms, playing },
    9 => Opened { stream, format },
    10 => Pcm { stream, data },
    11 => Flushed { stream, ms },
    12 => Eof { stream },
    13 => StreamFailed { stream, msg },
    14 => StreamInfo { stream, kbps, rate, channels },
    15 => VisClosed,
    16 => Command(a),
    17 => Log(a),
});

/// Encodes one frame.
pub fn encode<T: Wire>(id: u32, msg: &T) -> Vec<u8> {
    let mut out = vec![0; 8];
    msg.put(&mut out);
    let len = (out.len() - 4) as u32;
    out[0..4].copy_from_slice(&len.to_le_bytes());
    out[4..8].copy_from_slice(&id.to_le_bytes());
    out
}

pub fn write_frame<T: Wire>(w: &mut impl Write, id: u32, msg: &T) -> io::Result<()> {
    w.write_all(&encode(id, msg))?;
    w.flush()
}

/// Reads one frame; `Ok(None)` at a clean end of stream.
pub fn read_frame<T: Wire>(r: &mut impl Read) -> io::Result<Option<(u32, T)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len);
    if !(4..=MAX_FRAME).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad frame length",
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    let id = u32::from_le_bytes(body[0..4].try_into().unwrap());
    let mut rd = Reader::new(&body[4..]);
    let msg = T::get(&mut rd).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some((id, msg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T: Wire + PartialEq + std::fmt::Debug>(m: T) {
        let bytes = encode(7, &m);
        let (id, back): (u32, T) = read_frame(&mut &bytes[..]).unwrap().unwrap();
        assert_eq!(id, 7);
        assert_eq!(back, m);
    }

    #[test]
    fn messages_roundtrip() {
        roundtrip(ToHost::Hello {
            protocol: PROTOCOL,
            ini_dir: "Z:\\home\\x".into(),
            plugin_dirs: vec!["a".into(), "b".into()],
        });
        roundtrip(ToHost::DspProcess {
            rate: 44100,
            channels: 2,
            samples: vec![-32768, 0, 32767],
        });
        roundtrip(ToHost::State(PlayerState {
            status: 1,
            eq: vec![31; 12],
            ..Default::default()
        }));
        roundtrip(ToHost::Shutdown);
        roundtrip(FromHost::Plugins(vec![PluginInfo {
            id: 3,
            file: "in_x.dll".into(),
            kind: Some(Kind::Input),
            extensions: vec![("vgm;vgz".into(), "VGM".into())],
            error: Some("x".into()),
            ..Default::default()
        }]));
        roundtrip(FromHost::Command(Command::Enqueue("a b".into())));
        roundtrip(FromHost::Command(Command::Shuffle(true)));
        roundtrip(FromHost::Opened {
            stream: 1,
            format: StreamFormat {
                rate: 48000,
                channels: 2,
                bits: 16,
                length_ms: -1,
                seekable: true,
                kbps: 0,
            },
        });
    }

    #[test]
    fn rejects_garbage() {
        let mut bad = encode(1, &ToHost::Scan);
        bad[8] = 200;
        assert!(read_frame::<ToHost>(&mut &bad[..]).is_err());
        let huge = [0xFFu8; 8];
        assert!(read_frame::<ToHost>(&mut &huge[..]).is_err());
        // A vector claiming more elements than bytes remain.
        let mut v = encode(
            1,
            &ToHost::DspProcess {
                rate: 1,
                channels: 1,
                samples: vec![],
            },
        );
        let n = v.len();
        v[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame::<ToHost>(&mut &v[..]).is_err());
        assert!(read_frame::<ToHost>(&mut &[][..]).unwrap().is_none());
    }

    #[test]
    fn kinds_from_names() {
        assert_eq!(Kind::from_file_name("IN_VGM.DLL"), Some(Kind::Input));
        assert_eq!(Kind::from_file_name("vis_milk2.dll"), Some(Kind::Vis));
        assert_eq!(Kind::from_file_name("gen_ml.dll"), Some(Kind::General));
        assert_eq!(Kind::from_file_name("in_x.txt"), None);
        assert_eq!(Kind::from_file_name("pmp_usb.dll"), None);
    }
}
