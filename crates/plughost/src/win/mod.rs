//! The Windows side of the host: stdio protocol, the player window that
//! plugins talk to, and dispatch of requests to the plugin modules.
//!
//! Threads: the main thread owns the player window and calls plugins the
//! way the original player did (load, Init, Config, Play, Stop...). A
//! reader thread decodes requests; latency-sensitive ones (credits, DSP
//! blocks, visualization PCM, output writes) are handled off the main
//! thread, the rest are queued and the window is poked to run them.

mod dsp;
mod general;
mod input;
mod menu;
mod output;
mod registry;
pub mod text;
mod vis;
mod wasabi;

use std::collections::VecDeque;
use std::fs::File;
use std::os::windows::io::FromRawHandle;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, OnceLock};

use ferramp_plugproto::{FromHost, PROTOCOL, ToHost};
use ferramp_winampabi::{HWND, PLAYER_CLASS, WM_WA_IPC, WM_WA_MPEG_EOF};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, LPARAM, LRESULT,
    WPARAM,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_COPYDATA,
    WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use text::wide;

/// A raw pointer that may cross threads; plugin modules are process-wide.
pub struct Ptr<T>(pub *mut T);
unsafe impl<T> Send for Ptr<T> {}
unsafe impl<T> Sync for Ptr<T> {}
impl<T> Clone for Ptr<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Ptr<T> {}

const WM_APP_WORK: u32 = WM_APP + 1;
const WM_APP_STATE: u32 = WM_APP + 2;
const WM_APP_QUIT: u32 = WM_APP + 3;

static OUT: OnceLock<Mutex<File>> = OnceLock::new();
static MAIN: AtomicIsize = AtomicIsize::new(0);
static QUEUE: Mutex<VecDeque<(u32, ToHost)>> = Mutex::new(VecDeque::new());

pub fn send(id: u32, msg: &FromHost) {
    if let Some(out) = OUT.get()
        && let Ok(mut f) = out.lock()
        && ferramp_plugproto::write_frame(&mut *f, id, msg).is_err()
    {
        // The player went away; nothing left to do.
        std::process::exit(0);
    }
}

pub fn event(msg: FromHost) {
    send(0, &msg);
}

pub fn log(s: impl Into<String>) {
    event(FromHost::Log(s.into()));
}

pub fn main_hwnd() -> HWND {
    MAIN.load(Ordering::SeqCst) as HWND
}

pub fn run() -> i32 {
    let (input, output) = unsafe { take_std_handles() };
    let _ = OUT.set(Mutex::new(output));
    unsafe { install_crash_handler() };
    let Some(hwnd) = (unsafe { create_main_window() }) else {
        return 1;
    };
    MAIN.store(hwnd as isize, Ordering::SeqCst);
    std::thread::spawn(move || reader(input));
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    0
}

