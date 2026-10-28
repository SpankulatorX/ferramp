//! The player window's side of the conversation with plugins: answering
//! `WM_WA_IPC` queries from the player state Ferramp keeps sending, turning
//! commands (`WM_COMMAND`, IPC setters, `WM_COPYDATA`) into requests for
//! Ferramp, and announcing changes the way the original window did so that
//! plugins which subclass it see them.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::Mutex;

use ferramp_plugproto::{Command, FromHost, PlEntry, PlayerState};
use ferramp_winampabi::{HWND, WM_WA_IPC, cmd, embedWindowState, ipc};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::DataExchange::COPYDATASTRUCT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GW_CHILD, GetClientRect, GetWindow, KillTimer, MoveWindow,
    RegisterClassW, SW_HIDE, SendMessageW, SetTimer, ShowWindow, WM_CLOSE, WM_SHOWWINDOW, WM_SIZE,
    WM_TIMER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use super::text::{self, from_ansi_ptr, from_wide_ptr, wide};
use super::{event, main_hwnd};

/// Strings handed to plugins must stay valid after the call returns; they
/// live here until the next change.
#[derive(Default)]
struct Shared {
    state: PlayerState,
    playlist: Vec<PlEntry>,
    files_a: HashMap<i32, CString>,
    titles_a: HashMap<i32, CString>,
    files_w: HashMap<i32, Vec<u16>>,
    titles_w: HashMap<i32, Vec<u16>>,
    ini_file: CString,
    ini_dir: CString,
    plugin_dir: CString,
    ini_file_w: Vec<u16>,
    ini_dir_w: Vec<u16>,
    plugin_dir_w: Vec<u16>,
    registered: HashMap<String, isize>,
    last_eq_pos: usize,
    /// What the last notification saw, to announce only changes.
    notified: Option<(u32, u32, i32, Vec<u8>, u32, u32)>,
}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Shared) -> R) -> R {
    let mut g = SHARED.lock().unwrap();
    f(g.get_or_insert_with(Shared::default))
}

pub fn set_dirs(ini_dir: &str, plugin_dir: &str) {
    let _ = std::fs::create_dir_all(ini_dir);
    let ini_file = format!("{}\\winamp.ini", ini_dir.trim_end_matches('\\'));
    with(|s| {
        s.ini_file = CString::new(text::ansi_path(&ini_file).split_last().unwrap().1.to_vec())
            .unwrap_or_default();
        s.ini_dir = CString::new(text::ansi_path(ini_dir).split_last().unwrap().1.to_vec())
            .unwrap_or_default();
        s.plugin_dir = CString::new(text::ansi_path(plugin_dir).split_last().unwrap().1.to_vec())
            .unwrap_or_default();
        s.ini_file_w = wide(&ini_file);
        s.ini_dir_w = wide(ini_dir);
        s.plugin_dir_w = wide(plugin_dir);
    });
}

pub fn set_state(st: PlayerState) {
    with(|s| s.state = st);
}

pub fn set_playlist(p: Vec<PlEntry>) {
    with(|s| {
        s.playlist = p;
        s.files_a.clear();
        s.titles_a.clear();
        s.files_w.clear();
        s.titles_w.clear();
    });
}

fn entry_file(s: &Shared, i: i32) -> Option<String> {
    s.playlist.get(usize::try_from(i).ok()?).map(|e| {
        if e.file.contains("://") {
            e.file.clone()
        } else {
            text::to_win_path(&e.file)
        }
    })
}

fn file_a(s: &mut Shared, i: i32) -> isize {
    let Some(f) = entry_file(s, i) else { return 0 };
    let c = CString::new(text::ansi_path(&f).split_last().unwrap().1.to_vec()).unwrap_or_default();
    s.files_a.entry(i).or_insert(c).as_ptr() as isize
}

