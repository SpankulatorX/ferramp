//! Finding, loading and keeping track of plugins.

use std::ffi::{c_char, c_int, c_void};
use std::sync::Mutex;

use ferramp_plugproto::{FileInfo, Kind, PluginInfo};
use ferramp_winampabi::*;
use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW, SetDllDirectoryW,
};

use super::text::{self, from_ansi_ptr, from_wide_ptr, wide};
use super::{Ptr, log, main_hwnd};

#[derive(Clone, Copy)]
pub struct Xr {
    pub open: Option<ExtendedReadOpen>,
    pub openw: Option<ExtendedReadOpenW>,
    pub get: ExtendedReadGetData,
    pub set_time: Option<ExtendedReadSetTime>,
    pub close: ExtendedReadClose,
}

#[derive(Clone, Copy)]
pub struct Input {
    pub m: Ptr<In_Module>,
    pub unicode: bool,
    pub xr: Option<Xr>,
    pub efi: Option<GetExtendedFileInfo>,
    pub efiw: Option<GetExtendedFileInfoW>,
}

#[derive(Clone, Copy)]
pub enum Data {
    Failed,
    Input(Input),
    Output(Ptr<Out_Module>),
    Dsp(Ptr<winampDSPHeader>),
    Vis(Ptr<winampVisHeader>),
    General(Ptr<winampGeneralPurposePlugin>),
}

pub struct Plugin {
    pub info: PluginInfo,
    pub hmod: Ptr<c_void>,
    pub data: Data,
}

struct Registry {
    plugins: Vec<Plugin>,
    scanned: bool,
    ini_dir: String,
    dirs: Vec<String>,
    skip: Vec<String>,
    inputs_only: bool,
}

static REG: Mutex<Registry> = Mutex::new(Registry {
    plugins: Vec::new(),
    scanned: false,
    ini_dir: String::new(),
    dirs: Vec::new(),
    skip: Vec::new(),
    inputs_only: false,
});

/// The plugin being loaded right now, for the responsiveness check.
static LOADING: Mutex<Option<String>> = Mutex::new(None);

pub fn set_dirs(ini_dir: &str, dirs: &[String], skip: Vec<String>, inputs_only: bool) {
    let mut r = REG.lock().unwrap();
    r.inputs_only = inputs_only;
    r.skip = skip.into_iter().map(|s| s.to_lowercase()).collect();
    r.ini_dir = text::to_win_path(ini_dir);
    r.dirs = dirs.iter().map(|d| text::to_win_path(d)).collect();
    let plugins = r.dirs.first().map(String::as_str).unwrap_or("");
    super::general::set_dirs(&r.ini_dir, plugins);
    let app = plugins.rsplit_once('\\').map(|(a, _)| a).unwrap_or(plugins);
    super::wasabi::set_paths(app, &r.ini_dir);
}

pub fn data(id: u32) -> Option<Data> {
    REG.lock().unwrap().plugins.get(id as usize).map(|p| p.data)
}

pub fn hmod(id: u32) -> *mut c_void {
    REG.lock()
        .unwrap()
        .plugins
        .get(id as usize)
        .map(|p| p.hmod.0)
        .unwrap_or(std::ptr::null_mut())
}

pub fn inputs() -> Vec<(u32, Input)> {
    REG.lock()
        .unwrap()
        .plugins
        .iter()
        .filter_map(|p| match p.data {
            Data::Input(i) => Some((p.info.id, i)),
            _ => None,
        })
        .collect()
}

/// The input plugin that claims `win_path` by its extension.
pub fn input_for_extension(win_path: &str) -> Option<(u32, Input)> {
    let ext = win_path.rsplit_once('.')?.1.to_lowercase();
    let r = REG.lock().unwrap();
    r.plugins.iter().find_map(|p| match p.data {
        Data::Input(i)
            if p.info
                .extensions
                .iter()
                .any(|(pat, _)| pat.split(';').any(|e| e.trim() == ext)) =>
        {
            Some((p.info.id, i))
        }
        _ => None,
    })
}

unsafe fn proc<T>(h: HMODULE, name: &std::ffi::CStr) -> Option<T> {
    unsafe { GetProcAddress(h, name.as_ptr().cast()).map(|f| std::mem::transmute_copy(&f)) }
}