/// Keeps private copies of the stdio pipes for the protocol and points
/// every standard handle (including those of already loaded C runtimes) at
/// NUL, so a plugin that prints cannot corrupt the stream.
unsafe fn take_std_handles() -> (File, File) {
    unsafe {
        let me = GetCurrentProcess();
        let dup = |h: HANDLE| {
            let mut out: HANDLE = std::ptr::null_mut();
            DuplicateHandle(me, h, me, &mut out, 0, 0, DUPLICATE_SAME_ACCESS);
            out
        };
        let pin = dup(GetStdHandle(STD_INPUT_HANDLE));
        let pout = dup(GetStdHandle(STD_OUTPUT_HANDLE));
        let nul = CreateFileW(
            wide("NUL").as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        SetStdHandle(STD_INPUT_HANDLE, nul);
        SetStdHandle(STD_OUTPUT_HANDLE, nul);
        type Open = unsafe extern "C" fn(*const u8, i32) -> i32;
        type Dup2 = unsafe extern "C" fn(i32, i32) -> i32;
        for crt in ["msvcrt.dll", "ucrtbase.dll"] {
            let m = GetModuleHandleW(wide(crt).as_ptr());
            if m.is_null() {
                continue;
            }
            let (Some(open), Some(dup2)) = (
                GetProcAddress(m, c"_open".as_ptr().cast()),
                GetProcAddress(m, c"_dup2".as_ptr().cast()),
            ) else {
                continue;
            };
            let open: Open = std::mem::transmute(open);
            let dup2: Dup2 = std::mem::transmute(dup2);
            let w = open(c"NUL".as_ptr().cast(), 1);
            if w >= 0 {
                dup2(w, 1);
            }
            let r = open(c"NUL".as_ptr().cast(), 0);
            if r >= 0 {
                dup2(r, 0);
            }
        }
        (
            File::from_raw_handle(pin as _),
            File::from_raw_handle(pout as _),
        )
    }
}

/// A plugin that crashes must not leave a crash dialog keeping the host
/// alive: report where it happened and exit, so Ferramp can restart the
/// host without that plugin.
unsafe fn install_crash_handler() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        EXCEPTION_POINTERS, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        SetUnhandledExceptionFilter,
    };
    unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
        unsafe {
            let rec = &*(*info).ExceptionRecord;
            let addr = rec.ExceptionAddress as usize;
            let module = module_at(addr);
            event(FromHost::Log(format!(
                "crash: exception {:#010x} at {addr:#x} in {module}",
                rec.ExceptionCode as u32
            )));
        }
        std::process::exit(3);
    }
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
        SetUnhandledExceptionFilter(Some(filter));
    }
}

/// The file name of the module containing `addr`.
unsafe fn module_at(addr: usize) -> String {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    unsafe {
        let mut h = std::ptr::null_mut();
        if GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            addr as *const u16,
            &mut h,
        ) == 0
        {
            return "unknown module".into();
        }
        let mut buf = [0u16; 520];
        let n = GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as u32) as usize;
        let path = String::from_utf16_lossy(&buf[..n]);
        let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
        format!("{name}+{:#x}", addr - h as usize)
    }
}

