//! Driving an output plugin when Ferramp plays through one. Writes go
//! through a dedicated thread that waits for `CanWrite`, as an input
//! plugin's decoding thread would; status queries are answered directly.

use std::collections::VecDeque;
use std::ffi::{c_char, c_int};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use ferramp_plugproto::FromHost;
use ferramp_winampabi::Out_Module;

use super::registry::{self, Data};
use super::{Ptr, send};

static ACTIVE: Mutex<Option<Ptr<Out_Module>>> = Mutex::new(None);
/// Bytes accepted from the player but not yet written to the plugin.
static QUEUED: AtomicU64 = AtomicU64::new(0);

enum Job {
    Write(Vec<u8>),
    Flush(c_int),
}

fn writer() -> &'static Mutex<Sender<Job>> {
    static TX: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            let mut pending: VecDeque<u8> = VecDeque::new();
            loop {
                let job = if pending.is_empty() {
                    match rx.recv() {
                        Ok(j) => Some(j),
                        Err(_) => return,
                    }
                } else {
                    rx.try_recv().ok()
                };
                match job {
                    Some(Job::Write(d)) => pending.extend(d),
                    Some(Job::Flush(t)) => {
                        QUEUED.fetch_sub(pending.len() as u64, Ordering::SeqCst);
                        pending.clear();
                        if let Some(m) = *ACTIVE.lock().unwrap() {
                            unsafe {
                                if let Some(f) = (*m.0).Flush {
                                    f(t);
                                }
                            }
                        }
                    }
                    None => {}
                }
                if pending.is_empty() {
                    continue;
                }
                let Some(m) = *ACTIVE.lock().unwrap() else {
                    QUEUED.fetch_sub(pending.len() as u64, Ordering::SeqCst);
                    pending.clear();
                    continue;
                };
                let room = unsafe { (*m.0).CanWrite.map(|f| f()).unwrap_or(0) }.max(0) as usize;
                if room < 1024.min(pending.len()) {
                    std::thread::sleep(Duration::from_millis(3));
                    continue;
                }
                let n = room.min(pending.len()).min(65536);
                let mut chunk: Vec<u8> = pending.drain(..n).collect();
                QUEUED.fetch_sub(n as u64, Ordering::SeqCst);
                unsafe {
                    if let Some(f) = (*m.0).Write {
                        f(chunk.as_mut_ptr() as *mut c_char, n as c_int);
                    }
                }
            }
        });
        Mutex::new(tx)
    })
}

pub fn open(id: u32, plugin: u32, rate: u32, channels: u32, bits: u32) {
    close();
    let Some(Data::Output(m)) = registry::data(plugin) else {
        send(id, &FromHost::Failed("not an output plugin".into()));
        return;
    };
    let latency = unsafe {
        match (*m.0).Open {
            Some(f) => f(rate as c_int, channels as c_int, bits as c_int, -1, -1),
            None => -1,
        }
    };
    if latency < 0 {
        send(
            id,
            &FromHost::Failed("the output plugin could not open the device".into()),
        );
        return;
    }
    *ACTIVE.lock().unwrap() = Some(m);
    send(
        id,
        &FromHost::OutOpened {
            latency_ms: latency,
        },
    );
}

pub fn write(data: Vec<u8>) {
    QUEUED.fetch_add(data.len() as u64, Ordering::SeqCst);
    let _ = writer().lock().unwrap().send(Job::Write(data));
}

pub fn query(id: u32) {
    let m = *ACTIVE.lock().unwrap();
    let msg = match m {
        Some(m) => unsafe {
            let can = (*m.0).CanWrite.map(|f| f()).unwrap_or(0).max(0) as u64;
            let queued = QUEUED.load(Ordering::SeqCst);
            FromHost::OutStatus {
                can_write: can.saturating_sub(queued) as u32,
                output_ms: (*m.0).GetOutputTime.map(|f| f()).unwrap_or(0).max(0) as u32,
                written_ms: (*m.0).GetWrittenTime.map(|f| f()).unwrap_or(0).max(0) as u32,
                playing: (*m.0).IsPlaying.map(|f| f()).unwrap_or(0) != 0 || queued > 0,
            }
        },
        None => FromHost::Failed("no output open".into()),
    };
    send(id, &msg);
}

pub fn pause(p: bool) {
    if let Some(m) = *ACTIVE.lock().unwrap() {
        unsafe {
            if let Some(f) = (*m.0).Pause {
                f(p as c_int);
            }
        }
    }
}

pub fn flush(ms: u32) {
    let _ = writer().lock().unwrap().send(Job::Flush(ms as c_int));
}

pub fn volume(v: i32) {
    if let Some(m) = *ACTIVE.lock().unwrap() {
        unsafe {
            if let Some(f) = (*m.0).SetVolume {
                f(v.clamp(0, 255));
            }
        }
    }
}

pub fn close() {
    let m = ACTIVE.lock().unwrap().take();
    if let Some(m) = m {
        unsafe {
            if let Some(f) = (*m.0).Close {
                f();
            }
        }
    }
}
