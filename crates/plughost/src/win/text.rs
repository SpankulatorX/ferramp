//! Strings and paths: UTF-16 and ANSI conversion, and translating between
//! the player's native paths and Windows paths when running under Wine.

use std::ffi::{CStr, c_char};
use std::sync::OnceLock;

use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar, WideCharToMultiByte};
use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapFree};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub unsafe fn from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut n = 0;
    while unsafe { *p.add(n) } != 0 {
        n += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, n) })
}

/// A NUL-terminated string in the system ANSI code page.
pub unsafe fn from_ansi_ptr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    let bytes = unsafe { CStr::from_ptr(p) }.to_bytes();
    from_ansi(bytes)
}

pub fn from_ansi(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    unsafe {
        let n = MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        let mut w = vec![0u16; n.max(0) as usize];
        MultiByteToWideChar(
            CP_ACP,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            w.as_mut_ptr(),
            n,
        );
        String::from_utf16_lossy(&w)
    }
}

/// `s` in the ANSI code page, NUL-terminated; `None` if it cannot be
/// represented exactly.
pub fn to_ansi(s: &str) -> Option<Vec<u8>> {
    if s.is_ascii() {
        return Some(s.bytes().chain(std::iter::once(0)).collect());
    }
    let w: Vec<u16> = s.encode_utf16().collect();
    unsafe {
        let mut lossy = 0i32;
        let n = WideCharToMultiByte(
            CP_ACP,
            0,
            w.as_ptr(),
            w.len() as i32,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            &mut lossy,
        );
        if lossy != 0 || n <= 0 {
            return None;
        }
        let mut out = vec![0u8; n as usize + 1];
        WideCharToMultiByte(
            CP_ACP,
            0,
            w.as_ptr(),
            w.len() as i32,
            out.as_mut_ptr(),
            n,
            std::ptr::null(),
            std::ptr::null_mut(),
        );
        Some(out)
    }
}

/// A Windows path for a plugin that only takes ANSI file names: the path
/// itself when the code page can hold it, otherwise its 8.3 short form.
pub fn ansi_path(win_path: &str) -> Vec<u8> {
    if let Some(a) = to_ansi(win_path) {
        return a;
    }
    let w = wide(win_path);
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetShortPathNameW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n > 0 && (n as usize) < buf.len() {
        let short = String::from_utf16_lossy(&buf[..n as usize]);
        if let Some(a) = to_ansi(&short) {
            return a;
        }
    }
    // Last resort: lossy conversion (the plugin will likely fail to open it).
    win_path
        .bytes()
        .map(|b| if b.is_ascii() { b } else { b'?' })
        .chain(std::iter::once(0))
        .collect()
}

type DosName = unsafe extern "C" fn(*const c_char) -> *mut u16;
type UnixName = unsafe extern "C" fn(*const u16) -> *mut c_char;

struct Wine {
    dos: Option<DosName>,
    unix: Option<UnixName>,
}

fn wine() -> &'static Wine {
    static W: OnceLock<Wine> = OnceLock::new();
    W.get_or_init(|| unsafe {
        let k32 = GetModuleHandleW(wide("kernel32.dll").as_ptr());
        let ntdll = GetModuleHandleW(wide("ntdll.dll").as_ptr());
        let is_wine = !ntdll.is_null()
            && GetProcAddress(ntdll, c"wine_get_version".as_ptr().cast()).is_some();
        if !is_wine || k32.is_null() {
            return Wine {
                dos: None,
                unix: None,
            };
        }
        Wine {
            dos: GetProcAddress(k32, c"wine_get_dos_file_name".as_ptr().cast())
                .map(|f| std::mem::transmute(f)),
            unix: GetProcAddress(k32, c"wine_get_unix_file_name".as_ptr().cast())
                .map(|f| std::mem::transmute(f)),
        }
    })
}

pub fn under_wine() -> bool {
    wine().dos.is_some()
}

/// The player's path (a Unix path under Wine) as a Windows path.
pub fn to_win_path(p: &str) -> String {
    if !p.starts_with('/') {
        return p.to_string();
    }
    if let Some(f) = wine().dos {
        let mut c = p.as_bytes().to_vec();
        c.push(0);
        unsafe {
            let w = f(c.as_ptr().cast());
            if !w.is_null() {
                let s = from_wide_ptr(w);
                HeapFree(GetProcessHeap(), 0, w.cast());
                return s;
            }
        }
    }
    // Wine maps the root file system to Z: by default.
    format!("Z:{}", p.replace('/', "\\"))
}

/// A Windows path from a plugin, as a path the player can open.
pub fn to_player_path(p: &str) -> String {
    if let Some(f) = wine().unix {
        let w = wide(p);
        unsafe {
            let u = f(w.as_ptr());
            if !u.is_null() {
                let s = CStr::from_ptr(u).to_string_lossy().into_owned();
                HeapFree(GetProcessHeap(), 0, u.cast());
                return s;
            }
        }
    }
    p.to_string()
}

/// Splits a plugin's `FileExtensions` block (`"a;b\0Desc\0c\0Desc\0\0"`).
pub unsafe fn parse_extensions(p: *const c_char) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if p.is_null() {
        return out;
    }
    let mut cur = p;
    for _ in 0..256 {
        let ext = unsafe { CStr::from_ptr(cur) };
        if ext.to_bytes().is_empty() {
            break;
        }
        cur = unsafe { cur.add(ext.to_bytes().len() + 1) };
        let desc = unsafe { CStr::from_ptr(cur) };
        cur = unsafe { cur.add(desc.to_bytes().len() + 1) };
        out.push((
            from_ansi(ext.to_bytes()).to_lowercase(),
            from_ansi(desc.to_bytes()),
        ));
        if desc.to_bytes().is_empty() {
            break;
        }
    }
    out
}
