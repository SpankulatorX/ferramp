//! A small stand-in for the later player's service layer ("Wasabi"), for
//! plugins that ask for it with `IPC_GET_API_SERVICE`: the service
//! directory, the language service (strings, dialogs and menus come from the
//! plugin's own resources, as with no language pack installed), the memory
//! manager and the application service.
//!
//! Every object is a C++ object with one virtual method,
//! `int _dispatch(int msg, void *retval, void **params, int nparam)`, where
//! `params[i]` points at the i-th argument and the method writes the return
//! value to `retval` and returns non-zero when it handled the message. On
//! x86 it is a member function (`thiscall`).

use std::ffi::{c_char, c_int, c_void};
use std::sync::Mutex;

use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapAlloc, HeapFree, HeapReAlloc};
use windows_sys::Win32::System::Threading::{TlsAlloc, TlsGetValue, TlsSetValue};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateDialogParamA, CreateDialogParamW, DialogBoxParamA, DialogBoxParamW, LoadAcceleratorsA,
    LoadAcceleratorsW, LoadMenuA, LoadMenuW, LoadStringA, LoadStringW,
};

use super::log;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

impl std::fmt::Display for Guid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{{:08X}-{:04X}-{:04X}-", self.d1, self.d2, self.d3)?;
        for (i, b) in self.d4.iter().enumerate() {
            if i == 2 {
                write!(f, "-")?;
            }
            write!(f, "{b:02X}")?;
        }
        write!(f, "}}")
    }
}

const LANGUAGE: Guid = Guid {
    d1: 0x30aed4e5,
    d2: 0xef10,
    d3: 0x4277,
    d4: [0x8d, 0x49, 0x27, 0xab, 0x55, 0x70, 0xe8, 0x91],
};
const MEMMGR: Guid = Guid {
    d1: 0x000cf46e,
    d2: 0x4df6,
    d3: 0x4a43,
    d4: [0xbb, 0xe7, 0x40, 0xe7, 0xa3, 0xea, 0x02, 0xed],
};
const APPLICATION: Guid = Guid {
    d1: 0x23b96771,
    d2: 0x09d7,
    d3: 0x46d3,
    d4: [0x9a, 0xe2, 0x20, 0xdc, 0xea, 0x6c, 0x86, 0xea],
};

#[cfg(target_arch = "x86")]
type Dispatch =
    unsafe extern "thiscall" fn(*mut Obj, c_int, *mut c_void, *mut *mut c_void, c_int) -> c_int;
#[cfg(not(target_arch = "x86"))]
type Dispatch =
    unsafe extern "C" fn(*mut Obj, c_int, *mut c_void, *mut *mut c_void, c_int) -> c_int;

#[repr(C)]
pub struct Obj {
    vtbl: *const Dispatch,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Service,
    Factory(Guid),
    Language,
    MemMgr,
    Application,
}

static VTBL: [Dispatch; 1] = [dispatch];

static mut SERVICE: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::Service,
};
static mut LANG_FACTORY: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::Factory(LANGUAGE),
};
static mut MEM_FACTORY: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::Factory(MEMMGR),
};
static mut APP_FACTORY: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::Factory(APPLICATION),
};
static mut LANG: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::Language,
};
static mut MEM: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::MemMgr,
};
static mut APP: Obj = Obj {
    vtbl: VTBL.as_ptr(),
    kind: Kind::Application,
};

/// What `IPC_GET_API_SERVICE` returns.
pub fn service() -> isize {
    (&raw mut SERVICE) as isize
}

/// Strings the application service hands out (they must outlive the call).
struct Paths {
    app: Vec<u16>,
    settings: Vec<u16>,
}

static PATHS: Mutex<Option<Paths>> = Mutex::new(None);

pub fn set_paths(app_dir: &str, settings_dir: &str) {
    *PATHS.lock().unwrap() = Some(Paths {
        app: super::text::wide(app_dir),
        settings: super::text::wide(settings_dir),
    });
}

fn path_ptr(f: impl Fn(&Paths) -> *const u16) -> *const u16 {
    PATHS
        .lock()
        .unwrap()
        .as_ref()
        .map(f)
        .unwrap_or(std::ptr::null())
}

