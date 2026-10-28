// The plugin modules are `static mut` the host writes into, so they are
// reached through raw pointers (`&raw mut`); a reference to them would be
// undefined behaviour, which is what `deref_addrof` suggests. The extension
// block holds embedded NULs, which a `c""` literal cannot.
#![allow(clippy::deref_addrof, clippy::manual_c_str_literals)]

use std::ffi::{CStr, c_char, c_int, c_short, c_void};
use std::io::Write;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ferramp_winampabi::*;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, KillTimer, PostMessageW, SendMessageW, SetTimer, WM_COMMAND, WNDPROC,
};

use crate::{parse_tone, tone_sample};

fn log(file: &str, line: &str) {
    let Ok(dir) = std::env::var("FERRAMP_TEST_DIR") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{dir}\\{file}"))
    {
        let _ = writeln!(f, "{line}");
    }
}

unsafe fn cstr(p: *const c_void) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p as *const c_char) }
        .to_string_lossy()
        .into_owned()
}

/// Copies `s` into a C buffer of `cap` bytes.
unsafe fn put_cstr(dst: *mut c_void, cap: usize, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(cap.saturating_sub(1));
    unsafe {
        std::ptr::copy_nonoverlapping(b.as_ptr(), dst as *mut u8, n);
        *(dst as *mut u8).add(n) = 0;
    }
}

fn read_tone(path: &str) -> Option<(f64, f64, u32, u32)> {
    std::fs::read_to_string(path).ok().map(|t| parse_tone(&t))
}

// ----------------------------------------------------------------------
// Input

static mut IN_MOD: In_Module = In_Module {
    version: IN_VER,
    description: c"Ferramp Test Input v1".as_ptr(),
    hMainWindow: null_mut(),
    hDllInstance: null_mut(),
    FileExtensions: b"fttone\0Ferramp test tone (*.fttone)\0\0".as_ptr() as *const c_char,
    is_seekable: 1,
    UsesOutputPlug: IN_USES_OUTPUT_PLUGIN,
    Config: Some(in_config),
    About: Some(in_about),
    Init: Some(in_init),
    Quit: Some(in_quit),
    GetFileInfo: Some(in_get_file_info),
    InfoBox: Some(in_info_box),
    IsOurFile: Some(in_is_our_file),
    Play: Some(in_play),
    Pause: Some(in_pause),
    UnPause: Some(in_unpause),
    IsPaused: Some(in_is_paused),
    Stop: Some(in_stop),
    GetLength: Some(in_get_length),
    GetOutputTime: Some(in_get_output_time),
    SetOutputTime: Some(in_set_output_time),
    SetVolume: Some(in_set_volume),
    SetPan: Some(in_set_pan),
    SAVSAInit: None,
    SAVSADeInit: None,
    SAAddPCMData: None,
    SAGetMode: None,
    SAAdd: None,
    VSAAddPCMData: None,
    VSAGetMode: None,
    VSAAdd: None,
    VSASetInfo: None,
    dsp_isactive: None,
    dsp_dosamples: None,
    EQSet: Some(in_eq_set),
    SetInfo: None,
    outMod: null_mut(),
};

struct Playing {
    kill: Arc<AtomicBool>,
    seek: Arc<AtomicI32>,
    thread: Option<JoinHandle<()>>,
    length_ms: i32,
}

static PLAYING: Mutex<Option<Playing>> = Mutex::new(None);
static PAUSED: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetInModule2() -> *mut In_Module {
    &raw mut IN_MOD
}