fn title_a(s: &mut Shared, i: i32) -> isize {
    let Some(t) = s.playlist.get(i.max(0) as usize).map(|e| e.title.clone()) else {
        return 0;
    };
    let bytes = text::to_ansi(&t).unwrap_or_else(|| {
        t.bytes()
            .map(|b| if b.is_ascii() { b } else { b'?' })
            .chain([0])
            .collect()
    });
    let c = CString::new(bytes.split_last().unwrap().1.to_vec()).unwrap_or_default();
    s.titles_a.entry(i).or_insert(c).as_ptr() as isize
}

fn file_w(s: &mut Shared, i: i32) -> isize {
    let Some(f) = entry_file(s, i) else { return 0 };
    s.files_w.entry(i).or_insert_with(|| wide(&f)).as_ptr() as isize
}

fn title_w(s: &mut Shared, i: i32) -> isize {
    let Some(t) = s.playlist.get(i.max(0) as usize).map(|e| e.title.clone()) else {
        return 0;
    };
    s.titles_w.entry(i).or_insert_with(|| wide(&t)).as_ptr() as isize
}

fn cmd_event(c: Command) {
    event(FromHost::Command(c));
}

/// IPC numbers already noted in the log (each is noted once).
static SEEN_IPC: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// Answers `SendMessage(player, WM_WA_IPC, w, l)`.
pub fn ipc(h: HWND, w: WPARAM, l: LPARAM) -> LRESULT {
    {
        let mut seen = SEEN_IPC.lock().unwrap();
        if !seen.contains(&l) {
            seen.push(l);
            super::log(format!("ipc {l} (wparam {w:#x})"));
        }
    }
    let wi = w as isize;
    match l {
        ipc::GETVERSION => 0x5066,
        ipc::GETREGISTEREDVERSION => 0,
        ipc::ISPLAYING => with(|s| s.state.status as isize),
        ipc::ISFULLSTOP => with(|s| (s.state.status == 0) as isize),
        ipc::GETOUTPUTTIME => with(|s| match wi {
            0 => s.state.position_ms as isize,
            1 => {
                if s.state.length_ms < 0 {
                    -1
                } else {
                    (s.state.length_ms / 1000) as isize
                }
            }
            2 => s.state.length_ms as isize,
            _ => -1,
        }),
        ipc::JUMPTOTIME => {
            cmd_event(Command::SeekMs(wi.max(0) as u32));
            0
        }
        ipc::GETLISTLENGTH => with(|s| s.playlist.len() as isize),
        ipc::GETLISTPOS => with(|s| s.state.playlist_pos as isize),
        ipc::SETPLAYLISTPOS => {
            with(|s| s.state.playlist_pos = wi as i32);
            cmd_event(Command::PlaylistPos(wi as i32));
            0
        }
        ipc::STARTPLAY => {
            cmd_event(Command::StartPlay);
            0
        }
        ipc::DELETE => {
            cmd_event(Command::ClearPlaylist);
            0
        }
        ipc::SETVOLUME => {
            if wi == -666 {
                with(|s| s.state.volume as isize)
            } else {
                let v = wi.clamp(0, 255) as u32;
                with(|s| s.state.volume = v);
                cmd_event(Command::Volume(v));
                0
            }
        }
        ipc::SETPANNING => {
            if wi == -666 {
                with(|s| s.state.pan as isize)
            } else {
                let p = wi.clamp(-127, 127) as i32;
                with(|s| s.state.pan = p);
                cmd_event(Command::Pan(p));
                0
            }
        }
        ipc::GETINFO => with(|s| match wi {
            0 => (s.state.rate / 1000) as isize,
            1 => s.state.kbps as isize,
            2 => s.state.channels as isize,
            5 => s.state.rate as isize,
            _ => 0,
        }),
        ipc::GETEQDATA => with(|s| {
            s.last_eq_pos = wi.clamp(0, 12) as usize;
            s.state.eq.get(s.last_eq_pos).copied().unwrap_or(31) as isize
        }),
        ipc::SETEQDATA => {
            let (pos, val) = if (w >> 24) & 0xFF == 0xDB {
                ((w >> 16) & 0xFF, w & 0xFFFF)
            } else {
                (with(|s| s.last_eq_pos), w)
            };
            let eq = with(|s| {
                if s.state.eq.len() < 12 {
                    s.state.eq.resize(12, 31);
                }
                if pos < s.state.eq.len() {
                    s.state.eq[pos] = val.min(63) as u8;
                }
                s.state.eq.clone()
            });
            cmd_event(Command::Eq(eq));
            0
        }
        ipc::GET_SHUFFLE => with(|s| s.state.shuffle as isize),
        ipc::GET_REPEAT => with(|s| s.state.repeat as isize),
        ipc::SET_SHUFFLE => {
            with(|s| s.state.shuffle = wi != 0);
            cmd_event(Command::Shuffle(wi != 0));
            0
        }
        ipc::SET_REPEAT => {
            with(|s| s.state.repeat = wi != 0);
            cmd_event(Command::Repeat(wi != 0));
            0
        }
        ipc::GETPLAYLISTFILE => with(|s| file_a(s, wi as i32)),
        ipc::GETPLAYLISTTITLE => with(|s| title_a(s, wi as i32)),
        ipc::GETPLAYLISTFILEW => with(|s| file_w(s, wi as i32)),
        ipc::GETPLAYLISTTITLEW => with(|s| title_w(s, wi as i32)),
        ipc::GET_PLAYING_FILENAME => with(|s| {
            let pos = s.state.playlist_pos;
            file_w(s, pos)
        }),
        ipc::GET_PLAYING_TITLE => with(|s| {
            let pos = s.state.playlist_pos;
            title_w(s, pos)
        }),
        ipc::PLAYFILE => {
            if w != 0 {
                let f = unsafe { from_ansi_ptr(w as *const c_char) };
                cmd_event(Command::Enqueue(text::to_player_path(&f)));
            }
            0
        }
        ipc::PLAYFILEW => {
            if w != 0 {
                let f = unsafe { from_wide_ptr(w as *const u16) };
                cmd_event(Command::Enqueue(text::to_player_path(&f)));
            }
            0
        }
        ipc::GETINIFILE => with(|s| s.ini_file.as_ptr() as isize),
        ipc::GETINIDIRECTORY => with(|s| s.ini_dir.as_ptr() as isize),
        ipc::GETPLUGINDIRECTORY => with(|s| s.plugin_dir.as_ptr() as isize),
        ipc::GETM3UDIRECTORY => with(|s| s.ini_dir.as_ptr() as isize),
        ipc::GETINIFILEW => with(|s| s.ini_file_w.as_ptr() as isize),
        ipc::GETINIDIRECTORYW => with(|s| s.ini_dir_w.as_ptr() as isize),
        ipc::GETPLUGINDIRECTORYW => with(|s| s.plugin_dir_w.as_ptr() as isize),
        ipc::GETM3UDIRECTORYW => with(|s| s.ini_dir_w.as_ptr() as isize),
        ipc::GETSKIN => {
            // An empty name means the base skin.
            if w != 0 {
                unsafe { *(w as *mut c_char) = 0 };
            }
            w as isize
        }
        ipc::INETAVAILABLE => 1,
        ipc::IS_PLAYING_VIDEO => 0,
        ipc::GETWND | ipc::ISWNDVISIBLE | ipc::ISMAINWNDVISIBLE => 0,
        ipc::GETDIALOGBOXPARENT => h as isize,
        // With a window state: create the frame now and return it.
        ipc::GET_EMBEDIF if w != 0 => {
            (unsafe { embed_window(w as *mut embedWindowState) }) as isize
        }
        ipc::GET_EMBEDIF => embed_window as *const () as isize,
        ipc::GET_HMENU => super::menu::get(w),
        ipc::ADJUST_OPTIONSMENUPOS => super::menu::adjust(0, w as isize),
        ipc::ADJUST_FFWINDOWSMENUPOS => super::menu::adjust(1, w as isize),
        ipc::ADJUST_FFOPTIONSMENUPOS => super::menu::adjust(2, w as isize),
        ipc::REGISTER_LOWORD_COMMAND => super::menu::register_ids(w),
        ipc::INITIAL_SHOW_STATE => 1, // SW_SHOWNORMAL
        // The analyzer data the player's own display shows. Plugins call
        // these without checking for null.
        ipc::GETSADATAFUNC => {
            want_pcm();
            match w {
                0 => sa_get as *const () as isize,
                1 => sa_setreq as *const () as isize,
                _ => sa_get_into as *const () as isize,
            }
        }
        ipc::GETVUDATAFUNC => {
            want_pcm();
            vu_get as *const () as isize
        }
        // No Wasabi service layer; plugins that need it degrade or refuse.
        ipc::GET_API_SERVICE => super::wasabi::service(),
        ipc::GET_BASIC_FILE_INFO => basic_file_info(w, false),
        ipc::GET_BASIC_FILE_INFOW => basic_file_info(w, true),
        ipc::GET_EXTENDED_FILE_INFO => extended_file_info(w),
        ipc::REGISTER_WINAMP_IPCMESSAGE => {
            if w == 0 {
                return 0;
            }
            let name = unsafe { CStr::from_ptr(w as *const c_char) }
                .to_string_lossy()
                .into_owned();
            with(|s| {
                let next = 65537 + s.registered.len() as isize;
                *s.registered.entry(name).or_insert(next)
            })
        }
        ipc::UPDTITLE
        | ipc::REFRESHPLCACHE
        | ipc::SETDIALOGBOXPARENT
        | ipc::ENABLEDISABLE_ALL_WINDOWS => 0,
        _ => 0,
    }
}

