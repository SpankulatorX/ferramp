//! Playback engine: a decoder thread per track feeding a ring buffer that
//! the audio device callback drains. Volume, balance and the visualisation
//! tap live in the callback so they respond immediately.

pub mod dsp;
pub mod stream;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use dsp::{EqSettings, Equalizer, Resampler};

pub const VIS_SAMPLES: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Url(String),
}

impl Source {
    pub fn parse(s: &str) -> Source {
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            Source::Url(s.to_string())
        } else if let Some(rest) = s.strip_prefix("file://") {
            Source::File(PathBuf::from(percent_decode(rest)))
        } else {
            Source::File(PathBuf::from(s))
        }
    }
}

pub fn percent_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug)]
pub enum Event {
    /// The track opened; values are what the display shows.
    Opened {
        duration: Option<f64>,
        kbps: u32,
        khz: u32,
        channels: u32,
        stream_name: Option<String>,
    },
    Error(String),
    StreamTitle(String),
}

/// State shared between the UI thread, the decoder and the device callback.
pub struct Shared {
    pub volume: AtomicU32,  // f32 bits, 0..1
    pub balance: AtomicU32, // f32 bits, -1..1
    paused: AtomicBool,
    consumed: AtomicU64,      // frames taken from the ring by the callback
    discard_until: AtomicU64, // frames below this index are dropped (after seek/stop)
    pub kbps: AtomicU32,
    vis: Mutex<VecDeque<f32>>,
    timeline: Mutex<Timeline>,
    eq: Mutex<EqSettings>,
    eq_version: AtomicU64,
    out_rate: AtomicU32,
    /// Generation of the decoder allowed to push audio; older decoders that
    /// are still winding down stop at their next push.
    active: AtomicU64,
    /// Input and DSP plugins, once they are loaded.
    plugins: Mutex<Option<Arc<crate::plugins::Plugins>>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Timeline {
    /// Ring frame index where the current playback position starts.
    base_frame: u64,
    /// Track time at `base_frame`.
    base_time: f64,
    /// Ring frame index at which the track's last frame has been consumed.
    end_frame: Option<u64>,
    generation: u64,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            volume: AtomicU32::new(1.0f32.to_bits()),
            balance: AtomicU32::new(0.0f32.to_bits()),
            paused: AtomicBool::new(false),
            consumed: AtomicU64::new(0),
            discard_until: AtomicU64::new(0),
            kbps: AtomicU32::new(0),
            vis: Mutex::new(VecDeque::from(vec![0.0; VIS_SAMPLES])),
            timeline: Mutex::new(Timeline::default()),
            eq: Mutex::new(EqSettings::default()),
            eq_version: AtomicU64::new(0),
            out_rate: AtomicU32::new(44100),
            active: AtomicU64::new(0),
            plugins: Mutex::new(None),
        }
    }
}

enum Cmd {
    Seek(f64),
    Stop,
}

struct Decoding {
    cmd: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

pub struct Engine {
    pub shared: Arc<Shared>,
    events_tx: Sender<(u64, Event)>,
    events: Receiver<(u64, Event)>,
    producer: Arc<Mutex<Option<rtrb::Producer<[f32; 2]>>>>,
    pushed: Arc<AtomicU64>,
    current: Option<Decoding>,
    source: Option<Source>,
    status: Status,
    generation: u64,
    _stream: Option<cpal::Stream>,
    pub device_error: Option<String>,
    /// Headless rendering: a fixed position reported instead of the device clock.
    simulated_position: Option<f64>,
    quit: Arc<AtomicBool>,
}

const RING_SECONDS: f64 = 0.35;

impl Engine {
    pub fn new() -> Engine {
        Engine::build(std::env::var_os("FERRAMP_NO_AUDIO").is_none())
    }

    /// An engine without an audio device (used for headless rendering).
    pub fn headless() -> Engine {
        Engine::build(false)
    }

    fn build(with_device: bool) -> Engine {
        let shared = Arc::new(Shared::new());
        let (events_tx, events) = mpsc::channel();
        let pushed = Arc::new(AtomicU64::new(0));
        let mut engine = Engine {
            shared,
            events_tx,
            events,
            producer: Arc::new(Mutex::new(None)),
            pushed,
            current: None,
            source: None,
            status: Status::Stopped,
            generation: 0,
            _stream: None,
            device_error: None,
            simulated_position: None,
            quit: Arc::new(AtomicBool::new(false)),
        };
        if !with_device {
            engine.device_error = Some("disabled".into());
            return engine;
        }
        match engine.open_device() {
            Ok(stream) => engine._stream = Some(stream),
            Err(e) if e == "null output" => {}
            Err(e) => engine.device_error = Some(e),
        }
        engine
    }