/// Loads every plugin once, in the order the original player did: input,
/// output, DSP and visualization headers, then general purpose plugins
/// (which may look at the others).
pub fn scan() -> Vec<PluginInfo> {
    {
        let r = REG.lock().unwrap();
        if r.scanned {
            return r.plugins.iter().map(|p| p.info.clone()).collect();
        }
    }
    let (dirs, skip, inputs_only) = {
        let r = REG.lock().unwrap();
        (r.dirs.clone(), r.skip.clone(), r.inputs_only)
    };
    let mut files: Vec<(Kind, String, String)> = Vec::new();
    for dir in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        for name in names {
            if let Some(k) = Kind::from_file_name(&name)
                && !files.iter().any(|(_, n, _)| n.eq_ignore_ascii_case(&name))
            {
                files.push((
                    k,
                    name.clone(),
                    format!("{}\\{}", dir.trim_end_matches('\\'), name),
                ));
            }
        }
    }
    // While a plugin loads, tell Ferramp whether the main thread still
    // answers messages: a plugin showing a dialog (a first-run notice, a
    // missing-file warning) is waiting for the user, not hung.
    let loading_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = loading_done.clone();
    let hwnd = super::Ptr(main_hwnd());
    std::thread::spawn(move || {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_NULL,
        };
        let hwnd = hwnd;
        while !done.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let current = LOADING.lock().unwrap().clone();
            if let Some(name) = current {
                let mut r = 0usize;
                let ok = unsafe {
                    SendMessageTimeoutW(hwnd.0, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, 1500, &mut r)
                };
                if ok != 0 {
                    log(format!("waiting {name}"));
                }
            }
        }
    });
    let order: &[Kind] = if inputs_only {
        &[Kind::Input]
    } else {
        &[
            Kind::Input,
            Kind::Output,
            Kind::Dsp,
            Kind::Vis,
            Kind::General,
        ]
    };
    for &kind in order {
        for (k, name, path) in files.iter().filter(|f| f.0 == kind) {
            let id = REG.lock().unwrap().plugins.len() as u32;
            if skip.contains(&name.to_lowercase()) {
                REG.lock().unwrap().plugins.push(Plugin {
                    info: PluginInfo {
                        id,
                        file: name.clone(),
                        kind: Some(*k),
                        description: name.clone(),
                        error: Some("disabled".into()),
                        ..Default::default()
                    },
                    hmod: Ptr(std::ptr::null_mut()),
                    data: Data::Failed,
                });
                continue;
            }
            log(format!("loading {name}"));
            *LOADING.lock().unwrap() = Some(name.clone());
            let plugin = unsafe { load(id, *k, name, path) };
            *LOADING.lock().unwrap() = None;
            log(format!("loaded {name}"));
            REG.lock().unwrap().plugins.push(plugin);
        }
    }
    loading_done.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut r = REG.lock().unwrap();
    r.scanned = true;
    r.plugins.iter().map(|p| p.info.clone()).collect()
}