/// Tells Ferramp once that a plugin wants the sound for analyzer data.
fn want_pcm() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| event(FromHost::WantPcm));
}

static mut SA_BUF: [u8; 158] = [0; 158];

unsafe extern "C" fn sa_get() -> *mut c_char {
    // The buffer outlives the call, as the player's did; it is only ever
    // reached through this raw pointer.
    let p = (&raw mut SA_BUF).cast::<u8>();
    super::vis::sa_data(unsafe { std::slice::from_raw_parts_mut(p, 158) });
    p as *mut c_char
}

unsafe extern "C" fn sa_get_into(data: *mut c_char) -> *mut c_char {
    if data.is_null() {
        return unsafe { sa_get() };
    }
    let buf = unsafe { std::slice::from_raw_parts_mut(data as *mut u8, 150) };
    super::vis::sa_data(buf);
    data
}

unsafe extern "C" fn sa_setreq(_want: c_int) {}

unsafe extern "C" fn vu_get(channel: c_int) -> c_int {
    match usize::try_from(channel).ok().and_then(super::vis::vu) {
        Some(v) => v as c_int,
        None => -1,
    }
}

/// `basicFileInfoStruct { const char *filename; int quickCheck; int length;
/// char *title; int titlelen; }` (wide variant with `wchar_t` strings).
fn basic_file_info(w: WPARAM, wide_strings: bool) -> LRESULT {
    #[repr(C)]
    struct Basic {
        filename: *const u8,
        quick: c_int,
        length: c_int,
        title: *mut u8,
        titlelen: c_int,
    }
    if w == 0 {
        return 0;
    }
    let b = unsafe { &mut *(w as *mut Basic) };
    let name = unsafe {
        if wide_strings {
            from_wide_ptr(b.filename as *const u16)
        } else {
            from_ansi_ptr(b.filename as *const c_char)
        }
    };
    let found = with(|s| {
        s.playlist
            .iter()
            .find(|e| text::to_win_path(&e.file).eq_ignore_ascii_case(&name) || e.file == name)
            .cloned()
    });
    if let Some(e) = found {
        b.length = e.length_s;
        if !b.title.is_null() && b.titlelen > 0 {
            unsafe {
                if wide_strings {
                    let t = wide(&e.title);
                    let n = t.len().min(b.titlelen as usize);
                    std::ptr::copy_nonoverlapping(t.as_ptr(), b.title as *mut u16, n);
                    *(b.title as *mut u16).add(n - 1) = 0;
                } else {
                    let t = text::to_ansi(&e.title).unwrap_or_else(|| vec![0]);
                    let n = t.len().min(b.titlelen as usize);
                    std::ptr::copy_nonoverlapping(t.as_ptr(), b.title, n);
                    *b.title.add(n - 1) = 0;
                }
            }
        }
    } else {
        b.length = -1;
    }
    1
}

