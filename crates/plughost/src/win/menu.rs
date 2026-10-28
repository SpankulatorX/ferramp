//! The player's menus, as general plugins see them. Plugins ask for them
//! with `IPC_GET_HMENU` and insert their own items (show or hide my window,
//! open my settings); Ferramp draws its own menus, so it asks which items
//! were added and shows them, and a click comes back as the `WM_COMMAND`
//! the plugin listens for on the player window.

use std::collections::HashSet;
use std::sync::Mutex;

use ferramp_plugproto::MenuEntry;
use ferramp_winampabi::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, GetMenuItemCount, GetMenuItemInfoW, HMENU, MENUITEMINFOW,
    MF_SEPARATOR, MF_STRING, MFS_CHECKED, MFS_DISABLED, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID,
    MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, PostMessageW, SendMessageW, WM_COMMAND,
    WM_INITMENUPOPUP,
};

use super::text::wide;

const OPTIONS_EQ: u32 = 40036;
const OPTIONS_PLEDIT: u32 = 40040;
const HELP_ABOUT: u32 = 40041;

/// The first id handed out by `IPC_REGISTER_LOWORD_COMMAND`, above the
/// player's own commands.
const FIRST_FREE_ID: u32 = 45000;

struct Menus {
    /// Main popup, then the file, options, windows and help menus.
    menus: [isize; 5],
    next_id: u32,
    options_pos: isize,
    windows_pos: isize,
    ff_options_pos: isize,
}

static MENUS: Mutex<Option<Menus>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Menus) -> R) -> R {
    let mut m = MENUS.lock().unwrap();
    let m = m.get_or_insert_with(|| {
        // Each menu holds the items plugins look for to place theirs.
        let make = |items: &[(u32, &str)]| unsafe {
            let h = CreatePopupMenu();
            for &(id, label) in items {
                if id == 0 {
                    AppendMenuW(h, MF_SEPARATOR, 0, std::ptr::null());
                } else {
                    AppendMenuW(h, MF_STRING, id as usize, wide(label).as_ptr());
                }
            }
            h as isize
        };
        let windows = [
            (OPTIONS_EQ, "Equalizer"),
            (OPTIONS_PLEDIT, "Playlist Editor"),
        ];
        Menus {
            menus: [
                make(&[
                    (HELP_ABOUT, "Winamp..."),
                    (0, ""),
                    (OPTIONS_EQ, "Equalizer"),
                    (OPTIONS_PLEDIT, "Playlist Editor"),
                ]),
                make(&[]),
                make(&[]),
                make(&windows),
                make(&[(HELP_ABOUT, "About Winamp...")]),
            ],
            next_id: FIRST_FREE_ID,
            options_pos: 2,
            windows_pos: 2,
            ff_options_pos: 0,
        }
    });
    f(m)
}

/// `IPC_GET_HMENU`.
pub fn get(which: usize) -> isize {
    with(|m| m.menus.get(which).copied().unwrap_or(0))
}

/// `IPC_REGISTER_LOWORD_COMMAND`: `count` fresh command ids.
pub fn register_ids(count: usize) -> isize {
    with(|m| {
        let id = m.next_id;
        m.next_id += count.max(1) as u32;
        id as isize
    })
}

/// `IPC_ADJUST_OPTIONSMENUPOS`, `IPC_ADJUST_FFWINDOWSMENUPOS` and
/// `IPC_ADJUST_FFOPTIONSMENUPOS`: where the next item goes.
pub fn adjust(which: u8, by: isize) -> isize {
    with(|m| {
        let pos = match which {
            0 => &mut m.options_pos,
            1 => &mut m.windows_pos,
            _ => &mut m.ff_options_pos,
        };
        *pos += by;
        *pos
    })
}

/// The items plugins have added, after letting them update their checks.
pub fn entries(player: HWND) -> Vec<MenuEntry> {
    let menus = with(|m| m.menus);
    let own: HashSet<u32> = [OPTIONS_EQ, OPTIONS_PLEDIT, HELP_ABOUT].into();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (i, &h) in menus.iter().enumerate() {
        unsafe { SendMessageW(player, WM_INITMENUPOPUP, h as usize, i as isize) };
        collect(h as HMENU, &own, &mut seen, &mut out, 0);
    }
    out
}

fn collect(
    h: HMENU,
    own: &HashSet<u32>,
    seen: &mut HashSet<u32>,
    out: &mut Vec<MenuEntry>,
    depth: u32,
) {
    if depth > 4 {
        return;
    }
    let n = unsafe { GetMenuItemCount(h) };
    for i in 0..n.max(0) {
        let mut buf = [0u16; 256];
        let mut info: MENUITEMINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<MENUITEMINFOW>() as u32;
        info.fMask = MIIM_ID | MIIM_STATE | MIIM_STRING | MIIM_SUBMENU | MIIM_FTYPE;
        info.dwTypeData = buf.as_mut_ptr();
        info.cch = buf.len() as u32 - 1;
        if unsafe { GetMenuItemInfoW(h, i as u32, 1, &mut info) } == 0
            || info.fType & MFT_SEPARATOR != 0
        {
            continue;
        }
        if !info.hSubMenu.is_null() {
            collect(info.hSubMenu, own, seen, out, depth + 1);
            continue;
        }
        if info.wID == 0 || own.contains(&info.wID) || !seen.insert(info.wID) {
            continue;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let label = String::from_utf16_lossy(&buf[..len]);
        if label.trim().is_empty() {
            continue;
        }
        out.push(MenuEntry {
            id: info.wID,
            label,
            checked: info.fState & MFS_CHECKED != 0,
            enabled: info.fState & MFS_DISABLED == 0,
        });
    }
}

/// The user picked item `id`: the plugin sees it as a menu command.
pub fn command(player: HWND, id: u32) {
    unsafe { PostMessageW(player, WM_COMMAND, id as usize, 0) };
}
