//! The active DSP/effect module. The player sends blocks of 16-bit PCM;
//! a dedicated thread runs them through `ModifySamples` (as the original
//! player did from its decoding thread) and sends the result back.

use std::ffi::c_int;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};

use ferramp_plugproto::FromHost;
use ferramp_winampabi::winampDSPModule;

use super::registry::{self, Data};
use super::{Ptr, main_hwnd, send};

/// Held while the module processes, so stopping waits for a block to end.
static ACTIVE: Mutex<Option<Ptr<winampDSPModule>>> = Mutex::new(None);

type Job = (u32, u32, u32, Vec<i16>);

fn worker() -> &'static Mutex<Sender<Job>> {
    static TX: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            for (id, rate, ch, samples) in rx {
                let out = run(rate, ch, samples);
                send(id, &FromHost::DspOut(out));
            }
        });
        Mutex::new(tx)
    })
}

fn run(rate: u32, ch: u32, mut samples: Vec<i16>) -> Vec<i16> {
    let guard = ACTIVE.lock().unwrap();
    let Some(m) = *guard else { return samples };
    let ch = ch.max(1) as usize;
    let frames = samples.len() / ch;
    if frames == 0 {
        return samples;
    }
    let original = samples.clone();
    // Plugins may return up to twice as many frames as they were given.
    samples.resize(frames * ch * 2, 0);
    let n = unsafe {
        match (*m.0).ModifySamples {
            Some(f) => f(
                m.0,
                samples.as_mut_ptr(),
                frames as c_int,
                16,
                ch as c_int,
                rate as c_int,
            ),
            None => frames as c_int,
        }
    };
    if n < 0 || n as usize > frames * 2 {
        // A broken return value; pass the block through unchanged.
        return original;
    }
    samples.truncate(n as usize * ch);
    samples
}

pub fn process(id: u32, rate: u32, channels: u32, samples: Vec<i16>) {
    let _ = worker().lock().unwrap().send((id, rate, channels, samples));
}

pub fn start(plugin: u32, module: u32) -> Result<(), String> {
    stop();
    let Some(Data::Dsp(hdr)) = registry::data(plugin) else {
        return Err("not a DSP plugin".into());
    };
    unsafe {
        let gm = (*hdr.0).getModule.ok_or("the plugin has no modules")?;
        let m = gm(module as c_int);
        if m.is_null() {
            return Err("no such module".into());
        }
        (*m).hwndParent = main_hwnd();
        (*m).hDllInstance = registry::hmod(plugin);
        if let Some(init) = (*m).Init
            && init(m) != 0
        {
            return Err("the module failed to start".into());
        }
        *ACTIVE.lock().unwrap() = Some(Ptr(m));
    }
    Ok(())
}

pub fn stop() {
    let m = ACTIVE.lock().unwrap().take();
    if let Some(m) = m {
        unsafe {
            if let Some(q) = (*m.0).Quit {
                q(m.0);
            }
        }
    }
}