/// `extendedFileInfoStruct { const char *filename; const char *metadata;
/// char *ret; size_t retlen; }`, answered from the playlist.
fn extended_file_info(w: WPARAM) -> LRESULT {
    #[repr(C)]
    struct Ext {
        filename: *const c_char,
        metadata: *const c_char,
        ret: *mut u8,
        retlen: usize,
    }
    if w == 0 {
        return 0;
    }
    let e = unsafe { &mut *(w as *mut Ext) };
    let name = unsafe { from_ansi_ptr(e.filename) };
    let key = unsafe { from_ansi_ptr(e.metadata) }.to_lowercase();
    let found = with(|s| {
        s.playlist
            .iter()
            .find(|p| text::to_win_path(&p.file).eq_ignore_ascii_case(&name))
            .cloned()
    });
    let Some(p) = found else { return 0 };
    let v = match key.as_str() {
        "title" => p.title.clone(),
        "length" if p.length_s >= 0 => (p.length_s * 1000).to_string(),
        "type" => "0".into(),
        _ => return 0,
    };
    if e.ret.is_null() || e.retlen == 0 {
        return 0;
    }
    let bytes = text::to_ansi(&v).unwrap_or_else(|| vec![0]);
    let n = bytes.len().min(e.retlen);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), e.ret, n);
        *e.ret.add(n - 1) = 0;
    }
    1
}

