//! Decoding through input plugins.
//!
//! Two paths: the pull decoder (`winampGetExtendedRead_*`) when a plugin
//! has one, read on a worker thread; otherwise the classic one, where the
//! plugin's own thread pushes PCM into the output module we give it. Both
//! send `Pcm` events and respect the player's credit (bytes it is ready
//! to take), so a plugin never runs far ahead of playback.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_short, c_void};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferramp_plugproto::{FromHost, StreamFormat};
use ferramp_winampabi::*;

use super::registry::{self, Arg, Data, Input};
use super::{Ptr, event, text};

struct Stream {
    id: u32,
    window: i64,
    credit: AtomicI64,
    played_ms: AtomicU32,
    pull: Option<Pull>,
}

struct Pull {
    seek: AtomicI32,
    close: AtomicBool,
}

/// The classic-path stream that the shared output module serves (the
/// original player also played one file at a time through one output).
struct Push {
    stream: Arc<Stream>,
    plugin: u32,
    module: Ptr<In_Module>,
    bytes_per_ms: AtomicU64,
    written: AtomicU64,
    base_ms: AtomicU64,
    paused: AtomicBool,
}

static STREAMS: Mutex<Option<HashMap<u32, Arc<Stream>>>> = Mutex::new(None);
static PUSH: Mutex<Option<Arc<Push>>> = Mutex::new(None);

fn stream(id: u32) -> Option<Arc<Stream>> {
    STREAMS.lock().unwrap().as_ref()?.get(&id).cloned()
}

fn push() -> Option<Arc<Push>> {
    PUSH.lock().unwrap().clone()
}

pub fn credit(id: u32, bytes: u32) {
    if let Some(s) = stream(id) {
        s.credit.fetch_add(bytes as i64, Ordering::SeqCst);
    }
}

pub fn played(id: u32, ms: u32) {
    if let Some(s) = stream(id) {
        s.played_ms.store(ms, Ordering::SeqCst);
    }
}

fn failed(stream: u32, msg: impl Into<String>) {
    event(FromHost::StreamFailed {
        stream,
        msg: msg.into(),
    });
}

pub fn open(id: u32, plugin: u32, path: &str, credit: u32) {
    let Some(Data::Input(inp)) = registry::data(plugin) else {
        return failed(id, "not an input plugin");
    };
    let win = text::to_win_path(path);
    let s = Arc::new(Stream {
        id,
        window: credit.max(4096) as i64,
        credit: AtomicI64::new(credit as i64),
        played_ms: AtomicU32::new(0),
        pull: None,
    });
    if let Some(xr) = inp.xr {
        let (mut size, mut bps, mut nch, mut srate): (c_int, c_int, c_int, c_int) = (0, 16, 0, 0);
        let h = unsafe {
            match (xr.openw, xr.open) {
                (Some(f), _) if inp.unicode || xr.open.is_none() => {
                    let w = text::wide(&win);
                    f(w.as_ptr(), &mut size, &mut bps, &mut nch, &mut srate)
                }
                (_, Some(f)) => {
                    let a = text::ansi_path(&win);
                    f(a.as_ptr().cast(), &mut size, &mut bps, &mut nch, &mut srate)
                }
                _ => 0,
            }
        };
        if h != 0 && srate > 0 && nch > 0 && matches!(bps, 8 | 16 | 24 | 32) {
            let s = Arc::new(Stream {
                pull: Some(Pull {
                    seek: AtomicI32::new(-1),
                    close: AtomicBool::new(false),
                }),
                ..Arc::into_inner(s).unwrap()
            });
            STREAMS
                .lock()
                .unwrap()
                .get_or_insert_with(HashMap::new)
                .insert(id, s.clone());
            let bytes_per_sec = srate as i64 * nch as i64 * bps as i64 / 8;
            let mut length_ms = if size > 0 {
                (size as i64 * 1000 / bytes_per_sec.max(1)) as i32
            } else {
                -1
            };
            if length_ms <= 0 {
                length_ms = registry::file_info(plugin, path)
                    .map(|i| i.length_ms)
                    .unwrap_or(-1);
            }
            event(FromHost::Opened {
                stream: id,
                format: StreamFormat {
                    rate: srate as u32,
                    channels: nch as u32,
                    bits: bps as u32,
                    length_ms,
                    seekable: xr.set_time.is_some(),
                    kbps: 0,
                },
            });
            let frame = (nch * bps / 8) as usize;
            std::thread::spawn(move || pull_worker(s, xr, h, frame));
            return;
        }
        if h != 0 {
            unsafe { (xr.close)(h) };
        }
    }
    play_classic(s, plugin, inp, &win);
}

