//! Starting `ferramp-plughost` (directly on Windows, under Wine elsewhere)
//! and talking to it: requests with replies, fire-and-forget messages, and
//! a stream of events (PCM for open streams, commands from plugins).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferramp_plugproto::{FromHost, ToHost};

/// Which build of the host to run: 32-bit plugins need the x86 host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arch {
    X86,
    X64,
}

impl Arch {
    pub fn exe_name(self) -> &'static str {
        match self {
            Arch::X86 => "ferramp-plughost-x86.exe",
            Arch::X64 => "ferramp-plughost-x64.exe",
        }
    }
}

/// The CPU a plugin DLL was built for, from its PE header.
pub fn dll_arch(path: &Path) -> Option<Arch> {
    let data = std::fs::read(path).ok()?;
    if data.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32::from_le_bytes(data.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if data.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    match u16::from_le_bytes(data.get(pe + 4..pe + 6)?.try_into().ok()?) {
        0x014C => Some(Arch::X86),
        0x8664 => Some(Arch::X64),
        _ => None,
    }
}

/// Finds the host executable for `arch`: `$FERRAMP_PLUGHOST_DIR`, next to
/// Ferramp, the packaged library folder, or a development build.
pub fn find_host(arch: Arch) -> Option<PathBuf> {
    let name = arch.exe_name();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("FERRAMP_PLUGHOST_DIR") {
        dirs.push(d.into());
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_path_buf());
        dirs.push(dir.join("../lib/ferramp"));
        dirs.push(dir.join("../Resources"));
    }
    dirs.push("/usr/lib/ferramp".into());
    for d in &dirs {
        let p = d.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    // A development checkout: target/<windows target>/<profile>/ferramp-plughost.exe.
    // Only in debug and test builds: a release build must not run programs
    // from the path it happened to be compiled in.
    #[cfg(any(debug_assertions, test))]
    {
        let triple = match arch {
            Arch::X86 => "i686-pc-windows-gnu",
            Arch::X64 => "x86_64-pc-windows-gnu",
        };
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        for t in [triple, &triple.replace("-gnu", "-msvc")] {
            for profile in ["release", "debug"] {
                let p = root.join(t).join(profile).join("ferramp-plughost.exe");
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Finds Wine on systems that need it: `$FERRAMP_WINE`, `wine` on the
/// path, or the usual macOS application bundles.
pub fn find_wine() -> Option<PathBuf> {
    if cfg!(windows) {
        return None;
    }
    if let Some(w) = std::env::var_os("FERRAMP_WINE") {
        return Some(w.into());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for name in ["wine", "wine64"] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    for p in [
        "/Applications/Wine Stable.app/Contents/Resources/wine/bin/wine",
        "/Applications/Wine Staging.app/Contents/Resources/wine/bin/wine",
        "/Applications/Wine Devel.app/Contents/Resources/wine/bin/wine",
        "/Applications/CrossOver.app/Contents/SharedSupport/CrossOver/bin/wine",
        "/opt/homebrew/bin/wine",
        "/usr/local/bin/wine",
    ] {
        if Path::new(p).is_file() {
            return Some(p.into());
        }
    }
    None
}

/// Copies the host into `dir` unless an identical copy is there already.
fn place_copy(exe: &Path, dir: &Path, arch: Arch) -> Option<PathBuf> {
    let target = dir.join(arch.exe_name());
    let src = std::fs::read(exe).ok()?;
    if std::fs::read(&target).ok().as_deref() != Some(&src[..]) {
        std::fs::create_dir_all(dir).ok()?;
        let part = dir.join(format!("{}.part", arch.exe_name()));
        std::fs::write(&part, &src).ok()?;
        std::fs::rename(&part, &target).ok()?;
    }
    Some(target)
}

type Pending = Arc<Mutex<HashMap<u32, Sender<FromHost>>>>;

pub struct Host {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU32,
    alive: Arc<AtomicBool>,
}

#[derive(Debug)]
pub enum HostError {
    NotFound(String),
    Dead,
    Timeout,
    Failed(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::NotFound(s) => write!(f, "{s}"),
            HostError::Dead => write!(f, "the plugin host stopped"),
            HostError::Timeout => write!(f, "the plugin host did not answer"),
            HostError::Failed(s) => write!(f, "{s}"),
        }
    }
}

impl Host {
    /// Starts the host; `events` receives every message with id 0.
    ///
    /// With `run_dir`, the host runs from a copy placed there, with that
    /// folder as its working directory: plugins find "the player's folder"
    /// from the program's own path and expect their `Plugins` folder (and
    /// the libraries next to it) there.
    pub fn spawn(
        arch: Arch,
        log: Option<&Path>,
        run_dir: Option<&Path>,
        events: Sender<FromHost>,
    ) -> Result<Host, HostError> {
        let mut exe = find_host(arch).ok_or_else(|| {
            HostError::NotFound(format!(
                "the plugin host ({}) is not installed",
                arch.exe_name()
            ))
        })?;
        if let Some(dir) = run_dir
            && let Some(copy) = place_copy(&exe, dir, arch)
        {
            exe = copy;
        }
        let mut cmd = if cfg!(windows) {
            Command::new(&exe)
        } else {
            let wine = find_wine().ok_or_else(|| {
                HostError::NotFound("Wine is needed to run Windows plugins on this system".into())
            })?;
            let mut c = Command::new(wine);
            c.arg(&exe);
            if std::env::var_os("WINEDEBUG").is_none() {
                c.env("WINEDEBUG", "-all");
            }
            c
        };
        let stderr = match log.and_then(|p| std::fs::File::create(p).ok()) {
            Some(f) => Stdio::from(f),
            None => Stdio::null(),
        };
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| HostError::Failed(format!("could not start the plugin host: {e}")))?;
        let stdin = child.stdin.take().ok_or(HostError::Dead)?;
        let mut stdout = child.stdout.take().ok_or(HostError::Dead)?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (p, a) = (pending.clone(), alive.clone());
        std::thread::Builder::new()
            .name("plughost-reader".into())
            .spawn(move || {
                loop {
                    match ferramp_plugproto::read_frame::<FromHost>(&mut stdout) {
                        Ok(Some((0, msg))) => {
                            if events.send(msg).is_err() {
                                break;
                            }
                        }
                        Ok(Some((id, msg))) => {
                            if let Some(tx) = p.lock().unwrap().remove(&id) {
                                let _ = tx.send(msg);
                            }
                        }
                        _ => break,
                    }
                }
                a.store(false, Ordering::SeqCst);
                // Wake everyone still waiting for a reply.
                p.lock().unwrap().clear();
            })
            .map_err(|e| HostError::Failed(e.to_string()))?;
        Ok(Host {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU32::new(1),
            alive,
        })
    }

    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn write(&self, id: u32, msg: &ToHost) -> Result<(), HostError> {
        if !self.alive() {
            return Err(HostError::Dead);
        }
        let frame = ferramp_plugproto::encode(id, msg);
        let mut w = self.stdin.lock().unwrap();
        w.write_all(&frame)
            .and_then(|_| w.flush())
            .map_err(|_| HostError::Dead)
    }

    /// Fire and forget.
    pub fn send(&self, msg: &ToHost) -> Result<(), HostError> {
        self.write(0, msg)
    }

    /// Sends a request and waits for its reply. `Failed` replies become
    /// errors.
    pub fn request(&self, msg: &ToHost, timeout: Duration) -> Result<FromHost, HostError> {
        let rx = self.request_async(msg)?;
        match rx.recv_timeout(timeout) {
            Ok(FromHost::Failed(e)) => Err(HostError::Failed(e)),
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => Err(HostError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(HostError::Dead),
        }
    }

    /// Sends a request; the reply arrives on the returned channel.
    pub fn request_async(&self, msg: &ToHost) -> Result<Receiver<FromHost>, HostError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst).max(1);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if let Err(e) = self.write(id, msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        Ok(rx)
    }

    /// Ends the host at once (it hung).
    pub fn kill(&self) {
        let mut c = self.child.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
        self.alive.store(false, Ordering::SeqCst);
    }

    pub fn shutdown(&self) {
        let _ = self.request(&ToHost::Shutdown, Duration::from_secs(3));
        let mut c = self.child.lock().unwrap();
        for _ in 0..20 {
            if matches!(c.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = c.kill();
        let _ = c.wait();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let mut c = self.child.lock().unwrap();
        if matches!(c.try_wait(), Ok(None)) {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pe_machine() {
        let mut dll = vec![0u8; 0x200];
        dll[0..2].copy_from_slice(b"MZ");
        dll[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        dll[0x80..0x84].copy_from_slice(b"PE\0\0");
        dll[0x84..0x86].copy_from_slice(&0x014Cu16.to_le_bytes());
        let dir = std::env::temp_dir().join(format!("ferramp-pe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("in_x.dll");
        std::fs::write(&p, &dll).unwrap();
        assert_eq!(dll_arch(&p), Some(Arch::X86));
        dll[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
        std::fs::write(&p, &dll).unwrap();
        assert_eq!(dll_arch(&p), Some(Arch::X64));
        std::fs::write(&p, b"not a dll").unwrap();
        assert_eq!(dll_arch(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