    fn open_device(&mut self) -> Result<cpal::Stream, String> {
        if std::env::var_os("FERRAMP_NULL_AUDIO").is_some() {
            let rate = 48000;
            self.shared.out_rate.store(rate, Ordering::Relaxed);
            let (prod, cons) =
                rtrb::RingBuffer::<[f32; 2]>::new((rate as f64 * RING_SECONDS) as usize);
            *self.producer.lock().unwrap() = Some(prod);
            start_null_output(rate, cons, self.shared.clone(), self.quit.clone());
            return Err("null output".into());
        }
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no audio output device")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let rate = supported.sample_rate();
        let channels = supported.channels() as usize;
        self.shared.out_rate.store(rate, Ordering::Relaxed);
        let cap = ((rate as f64) * RING_SECONDS) as usize;
        let (prod, cons) = rtrb::RingBuffer::<[f32; 2]>::new(cap.max(1024));
        *self.producer.lock().unwrap() = Some(prod);
        let config: cpal::StreamConfig = supported.config();
        let shared = self.shared.clone();
        let format = supported.sample_format();
        let stream = match format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, channels, cons, shared)
            }
            cpal::SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, channels, cons, shared)
            }
            cpal::SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, channels, cons, shared)
            }
            cpal::SampleFormat::I32 => {
                build_stream::<i32>(&device, &config, channels, cons, shared)
            }
            cpal::SampleFormat::F64 => {
                build_stream::<f64>(&device, &config, channels, cons, shared)
            }
            other => return Err(format!("unsupported sample format {other}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(stream)
    }

    /// Pretend to be in `status` at `position` (headless rendering only).
    pub fn simulate(&mut self, status: Status, position: f64) {
        self.status = status;
        self.simulated_position = Some(position);
    }

    /// Lets the decoder use input and DSP plugins.
    pub fn set_plugins(&self, p: Option<Arc<crate::plugins::Plugins>>) {
        *self.shared.plugins.lock().unwrap() = p;
    }

    pub fn out_rate(&self) -> u32 {
        self.shared.out_rate.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> Status {
        self.status
    }

    pub fn set_volume(&self, v: f32) {
        self.shared
            .volume
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_balance(&self, b: f32) {
        self.shared
            .balance
            .store(b.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn set_eq(&self, eq: EqSettings) {
        *self.shared.eq.lock().unwrap() = eq;
        self.shared.eq_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Next event from the current track; events of earlier tracks are dropped.
    pub fn next_event(&self) -> Option<Event> {
        loop {
            let (generation, ev) = self.events.try_recv().ok()?;
            if generation == self.generation {
                return Some(ev);
            }
        }
    }

    /// Start a new generation: older decoders may no longer push audio, and
    /// whatever they queued is skipped. Returns the first frame of the new one.
    fn new_generation(&mut self) -> u64 {
        // Holding the producer lock orders this against any push in progress.
        let _guard = self.producer.lock().unwrap();
        self.generation += 1;
        self.shared.active.store(self.generation, Ordering::SeqCst);
        let start_frame = self.pushed.load(Ordering::SeqCst);
        self.shared
            .discard_until
            .store(start_frame, Ordering::SeqCst);
        start_frame
    }

    pub fn play(&mut self, source: Source) {
        self.stop_decoder();
        let start_frame = self.new_generation();
        *self.shared.timeline.lock().unwrap() = Timeline {
            base_frame: start_frame,
            base_time: 0.0,
            end_frame: None,
            generation: self.generation,
        };
        self.shared.kbps.store(0, Ordering::Relaxed);
        self.shared.paused.store(false, Ordering::SeqCst);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        self.source = Some(source.clone());
        let ctx = DecodeCtx {
            source,
            shared: self.shared.clone(),
            producer: self.producer.clone(),
            pushed: self.pushed.clone(),
            events: self.events_tx.clone(),
            cmds: cmd_rx,
            generation: self.generation,
        };
        let thread = std::thread::Builder::new()
            .name("decoder".into())
            .spawn(move || decode_thread(ctx))
            .ok();
        self.current = Some(Decoding {
            cmd: cmd_tx,
            thread,
        });
        self.status = Status::Playing;
    }

    pub fn pause(&mut self) {
        match self.status {
            Status::Playing => {
                self.shared.paused.store(true, Ordering::SeqCst);
                self.status = Status::Paused;
            }
            Status::Paused => self.resume(),
            Status::Stopped => {}
        }
    }

    pub fn resume(&mut self) {
        if self.status == Status::Paused {
            self.shared.paused.store(false, Ordering::SeqCst);
            self.status = Status::Playing;
        }
    }

    pub fn stop(&mut self) {
        self.stop_decoder();
        self.new_generation();
        self.shared.paused.store(false, Ordering::SeqCst);
        self.status = Status::Stopped;
        self.shared.kbps.store(0, Ordering::Relaxed);
        let mut vis = self.shared.vis.lock().unwrap();
        vis.iter_mut().for_each(|s| *s = 0.0);
    }

    /// Tell the decoder to stop without waiting for it: it may be blocked on
    /// a stalled network stream. It can no longer push audio once a new
    /// generation starts, and its events are ignored.
    fn stop_decoder(&mut self) {
        if let Some(d) = self.current.take() {
            let _ = d.cmd.send(Cmd::Stop);
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        if self.status == Status::Stopped {
            return;
        }
        // A decoder that has exited (after an error) cannot seek; reopen the track.
        let exited = self
            .current
            .as_ref()
            .is_none_or(|d| d.thread.as_ref().is_none_or(|t| t.is_finished()));
        if exited && let Some(src) = self.source.clone() {
            let paused = self.status == Status::Paused;
            self.play(src);
            if paused {
                self.pause();
            }
        }
        if let Some(d) = &self.current {
            let _ = d.cmd.send(Cmd::Seek(seconds.max(0.0)));
        }
    }

    /// Current playback position in seconds.
    pub fn position(&self) -> f64 {
        if self.status == Status::Stopped {
            return 0.0;
        }
        if let Some(p) = self.simulated_position {
            return p;
        }
        let tl = *self.shared.timeline.lock().unwrap();
        let consumed = self.shared.consumed.load(Ordering::Relaxed);
        let rate = self.out_rate() as f64;
        tl.base_time + consumed.saturating_sub(tl.base_frame) as f64 / rate
    }

    /// Poll for end of track: true once all decoded audio has been played.
    pub fn finished(&self) -> bool {
        if self.status != Status::Playing {
            return false;
        }
        let tl = *self.shared.timeline.lock().unwrap();
        if tl.generation != self.generation {
            return false;
        }
        match tl.end_frame {
            Some(end) => self.shared.consumed.load(Ordering::Relaxed) >= end,
            None => false,
        }
    }

    pub fn mark_stopped(&mut self) {
        self.stop_decoder();
        self.new_generation();
        self.status = Status::Stopped;
    }

    /// True once audio of the current track has actually reached the output.
    pub fn has_played(&self) -> bool {
        let tl = *self.shared.timeline.lock().unwrap();
        tl.generation == self.generation
            && self.shared.consumed.load(Ordering::Relaxed) > tl.base_frame
    }

    /// Latest mono samples for the visualiser, oldest first.
    pub fn vis_samples(&self, out: &mut Vec<f32>) {
        out.clear();
        if let Ok(v) = self.shared.vis.lock() {
            out.extend(v.iter().copied());
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        self.stop_decoder();
    }
}

/// Drains the ring buffer into an output buffer: applies volume and balance
/// and feeds the visualisation tap.
struct Mixer {
    cons: rtrb::Consumer<[f32; 2]>,
    shared: Arc<Shared>,
    tap: Vec<f32>,
}

impl Mixer {
    fn render<T: cpal::Sample + cpal::FromSample<f32>>(&mut self, data: &mut [T], channels: usize) {
        let shared = &self.shared;
        let vol = f32::from_bits(shared.volume.load(Ordering::Relaxed));
        let bal = f32::from_bits(shared.balance.load(Ordering::Relaxed));
        let gl = vol * (1.0 - bal.max(0.0));
        let gr = vol * (1.0 + bal.min(0.0));
        let paused = shared.paused.load(Ordering::Relaxed);
        let discard_until = shared.discard_until.load(Ordering::Relaxed);
        let mut consumed = shared.consumed.load(Ordering::Relaxed);
        // Skip stale audio queued before a seek or track change.
        while consumed < discard_until {
            if self.cons.pop().is_err() {
                break;
            }
            consumed += 1;
        }
        self.tap.clear();
        for frame in data.chunks_mut(channels.max(1)) {
            let s = if paused || consumed < discard_until {
                [0.0, 0.0]
            } else {
                match self.cons.pop() {
                    Ok(s) => {
                        consumed += 1;
                        self.tap.push((s[0] + s[1]) * 0.5);
                        s
                    }
                    Err(_) => [0.0, 0.0],
                }
            };
            let l = (s[0] * gl).clamp(-1.0, 1.0);
            let r = (s[1] * gr).clamp(-1.0, 1.0);
            match frame.len() {
                1 => frame[0] = T::from_sample((l + r) * 0.5),
                _ => {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                    for x in frame.iter_mut().skip(2) {
                        *x = T::from_sample(0.0);
                    }
                }
            }
        }
        shared.consumed.store(consumed, Ordering::Relaxed);
        if !self.tap.is_empty()
            && let Ok(mut vis) = shared.vis.try_lock()
        {
            for &s in &self.tap {
                vis.pop_front();
                vis.push_back(s);
            }
        }
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    cons: rtrb::Consumer<[f32; 2]>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let mut mixer = Mixer {
        cons,
        shared,
        tap: Vec::with_capacity(8192),
    };
    device
        .build_output_stream(
            *config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| mixer.render(data, channels),
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// A silent output that consumes audio in real time (FERRAMP_NULL_AUDIO=1),
/// used for automated tests without sound.
fn start_null_output(
    rate: u32,
    cons: rtrb::Consumer<[f32; 2]>,
    shared: Arc<Shared>,
    quit: Arc<AtomicBool>,
) {
    let mut mixer = Mixer {
        cons,
        shared,
        tap: Vec::with_capacity(8192),
    };
    let _ = std::thread::Builder::new()
        .name("null-audio".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            let mut done: u64 = 0;
            let mut buf = vec![0f32; 2 * 4096];
            while !quit.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
                let due = (start.elapsed().as_secs_f64() * rate as f64) as u64;
                let n = (due - done).min(4096) as usize;
                mixer.render(&mut buf[..n * 2], 2);
                done += n as u64;
            }
        });
}

struct DecodeCtx {
    source: Source,
    shared: Arc<Shared>,
    producer: Arc<Mutex<Option<rtrb::Producer<[f32; 2]>>>>,
    pushed: Arc<AtomicU64>,
    events: Sender<(u64, Event)>,
    cmds: Receiver<Cmd>,
    generation: u64,
}

impl DecodeCtx {
    fn send(&self, ev: Event) {
        let _ = self.events.send((self.generation, ev));
    }
}

fn decode_thread(ctx: DecodeCtx) {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&ctx))) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => ctx.send(Event::Error(e)),
        Err(_) => ctx.send(Event::Error("decoder error".into())),
    }
}

/// What a decoder reports when a track opens.
#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub rate: u32,
    pub channels: u32,
    pub duration: Option<f64>,
    pub kbps: u32,
    pub stream_name: Option<String>,
}

/// A source of decoded audio for the decoder thread: the built-in decoder
/// or an input plugin.
pub trait Track {
    fn info(&self) -> &TrackInfo;
    /// Appends decoded stereo frames (possibly none, to let the caller look
    /// at its commands); `Ok(false)` at the end of the track.
    fn read(&mut self, frames: &mut Vec<[f32; 2]>) -> Result<bool, String>;
    /// Seeks and returns the time reached, or `None` if it cannot.
    fn seek(&mut self, secs: f64) -> Option<f64>;
    /// The current bitrate, for the variable-bitrate display.
    fn kbps(&self) -> Option<u32> {
        None
    }
    /// A new stream title (internet radio).
    fn title(&mut self) -> Option<String> {
        None
    }
}

/// The built-in decoder (Symphonia).
struct SymTrack {
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track_id: u32,
    tb: Option<symphonia::core::units::TimeBase>,
    info: TrackInfo,
    samples: Vec<f32>,
    bits_window: VecDeque<(f64, usize)>,
    vbr_capable: bool,
    kbps: Option<u32>,
    titles: Receiver<String>,
}

impl SymTrack {
    fn open(source: &Source) -> Result<SymTrack, String> {
        let (title_tx, title_rx) = mpsc::channel::<String>();
        let mut hint = Hint::new();
        let mut stream_name = None;
        let mut stream_kbps = None;
        let mss = match source {
            Source::File(path) => {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    hint.with_extension(ext);
                }
                let f =
                    std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
                MediaSourceStream::new(Box::new(f), Default::default())
            }
            Source::Url(url) => {
                let opened = stream::open(url, title_tx)?;
                if let Some(ct) = &opened.content_type {
                    hint.mime_type(ct);
                    if ct.contains("mpeg") {
                        hint.with_extension("mp3");
                    } else if ct.contains("aac") {
                        hint.with_extension("aac");
                    } else if ct.contains("ogg") {
                        hint.with_extension("ogg");
                    }
                }
                stream_name = opened.name;
                stream_kbps = opened.bitrate;
                MediaSourceStream::new(
                    Box::new(ReadOnlySource::new(opened.reader)),
                    Default::default(),
                )
            }
        };
        let format = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .map_err(|e| format!("unsupported or unreadable file ({e})"))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?
            .clone();
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or("no audio parameters")?
            .clone();
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())
            .map_err(|e| format!("unsupported codec ({e})"))?;
        let rate = params.sample_rate.unwrap_or(44100);
        let channels = params.channels.as_ref().map(|c| c.count()).unwrap_or(2) as u32;
        let duration = match (track.time_base, track.duration) {
            (Some(tb), Some(d)) => tb.calc_duration(d).map(|t| t.as_secs_f64()),
            _ => match track.num_frames {
                Some(n) if rate > 0 => Some(n as f64 / rate as f64),
                _ => None,
            },
        };
        let file_kbps = match (source, duration) {
            (Source::File(p), Some(d)) if d > 0.0 => std::fs::metadata(p)
                .ok()
                .map(|m| ((m.len() as f64 * 8.0) / d / 1000.0).round() as u32),
            _ => None,
        };
        Ok(SymTrack {
            format,
            decoder,
            track_id: track.id,
            tb: track.time_base,
            info: TrackInfo {
                rate,
                channels,
                duration,
                kbps: stream_kbps.or(file_kbps).unwrap_or(0),
                stream_name,
            },
            samples: Vec::new(),
            bits_window: VecDeque::new(),
            vbr_capable: matches!(source, Source::File(_)),
            kbps: None,
            titles: title_rx,
        })
    }
}

impl Track for SymTrack {
    fn info(&self) -> &TrackInfo {
        &self.info
    }

    fn read(&mut self, frames: &mut Vec<[f32; 2]>) -> Result<bool, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(false),
                Err(SymError::ResetRequired) => return Ok(false),
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(false);
                }
                Err(e) => return Err(format!("read error: {e}")),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let pkt_bytes = packet.data.len();
            let pkt_secs = self
                .tb
                .and_then(|tb| tb.calc_duration(packet.dur))
                .map(|t| t.as_secs_f64())
                .unwrap_or(0.0);
            let buf = match self.decoder.decode(&packet) {
                Ok(b) => b,
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(format!("decode error: {e}")),
            };
            let ch = buf.spec().channels().count().max(1);
            self.samples.clear();
            buf.copy_to_vec_interleaved(&mut self.samples);
            for f in self.samples.chunks_exact(ch) {
                frames.push(if ch == 1 { [f[0], f[0]] } else { [f[0], f[1]] });
            }
            // Moving bitrate display for variable-bitrate files.
            if self.vbr_capable && pkt_secs > 0.0 {
                self.bits_window.push_back((pkt_secs, pkt_bytes));
                let mut total_t: f64 = self.bits_window.iter().map(|x| x.0).sum();
                while total_t > 1.0 && self.bits_window.len() > 1 {
                    let (t, _) = self.bits_window.pop_front().unwrap();
                    total_t -= t;
                }
                let bytes: usize = self.bits_window.iter().map(|x| x.1).sum();
                if total_t > 0.2 {
                    self.kbps = Some(((bytes as f64 * 8.0) / total_t / 1000.0).round() as u32);
                }
            }
            return Ok(true);
        }
    }

    fn seek(&mut self, secs: f64) -> Option<f64> {
        let to = SeekTo::Time {
            time: Time::try_from_secs_f64(secs).unwrap_or(Time::ZERO),
            track_id: Some(self.track_id),
        };
        let seeked = self.format.seek(SeekMode::Accurate, to).ok()?;
        self.decoder.reset();
        Some(
            self.tb
                .and_then(|tb| tb.calc_time(seeked.actual_ts))
                .map(|t| t.as_secs_f64())
                .unwrap_or(secs),
        )
    }

    fn kbps(&self) -> Option<u32> {
        self.kbps
    }

    fn title(&mut self) -> Option<String> {
        self.titles.try_recv().ok()
    }
}