fn pull_worker(s: Arc<Stream>, xr: registry::Xr, h: isize, frame: usize) {
    let chunk = (16384 / frame).max(1) * frame;
    let mut buf = vec![0u8; chunk];
    let mut kill: c_int = 0;
    let mut at_end = false;
    let p = s.pull.as_ref().unwrap();
    loop {
        if p.close.load(Ordering::SeqCst) {
            break;
        }
        let seek = p.seek.swap(-1, Ordering::SeqCst);
        if seek >= 0 {
            if let Some(f) = xr.set_time {
                unsafe { f(h, seek) };
            }
            s.credit.store(s.window, Ordering::SeqCst);
            at_end = false;
            event(FromHost::Flushed {
                stream: s.id,
                ms: seek as u32,
            });
        }
        if at_end || s.credit.load(Ordering::SeqCst) < chunk as i64 {
            std::thread::sleep(Duration::from_millis(3));
            continue;
        }
        let n = unsafe {
            (xr.get)(
                h,
                buf.as_mut_ptr() as *mut c_char,
                chunk as c_int,
                &mut kill,
            )
        };
        if n <= 0 {
            at_end = true;
            event(FromHost::Eof { stream: s.id });
            continue;
        }
        let n = (n as usize).min(chunk) / frame * frame;
        s.credit.fetch_sub(n as i64, Ordering::SeqCst);
        event(FromHost::Pcm {
            stream: s.id,
            data: buf[..n].to_vec(),
        });
    }
    unsafe { (xr.close)(h) };
}

fn play_classic(s: Arc<Stream>, plugin: u32, inp: Input, win: &str) {
    let m = inp.m;
    unsafe {
        if (*m.0).UsesOutputPlug & IN_USES_OUTPUT_PLUGIN == 0 {
            return failed(s.id, "this plugin plays through its own output");
        }
    }
    stop_push();
    STREAMS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(s.id, s.clone());
    *PUSH.lock().unwrap() = Some(Arc::new(Push {
        stream: s.clone(),
        plugin,
        module: m,
        bytes_per_ms: AtomicU64::new(176),
        written: AtomicU64::new(0),
        base_ms: AtomicU64::new(0),
        paused: AtomicBool::new(false),
    }));
    let arg = Arg::new(win, inp.unicode);
    let r = unsafe {
        (*m.0).outMod = &raw mut FAKE_OUT;
        match (*m.0).Play {
            Some(f) => f(arg.ptr()),
            None => -1,
        }
    };
    if r != 0 {
        *PUSH.lock().unwrap() = None;
        if let Some(map) = STREAMS.lock().unwrap().as_mut() {
            map.remove(&s.id);
        }
        failed(
            s.id,
            if r == -1 {
                "file not found".to_string()
            } else {
                format!("the plugin could not play it ({r})")
            },
        );
    }
}

/// Stops the classic-path stream, if any.
fn stop_push() {
    let p = PUSH.lock().unwrap().take();
    if let Some(p) = p {
        unsafe {
            if let Some(f) = (*p.module.0).Stop {
                f();
            }
        }
        if let Some(map) = STREAMS.lock().unwrap().as_mut() {
            map.remove(&p.stream.id);
        }
    }
}