unsafe extern "C" fn in_config(_: HWND) {
    log("in.log", "config");
}
unsafe extern "C" fn in_about(_: HWND) {
    log("in.log", "about");
}
unsafe extern "C" fn in_init() {
    // The player must have filled in its callbacks by now.
    let m = unsafe { &*(&raw const IN_MOD) };
    let ok = m.SAVSAInit.is_some()
        && m.SAAddPCMData.is_some()
        && m.VSAAddPCMData.is_some()
        && m.dsp_isactive.is_some()
        && m.dsp_dosamples.is_some()
        && m.SetInfo.is_some()
        && !m.hMainWindow.is_null();
    log("in.log", &format!("init callbacks={ok}"));
}
unsafe extern "C" fn in_quit() {
    log("in.log", "quit");
}
unsafe extern "C" fn in_get_file_info(file: *const c_void, title: *mut c_void, len: *mut c_int) {
    let path = unsafe { cstr(file) };
    if let Some((freq, secs, _, _)) = read_tone(&path) {
        unsafe {
            if !title.is_null() {
                put_cstr(
                    title,
                    GETFILEINFO_TITLE_LENGTH,
                    &format!("Test tone {freq} Hz"),
                );
            }
            if !len.is_null() {
                *len = (secs * 1000.0) as c_int;
            }
        }
    }
}
unsafe extern "C" fn in_info_box(file: *const c_void, _: HWND) -> c_int {
    log("in.log", &format!("infobox {}", unsafe { cstr(file) }));
    0
}
unsafe extern "C" fn in_is_our_file(_: *const c_void) -> c_int {
    0
}