unsafe fn load(id: u32, kind: Kind, name: &str, path: &str) -> Plugin {
    let mut info = PluginInfo {
        id,
        file: name.to_string(),
        kind: Some(kind),
        ..Default::default()
    };
    let fail = |mut info: PluginInfo, e: &str| {
        info.error = Some(e.to_string());
        if info.description.trim().is_empty() {
            info.description = info.file.clone();
        }
        Plugin {
            info,
            hmod: Ptr(std::ptr::null_mut()),
            data: Data::Failed,
        }
    };
    // Dependencies next to the plugin, or one level up (the player folder).
    let dir = path.rsplit_once('\\').map(|(d, _)| d).unwrap_or("");
    let parent = dir.rsplit_once('\\').map(|(d, _)| d).unwrap_or(dir);
    unsafe { SetDllDirectoryW(wide(parent).as_ptr()) };
    let h = unsafe {
        LoadLibraryExW(
            wide(path).as_ptr(),
            std::ptr::null_mut(),
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
    };
    if h.is_null() {
        let code = std::io::Error::last_os_error();
        let missing = missing_imports(path);
        if !missing.is_empty() {
            return fail(
                info,
                &format!(
                    "needs {}, which is missing: put it in the Plugins folder or the folder above it",
                    missing.join(", ")
                ),
            );
        }
        return fail(info, &format!("could not load ({code})"));
    }
    let hwnd = main_hwnd();
    let data = unsafe {
        match kind {
            Kind::Input => {
                let Some(get) = proc::<GetInModule>(h, c"winampGetInModule2") else {
                    return fail(info, "not an input plugin");
                };
                let m = get();
                if m.is_null() {
                    return fail(info, "plugin returned no module");
                }
                let v = (*m).version;
                let unicode = v & IN_UNICODE == IN_UNICODE;
                if !(0x100..=0x1FF).contains(&(v & 0xFFFF)) {
                    return fail(info, &format!("unsupported version {v:#x}"));
                }
                (*m).hMainWindow = hwnd;
                (*m).hDllInstance = h;
                super::input::install_callbacks(m);
                let xr = proc::<ExtendedReadGetData>(h, c"winampGetExtendedRead_getData").and_then(
                    |get| {
                        let close = proc::<ExtendedReadClose>(h, c"winampGetExtendedRead_close")?;
                        Some(Xr {
                            open: proc(h, c"winampGetExtendedRead_open"),
                            openw: proc(h, c"winampGetExtendedRead_openW"),
                            get,
                            set_time: proc(h, c"winampGetExtendedRead_setTime"),
                            close,
                        })
                    },
                );
                let xr = xr.filter(|x| x.open.is_some() || x.openw.is_some());
                if let Some(init) = (*m).Init {
                    init();
                }
                info.description = from_ansi_ptr((*m).description);
                info.extensions = text::parse_extensions((*m).FileExtensions);
                info.seekable = (*m).is_seekable != 0;
                info.extended_read = xr.is_some();
                info.unicode = unicode;
                Data::Input(Input {
                    m: Ptr(m),
                    unicode,
                    xr,
                    efi: proc(h, c"winampGetExtendedFileInfo"),
                    efiw: proc(h, c"winampGetExtendedFileInfoW"),
                })
            }
            Kind::Output => {
                let Some(get) = proc::<GetOutModule>(h, c"winampGetOutModule") else {
                    return fail(info, "not an output plugin");
                };
                let m = get();
                if m.is_null() {
                    return fail(info, "plugin returned no module");
                }
                (*m).hMainWindow = hwnd;
                (*m).hDllInstance = h;
                if let Some(init) = (*m).Init {
                    init();
                }
                info.description = from_ansi_ptr((*m).description);
                Data::Output(Ptr(m))
            }
            Kind::Dsp => {
                let Some(get) = proc::<GetDspHeader>(h, c"winampDSPGetHeader2") else {
                    return fail(info, "not a DSP plugin");
                };
                let hdr = get(hwnd);
                if hdr.is_null() {
                    return fail(info, "plugin returned no header");
                }
                info.description = from_ansi_ptr((*hdr).description);
                if let Some(gm) = (*hdr).getModule {
                    for i in 0..64 {
                        let m = gm(i);
                        if m.is_null() {
                            break;
                        }
                        info.modules.push(from_ansi_ptr((*m).description));
                    }
                }
                Data::Dsp(Ptr(hdr))
            }
            Kind::Vis => {
                let Some(get) = proc::<GetVisHeader>(h, c"winampVisGetHeader") else {
                    return fail(info, "not a visualization plugin");
                };
                let hdr = get(hwnd);
                if hdr.is_null() {
                    return fail(info, "plugin returned no header");
                }
                info.description = from_ansi_ptr((*hdr).description);
                if let Some(gm) = (*hdr).getModule {
                    for i in 0..64 {
                        let m = gm(i);
                        if m.is_null() {
                            break;
                        }
                        info.modules.push(from_ansi_ptr((*m).description));
                    }
                }
                Data::Vis(Ptr(hdr))
            }
            Kind::General => {
                let Some(get) =
                    proc::<GetGeneralPurposePlugin>(h, c"winampGetGeneralPurposePlugin")
                else {
                    return fail(info, "not a general purpose plugin");
                };
                let g = get();
                if g.is_null() {
                    return fail(info, "plugin returned no header");
                }
                (*g).hwndParent = hwnd;
                (*g).hDllInstance = h;
                // A "unicode" header carries a UTF-16 description.
                info.unicode = (*g).version & 0xFF == 0x11;
                let describe = |g: *mut winampGeneralPurposePlugin| {
                    if (*g).version & 0xFF == 0x11 {
                        from_wide_ptr((*g).description as *const u16)
                    } else {
                        from_ansi_ptr((*g).description)
                    }
                };
                info.description = describe(g);
                if let Some(init) = (*g).init
                    && init() != 0
                {
                    return fail(info, "the plugin's init failed");
                }
                // Localized plugins fill in their name during init.
                let after = describe(g);
                if !after.is_empty() {
                    info.description = after;
                }
                Data::General(Ptr(g))
            }
        }
    };
    if info.description.is_empty() {
        info.description = name.to_string();
    }
    Plugin {
        info,
        hmod: Ptr(h),
        data,
    }
}

/// The libraries a DLL imports that cannot be loaded from here.
fn missing_imports(path: &str) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    import_names(&bytes)
        .into_iter()
        .filter(|name| {
            let h = unsafe { LoadLibraryExW(wide(name).as_ptr(), std::ptr::null_mut(), 0) };
            if h.is_null() {
                return true;
            }
            unsafe { FreeLibrary(h) };
            false
        })
        .collect()
}