/// `WM_COMMAND` ids from the player's menus and buttons.
pub fn command(w: WPARAM) {
    let c = match (w & 0xFFFF) as u16 {
        cmd::BUTTON1 => Command::Prev,
        cmd::BUTTON2 => Command::Play,
        cmd::BUTTON3 => Command::Pause,
        cmd::BUTTON4 => Command::Stop,
        cmd::BUTTON5 => Command::Next,
        cmd::BUTTON4_SHIFT => Command::FadeStop,
        cmd::VOLUMEUP => Command::VolumeUp,
        cmd::VOLUMEDOWN => Command::VolumeDown,
        cmd::FFWD5S => Command::Forward5,
        cmd::REW5S => Command::Rewind5,
        cmd::FILE_PLAY => Command::OpenFiles,
        cmd::OPTIONS_PREFS => Command::Preferences,
        cmd::FILE_SHUFFLE => {
            let v = with(|s| !s.state.shuffle);
            Command::Shuffle(v)
        }
        cmd::FILE_REPEAT => {
            let v = with(|s| !s.state.repeat);
            Command::Repeat(v)
        }
        cmd::FILE_QUIT => Command::Quit,
        _ => return,
    };
    cmd_event(c);
}

/// `WM_COPYDATA` with `dwData = IPC_PLAYFILE(W)` enqueues a file.
pub fn copydata(l: LPARAM) -> LRESULT {
    if l == 0 {
        return 0;
    }
    let cd = unsafe { &*(l as *const COPYDATASTRUCT) };
    if cd.lpData.is_null() {
        return 0;
    }
    let f = match cd.dwData as isize {
        ipc::PLAYFILE => unsafe { from_ansi_ptr(cd.lpData as *const c_char) },
        ipc::PLAYFILEW => unsafe { from_wide_ptr(cd.lpData as *const u16) },
        _ => return 0,
    };
    cmd_event(Command::Enqueue(text::to_player_path(&f)));
    1
}

/// Sends the change notifications the original window sent to itself, so
/// plugins that subclass it hear about them. Runs on the main thread.
/// The original window's title was "N. Title - Winamp" while playing; some
/// plugins and outside tools read it to learn what is playing.
fn update_title(h: HWND) {
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW;
    let t = with(|s| {
        let pos = s.state.playlist_pos;
        match s.playlist.get(pos.max(0) as usize) {
            Some(e) if s.state.status != 0 => {
                let paused = if s.state.status == 3 { " [Paused]" } else { "" };
                format!("{}. {} - Winamp{paused}", pos + 1, e.title)
            }
            _ => "Winamp 5.66".to_string(),
        }
    });
    unsafe { SetWindowTextW(h, wide(&t).as_ptr()) };
}