pub fn seek(id: u32, ms: u32) {
    let Some(s) = stream(id) else { return };
    if let Some(p) = &s.pull {
        p.seek.store(ms as i32, Ordering::SeqCst);
    } else if let Some(p) = push().filter(|p| p.stream.id == id) {
        unsafe {
            if let Some(f) = (*p.module.0).SetOutputTime {
                f(ms as c_int);
            }
        }
    }
}

pub fn close(id: u32) {
    let s = STREAMS.lock().unwrap().as_mut().and_then(|m| m.remove(&id));
    if let Some(s) = s {
        if let Some(p) = &s.pull {
            p.close.store(true, Ordering::SeqCst);
        } else if push().is_some_and(|p| p.stream.id == id) {
            stop_push();
        }
    }
}

pub fn close_all() {
    stop_push();
    if let Some(map) = STREAMS.lock().unwrap().take() {
        for s in map.values() {
            if let Some(p) = &s.pull {
                p.close.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// The plugin posted "end of file" to the player window.
pub fn eof() {
    if let Some(p) = push() {
        event(FromHost::Eof {
            stream: p.stream.id,
        });
    }
    let _ = registry::data;
}

// ----------------------------------------------------------------------
// Callbacks every input module gets

pub unsafe fn install_callbacks(m: *mut In_Module) {
    unsafe {
        (*m).SAVSAInit = Some(sa_vsa_init);
        (*m).SAVSADeInit = Some(sa_vsa_deinit);
        (*m).SAAddPCMData = Some(add_pcm);
        (*m).SAGetMode = Some(sa_get_mode);
        (*m).SAAdd = Some(sa_add);
        (*m).VSAAddPCMData = Some(add_pcm);
        (*m).VSAGetMode = Some(vsa_get_mode);
        (*m).VSAAdd = Some(vsa_add);
        (*m).VSASetInfo = Some(vsa_set_info);
        (*m).dsp_isactive = Some(dsp_isactive);
        (*m).dsp_dosamples = Some(dsp_dosamples);
        (*m).SetInfo = Some(set_info);
        (*m).outMod = &raw mut FAKE_OUT;
    }
}

// The player computes its own visualization data and applies DSP in its
// own pipeline, so these are quiet stand-ins.
unsafe extern "C" fn sa_vsa_init(_: c_int, _: c_int) {}
unsafe extern "C" fn sa_vsa_deinit() {}
unsafe extern "C" fn add_pcm(_: *mut c_void, _: c_int, _: c_int, _: c_int) {}
unsafe extern "C" fn sa_get_mode() -> c_int {
    0
}
unsafe extern "C" fn sa_add(_: *mut c_void, _: c_int, _: c_int) -> c_int {
    0
}
unsafe extern "C" fn vsa_get_mode(spec: *mut c_int, wave: *mut c_int) -> c_int {
    unsafe {
        if !spec.is_null() {
            *spec = 0;
        }
        if !wave.is_null() {
            *wave = 0;
        }
    }
    0
}
unsafe extern "C" fn vsa_add(_: *mut c_void, _: c_int) -> c_int {
    0
}
unsafe extern "C" fn vsa_set_info(_: c_int, _: c_int) {}
unsafe extern "C" fn dsp_isactive() -> c_int {
    0
}
unsafe extern "C" fn dsp_dosamples(
    _: *mut c_short,
    n: c_int,
    _: c_int,
    _: c_int,
    _: c_int,
) -> c_int {
    n
}
unsafe extern "C" fn set_info(kbps: c_int, khz: c_int, _: c_int, _: c_int) {
    if let Some(p) = push() {
        event(FromHost::StreamInfo {
            stream: p.stream.id,
            kbps: kbps.max(0) as u32,
            rate: khz.max(0) as u32 * 1000,
            channels: 0,
        });
    }
}

// ----------------------------------------------------------------------
// The output module handed to classic-path input plugins

static mut FAKE_OUT: Out_Module = Out_Module {
    version: OUT_VER,
    description: c"Ferramp".as_ptr(),
    id: 0x4652_4D50,
    hMainWindow: null_mut(),
    hDllInstance: null_mut(),
    Config: None,
    About: None,
    Init: None,
    Quit: None,
    Open: Some(fo_open),
    Close: Some(fo_close),
    Write: Some(fo_write),
    CanWrite: Some(fo_can_write),
    IsPlaying: Some(fo_is_playing),
    Pause: Some(fo_pause),
    SetVolume: Some(fo_set_volume),
    SetPan: Some(fo_set_pan),
    Flush: Some(fo_flush),
    GetOutputTime: Some(fo_output_time),
    GetWrittenTime: Some(fo_written_time),
};

unsafe extern "C" fn fo_open(rate: c_int, nch: c_int, bps: c_int, _: c_int, _: c_int) -> c_int {
    let Some(p) = push() else { return -1 };
    if rate <= 0 || nch <= 0 || !matches!(bps, 8 | 16 | 24 | 32) {
        return -1;
    }
    p.bytes_per_ms.store(
        ((rate * nch * bps / 8) / 1000).max(1) as u64,
        Ordering::SeqCst,
    );
    p.written.store(0, Ordering::SeqCst);
    p.base_ms.store(0, Ordering::SeqCst);
    let (length_ms, seekable) = unsafe {
        let m = &*p.module.0;
        (m.GetLength.map(|f| f()).unwrap_or(-1), m.is_seekable != 0)
    };
    event(FromHost::Opened {
        stream: p.stream.id,
        format: StreamFormat {
            rate: rate as u32,
            channels: nch as u32,
            bits: bps as u32,
            length_ms,
            seekable,
            kbps: 0,
        },
    });
    let _ = p.plugin;
    0
}
unsafe extern "C" fn fo_close() {}
unsafe extern "C" fn fo_write(buf: *mut c_char, len: c_int) -> c_int {
    let Some(p) = push() else { return 0 };
    if len <= 0 || buf.is_null() {
        return 0;
    }
    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, len as usize) }.to_vec();
    p.stream.credit.fetch_sub(len as i64, Ordering::SeqCst);
    p.written.fetch_add(len as u64, Ordering::SeqCst);
    event(FromHost::Pcm {
        stream: p.stream.id,
        data,
    });
    0
}
unsafe extern "C" fn fo_can_write() -> c_int {
    match push() {
        Some(p) if !p.paused.load(Ordering::SeqCst) => {
            p.stream.credit.load(Ordering::SeqCst).clamp(0, 1 << 30) as c_int
        }
        _ => 0,
    }
}
/// Everything written has been handed to the player, which plays the rest.
unsafe extern "C" fn fo_is_playing() -> c_int {
    0
}
unsafe extern "C" fn fo_pause(pause: c_int) -> c_int {
    push()
        .map(|p| p.paused.swap(pause != 0, Ordering::SeqCst) as c_int)
        .unwrap_or(0)
}
unsafe extern "C" fn fo_set_volume(_: c_int) {}
unsafe extern "C" fn fo_set_pan(_: c_int) {}
unsafe extern "C" fn fo_flush(t: c_int) {
    let Some(p) = push() else { return };
    p.stream.credit.store(p.stream.window, Ordering::SeqCst);
    p.written.store(0, Ordering::SeqCst);
    p.base_ms.store(t.max(0) as u64, Ordering::SeqCst);
    p.stream.played_ms.store(t.max(0) as u32, Ordering::SeqCst);
    event(FromHost::Flushed {
        stream: p.stream.id,
        ms: t.max(0) as u32,
    });
}
unsafe extern "C" fn fo_output_time() -> c_int {
    push()
        .map(|p| p.stream.played_ms.load(Ordering::SeqCst) as c_int)
        .unwrap_or(0)
}
unsafe extern "C" fn fo_written_time() -> c_int {
    push()
        .map(|p| {
            (p.base_ms.load(Ordering::SeqCst)
                + p.written.load(Ordering::SeqCst) / p.bytes_per_ms.load(Ordering::SeqCst))
                as c_int
        })
        .unwrap_or(0)
}