static APP_NAME: [u16; 7] = utf16("Winamp");
static VERSION: [u16; 12] = utf16("Winamp 5.66");
static LANG_ID: [u16; 6] = utf16("en-US");

const fn utf16<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() && i < N - 1 {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}

/// Rotating buffers for `GetString` calls that pass no buffer of their own.
static STRINGS: Mutex<(usize, Vec<Vec<u8>>)> = Mutex::new((0, Vec::new()));

fn scratch(bytes: usize) -> *mut u8 {
    let mut s = STRINGS.lock().unwrap();
    if s.1.is_empty() {
        s.1 = (0..32).map(|_| vec![0u8; 4096]).collect();
    }
    s.0 = (s.0 + 1) % s.1.len();
    let i = s.0;
    let buf = &mut s.1[i];
    if buf.len() < bytes {
        buf.resize(bytes, 0);
    }
    buf.as_mut_ptr()
}

/// The i-th argument, as a `T` (params[i] points at it).
unsafe fn arg<T: Copy>(params: *mut *mut c_void, nparam: c_int, i: c_int) -> Option<T> {
    if params.is_null() || i >= nparam {
        return None;
    }
    let p = unsafe { *params.add(i as usize) } as *const T;
    (!p.is_null()).then(|| unsafe { *p })
}

unsafe fn ret<T>(retval: *mut c_void, v: T) -> c_int {
    if !retval.is_null() {
        unsafe { (retval as *mut T).write_unaligned(v) };
    }
    1
}

static UNKNOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(what: String) {
    let mut u = UNKNOWN.lock().unwrap();
    if !u.contains(&what) {
        log(format!("wasabi: {what}"));
        u.push(what);
    }
}

#[cfg(target_arch = "x86")]
unsafe extern "thiscall" fn dispatch(
    this: *mut Obj,
    msg: c_int,
    rv: *mut c_void,
    p: *mut *mut c_void,
    n: c_int,
) -> c_int {
    unsafe { handle(this, msg, rv, p, n) }
}
#[cfg(not(target_arch = "x86"))]
unsafe extern "C" fn dispatch(
    this: *mut Obj,
    msg: c_int,
    rv: *mut c_void,
    p: *mut *mut c_void,
    n: c_int,
) -> c_int {
    unsafe { handle(this, msg, rv, p, n) }
}

unsafe fn handle(
    this: *mut Obj,
    msg: c_int,
    rv: *mut c_void,
    p: *mut *mut c_void,
    n: c_int,
) -> c_int {
    let kind = unsafe { (*this).kind };
    unsafe {
        match kind {
            Kind::Service => service_msg(msg, rv, p, n),
            Kind::Factory(g) => factory_msg(g, msg, rv, p, n),
            Kind::Language => language_msg(msg, rv, p, n),
            Kind::MemMgr => memmgr_msg(msg, rv, p, n),
            Kind::Application => application_msg(msg, rv, p, n),
        }
    }
}

unsafe fn service_msg(msg: c_int, rv: *mut c_void, p: *mut *mut c_void, n: c_int) -> c_int {
    unsafe {
        match msg {
            // Registering, locking and releasing: accepted, nothing to keep.
            10 | 20 | 60 | 70 | 80 | 120 | 130 => ret::<c_int>(rv, 1),
            30 => ret::<c_int>(rv, 0),
            40 => ret::<*mut Obj>(rv, std::ptr::null_mut()),
            50 => {
                let Some(g) = arg::<Guid>(p, n, 0) else {
                    return 0;
                };
                let f: *mut Obj = if g == LANGUAGE {
                    &raw mut LANG_FACTORY
                } else if g == MEMMGR {
                    &raw mut MEM_FACTORY
                } else if g == APPLICATION {
                    &raw mut APP_FACTORY
                } else {
                    note(format!("service {g} not available"));
                    std::ptr::null_mut()
                };
                ret(rv, f)
            }
            _ => 0,
        }
    }
}