pub fn notify(h: HWND) {
    update_title(h);
    let (now, file_a, file_w) = with(|s| {
        let st = &s.state;
        let now = (
            st.status,
            st.volume,
            st.playlist_pos,
            st.eq.clone(),
            st.rate,
            st.kbps,
        );
        let pos = st.playlist_pos;
        let fa = file_a(s, pos);
        let fw = file_w(s, pos);
        (now, fa, fw)
    });
    let before = with(|s| s.notified.replace(now.clone()));
    let send = |w: usize, l: isize| unsafe { SendMessageW(h, WM_WA_IPC, w, l) };
    let b = before.unwrap_or((0, 0, -1, Vec::new(), 0, 0));
    let playing_changed = now.0 == 1 && (b.0 != 1 || b.2 != now.2);
    if playing_changed && file_a != 0 {
        send(file_a as usize, ipc::PLAYING_FILE);
        send(file_w as usize, ipc::PLAYING_FILEW);
        send(ipc::CB_MISC_TITLE, ipc::CB_MISC);
    }
    if b.0 != now.0 {
        send(ipc::CB_MISC_STATUS, ipc::CB_MISC);
    }
    if b.1 != now.1 {
        send(ipc::CB_MISC_VOLUME, ipc::CB_MISC);
    }
    if b.3 != now.3 {
        send(ipc::CB_MISC_EQ, ipc::CB_MISC);
    }
    if (b.4, b.5) != (now.4, now.5) {
        send(ipc::CB_MISC_INFO, ipc::CB_MISC);
    }
}

// ----------------------------------------------------------------------
// Frame windows for plugins that ask to be embedded in the player

/// The frame around a plugin's window: keeps the plugin's child window
/// filling it, and passes the close button on to it, as the player did.
unsafe extern "system" fn frame_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE | WM_SHOWWINDOW => {
                fit_child(h);
                if msg == WM_SIZE {
                    0
                } else {
                    DefWindowProcW(h, msg, w, l)
                }
            }
            // The plugin creates its window inside the frame right after
            // asking for it; size it before it first draws.
            WM_TIMER if w == FIT_TIMER => {
                KillTimer(h, FIT_TIMER);
                fit_child(h);
                0
            }
            WM_CLOSE => {
                let child = GetWindow(h, GW_CHILD);
                if child.is_null() {
                    ShowWindow(h, SW_HIDE);
                } else {
                    SendMessageW(child, WM_CLOSE, 0, 0);
                }
                0
            }
            _ => DefWindowProcW(h, msg, w, l),
        }
    }
}

const FIT_TIMER: usize = 1;

/// Makes the plugin's window fill the frame.
unsafe fn fit_child(frame: HWND) {
    unsafe {
        let child = GetWindow(frame, GW_CHILD);
        if !child.is_null() {
            let mut r = std::mem::zeroed();
            GetClientRect(frame, &mut r);
            MoveWindow(child, 0, 0, r.right, r.bottom, 1);
        }
    }
}

unsafe extern "C" fn embed_window(state: *mut embedWindowState) -> HWND {
    if state.is_null() {
        return std::ptr::null_mut();
    }
    unsafe {
        let class = wide("FerrampEmbed");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(frame_proc),
            hInstance: std::ptr::null_mut(),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let r = (*state).r;
        let (mut x, mut y, mut w, mut hgt) = (r[0], r[1], r[2] - r[0], r[3] - r[1]);
        if w <= 0 || hgt <= 0 {
            (x, y, w, hgt) = (100, 100, 400, 300);
        }
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Ferramp").as_ptr(),
            // Hidden until the plugin shows it.
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            w,
            hgt,
            main_hwnd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        (*state).me = hwnd;
        SetTimer(hwnd, FIT_TIMER, 1, None);
        hwnd
    }
}