/// Opens the track with the built-in decoder or an input plugin: plugins
/// first for formats only they know, and as a fallback when the built-in
/// decoder cannot read a file.
fn open_track(
    source: &Source,
    plugins: Option<&Arc<crate::plugins::Plugins>>,
) -> Result<Box<dyn Track>, String> {
    use crate::plugins::input::PluginTrack;
    let file = match source {
        Source::File(p) => Some(p),
        Source::Url(_) => None,
    };
    let mut plugin_error = None;
    if let (Some(path), Some(sys)) = (file, plugins)
        && sys.prefers(path)
    {
        match PluginTrack::open(sys, path) {
            Ok(t) => return Ok(Box::new(t)),
            Err(e) => plugin_error = Some(e),
        }
    }
    match SymTrack::open(source) {
        Ok(t) => Ok(Box::new(t)),
        Err(e) => {
            if let (Some(path), Some(sys), None) = (file, plugins, &plugin_error)
                && sys.input_for(path).is_some()
                && let Ok(t) = PluginTrack::open(sys, path)
            {
                return Ok(Box::new(t));
            }
            Err(plugin_error.unwrap_or(e))
        }
    }
}

/// Runs a block through the active DSP plugin, if any.
fn apply_dsp(sys: &crate::plugins::Plugins, rate: u32, frames: &mut Vec<[f32; 2]>) {
    if !sys.dsp_active() || frames.is_empty() {
        return;
    }
    let to_i16 = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    let samples: Vec<i16> = frames
        .iter()
        .flat_map(|f| [to_i16(f[0]), to_i16(f[1])])
        .collect();
    if let Some(out) = sys.dsp_process(rate, 2, samples) {
        frames.clear();
        frames.extend(
            out.chunks_exact(2)
                .map(|c| [c[0] as f32 / 32768.0, c[1] as f32 / 32768.0]),
        );
    }
}