unsafe fn create_main_window() -> Option<HWND> {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let class = wide(PLAYER_CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let h = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Winamp 5.66").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            275,
            116,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        (!h.is_null()).then_some(h)
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_APP_WORK => {
            work();
            0
        }
        WM_APP_STATE => {
            general::notify(h);
            0
        }
        WM_APP_QUIT => {
            registry::quit_all();
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_WA_IPC => general::ipc(h, w, l),
        WM_WA_MPEG_EOF => {
            input::eof();
            0
        }
        WM_COMMAND => {
            general::command(w);
            0
        }
        WM_COPYDATA => general::copydata(l),
        // Plugins sometimes "close the player"; that is the player's call.
        WM_CLOSE => 0,
        _ => unsafe { DefWindowProcW(h, msg, w, l) },
    }
}

fn post(msg: u32) {
    unsafe { PostMessageW(main_hwnd(), msg, 0, 0) };
}

fn reader(mut f: File) {
    loop {
        match ferramp_plugproto::read_frame::<ToHost>(&mut f) {
            Ok(Some((id, msg))) => route(id, msg),
            _ => {
                post(WM_APP_QUIT);
                return;
            }
        }
    }
}

/// Runs on the reader thread.
fn route(id: u32, msg: ToHost) {
    match msg {
        ToHost::Credit { stream, bytes } => input::credit(stream, bytes),
        ToHost::Played { stream, ms } => input::played(stream, ms),
        ToHost::VisPcm {
            rate,
            channels,
            samples,
        } => vis::pcm(rate, channels, samples),
        ToHost::State(s) => {
            general::set_state(s);
            post(WM_APP_STATE);
        }
        ToHost::Playlist(p) => {
            general::set_playlist(p);
            post(WM_APP_STATE);
        }
        ToHost::DspProcess {
            rate,
            channels,
            samples,
        } => dsp::process(id, rate, channels, samples),
        ToHost::OutWrite(data) => output::write(data),
        ToHost::OutQuery => output::query(id),
        other => {
            QUEUE.lock().unwrap().push_back((id, other));
            post(WM_APP_WORK);
        }
    }
}

/// Runs on the main thread, possibly re-entered from a plugin's modal
/// dialog loop; the queue lock is never held while a plugin runs.
fn work() {
    loop {
        let next = QUEUE.lock().unwrap().pop_front();
        let Some((id, msg)) = next else { return };
        handle(id, msg);
    }
}

fn reply(id: u32, msg: FromHost) {
    if id != 0 {
        send(id, &msg);
    }
}

fn handle(id: u32, msg: ToHost) {
    match msg {
        ToHost::Hello {
            protocol: _,
            ini_dir,
            plugin_dirs,
            skip,
            inputs_only,
        } => {
            registry::set_dirs(&ini_dir, &plugin_dirs, skip, inputs_only);
            reply(
                id,
                FromHost::HelloOk {
                    protocol: PROTOCOL,
                    arch: format!(
                        "{}{}",
                        if cfg!(target_pointer_width = "64") {
                            "x64"
                        } else {
                            "x86"
                        },
                        if text::under_wine() { " (Wine)" } else { "" }
                    ),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            );
        }
        ToHost::Scan => reply(id, FromHost::Plugins(registry::scan())),
        ToHost::Configure { plugin, module } => {
            reply(id, FromHost::Done);
            registry::configure(plugin, module);
        }
        ToHost::About { plugin, module } => {
            reply(id, FromHost::Done);
            registry::about(plugin, module);
        }
        ToHost::MenuQuery => reply(id, FromHost::Menu(menu::entries(main_hwnd()))),
        ToHost::MenuCommand { id: cmd } => {
            reply(id, FromHost::Done);
            menu::command(main_hwnd(), cmd);
        }
        ToHost::IsOurFile { path } => reply(id, FromHost::Plugin(registry::is_our_file(&path))),
        ToHost::FileInfo { plugin, path } => match registry::file_info(plugin, &path) {
            Some(info) => reply(id, FromHost::Info(info)),
            None => reply(id, FromHost::Failed("no information".into())),
        },
        ToHost::InfoBox { plugin, path } => {
            reply(id, FromHost::Done);
            registry::info_box(plugin, &path);
        }
        ToHost::Open {
            stream,
            plugin,
            path,
            credit,
        } => input::open(stream, plugin, &path, credit),
        ToHost::Seek { stream, ms } => input::seek(stream, ms),
        ToHost::Close { stream } => input::close(stream),
        ToHost::DspStart { plugin, module } => match dsp::start(plugin, module) {
            Ok(()) => reply(id, FromHost::Done),
            Err(e) => reply(id, FromHost::Failed(e)),
        },
        ToHost::DspStop => {
            dsp::stop();
            reply(id, FromHost::Done);
        }
        ToHost::VisStart { plugin, module } => {
            vis::start(plugin, module);
            reply(id, FromHost::Done);
        }
        ToHost::VisStop => {
            vis::stop();
            reply(id, FromHost::Done);
        }
        ToHost::OutOpen {
            plugin,
            rate,
            channels,
            bits,
        } => output::open(id, plugin, rate, channels, bits),
        ToHost::OutPause(p) => output::pause(p),
        ToHost::OutFlush(ms) => output::flush(ms),
        ToHost::OutVolume(v) => output::volume(v),
        ToHost::OutClose => output::close(),
        ToHost::Shutdown => {
            registry::quit_all();
            reply(id, FromHost::Done);
            unsafe { PostQuitMessage(0) };
        }
        // Handled on the reader thread.
        ToHost::Credit { .. }
        | ToHost::Played { .. }
        | ToHost::VisPcm { .. }
        | ToHost::State(_)
        | ToHost::Playlist(_)
        | ToHost::DspProcess { .. }
        | ToHost::OutWrite(_)
        | ToHost::OutQuery => {}
    }
}