unsafe fn factory_msg(
    g: Guid,
    msg: c_int,
    rv: *mut c_void,
    _p: *mut *mut c_void,
    _n: c_int,
) -> c_int {
    unsafe {
        match msg {
            210 => ret(rv, g),
            300 => {
                let i: *mut Obj = if g == LANGUAGE {
                    &raw mut LANG
                } else if g == MEMMGR {
                    &raw mut MEM
                } else {
                    &raw mut APP
                };
                ret(rv, i)
            }
            301 | 310 => ret::<c_int>(rv, 1),
            600 => ret::<c_int>(rv, 0),
            _ => 0,
        }
    }
}

type Hinst = *mut c_void;

unsafe fn language_msg(msg: c_int, rv: *mut c_void, p: *mut *mut c_void, n: c_int) -> c_int {
    unsafe {
        let inst = |i: c_int| arg::<Hinst>(p, n, i).unwrap_or(std::ptr::null_mut());
        // Localized instance if given, otherwise the plugin's own.
        let pick = |a: Hinst, b: Hinst| if a.is_null() { b } else { a };
        match msg {
            // GetString(hinst, owner, id, buf, maxlen); FromGUID(guid, owner, id, buf, maxlen)
            10 | 12 => {
                let owner = inst(1);
                let id = arg::<u32>(p, n, 2).unwrap_or(0);
                let mut buf = arg::<*mut u8>(p, n, 3).unwrap_or(std::ptr::null_mut());
                let mut max = arg::<usize>(p, n, 4).unwrap_or(0);
                if buf.is_null() || max == 0 {
                    buf = scratch(4096);
                    max = 4096;
                }
                *buf = 0;
                LoadStringA(owner, id, buf, max as i32);
                ret(rv, buf as *mut c_char)
            }
            11 | 13 => {
                let owner = inst(1);
                let id = arg::<u32>(p, n, 2).unwrap_or(0);
                let mut buf = arg::<*mut u16>(p, n, 3).unwrap_or(std::ptr::null_mut());
                let mut max = arg::<usize>(p, n, 4).unwrap_or(0);
                if buf.is_null() || max == 0 {
                    buf = scratch(8192) as *mut u16;
                    max = 4096;
                }
                *buf = 0;
                LoadStringW(owner, id, buf, max as i32);
                ret(rv, buf)
            }
            // No language packs: there is no other module to find.
            20..=22 => ret::<Hinst>(rv, std::ptr::null_mut()),
            // StartLanguageSupport(hinst, guid): the plugin's own resources.
            30 => ret(rv, inst(0)),
            31 => ret::<c_int>(rv, 1),
            40 => ret::<*const u16>(rv, std::ptr::null()),
            50 | 53 | 51 | 54 => {
                let h = pick(inst(0), inst(1));
                let id = arg::<usize>(p, n, 2).unwrap_or(0);
                let parent = arg::<*mut c_void>(p, n, 3).unwrap_or(std::ptr::null_mut());
                let proc =
                    arg::<windows_sys::Win32::UI::WindowsAndMessaging::DLGPROC>(p, n, 4).flatten();
                let lp = arg::<isize>(p, n, 5).unwrap_or(0);
                match msg {
                    50 => ret(rv, CreateDialogParamA(h, id as *const u8, parent, proc, lp)),
                    53 => ret(
                        rv,
                        CreateDialogParamW(h, id as *const u16, parent, proc, lp),
                    ),
                    51 => ret(rv, DialogBoxParamA(h, id as *const u8, parent, proc, lp)),
                    _ => ret(rv, DialogBoxParamW(h, id as *const u16, parent, proc, lp)),
                }
            }
            52 | 55 => {
                let h = pick(inst(0), inst(1));
                let id = arg::<usize>(p, n, 2).unwrap_or(0);
                if msg == 52 {
                    ret(rv, LoadMenuA(h, id as *const u8))
                } else {
                    ret(rv, LoadMenuW(h, id as *const u16))
                }
            }
            60 => ret(rv, LANG_ID.as_ptr()),
            70 | 71 => {
                // LoadResourceFromFile(hinst, owner, type, name, size*)
                let h = pick(inst(0), inst(1));
                let ty = arg::<*const u16>(p, n, 2).unwrap_or(std::ptr::null());
                let name = arg::<*const u16>(p, n, 3).unwrap_or(std::ptr::null());
                let size = arg::<*mut u32>(p, n, 4).unwrap_or(std::ptr::null_mut());
                if msg == 70 {
                    // The ANSI variant: let the caller fall back.
                    return ret::<*mut c_void>(rv, std::ptr::null_mut());
                }
                let r = FindResourceW(h, name, ty);
                if r.is_null() {
                    return ret::<*mut c_void>(rv, std::ptr::null_mut());
                }
                if !size.is_null() {
                    *size = SizeofResource(h, r);
                }
                ret(rv, LockResource(LoadResource(h, r)))
            }
            80 | 81 => {
                let h = pick(inst(0), inst(1));
                let name = arg::<*const u8>(p, n, 2).unwrap_or(std::ptr::null());
                if msg == 80 {
                    ret(rv, LoadAcceleratorsA(h, name))
                } else {
                    ret(rv, LoadAcceleratorsW(h, name as *const u16))
                }
            }
            _ => {
                note(format!("language message {msg}"));
                0
            }
        }
    }
}