unsafe extern "C" fn in_play(file: *const c_void) -> c_int {
    let path = unsafe { cstr(file) };
    let Some((freq, secs, rate, ch)) = read_tone(&path) else {
        return -1;
    };
    in_stop_inner();
    let kill = Arc::new(AtomicBool::new(false));
    let seek = Arc::new(AtomicI32::new(-1));
    let (k, s) = (kill.clone(), seek.clone());
    let m = &raw mut IN_MOD;
    let out = unsafe { (*m).outMod };
    if out.is_null() {
        return 1;
    }
    let latency = unsafe { ((*out).Open.unwrap())(rate as c_int, ch as c_int, 16, -1, -1) };
    if latency < 0 {
        return 1;
    }
    log("in.log", &format!("play {path} latency={latency}"));
    PAUSED.store(false, Ordering::SeqCst);
    let thread = std::thread::spawn(move || unsafe {
        // The module is a static; re-derive the pointers on this thread.
        let mm = &*(&raw const IN_MOD);
        let out = mm.outMod;
        (mm.SetInfo.unwrap())(1411, (rate / 1000) as c_int, ch as c_int, 1);
        (mm.SAVSAInit.unwrap())(latency, rate as c_int);
        (mm.VSASetInfo.unwrap())(rate as c_int, ch as c_int);
        let total = (secs * rate as f64) as u64;
        let mut pos = 0u64;
        let frames = 576usize;
        let mut buf = vec![0i16; frames * ch as usize * 2];
        loop {
            if k.load(Ordering::SeqCst) {
                return;
            }
            let sk = s.swap(-1, Ordering::SeqCst);
            if sk >= 0 {
                pos = (sk as u64 * rate as u64 / 1000).min(total);
                ((*out).Flush.unwrap())(sk);
            }
            if pos >= total {
                break;
            }
            let n = frames.min((total - pos) as usize);
            let bytes = (n * ch as usize * 2) as c_int;
            let dsp = (mm.dsp_isactive.unwrap())() != 0;
            if ((*out).CanWrite.unwrap())() < bytes * if dsp { 2 } else { 1 } {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            for i in 0..n {
                let v = tone_sample(freq, rate, pos + i as u64);
                for c in 0..ch as usize {
                    buf[i * ch as usize + c] = v;
                }
            }
            let t = (pos * 1000 / rate as u64) as c_int;
            (mm.SAAddPCMData.unwrap())(buf.as_mut_ptr() as *mut c_void, ch as c_int, 16, t);
            (mm.VSAAddPCMData.unwrap())(buf.as_mut_ptr() as *mut c_void, ch as c_int, 16, t);
            let mut n_out = n as c_int;
            if dsp {
                n_out = (mm.dsp_dosamples.unwrap())(
                    buf.as_mut_ptr(),
                    n as c_int,
                    16,
                    ch as c_int,
                    rate as c_int,
                );
            }
            ((*out).Write.unwrap())(buf.as_mut_ptr() as *mut c_char, n_out * ch as c_int * 2);
            pos += n as u64;
        }
        while ((*out).IsPlaying.unwrap())() != 0 {
            if k.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !k.load(Ordering::SeqCst) {
            PostMessageW(mm.hMainWindow, WM_WA_MPEG_EOF, 0, 0);
        }
    });
    *PLAYING.lock().unwrap() = Some(Playing {
        kill,
        seek,
        thread: Some(thread),
        length_ms: (secs * 1000.0) as i32,
    });
    0
}

fn in_stop_inner() -> bool {
    let p = PLAYING.lock().unwrap().take();
    if let Some(mut p) = p {
        p.kill.store(true, Ordering::SeqCst);
        if let Some(t) = p.thread.take() {
            let _ = t.join();
        }
        true
    } else {
        false
    }
}

unsafe extern "C" fn in_stop() {
    if in_stop_inner() {
        let m = &raw mut IN_MOD;
        unsafe {
            ((*(*m).outMod).Close.unwrap())();
            ((*m).SAVSADeInit.unwrap())();
        }
        log("in.log", "stop");
    }
}
unsafe extern "C" fn in_pause() {
    PAUSED.store(true, Ordering::SeqCst);
    unsafe { ((*(*(&raw mut IN_MOD)).outMod).Pause.unwrap())(1) };
}
unsafe extern "C" fn in_unpause() {
    PAUSED.store(false, Ordering::SeqCst);
    unsafe { ((*(*(&raw mut IN_MOD)).outMod).Pause.unwrap())(0) };
}
unsafe extern "C" fn in_is_paused() -> c_int {
    PAUSED.load(Ordering::SeqCst) as c_int
}
unsafe extern "C" fn in_get_length() -> c_int {
    PLAYING
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.length_ms)
        .unwrap_or(-1)
}
unsafe extern "C" fn in_get_output_time() -> c_int {
    unsafe { ((*(*(&raw mut IN_MOD)).outMod).GetOutputTime.unwrap())() }
}
unsafe extern "C" fn in_set_output_time(ms: c_int) {
    if let Some(p) = PLAYING.lock().unwrap().as_ref() {
        p.seek.store(ms, Ordering::SeqCst);
    }
}
unsafe extern "C" fn in_set_volume(v: c_int) {
    unsafe { ((*(*(&raw mut IN_MOD)).outMod).SetVolume.unwrap())(v) };
}
unsafe extern "C" fn in_set_pan(p: c_int) {
    unsafe { ((*(*(&raw mut IN_MOD)).outMod).SetPan.unwrap())(p) };
}
unsafe extern "C" fn in_eq_set(on: c_int, _: *mut c_char, preamp: c_int) {
    log("in.log", &format!("eq on={on} preamp={preamp}"));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetExtendedFileInfo(
    file: *const c_char,
    meta: *const c_char,
    ret: *mut c_char,
    retlen: c_int,
) -> c_int {
    let path = unsafe { cstr(file as *const c_void) };
    let key = unsafe { cstr(meta as *const c_void) };
    let Some((freq, secs, _, _)) = read_tone(&path) else {
        return 0;
    };
    let v = match key.as_str() {
        "title" => format!("Test tone {freq} Hz"),
        "artist" => "Ferramp".to_string(),
        "album" => "Plugin tests".to_string(),
        "length" => ((secs * 1000.0) as i32).to_string(),
        _ => return 0,
    };
    unsafe { put_cstr(ret as *mut c_void, retlen.max(0) as usize, &v) };
    1
}

struct XRead {
    freq: f64,
    rate: u32,
    ch: u32,
    total: u64,
    pos: u64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetExtendedRead_open(
    file: *const c_char,
    size: *mut c_int,
    bps: *mut c_int,
    nch: *mut c_int,
    srate: *mut c_int,
) -> isize {
    let path = unsafe { cstr(file as *const c_void) };
    if path.contains("noxr") {
        return 0;
    }
    let Some((freq, secs, rate, ch)) = read_tone(&path) else {
        return 0;
    };
    let total = (secs * rate as f64) as u64;
    unsafe {
        *size = (total * ch as u64 * 2) as c_int;
        *bps = 16;
        *nch = ch as c_int;
        *srate = rate as c_int;
    }
    log("in.log", &format!("xread open {path}"));
    Box::into_raw(Box::new(XRead {
        freq,
        rate,
        ch,
        total,
        pos: 0,
    })) as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetExtendedRead_getData(
    h: isize,
    dest: *mut c_char,
    len: c_int,
    kill: *mut c_int,
) -> isize {
    let x = unsafe { &mut *(h as *mut XRead) };
    if !kill.is_null() && unsafe { *kill } != 0 {
        return 0;
    }
    let frame = x.ch as usize * 2;
    let n = ((len.max(0) as usize) / frame).min((x.total - x.pos) as usize);
    let out = dest as *mut i16;
    for i in 0..n {
        let v = tone_sample(x.freq, x.rate, x.pos + i as u64);
        for c in 0..x.ch as usize {
            unsafe { *out.add(i * x.ch as usize + c) = v };
        }
    }
    x.pos += n as u64;
    (n * frame) as isize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetExtendedRead_setTime(h: isize, ms: c_int) -> c_int {
    let x = unsafe { &mut *(h as *mut XRead) };
    x.pos = (ms.max(0) as u64 * x.rate as u64 / 1000).min(x.total);
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetExtendedRead_close(h: isize) {
    drop(unsafe { Box::from_raw(h as *mut XRead) });
    log("in.log", "xread close");
}

// ----------------------------------------------------------------------
// Output

static mut OUT_MOD: Out_Module = Out_Module {
    version: OUT_VER,
    description: c"Ferramp Test Output v1".as_ptr(),
    id: 0x7E57,
    hMainWindow: null_mut(),
    hDllInstance: null_mut(),
    Config: Some(out_config),
    About: Some(out_about),
    Init: Some(out_init),
    Quit: Some(out_quit),
    Open: Some(out_open),
    Close: Some(out_close),
    Write: Some(out_write),
    CanWrite: Some(out_can_write),
    IsPlaying: Some(out_is_playing),
    Pause: Some(out_pause),
    SetVolume: Some(out_set_volume),
    SetPan: Some(out_set_pan),
    Flush: Some(out_flush),
    GetOutputTime: Some(out_time),
    GetWrittenTime: Some(out_time),
};

static OUT_BYTES: AtomicU64 = AtomicU64::new(0);
static OUT_BASE_MS: AtomicU64 = AtomicU64::new(0);
static OUT_BPMS: AtomicU64 = AtomicU64::new(176); // bytes per ms
static OUT_PAUSED: AtomicBool = AtomicBool::new(false);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetOutModule() -> *mut Out_Module {
    &raw mut OUT_MOD
}

unsafe extern "C" fn out_config(_: HWND) {
    log("out.log", "config");
}
unsafe extern "C" fn out_about(_: HWND) {
    log("out.log", "about");
}
unsafe extern "C" fn out_init() {
    log("out.log", "init");
}
unsafe extern "C" fn out_quit() {
    log("out.log", "quit");
}
unsafe extern "C" fn out_open(rate: c_int, ch: c_int, bps: c_int, _: c_int, _: c_int) -> c_int {
    OUT_BYTES.store(0, Ordering::SeqCst);
    OUT_BASE_MS.store(0, Ordering::SeqCst);
    OUT_BPMS.store(
        ((rate * ch * bps / 8) / 1000).max(1) as u64,
        Ordering::SeqCst,
    );
    log("out.log", &format!("open {rate} {ch} {bps}"));
    if let Ok(dir) = std::env::var("FERRAMP_TEST_DIR") {
        let _ = std::fs::write(format!("{dir}\\out.raw"), []);
    }
    0
}
unsafe extern "C" fn out_close() {
    log(
        "out.log",
        &format!("close bytes={}", OUT_BYTES.load(Ordering::SeqCst)),
    );
}
unsafe extern "C" fn out_write(buf: *mut c_char, len: c_int) -> c_int {
    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, len.max(0) as usize) };
    if let Ok(dir) = std::env::var("FERRAMP_TEST_DIR")
        && let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .open(format!("{dir}\\out.raw"))
    {
        let _ = f.write_all(data);
    }
    OUT_BYTES.fetch_add(data.len() as u64, Ordering::SeqCst);
    0
}
unsafe extern "C" fn out_can_write() -> c_int {
    if OUT_PAUSED.load(Ordering::SeqCst) {
        0
    } else {
        65536
    }
}
unsafe extern "C" fn out_is_playing() -> c_int {
    0
}
unsafe extern "C" fn out_pause(p: c_int) -> c_int {
    OUT_PAUSED.swap(p != 0, Ordering::SeqCst) as c_int
}
unsafe extern "C" fn out_set_volume(v: c_int) {
    log("out.log", &format!("volume {v}"));
}
unsafe extern "C" fn out_set_pan(p: c_int) {
    log("out.log", &format!("pan {p}"));
}
unsafe extern "C" fn out_flush(t: c_int) {
    OUT_BYTES.store(0, Ordering::SeqCst);
    OUT_BASE_MS.store(t.max(0) as u64, Ordering::SeqCst);
    log("out.log", &format!("flush {t}"));
}
unsafe extern "C" fn out_time() -> c_int {
    (OUT_BASE_MS.load(Ordering::SeqCst)
        + OUT_BYTES.load(Ordering::SeqCst) / OUT_BPMS.load(Ordering::SeqCst)) as c_int
}

// ----------------------------------------------------------------------
// DSP

static mut DSP_MODS: [winampDSPModule; 2] = [
    winampDSPModule {
        description: c"Invert samples".as_ptr(),
        hwndParent: null_mut(),
        hDllInstance: null_mut(),
        Config: Some(dsp_config),
        Init: Some(dsp_init),
        ModifySamples: Some(dsp_invert),
        Quit: Some(dsp_quit),
        userData: null_mut(),
    },
    winampDSPModule {
        description: c"Double every frame".as_ptr(),
        hwndParent: null_mut(),
        hDllInstance: null_mut(),
        Config: Some(dsp_config),
        Init: Some(dsp_init),
        ModifySamples: Some(dsp_double),
        Quit: Some(dsp_quit),
        userData: null_mut(),
    },
];

static mut DSP_HDR: winampDSPHeader = winampDSPHeader {
    version: DSP_HDRVER,
    description: c"Ferramp Test DSP v1".as_ptr(),
    getModule: Some(dsp_get_module),
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampDSPGetHeader2(_: HWND) -> *mut winampDSPHeader {
    &raw mut DSP_HDR
}

unsafe extern "C" fn dsp_get_module(i: c_int) -> *mut winampDSPModule {
    match i {
        0 | 1 => unsafe {
            (&raw mut DSP_MODS)
                .cast::<winampDSPModule>()
                .add(i as usize)
        },
        _ => null_mut(),
    }
}
unsafe extern "C" fn dsp_config(m: *mut winampDSPModule) {
    log(
        "dsp.log",
        &format!("config {}", unsafe {
            cstr((*m).description as *const c_void)
        }),
    );
}
unsafe extern "C" fn dsp_init(m: *mut winampDSPModule) -> c_int {
    let parent_ok = unsafe { !(*m).hwndParent.is_null() };
    log(
        "dsp.log",
        &format!("init {} parent={parent_ok}", unsafe {
            cstr((*m).description as *const c_void)
        }),
    );
    0
}
unsafe extern "C" fn dsp_quit(_: *mut winampDSPModule) {
    log("dsp.log", "quit");
}
unsafe extern "C" fn dsp_invert(
    _: *mut winampDSPModule,
    s: *mut c_short,
    n: c_int,
    bps: c_int,
    nch: c_int,
    _: c_int,
) -> c_int {
    if bps == 16 {
        for i in 0..(n * nch) as usize {
            unsafe { *s.add(i) = (*s.add(i)).saturating_neg() };
        }
    }
    n
}
unsafe extern "C" fn dsp_double(
    _: *mut winampDSPModule,
    s: *mut c_short,
    n: c_int,
    bps: c_int,
    nch: c_int,
    _: c_int,
) -> c_int {
    if bps != 16 {
        return n;
    }
    let ch = nch as usize;
    // Back to front so the source frames are read before they are overwritten.
    for i in (0..n as usize).rev() {
        for c in 0..ch {
            let v = unsafe { *s.add(i * ch + c) };
            unsafe {
                *s.add(2 * i * ch + c) = v;
                *s.add((2 * i + 1) * ch + c) = v;
            }
        }
    }
    n * 2
}

// ----------------------------------------------------------------------
// Visualization

static mut VIS_MOD: winampVisModule = winampVisModule {
    description: c"Spectrum logger".as_ptr(),
    hwndParent: null_mut(),
    hDllInstance: null_mut(),
    sRate: 0,
    nCh: 0,
    latencyMs: 25,
    delayMs: 25,
    spectrumNch: 2,
    waveformNch: 2,
    spectrumData: [[0; 576]; 2],
    waveformData: [[0; 576]; 2],
    Config: Some(vis_config),
    Init: Some(vis_init),
    Render: Some(vis_render),
    Quit: Some(vis_quit),
    userData: null_mut(),
};

static mut VIS_HDR: winampVisHeader = winampVisHeader {
    version: VIS_HDRVER,
    description: c"Ferramp Test Vis v1".as_ptr(),
    getModule: Some(vis_get_module),
};

static VIS_RENDERS: AtomicU64 = AtomicU64::new(0);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampVisGetHeader(_: HWND) -> *mut winampVisHeader {
    &raw mut VIS_HDR
}

unsafe extern "C" fn vis_get_module(i: c_int) -> *mut winampVisModule {
    if i == 0 { &raw mut VIS_MOD } else { null_mut() }
}
unsafe extern "C" fn vis_config(_: *mut winampVisModule) {
    log("vis.log", "config");
}
unsafe extern "C" fn vis_init(m: *mut winampVisModule) -> c_int {
    VIS_RENDERS.store(0, Ordering::SeqCst);
    let parent_ok = unsafe { !(*m).hwndParent.is_null() };
    log("vis.log", &format!("init parent={parent_ok}"));
    0
}
unsafe extern "C" fn vis_render(m: *mut winampVisModule) -> c_int {
    let n = VIS_RENDERS.fetch_add(1, Ordering::SeqCst) + 1;
    if n.is_multiple_of(20) {
        let m = unsafe { &*m };
        let spec: u64 = m.spectrumData[0].iter().map(|&v| v as u64).sum();
        let wave = m.waveformData[0]
            .iter()
            .map(|&v| (v as i8).unsigned_abs())
            .max()
            .unwrap_or(0);
        log(
            "vis.log",
            &format!("render {n} rate={} spectrum={spec} wave={wave}", m.sRate),
        );
    }
    let limit: u64 = std::env::var("FERRAMP_TEST_VIS_RENDERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(u64::MAX);
    (n >= limit) as c_int
}
unsafe extern "C" fn vis_quit(_: *mut winampVisModule) {
    log(
        "vis.log",
        &format!("quit renders={}", VIS_RENDERS.load(Ordering::SeqCst)),
    );
}

// ----------------------------------------------------------------------
// General purpose

static mut GEN: winampGeneralPurposePlugin = winampGeneralPurposePlugin {
    version: GPPHDR_VER,
    description: c"Ferramp Test General v1".as_ptr(),
    init: Some(gen_init),
    config: Some(gen_config),
    quit: Some(gen_quit),
    hwndParent: null_mut(),
    hDllInstance: null_mut(),
};

static OLD_PROC: AtomicU64 = AtomicU64::new(0);
static GEN_TICKS: AtomicU64 = AtomicU64::new(0);
const TIMER_ID: usize = 0x7E57;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn winampGetGeneralPurposePlugin() -> *mut winampGeneralPurposePlugin {
    &raw mut GEN
}

#[cfg(target_pointer_width = "64")]
unsafe fn set_wndproc(h: HWND, p: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWLP_WNDPROC, SetWindowLongPtrW};
    unsafe { SetWindowLongPtrW(h, GWLP_WNDPROC, p) }
}
#[cfg(target_pointer_width = "32")]
unsafe fn set_wndproc(h: HWND, p: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_WNDPROC, SetWindowLongW};
    unsafe { SetWindowLongW(h, GWL_WNDPROC, p as i32) as isize }
}

unsafe extern "system" fn gen_wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_WA_IPC {
        if l == ipc::PLAYING_FILE {
            log(
                "gen.log",
                &format!("playing_file {}", unsafe { cstr(w as *const c_void) }),
            );
        } else if l == ipc::CB_MISC {
            log("gen.log", &format!("cb_misc {w}"));
        }
    }
    let old: WNDPROC = unsafe { std::mem::transmute(OLD_PROC.load(Ordering::SeqCst) as usize) };
    unsafe { CallWindowProcW(old, h, msg, w, l) }
}

unsafe extern "system" fn gen_tick(h: HWND, _: u32, _: usize, _: u32) {
    let q = |w: usize, l: isize| unsafe { SendMessageW(h, WM_WA_IPC, w, l) };
    let tick = GEN_TICKS.fetch_add(1, Ordering::SeqCst);
    if tick == 0
        && let Ok(cmd) = std::env::var("FERRAMP_TEST_GEN_CMD")
    {
        let id = match cmd.as_str() {
            "next" => cmd::BUTTON5,
            "prev" => cmd::BUTTON1,
            "stop" => cmd::BUTTON4,
            "pause" => cmd::BUTTON3,
            _ => cmd::BUTTON2,
        };
        unsafe { PostMessageW(h, WM_COMMAND, id as usize, 0) };
        log("gen.log", &format!("sent {cmd}"));
    }
    let pos = q(0, ipc::GETLISTPOS);
    let file = unsafe { cstr(q(pos as usize, ipc::GETPLAYLISTFILE) as *const c_void) };
    let title = unsafe { cstr(q(pos as usize, ipc::GETPLAYLISTTITLE) as *const c_void) };
    log(
        "gen.log",
        &format!(
            "tick {tick} version={:#x} status={} pos_ms={} len_s={} list={}/{} vol={} shuffle={} repeat={} rate={} file={file} title={title}",
            q(0, ipc::GETVERSION),
            q(0, ipc::ISPLAYING),
            q(0, ipc::GETOUTPUTTIME),
            q(1, ipc::GETOUTPUTTIME),
            pos,
            q(0, ipc::GETLISTLENGTH),
            q(-666isize as usize, ipc::SETVOLUME),
            q(0, ipc::GET_SHUFFLE),
            q(0, ipc::GET_REPEAT),
            q(0, ipc::GETINFO),
        ),
    );
}

unsafe extern "C" fn gen_init() -> c_int {
    let h = unsafe { (*(&raw const GEN)).hwndParent };
    let ini = unsafe { cstr(SendMessageW(h, WM_WA_IPC, 0, ipc::GETINIFILE) as *const c_void) };
    log(
        "gen.log",
        &format!("init parent={} ini={ini}", !h.is_null()),
    );
    let old = unsafe { set_wndproc(h, gen_wndproc as *const () as isize) };
    OLD_PROC.store(old as u64, Ordering::SeqCst);
    unsafe { SetTimer(h, TIMER_ID, 300, Some(gen_tick)) };
    0
}
unsafe extern "C" fn gen_config() {
    log("gen.log", "config");
}
unsafe extern "C" fn gen_quit() {
    let h = unsafe { (*(&raw const GEN)).hwndParent };
    unsafe {
        KillTimer(h, TIMER_ID);
        set_wndproc(h, OLD_PROC.load(Ordering::SeqCst) as isize);
    }
    log("gen.log", "quit");
}