fn decode(ctx: &DecodeCtx) -> Result<(), String> {
    let plugins = ctx.shared.plugins.lock().unwrap().clone();
    let mut track = open_track(&ctx.source, plugins.as_ref())?;
    let info = track.info().clone();
    let in_rate = info.rate;
    if !(1000..=768_000).contains(&in_rate) {
        return Err(format!("unsupported sample rate {in_rate} Hz"));
    }
    ctx.shared.kbps.store(info.kbps, Ordering::Relaxed);
    ctx.send(Event::Opened {
        duration: info.duration,
        kbps: info.kbps,
        khz: (in_rate as f64 / 1000.0).round() as u32,
        channels: info.channels,
        stream_name: info.stream_name.clone(),
    });

    let out_rate = ctx.shared.out_rate.load(Ordering::Relaxed);
    let mut resampler = Resampler::new(in_rate, out_rate);
    let mut eq = Equalizer::new(out_rate, *ctx.shared.eq.lock().unwrap());
    let mut eq_version = ctx.shared.eq_version.load(Ordering::Relaxed);
    let mut frames: Vec<[f32; 2]> = Vec::new();
    let mut out: Vec<[f32; 2]> = Vec::new();

    'track: loop {
        loop {
            // Commands (a seek that arrived while the ring was full comes first).
            let cmd = match PENDING_SEEK.with(|p| p.take()) {
                Some(s) => Ok(Cmd::Seek(s)),
                None => ctx.cmds.try_recv(),
            };
            match cmd {
                Ok(Cmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Ok(Cmd::Seek(secs)) => {
                    if let Some(actual) = track.seek(secs) {
                        resampler.reset();
                        let now = ctx.pushed.load(Ordering::SeqCst);
                        ctx.shared.discard_until.store(now, Ordering::SeqCst);
                        let mut tl = ctx.shared.timeline.lock().unwrap();
                        if tl.generation == ctx.generation {
                            tl.base_frame = now;
                            tl.base_time = actual;
                            tl.end_frame = None;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            while let Some(t) = track.title() {
                ctx.send(Event::StreamTitle(t));
            }
            let v = ctx.shared.eq_version.load(Ordering::Relaxed);
            if v != eq_version {
                eq_version = v;
                eq.set(*ctx.shared.eq.lock().unwrap());
            }

            frames.clear();
            if !track.read(&mut frames)? {
                break;
            }
            if let Some(k) = track.kbps() {
                ctx.shared.kbps.store(k, Ordering::Relaxed);
            }
            if frames.is_empty() {
                continue;
            }
            if let Some(sys) = &plugins {
                apply_dsp(sys, in_rate, &mut frames);
            }
            out.clear();
            resampler.process(&frames, &mut out);
            eq.process(&mut out);
            if !push_all(ctx, &out)? {
                return Ok(());
            }
        }
        // Flush the resampler tail with a little silence, then mark the end.
        out.clear();
        resampler.process(&[[0.0; 2]; 32], &mut out);
        eq.process(&mut out);
        if !push_all(ctx, &out)? {
            return Ok(());
        }
        // A seek that arrived during the flush restarts decoding.
        if PENDING_SEEK.with(|p| p.get()).is_some() {
            continue 'track;
        }
        let end = ctx.pushed.load(Ordering::SeqCst);
        {
            let mut tl = ctx.shared.timeline.lock().unwrap();
            if tl.generation == ctx.generation {
                tl.end_frame = Some(end);
            }
        }
        // Wait (interruptibly) for the UI to stop us or start the next track; a
        // seek after the end resumes decoding of this track.
        loop {
            match ctx.cmds.recv_timeout(Duration::from_millis(50)) {
                Ok(Cmd::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                Ok(Cmd::Seek(s)) => {
                    PENDING_SEEK.with(|p| p.set(Some(s)));
                    continue 'track;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if ctx.shared.active.load(Ordering::SeqCst) != ctx.generation {
                return Ok(());
            }
        }
    }
}

/// Push frames into the ring, blocking while it is full. Returns false if
/// told to stop meanwhile.
fn push_all(ctx: &DecodeCtx, frames: &[[f32; 2]]) -> Result<bool, String> {
    let mut i = 0;
    while i < frames.len() {
        {
            let mut guard = ctx.producer.lock().unwrap();
            if ctx.shared.active.load(Ordering::SeqCst) != ctx.generation {
                return Ok(false);
            }
            let Some(prod) = guard.as_mut() else {
                return Err("audio device unavailable".into());
            };
            while i < frames.len() {
                if prod.push(frames[i]).is_err() {
                    break;
                }
                i += 1;
                ctx.pushed.fetch_add(1, Ordering::SeqCst);
            }
        }
        if i < frames.len() {
            // Ring full: wait, but stay responsive to commands.
            std::thread::sleep(Duration::from_millis(5));
            if let Ok(cmd) = ctx.cmds.try_recv() {
                match cmd {
                    Cmd::Stop => return Ok(false),
                    Cmd::Seek(s) => {
                        // Re-queue the seek for the main loop and drop the rest of this chunk.
                        requeue_seek(ctx, s);
                        return Ok(true);
                    }
                }
            }
        }
    }
    Ok(true)
}

fn requeue_seek(ctx: &DecodeCtx, s: f64) {
    // The command channel only has one receiver (us); store it in a side slot.
    PENDING_SEEK.with(|p| p.set(Some(s)));
    let _ = ctx;
}

thread_local! {
    static PENDING_SEEK: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays every file in $FERRAMP_TEST_MEDIA through the engine with the
    /// silent output: `FERRAMP_TEST_MEDIA=dir cargo test -- --ignored formats`.
    #[test]
    #[ignore]
    fn formats() {
        let Some(dir) = std::env::var_os("FERRAMP_TEST_MEDIA") else {
            return;
        };
        // SAFETY: set before any other thread reads the environment in this test.
        unsafe { std::env::set_var("FERRAMP_NULL_AUDIO", "1") };
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        let mut engine = Engine::new();
        for f in files {
            engine.play(Source::File(f.clone()));
            let start = std::time::Instant::now();
            let mut opened = None;
            while start.elapsed() < Duration::from_secs(5) && opened.is_none() {
                match engine
                    .events
                    .recv_timeout(Duration::from_millis(100))
                    .map(|e| e.1)
                {
                    Ok(Event::Opened {
                        duration,
                        khz,
                        channels,
                        ..
                    }) => opened = Some((duration, khz, channels)),
                    Ok(Event::Error(e)) => panic!("{}: {e}", f.display()),
                    _ => {}
                }
            }
            let (duration, khz, channels) =
                opened.unwrap_or_else(|| panic!("{}: not opened", f.display()));
            std::thread::sleep(Duration::from_millis(600));
            let pos = engine.position();
            println!(
                "{}: {:?}s {khz}kHz {channels}ch, played {pos:.2}s",
                f.display(),
                duration
            );
            assert!(pos > 0.2, "{}: position did not advance", f.display());
            engine.stop();
        }
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;

    /// Seek and end-of-track with the silent output:
    /// `FERRAMP_TEST_FILE=short.wav cargo test -- --ignored seek_and_end`.
    #[test]
    #[ignore]
    fn seek_and_end() {
        let Some(file) = std::env::var_os("FERRAMP_TEST_FILE") else {
            return;
        };
        // SAFETY: set before any other thread reads the environment in this test.
        unsafe { std::env::set_var("FERRAMP_NULL_AUDIO", "1") };
        let mut engine = Engine::new();
        engine.play(Source::File(file.into()));
        std::thread::sleep(Duration::from_millis(500));
        engine.seek(3.0);
        std::thread::sleep(Duration::from_millis(400));
        let p = engine.position();
        assert!((3.2..3.8).contains(&p), "position after seek: {p}");
        engine.pause();
        let a = engine.position();
        std::thread::sleep(Duration::from_millis(300));
        assert!((engine.position() - a).abs() < 0.01, "paused but moving");
        engine.resume();
        let start = std::time::Instant::now();
        while !engine.finished() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(engine.finished(), "did not reach the end");
        // Seeking after the end resumes the same decoder.
        engine.seek(1.0);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!engine.finished());
        assert!(
            engine.position() > 1.0 && engine.position() < 2.0,
            "{}",
            engine.position()
        );
    }

    /// Switching tracks while the first is still opening must not deliver
    /// the first track's events: `FERRAMP_TEST_FILE=a.mp3 FERRAMP_TEST_FILE2=b.wav`.
    #[test]
    #[ignore]
    fn stale_events_are_dropped() {
        let (Some(a), Some(b)) = (
            std::env::var_os("FERRAMP_TEST_FILE"),
            std::env::var_os("FERRAMP_TEST_FILE2"),
        ) else {
            return;
        };
        // SAFETY: set before any other thread reads the environment in this test.
        unsafe { std::env::set_var("FERRAMP_NULL_AUDIO", "1") };
        let mut engine = Engine::new();
        engine.play(Source::File("/nonexistent/x.mp3".into()));
        engine.play(Source::File(a.into()));
        engine.stop();
        engine.play(Source::File(b.clone().into()));
        let want = crate::metadata::read(std::path::Path::new(&b)).and_then(|i| i.duration);
        let start = std::time::Instant::now();
        let mut got = None;
        while got.is_none() && start.elapsed() < Duration::from_secs(5) {
            match engine.next_event() {
                Some(Event::Opened { duration, .. }) => got = Some(duration),
                Some(Event::Error(e)) => panic!("stale error delivered: {e}"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let d = got.expect("not opened").unwrap_or(0.0);
        assert!((d - want.unwrap_or(d)).abs() < 0.5, "{d} vs {want:?}");
        std::thread::sleep(Duration::from_millis(500));
        assert!(engine.position() > 0.2 && engine.position() < 1.0);
    }

    /// Stop must not wait for a decoder blocked on a stalled stream.
    #[test]
    fn stop_does_not_block_on_a_stalled_stream() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accept the connection and never answer.
        std::thread::spawn(move || {
            let _held: Vec<_> = listener.incoming().take(1).collect();
            std::thread::sleep(Duration::from_secs(30));
        });
        let mut engine = Engine::headless();
        engine.play(Source::Url(format!("http://127.0.0.1:{port}/stream")));
        std::thread::sleep(Duration::from_millis(200));
        let t = std::time::Instant::now();
        engine.stop();
        engine.play(Source::Url(format!("http://127.0.0.1:{port}/other")));
        drop(engine);
        assert!(
            t.elapsed() < Duration::from_millis(100),
            "{:?}",
            t.elapsed()
        );
    }
}