unsafe fn memmgr_msg(msg: c_int, rv: *mut c_void, p: *mut *mut c_void, n: c_int) -> c_int {
    unsafe {
        let heap = GetProcessHeap();
        match msg {
            0 => {
                let size = arg::<usize>(p, n, 0).unwrap_or(0);
                ret(rv, HeapAlloc(heap, 0, size.max(1)))
            }
            10 => {
                if let Some(ptr) = arg::<*mut c_void>(p, n, 0).filter(|p| !p.is_null()) {
                    HeapFree(heap, 0, ptr);
                }
                1
            }
            20 => {
                let ptr = arg::<*mut c_void>(p, n, 0).unwrap_or(std::ptr::null_mut());
                let size = arg::<usize>(p, n, 1).unwrap_or(0).max(1);
                if ptr.is_null() {
                    ret(rv, HeapAlloc(heap, 0, size))
                } else {
                    ret(rv, HeapReAlloc(heap, 0, ptr, size))
                }
            }
            30 => 1,
            _ => 0,
        }
    }
}

unsafe fn application_msg(msg: c_int, rv: *mut c_void, p: *mut *mut c_void, n: c_int) -> c_int {
    unsafe {
        match msg {
            10 => ret(rv, APP_NAME.as_ptr()),
            20 => ret(rv, VERSION.as_ptr()),
            30 => ret::<c_int>(rv, 3512),
            40 => ret(rv, APPLICATION),
            50 => ret::<*mut c_void>(rv, std::ptr::null_mut()),
            60 => ret(rv, GetModuleHandleW(std::ptr::null())),
            70 => ret(rv, [0u16; 1].as_ptr()),
            80 | 90 => 1,
            100 => ret::<c_int>(rv, 0),
            110 | 190 => ret(rv, path_ptr(|p| p.app.as_ptr())),
            120 => ret(rv, path_ptr(|p| p.settings.as_ptr())),
            130 => ret::<c_int>(rv, 1),
            140 => ret::<c_int>(rv, 0),
            // Message processors, modeless dialogs, accelerators and global
            // windows: the plugin's own message handling does the job.
            150 | 160 | 170 | 180 | 200 | 240 | 250 | 280 | 290 => 1,
            260 => ret::<c_int>(rv, 0),
            270 => ret::<*mut c_void>(rv, std::ptr::null_mut()),
            // Machine, user and session ids: a zero GUID, success.
            210..=230 => {
                if let Some(g) = arg::<*mut Guid>(p, n, 0).filter(|g| !g.is_null()) {
                    *g = Guid {
                        d1: 0,
                        d2: 0,
                        d3: 0,
                        d4: [0; 8],
                    };
                }
                ret::<c_int>(rv, 0)
            }
            300 => ret::<usize>(rv, TlsAlloc() as usize),
            310 => {
                let i = arg::<usize>(p, n, 0).unwrap_or(0) as u32;
                ret(rv, TlsGetValue(i))
            }
            320 => {
                let i = arg::<usize>(p, n, 0).unwrap_or(0) as u32;
                let v = arg::<*mut c_void>(p, n, 1).unwrap_or(std::ptr::null_mut());
                TlsSetValue(i, v);
                1
            }
            _ => {
                note(format!("application message {msg}"));
                0
            }
        }
    }
}
