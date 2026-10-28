//! The active visualization module, run on its own thread with its own
//! message loop, as the original player did: Init, then Render every
//! `delayMs` with fresh spectrum and waveform data, then Quit.

use std::ffi::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ferramp_plugproto::FromHost;
use ferramp_winampabi::winampVisModule;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage, WM_QUIT,
};

use super::registry::{self, Data};
use super::{Ptr, event, main_hwnd};

struct Pcm {
    rate: u32,
    channels: u32,
    /// Interleaved, newest last.
    samples: Vec<i16>,
}

static PCM: Mutex<Pcm> = Mutex::new(Pcm {
    rate: 44100,
    channels: 2,
    samples: Vec::new(),
});

struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

pub fn pcm(rate: u32, channels: u32, samples: Vec<i16>) {
    let mut p = PCM.lock().unwrap();
    p.rate = rate;
    p.channels = channels.max(1);
    p.samples = samples;
}

pub fn start(plugin: u32, module: u32) {
    stop();
    let Some(Data::Vis(hdr)) = registry::data(plugin) else {
        event(FromHost::VisClosed);
        return;
    };
    let hinst = Ptr(registry::hmod(plugin));
    let parent = Ptr(main_hwnd());
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let thread = std::thread::spawn(move || unsafe {
        // Move the wrappers in whole; their raw fields alone are not Send.
        let (hinst, parent, hdr) = (hinst, parent, hdr);
        let Some(gm) = (*hdr.0).getModule else {
            event(FromHost::VisClosed);
            return;
        };
        let m = gm(module as c_int);
        if m.is_null() {
            event(FromHost::VisClosed);
            return;
        }
        (*m).hwndParent = parent.0;
        (*m).hDllInstance = hinst.0;
        {
            let p = PCM.lock().unwrap();
            (*m).sRate = p.rate as c_int;
            (*m).nCh = p.channels as c_int;
        }
        if (*m).Init.map(|f| f(m)).unwrap_or(1) != 0 {
            event(FromHost::VisClosed);
            return;
        }
        let mut msg: MSG = std::mem::zeroed();
        let mut ana = Analyzer::new();
        'run: loop {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    break 'run;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if s.load(Ordering::SeqCst) {
                break;
            }
            fill(&mut *m, &mut ana);
            if (*m).Render.map(|f| f(m)).unwrap_or(1) != 0 {
                break;
            }
            let delay = (*m).delayMs.clamp(5, 200);
            std::thread::sleep(Duration::from_millis(delay as u64));
        }
        if let Some(q) = (*m).Quit {
            q(m);
        }
        event(FromHost::VisClosed);
    });
    *RUNNING.lock().unwrap() = Some(Running {
        stop,
        thread: Some(thread),
    });
}

pub fn stop() {
    let r = RUNNING.lock().unwrap().take();
    if let Some(mut r) = r {
        r.stop.store(true, Ordering::SeqCst);
        if let Some(t) = r.thread.take() {
            let _ = t.join();
        }
    }
}

const N: usize = 1024;

/// The player's own analyzer data, for `IPC_GETSADATAFUNC`: 75 spectrum
/// bars (0..40) followed by 75 oscilloscope points (signed, about -32..32),
/// from the newest PCM. All zero when nothing is playing.
pub fn sa_data(out: &mut [u8]) {
    out.iter_mut().for_each(|b| *b = 0);
    let (ch, samples) = {
        let p = PCM.lock().unwrap();
        (p.channels.max(1) as usize, p.samples.clone())
    };
    let frames = samples.len() / ch;
    if frames == 0 || out.len() < 150 {
        return;
    }
    let mono: Vec<f32> = (0..frames)
        .map(|f| {
            samples[f * ch..f * ch + ch]
                .iter()
                .map(|&s| s as f32)
                .sum::<f32>()
                / (ch as f32 * 32768.0)
        })
        .collect();
    let mut spec = [0u8; 576];
    Analyzer::new().spectrum(&mono, &mut spec);
    // The lower bands carry the music; spread them over 75 bars.
    for (i, o) in out[..75].iter_mut().enumerate() {
        let (a, b) = (i * 400 / 75, (i + 1) * 400 / 75);
        let peak = spec[a..b.max(a + 1)].iter().copied().max().unwrap_or(0);
        *o = (peak as u32 * 40 / 255) as u8;
    }
    let start = frames.saturating_sub(576);
    for (i, o) in out[75..150].iter_mut().enumerate() {
        let v = mono.get(start + i * 576 / 75).copied().unwrap_or(0.0);
        *o = ((v * 32.0).clamp(-127.0, 127.0) as i8) as u8;
    }
}

/// `IPC_GETVUDATAFUNC`: the level of a channel, 0..255.
pub fn vu(channel: usize) -> Option<u8> {
    let p = PCM.lock().unwrap();
    let ch = p.channels.max(1) as usize;
    if channel >= ch.max(2) {
        return None;
    }
    let c = channel.min(ch - 1);
    let tail = p.samples.len().saturating_sub(576 * ch);
    let peak = p.samples[tail..]
        .iter()
        .skip(c)
        .step_by(ch)
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap_or(0);
    Some((peak as u32 * 255 / 32768) as u8)
}

/// A radix-2 FFT for the 576-band spectrum the modules expect.
struct Analyzer {
    window: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Analyzer {
    fn new() -> Analyzer {
        let window = (0..N)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos())
            .collect();
        Analyzer {
            window,
            re: vec![0.0; N],
            im: vec![0.0; N],
        }
    }

    /// Fills 576 spectrum bytes from `x` (mono, newest last, -1..1).
    fn spectrum(&mut self, x: &[f32], out: &mut [u8; 576]) {
        let start = x.len().saturating_sub(N);
        for i in 0..N {
            self.re[i] = x.get(start + i).copied().unwrap_or(0.0) * self.window[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);
        for (b, o) in out.iter_mut().enumerate() {
            // 576 bands over the lower half of the 1024-point spectrum.
            let k = 1 + b * (N / 2 - 1) / 576;
            let mag = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() / (N as f32 / 4.0);
            let db = 20.0 * mag.max(1e-6).log10();
            *o = (((db + 70.0) / 70.0) * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
}

fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

fn fill(m: &mut winampVisModule, ana: &mut Analyzer) {
    let (rate, ch, samples) = {
        let p = PCM.lock().unwrap();
        (p.rate, p.channels as usize, p.samples.clone())
    };
    m.sRate = rate as c_int;
    m.nCh = ch as c_int;
    let frames = samples.len() / ch;
    for c in 0..2 {
        let cc = c.min(ch - 1);
        let chan: Vec<f32> = (0..frames)
            .map(|f| samples[f * ch + cc] as f32 / 32768.0)
            .collect();
        let wstart = frames.saturating_sub(576);
        for i in 0..576 {
            let v = chan.get(wstart + i).copied().unwrap_or(0.0);
            m.waveformData[c][i] = ((v * 127.0) as i8) as u8;
        }
        ana.spectrum(&chan, &mut m.spectrumData[c]);
    }
}