/// The names in a PE file's import directory.
fn import_names(b: &[u8]) -> Vec<String> {
    let u16_at = |o: usize| {
        b.get(o..o + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
    };
    let u32_at = |o: usize| {
        b.get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
    };
    let mut out = Vec::new();
    let Some(pe) = u32_at(0x3C) else { return out };
    if b.get(pe..pe + 4) != Some(&b"PE\0\0"[..]) {
        return out;
    }
    let coff = pe + 4;
    let (Some(nsec), Some(opt_size)) = (u16_at(coff + 2), u16_at(coff + 16)) else {
        return out;
    };
    let opt = coff + 20;
    let dir = match u16_at(opt) {
        Some(0x10b) => opt + 96,
        Some(0x20b) => opt + 112,
        _ => return out,
    };
    // The import directory is entry 1.
    let Some(imp_rva) = u32_at(dir + 8) else {
        return out;
    };
    let sections = opt + opt_size;
    let to_off = |rva: usize| -> Option<usize> {
        (0..nsec).find_map(|i| {
            let sh = sections + i * 40;
            let (vsize, va, raw_size, raw) = (
                u32_at(sh + 8)?,
                u32_at(sh + 12)?,
                u32_at(sh + 16)?,
                u32_at(sh + 20)?,
            );
            (rva >= va && rva < va + vsize.max(raw_size)).then(|| rva - va + raw)
        })
    };
    let Some(mut d) = to_off(imp_rva) else {
        return out;
    };
    while let Some(name_rva) = u32_at(d + 12) {
        if name_rva == 0 || out.len() > 256 {
            break;
        }
        if let Some(o) = to_off(name_rva) {
            let end = b[o..]
                .iter()
                .position(|&c| c == 0)
                .map(|e| o + e)
                .unwrap_or(o);
            out.push(String::from_utf8_lossy(&b[o..end]).into_owned());
        }
        d += 20;
    }
    out
}

pub fn configure(id: u32, module: u32) {
    let hwnd = main_hwnd();
    let hinst = hmod(id);
    unsafe {
        match data(id) {
            Some(Data::Input(i)) => {
                if let Some(f) = (*i.m.0).Config {
                    f(hwnd);
                }
            }
            Some(Data::Output(o)) => {
                if let Some(f) = (*o.0).Config {
                    f(hwnd);
                }
            }
            Some(Data::Dsp(hdr)) => {
                if let Some(gm) = (*hdr.0).getModule {
                    let m = gm(module as c_int);
                    if !m.is_null() {
                        (*m).hwndParent = hwnd;
                        (*m).hDllInstance = hinst;
                        if let Some(f) = (*m).Config {
                            f(m);
                        }
                    }
                }
            }
            Some(Data::Vis(hdr)) => {
                if let Some(gm) = (*hdr.0).getModule {
                    let m = gm(module as c_int);
                    if !m.is_null() {
                        (*m).hwndParent = hwnd;
                        (*m).hDllInstance = hinst;
                        if let Some(f) = (*m).Config {
                            f(m);
                        }
                    }
                }
            }
            Some(Data::General(g)) => {
                if let Some(f) = (*g.0).config {
                    f();
                }
            }
            _ => {}
        }
    }
}

pub fn about(id: u32, module: u32) {
    let hwnd = main_hwnd();
    unsafe {
        match data(id) {
            Some(Data::Input(i)) => {
                if let Some(f) = (*i.m.0).About {
                    f(hwnd);
                }
            }
            Some(Data::Output(o)) => {
                if let Some(f) = (*o.0).About {
                    f(hwnd);
                }
            }
            // The other kinds have no About; their configuration window is it.
            _ => configure(id, module),
        }
    }
}

/// A file name argument in the form the plugin wants: UTF-16 or ANSI.
pub enum Arg {
    Wide(Vec<u16>),
    Ansi(Vec<u8>),
}

impl Arg {
    pub fn new(win_path: &str, unicode: bool) -> Arg {
        if unicode {
            Arg::Wide(wide(win_path))
        } else {
            Arg::Ansi(text::ansi_path(win_path))
        }
    }

    pub fn ptr(&self) -> *const c_void {
        match self {
            Arg::Wide(w) => w.as_ptr().cast(),
            Arg::Ansi(a) => a.as_ptr().cast(),
        }
    }
}

pub fn is_our_file(path: &str) -> Option<u32> {
    let win = text::to_win_path(path);
    if let Some((id, _)) = input_for_extension(&win) {
        return Some(id);
    }
    for (id, i) in inputs() {
        let arg = Arg::new(&win, i.unicode);
        if let Some(f) = unsafe { (*i.m.0).IsOurFile }
            && unsafe { f(arg.ptr()) } != 0
        {
            return Some(id);
        }
    }
    None
}

pub fn file_info(id: u32, path: &str) -> Option<FileInfo> {
    let Some(Data::Input(i)) = data(id) else {
        return None;
    };
    let win = text::to_win_path(path);
    let arg = Arg::new(&win, i.unicode);
    let mut info = FileInfo {
        length_ms: -1,
        ..Default::default()
    };
    unsafe {
        if let Some(f) = (*i.m.0).GetFileInfo {
            let mut len: c_int = -1;
            if i.unicode {
                let mut buf = vec![0u16; GETFILEINFO_TITLE_LENGTH * 2];
                f(arg.ptr(), buf.as_mut_ptr().cast(), &mut len);
                info.title = from_wide_ptr(buf.as_ptr());
            } else {
                let mut buf = vec![0u8; GETFILEINFO_TITLE_LENGTH * 4];
                f(arg.ptr(), buf.as_mut_ptr().cast(), &mut len);
                info.title = from_ansi_ptr(buf.as_ptr() as *const c_char);
            }
            info.length_ms = len;
        }
        for key in [
            "artist",
            "title",
            "album",
            "albumartist",
            "year",
            "genre",
            "track",
            "comment",
            "length",
            "bitrate",
        ] {
            let ckey = format!("{key}\0");
            let v = if let Some(f) = i.efiw {
                let w = wide(&win);
                let mut buf = vec![0u16; 2048];
                (f(
                    w.as_ptr(),
                    ckey.as_ptr().cast(),
                    buf.as_mut_ptr(),
                    buf.len() as c_int,
                ) != 0)
                    .then(|| from_wide_ptr(buf.as_ptr()))
            } else if let Some(f) = i.efi {
                let a = text::ansi_path(&win);
                let mut buf = vec![0u8; 4096];
                (f(
                    a.as_ptr().cast(),
                    ckey.as_ptr().cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len() as c_int,
                ) != 0)
                    .then(|| from_ansi_ptr(buf.as_ptr() as *const c_char))
            } else {
                None
            };
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                info.tags.push((key.to_string(), v));
            }
        }
    }
    Some(info)
}

pub fn info_box(id: u32, path: &str) {
    let Some(Data::Input(i)) = data(id) else {
        return;
    };
    let win = text::to_win_path(path);
    let arg = Arg::new(&win, i.unicode);
    unsafe {
        if let Some(f) = (*i.m.0).InfoBox {
            f(arg.ptr(), main_hwnd());
        }
    }
}

/// Shuts plugins down: visualization and DSP first, then general purpose,
/// input and output plugins.
pub fn quit_all() {
    super::vis::stop();
    super::dsp::stop();
    super::input::close_all();
    super::output::close();
    let plugins: Vec<Data> = REG.lock().unwrap().plugins.iter().map(|p| p.data).collect();
    unsafe {
        for d in plugins.iter().rev() {
            match d {
                Data::General(g) => {
                    if let Some(f) = (*g.0).quit {
                        f();
                    }
                }
                Data::Input(i) => {
                    if let Some(f) = (*i.m.0).Quit {
                        f();
                    }
                }
                Data::Output(o) => {
                    if let Some(f) = (*o.0).Quit {
                        f();
                    }
                }
                _ => {}
            }
        }
    }
    REG.lock().unwrap().plugins.clear();
}
