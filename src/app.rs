//! Window management and the event loop: one borderless OS window per
//! player window, pop-up menu windows, dialog windows, docking and snapping.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Cursor, CursorIcon, CustomCursor, Window, WindowId, WindowLevel};

use crate::audio::{Event as AudioEvent, Source, Status};
use crate::config::{self, Settings};
use crate::gfx::Bitmap;
use crate::metadata::{self, InfoWorker};
use crate::playlist::{self, format_time};
use crate::skin::{CursorKind, Skin, region_mask};
use crate::state::{PL_SEG_H, PL_SEG_W, State, WinGeom};
use crate::ui::dialogs::{Dialog, DialogKind, Key as DKey, Outcome, Prefs};
use crate::ui::eq_win::EqWin;
use crate::ui::main_win::MainWin;
use crate::ui::menu::{self, BUILTIN_PRESETS, Item, MenuContext, MenuLevel};
use crate::ui::pl_win::{self, PlWin};
use crate::ui::vis::VisMode;
use crate::ui::{Button, Cmd, MenuId, Mods, MouseEvent, WinKind, WmRequest};

const SNAP: i32 = 10;
const FRAME: Duration = Duration::from_millis(16);

#[derive(Debug)]
pub enum UserEvent {
    Wake,
    Picked(Pick, Vec<PathBuf>),
    Classic(crate::classic_skin::Fetched),
    Museum(crate::museum::Fetched),
    /// The plugin hosts have loaded their plugins.
    PluginsReady(Arc<crate::plugins::Plugins>),
}

/// What was last sent to the plugin hosts, to send only changes.
#[derive(Default)]
struct PlugSync {
    next_state: Option<Instant>,
    last_state: Option<ferramp_plugproto::PlayerState>,
    playlist_sig: u64,
    next_vis: Option<Instant>,
    vis_was_running: bool,
}

/// State of the Skin Museum dialog's searches and downloads.
#[derive(Default)]
struct Museum {
    query: String,
    results: Vec<crate::museum::Entry>,
    loading: bool,
    exhausted: bool,
    selected: Option<String>,
    shots: HashMap<String, Bitmap>,
    downloading: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    OpenFiles,
    AddFiles,
    OpenFolder,
    AddFolder,
    LoadPlaylist,
    SavePlaylist,
    LoadSkin,
    LoadEqf,
    SaveEqf,
    SaveHtml,
}

type SbSurface = softbuffer::Surface<Rc<Window>, Rc<Window>>;

struct OsWin {
    window: Rc<Window>,
    surface: SbSurface,
    frame: Bitmap,
    cursor: (f64, f64),
    cursor_kind: Option<CursorKind>,
}

impl OsWin {
    fn new(window: Window) -> Option<OsWin> {
        let window = Rc::new(window);
        let ctx = softbuffer::Context::new(window.clone()).ok()?;
        let surface = softbuffer::Surface::new(&ctx, window.clone()).ok()?;
        Some(OsWin {
            window,
            surface,
            frame: Bitmap::new(1, 1, 0),
            cursor: (0.0, 0.0),
            cursor_kind: None,
        })
    }

    fn present(&mut self, scale: i32) {
        let (w, h) = (
            (self.frame.w * scale).max(1) as u32,
            (self.frame.h * scale).max(1) as u32,
        );
        let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        if self.surface.resize(nw, nh).is_err() {
            return;
        }
        if let Ok(mut buf) = self.surface.buffer_mut() {
            self.frame
                .present_into(&mut buf, w as usize, h as usize, scale as usize);
            let _ = buf.present();
        }
    }
}

struct MenuPopup {
    level: MenuLevel,
    os: OsWin,
    x: i32,
    y: i32,
}

struct Tooltip {
    os: OsWin,
    w: i32,
    h: i32,
    text: String,
}

struct DialogWin {
    dialog: Dialog,
    os: OsWin,
}

enum Drag {
    Move {
        start_cursor: (i32, i32),
        windows: Vec<(WinKind, i32, i32)>,
    },
    Resize {
        start_cursor: (i32, i32),
        start_size: (i32, i32),
    },
}

pub struct App {
    pub st: State,
    main: MainWin,
    eq: EqWin,
    pl: PlWin,
    wins: HashMap<WinKind, OsWin>,
    ids: HashMap<WindowId, WinKind>,
    menus: Vec<MenuPopup>,
    menu_owner: Option<WinKind>,
    menu_opened_at: Instant,
    dialogs: Vec<DialogWin>,
    drag: Option<Drag>,
    capture: Option<WinKind>,
    mods: Mods,
    last_down: Option<(WindowId, Instant, (f64, f64))>,
    proxy: EventLoopProxy<UserEvent>,
    info: InfoWorker,
    settings: Settings,
    builtin: Option<Skin>,
    cursors: HashMap<CursorKind, CustomCursor>,
    ui_scale: i32,
    initial_files: Vec<String>,
    dropped: Vec<(WinKind, PathBuf, i32)>,
    hover_y: HashMap<WinKind, i32>,
    errors_in_row: usize,
    last_save: Instant,
    prefs: Prefs,
    eqf_presets: Vec<(String, u8, [u8; 10])>,
    eq_auto_presets: Vec<(String, u8, [u8; 10])>,
    bookmarks: Vec<(String, String)>,
    last_vis_mode: VisMode,
    created: bool,
    exiting: bool,
    cli_skin: bool,
    classic_busy: bool,
    museum: Museum,
    plugins: Option<Arc<crate::plugins::Plugins>>,
    plugins_ready: bool,
    plug: PlugSync,
    /// The plugin dialog's rows: (file name, module).
    plugin_rows: Vec<(String, u32)>,
    /// What the plugin items in the open main menu stand for.
    plugin_menu: Vec<(crate::plugins::host::Arch, u32)>,
    tip: Option<Tooltip>,
    hover_tip: Option<(WinKind, String, Instant)>,
    next_frame: Instant,
    window_title: String,
    focus_check: bool,
    menu_released: bool,
}

impl App {
    pub fn new(st: State, proxy: EventLoopProxy<UserEvent>, files: Vec<String>) -> App {
        let wake = proxy.clone();
        let info = InfoWorker::new(move || {
            let _ = wake.send_event(UserEvent::Wake);
        });
        let settings = Settings::load(&config::config_dir().join("ferramp.ini"));
        let mut app = App {
            st,
            main: MainWin::new(),
            eq: EqWin::new(),
            pl: PlWin::new(),
            wins: HashMap::new(),
            ids: HashMap::new(),
            menus: Vec::new(),
            menu_owner: None,
            menu_opened_at: Instant::now(),
            dialogs: Vec::new(),
            drag: None,
            capture: None,
            mods: Mods::default(),
            last_down: None,
            proxy,
            info,
            settings,
            builtin: None,
            cursors: HashMap::new(),
            ui_scale: 1,
            initial_files: files,
            dropped: Vec::new(),
            hover_y: HashMap::new(),
            errors_in_row: 0,
            last_save: Instant::now(),
            prefs: Prefs {
                snap: true,
                smooth_text: false,
                show_numbers: true,
            },
            eqf_presets: BUILTIN_PRESETS
                .iter()
                .map(|(n, p, b)| (n.to_string(), *p, *b))
                .collect(),
            eq_auto_presets: Vec::new(),
            bookmarks: Vec::new(),
            last_vis_mode: VisMode::Analyzer,
            created: false,
            exiting: false,
            cli_skin: false,
            classic_busy: false,
            museum: Museum::default(),
            plugins: None,
            plugins_ready: false,
            plug: PlugSync::default(),
            plugin_rows: Vec::new(),
            plugin_menu: Vec::new(),
            tip: None,
            hover_tip: None,
            next_frame: Instant::now(),
            window_title: String::new(),
            focus_check: false,
            menu_released: true,
        };
        app.apply_settings();
        app.load_libraries();
        app
    }

    fn load_libraries(&mut self) {
        let dir = config::config_dir();
        if let Some(p) = std::fs::read(dir.join("ferramp.q1"))
            .ok()
            .and_then(|b| config::parse_eqf(&b))
            && !p.is_empty()
        {
            self.eqf_presets = p;
        }
        if let Some(p) = std::fs::read(dir.join("ferramp.q2"))
            .ok()
            .and_then(|b| config::parse_eqf(&b))
        {
            self.eq_auto_presets = p;
        }
        self.bookmarks = playlist::load_playlist_file(&dir.join("bookmarks.m3u"))
            .into_iter()
            .map(|(src, title, _)| {
                let loc = match &src {
                    Source::File(p) => p.to_string_lossy().into_owned(),
                    Source::Url(u) => u.clone(),
                };
                (title.unwrap_or_else(|| playlist::default_title(&src)), loc)
            })
            .collect();
    }

    fn save_libraries(&self) {
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("ferramp.q1"),
            config::write_eqf_library(&self.eqf_presets),
        );
        let _ = std::fs::write(
            dir.join("ferramp.q2"),
            config::write_eqf_library(&self.eq_auto_presets),
        );
        let mut m3u = String::from("#EXTM3U\n");
        for (t, l) in &self.bookmarks {
            m3u.push_str(&format!(
                "#EXTINF:-1,{}\n{}\n",
                playlist::clean_title(t),
                playlist::clean_title(l)
            ));
        }
        let _ = std::fs::write(dir.join("bookmarks.m3u"), m3u);
    }

    fn current_file_key(&self) -> Option<String> {
        let e = self.st.playlist.current_entry()?;
        match &e.source {
            Source::File(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
            Source::Url(u) => Some(u.clone()),
        }
    }

    fn eq_lib(&mut self, op: crate::ui::EqLibOp, el: &ActiveEventLoop) {
        use crate::ui::EqLibOp::*;
        match op {
            LoadDefault => {
                let d = self.settings.get_str("eq_default").map(|s| s.to_string());
                let (pre, bands) = parse_eq_values(d.as_deref());
                self.st.eq.preamp = pre;
                self.st.eq.bands = bands;
                self.st.engine.set_eq(self.st.eq);
            }
            SaveDefault => {
                let v = format_eq_values(self.st.eq.preamp, &self.st.eq.bands);
                self.settings.set("eq_default", v);
            }
            LoadPreset | SavePreset | DeletePreset => {
                let names = self.eqf_presets.iter().map(|p| p.0.clone()).collect();
                self.open_dialog(el, Dialog::eq_presets(op, names));
            }
            LoadAuto | SaveAuto | DeleteAuto => {
                let mut names: Vec<String> =
                    self.eq_auto_presets.iter().map(|p| p.0.clone()).collect();
                if op == SaveAuto
                    && let Some(k) = self.current_file_key()
                    && !names.contains(&k)
                {
                    names.insert(0, k);
                }
                self.open_dialog(el, Dialog::eq_presets(op, names));
            }
        }
    }

    fn eq_lib_commit(&mut self, op: crate::ui::EqLibOp, name: String) {
        use crate::ui::EqLibOp::*;
        let (lib, is_auto) = match op {
            LoadAuto | SaveAuto | DeleteAuto => (&mut self.eq_auto_presets, true),
            _ => (&mut self.eqf_presets, false),
        };
        match op {
            LoadPreset | LoadAuto => {
                if let Some((_, p, b)) = lib.iter().find(|x| x.0 == name).cloned() {
                    self.st.eq.preamp = p;
                    self.st.eq.bands = b;
                    self.st.engine.set_eq(self.st.eq);
                }
            }
            SavePreset | SaveAuto => {
                let entry = (name.clone(), self.st.eq.preamp, self.st.eq.bands);
                match lib.iter_mut().find(|x| x.0 == name) {
                    Some(e) => *e = entry,
                    None => lib.push(entry),
                }
            }
            DeletePreset | DeleteAuto => lib.retain(|x| x.0 != name),
            _ => {}
        }
        let _ = is_auto;
        self.save_libraries();
    }

    /// A skin given on the command line wins over the saved one.
    pub fn keep_cli_skin(&mut self) {
        self.cli_skin = true;
    }

    // ------------------------------------------------------------------
    // Settings

    fn apply_settings(&mut self) {
        let s = self.settings.clone();
        let st = &mut self.st;
        st.volume = s.get_i32("volume", 78).clamp(0, 100) as u8;
        st.balance = s.get_i32("balance", 0).clamp(-100, 100) as i8;
        st.shuffle = s.get_bool("shuffle", false);
        st.repeat = s.get_bool("repeat", false);
        st.time_remaining = s.get_bool("time_remaining", false);
        st.doubled = s.get_bool("doublesize", false);
        st.always_on_top = s.get_bool("always_on_top", false);
        st.easy_move = s.get_bool("easymove", true);
        st.autoscroll = s.get_bool("autoscroll", true);
        st.vis_mode = VisMode::parse(s.get_str("vis_mode").unwrap_or("analyzer"));
        st.vis_opts.peaks = s.get_bool("vis_peaks", true);
        st.vis_opts.wide_bands = s.get_bool("vis_wide", true);
        st.vis_opts.falloff = s.get_i32("vis_falloff", 2).clamp(0, 4) as u8;
        st.vis_opts.peak_falloff = s.get_i32("vis_peak_falloff", 1).clamp(0, 4) as u8;
        st.vis_opts.analyzer = match s.get_i32("vis_style", 0) {
            1 => crate::ui::vis::AnalyzerStyle::Fire,
            2 => crate::ui::vis::AnalyzerStyle::Line,
            _ => crate::ui::vis::AnalyzerStyle::Normal,
        };
        st.vis_opts.osc = match s.get_i32("vis_osc", 1) {
            0 => crate::ui::vis::OscStyle::Dots,
            2 => crate::ui::vis::OscStyle::Solid,
            _ => crate::ui::vis::OscStyle::Lines,
        };
        st.eq = s.eq();
        st.eq_auto = s.get_bool("eq_auto", false);
        let geom = |p: &str, def: WinGeom| WinGeom {
            x: s.get_i32(&format!("{p}_x"), def.x),
            y: s.get_i32(&format!("{p}_y"), def.y),
            visible: s.get_bool(&format!("{p}_visible"), def.visible),
            shade: s.get_bool(&format!("{p}_shade"), def.shade),
        };
        st.main = geom("main", st.main);
        st.main.visible = true;
        st.eqw = geom("eq", st.eqw);
        st.pl = geom("pl", st.pl);
        st.pl_size = (
            s.get_i32("pl_w", 0).clamp(0, 100),
            s.get_i32("pl_h", 0).clamp(0, 100),
        );
        st.playlist.scroll = 0;
        self.prefs.snap = s.get_bool("snap", true);
        self.prefs.smooth_text = s.get_bool("smooth_text", false);
        st.engine.set_volume(st.volume as f32 / 100.0);
        st.engine.set_balance(st.balance as f32 / 100.0);
        st.engine.set_eq(st.eq);
    }

    fn save_settings(&mut self) {
        let st = &self.st;
        let s = &mut self.settings;
        s.set("volume", st.volume);
        s.set("balance", st.balance);
        s.set_bool("shuffle", st.shuffle);
        s.set_bool("repeat", st.repeat);
        s.set_bool("time_remaining", st.time_remaining);
        s.set_bool("doublesize", st.doubled);
        s.set_bool("always_on_top", st.always_on_top);
        s.set_bool("easymove", st.easy_move);
        s.set_bool("autoscroll", st.autoscroll);
        s.set("vis_mode", st.vis_mode.as_str());
        s.set_bool("vis_peaks", st.vis_opts.peaks);
        s.set_bool("vis_wide", st.vis_opts.wide_bands);
        s.set("vis_falloff", st.vis_opts.falloff);
        s.set("vis_peak_falloff", st.vis_opts.peak_falloff);
        s.set("vis_style", st.vis_opts.analyzer as i32);
        s.set(
            "vis_osc",
            match st.vis_opts.osc {
                crate::ui::vis::OscStyle::Dots => 0,
                crate::ui::vis::OscStyle::Lines => 1,
                crate::ui::vis::OscStyle::Solid => 2,
            },
        );
        s.set_eq(&st.eq);
        s.set_bool("eq_auto", st.eq_auto);
        for (p, g) in [("main", st.main), ("eq", st.eqw), ("pl", st.pl)] {
            s.set(&format!("{p}_x"), g.x);
            s.set(&format!("{p}_y"), g.y);
            s.set_bool(&format!("{p}_visible"), g.visible);
            s.set_bool(&format!("{p}_shade"), g.shade);
        }
        s.set("pl_w", st.pl_size.0);
        s.set("pl_h", st.pl_size.1);
        s.set(
            "skin",
            st.skin
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        s.set("current", st.playlist.current.unwrap_or(0));
        s.set_bool("snap", self.prefs.snap);
        s.set_bool("smooth_text", self.prefs.smooth_text);
        let dir = config::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = s.save(&dir.join("ferramp.ini"));
        let _ = st.playlist.save_m3u(&dir.join("ferramp.m3u"));
        self.last_save = Instant::now();
    }

    // ------------------------------------------------------------------
    // Window creation

    fn scale(&self, k: WinKind) -> i32 {
        self.st.scale_of(k) * self.ui_scale
    }

    fn phys_size(&self, k: WinKind) -> (i32, i32) {
        let (w, h) = self.st.base_size(k);
        let s = self.scale(k);
        (w * s, h * s)
    }

    fn create_windows(&mut self, el: &ActiveEventLoop) {
        if let Ok(v) = std::env::var("FERRAMP_SCALE") {
            self.ui_scale = v.parse().unwrap_or(1).clamp(1, 4);
        } else if let Some(m) = el
            .primary_monitor()
            .or_else(|| el.available_monitors().next())
        {
            self.ui_scale = (m.scale_factor().round() as i32).clamp(1, 4);
        }
        // Skin: built-in first, then the saved one.
        self.builtin = Some(Skin::builtin());
        if !self.cli_skin
            && let Some(p) = self.settings.get_str("skin").map(PathBuf::from)
            && p.exists()
            && let Ok(s) = Skin::load(&p, self.builtin.as_ref().unwrap())
        {
            self.st.skin = s;
        }
        // First start: dock the windows at their real size, which depends on
        // the monitor scale, instead of the 1x defaults.
        if self.settings.get_str("main_y").is_none() {
            let main_h = self.phys_size(WinKind::Main).1;
            let eq_h = self.phys_size(WinKind::Eq).1;
            let (x, y) = (self.st.main.x, self.st.main.y);
            (self.st.eqw.x, self.st.eqw.y) = (x, y + main_h);
            (self.st.pl.x, self.st.pl.y) = (x, y + main_h + eq_h);
        }
        // Keep windows on screen if the saved positions are off every monitor.
        self.ensure_on_screen(el);
        for k in [WinKind::Main, WinKind::Eq, WinKind::Playlist] {
            let (w, h) = self.phys_size(k);
            let g = *self.st.geom(k);
            let title = match k {
                WinKind::Main => "Ferramp",
                WinKind::Eq => "Ferramp Equalizer",
                WinKind::Playlist => "Ferramp Playlist Editor",
            };
            #[allow(unused_mut)]
            let mut attrs = Window::default_attributes()
                .with_title(title)
                .with_decorations(false)
                .with_resizable(false)
                .with_visible(false)
                .with_inner_size(PhysicalSize::new(w as u32, h as u32))
                .with_position(PhysicalPosition::new(g.x, g.y))
                .with_window_icon(Some(app_icon()));
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                use winit::platform::x11::WindowAttributesExtX11;
                attrs = attrs.with_name("ferramp", "Ferramp");
            }
            #[cfg(windows)]
            if k != WinKind::Main
                && let Some(main) = self.wins.get(&WinKind::Main)
            {
                use winit::platform::windows::WindowAttributesExtWindows;
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(h) = main.window.window_handle()
                    && let RawWindowHandle::Win32(h) = h.as_raw()
                {
                    attrs = attrs
                        .with_owner_window(h.hwnd.get())
                        .with_skip_taskbar(true);
                }
            }
            let Ok(window) = el.create_window(attrs) else {
                eprintln!("ferramp: could not create window");
                el.exit();
                return;
            };
            let id = window.id();
            let Some(os) = OsWin::new(window) else {
                eprintln!("ferramp: could not create drawing surface");
                el.exit();
                return;
            };
            if k != WinKind::Main
                && let Some(main) = self.wins.get(&WinKind::Main)
            {
                crate::platform::make_auxiliary(&os.window, &main.window);
            }
            self.ids.insert(id, k);
            self.wins.insert(k, os);
        }
        self.rebuild_cursors(el);
        self.apply_window_level();
        for k in WinKind::ALL {
            self.apply_shape(k);
            if self.st.geom(k).visible
                && let Some(w) = self.wins.get(&k)
            {
                w.window.set_visible(true);
                let g = *self.st.geom(k);
                w.window.set_outer_position(PhysicalPosition::new(g.x, g.y));
                w.window.request_redraw();
            }
        }
        if let Some(w) = self.wins.get(&WinKind::Main) {
            w.window.focus_window();
        }
        // Offer the original base skin once, to anyone still on the built-in one.
        if !self.settings.get_bool("classic_offered", false)
            && !self.cli_skin
            && self.st.skin.path.is_none()
        {
            self.settings.set_bool("classic_offered", true);
            self.open_classic_skin(el);
        }
        // Plugins first: a file given on the command line may need one, and
        // the decoder waits for them only once loading has begun.
        self.start_plugins();
        // Restore the playlist, then handle files given on the command line.
        let files = std::mem::take(&mut self.initial_files);
        let saved = config::config_dir().join("ferramp.m3u");
        if saved.exists() {
            let items = playlist::load_playlist_file(&saved);
            let ids = self.st.playlist.append(items);
            self.request_info(&ids);
            let cur = self.settings.get_i32("current", 0).max(0) as usize;
            if cur < self.st.playlist.len() {
                self.st.playlist.set_current(cur);
            }
        }
        if !files.is_empty() {
            let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            self.open_paths(paths, true);
        }
        self.created = true;
    }

    fn ensure_on_screen(&mut self, el: &ActiveEventLoop) {
        let mons: Vec<(i32, i32, i32, i32)> = el
            .available_monitors()
            .map(|m| {
                (
                    m.position().x,
                    m.position().y,
                    m.size().width as i32,
                    m.size().height as i32,
                )
            })
            .collect();
        if mons.is_empty() {
            return;
        }
        let (mx, my, _, _) = mons[0];
        let ok = |x: i32, y: i32| {
            mons.iter().any(|(ax, ay, aw, ah)| {
                x + 20 > *ax && x < ax + aw - 20 && y + 10 > *ay && y < ay + ah - 10
            })
        };
        let main = self.st.main;
        if !ok(main.x, main.y) {
            let dx = mx + 100 - main.x;
            let dy = my + 100 - main.y;
            for k in WinKind::ALL {
                let g = self.st.geom_mut(k);
                g.x += dx;
                g.y += dy;
            }
        }
    }

    fn rebuild_cursors(&mut self, el: &ActiveEventLoop) {
        self.cursors.clear();
        for (kind, c) in &self.st.skin.cursors {
            let rgba = c.image.to_rgba8();
            if let Ok(src) =
                CustomCursor::from_rgba(rgba, c.image.w as u16, c.image.h as u16, c.hot_x, c.hot_y)
            {
                self.cursors.insert(*kind, el.create_custom_cursor(src));
            }
        }
        for w in self.wins.values_mut() {
            w.cursor_kind = None;
        }
    }

    fn apply_window_level(&self) {
        let level = if self.st.always_on_top {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        };
        for w in self.wins.values() {
            w.window.set_window_level(level);
        }
    }

    fn apply_shape(&self, k: WinKind) {
        let Some(w) = self.wins.get(&k) else { return };
        let r = &self.st.skin.regions;
        let polys = match k {
            WinKind::Main if self.st.main.shade => &r.windowshade,
            WinKind::Main => &r.normal,
            WinKind::Eq if self.st.eqw.shade => &r.equalizer_ws,
            WinKind::Eq => &r.equalizer,
            WinKind::Playlist => {
                crate::platform::set_shape(&w.window, None);
                return;
            }
        };
        if polys.is_empty() {
            crate::platform::set_shape(&w.window, None);
            return;
        }
        let (bw, bh) = self.st.base_size(k);
        let mask = region_mask(polys, bw, bh);
        crate::platform::set_shape(&w.window, Some((&mask, bw, bh, self.scale(k))));
    }

    /// Resize OS windows to match state (shade, double size, playlist size).
    fn sync_sizes(&mut self) {
        for k in WinKind::ALL {
            let (w, h) = self.phys_size(k);
            if let Some(win) = self.wins.get(&k) {
                let cur = win.window.inner_size();
                if cur.width != w as u32 || cur.height != h as u32 {
                    let _ = win
                        .window
                        .request_inner_size(PhysicalSize::new(w as u32, h as u32));
                }
            }
            self.apply_shape(k);
        }
        self.redraw_all();
    }

    fn redraw_all(&mut self) {
        for (k, w) in &self.wins {
            if self.st.geom(*k).visible {
                w.window.request_redraw();
            }
        }
    }

    fn set_window_pos(&mut self, k: WinKind, x: i32, y: i32) {
        debug!("move {k:?} -> {x},{y}");
        let g = self.st.geom_mut(k);
        g.x = x;
        g.y = y;
        if let Some(w) = self.wins.get(&k) {
            w.window.set_outer_position(PhysicalPosition::new(x, y));
        }
    }

    // ------------------------------------------------------------------
    // Geometry: docking and snapping

    fn rect_of(&self, k: WinKind) -> (i32, i32, i32, i32) {
        let g = self.st.geom(k);
        let (w, h) = self.phys_size(k);
        (g.x, g.y, w, h)
    }

    fn visible_kinds(&self) -> Vec<WinKind> {
        WinKind::ALL
            .into_iter()
            .filter(|k| self.st.geom(*k).visible)
            .collect()
    }

    /// Windows attached to `start` through touching edges.
    fn docked_group(&self, start: WinKind) -> Vec<WinKind> {
        let vis = self.visible_kinds();
        let mut group = vec![start];
        let mut i = 0;
        while i < group.len() {
            let a = self.rect_of(group[i]);
            for &k in &vis {
                if !group.contains(&k) && touching(a, self.rect_of(k)) {
                    group.push(k);
                }
            }
            i += 1;
        }
        group
    }

    fn monitor_rects(&self) -> Vec<(i32, i32, i32, i32)> {
        let Some(w) = self.wins.get(&WinKind::Main) else {
            return vec![];
        };
        w.window
            .available_monitors()
            .map(|m| {
                (
                    m.position().x,
                    m.position().y,
                    m.size().width as i32,
                    m.size().height as i32,
                )
            })
            .collect()
    }

    fn snap_offset(&self, moving: &[WinKind], dx: i32, dy: i32) -> (i32, i32) {
        if !self.prefs.snap {
            return (dx, dy);
        }
        let mut best_x: Option<i32> = None;
        let mut best_y: Option<i32> = None;
        let others: Vec<(i32, i32, i32, i32)> = self
            .visible_kinds()
            .into_iter()
            .filter(|k| !moving.contains(k))
            .map(|k| self.rect_of(k))
            .collect();
        let mons = self.monitor_rects();
        let consider = |cur: &mut Option<i32>, d: i32| {
            if d.abs() < SNAP && cur.is_none_or(|c| d.abs() < c.abs()) {
                *cur = Some(d);
            }
        };
        for &k in moving {
            let (x, y, w, h) = self.rect_of(k);
            let (x, y) = (x + dx, y + dy);
            for &(ox, oy, ow, oh) in &others {
                let overlap_y = y <= oy + oh + SNAP && oy <= y + h + SNAP;
                let overlap_x = x <= ox + ow + SNAP && ox <= x + w + SNAP;
                if overlap_y {
                    consider(&mut best_x, (ox + ow) - x);
                    consider(&mut best_x, ox - (x + w));
                    consider(&mut best_x, ox - x);
                    consider(&mut best_x, (ox + ow) - (x + w));
                }
                if overlap_x {
                    consider(&mut best_y, (oy + oh) - y);
                    consider(&mut best_y, oy - (y + h));
                    consider(&mut best_y, oy - y);
                    consider(&mut best_y, (oy + oh) - (y + h));
                }
            }
            for &(mx, my, mw, mh) in &mons {
                if x + w > mx && x < mx + mw && y + h > my && y < my + mh {
                    consider(&mut best_x, mx - x);
                    consider(&mut best_x, (mx + mw) - (x + w));
                    consider(&mut best_y, my - y);
                    consider(&mut best_y, (my + mh) - (y + h));
                }
            }
        }
        (dx + best_x.unwrap_or(0), dy + best_y.unwrap_or(0))
    }

    /// Snapshot of the visible windows' rectangles (screen pixels).
    fn layout_snapshot(&self) -> Vec<(WinKind, (i32, i32, i32, i32))> {
        self.visible_kinds()
            .into_iter()
            .map(|k| (k, self.rect_of(k)))
            .collect()
    }

    /// After window sizes changed (shade, double size, playlist resize), keep
    /// docked windows attached: each window anchored below or right of
    /// another follows that window's new edge.
    fn relayout_from(&mut self, before: Vec<(WinKind, (i32, i32, i32, i32))>) {
        let sizes: Vec<(WinKind, (i32, i32))> = before
            .iter()
            .map(|(k, _)| (*k, (self.rect_of(*k).2, self.rect_of(*k).3)))
            .collect();
        for (k, (x, y)) in relayout(&before, &sizes) {
            let g = self.st.geom(k);
            if g.x != x || g.y != y {
                self.set_window_pos(k, x, y);
            }
        }
    }

    fn cursor_global(&self, k: WinKind) -> (i32, i32) {
        if let Some(p) = crate::platform::global_cursor() {
            return p;
        }
        let g = self.st.geom(k);
        let c = self.wins.get(&k).map(|w| w.cursor).unwrap_or((0.0, 0.0));
        (g.x + c.0 as i32, g.y + c.1 as i32)
    }

    fn update_drag(&mut self) {
        let Some(drag) = &self.drag else { return };
        let Some(cur) = crate::platform::global_cursor().or_else(|| {
            let k = self.capture?;
            Some(self.cursor_global(k))
        }) else {
            return;
        };
        match drag {
            Drag::Move {
                start_cursor,
                windows,
            } => {
                let dx = cur.0 - start_cursor.0;
                let dy = cur.1 - start_cursor.1;
                let windows = windows.clone();
                // Snap relative to the start positions.
                let kinds: Vec<WinKind> = windows.iter().map(|w| w.0).collect();
                // Temporarily evaluate snapping from the original positions.
                let saved: Vec<(WinKind, i32, i32)> = kinds
                    .iter()
                    .map(|k| (*k, self.st.geom(*k).x, self.st.geom(*k).y))
                    .collect();
                for (k, x, y) in &windows {
                    let g = self.st.geom_mut(*k);
                    g.x = *x;
                    g.y = *y;
                }
                let (sdx, sdy) = self.snap_offset(&kinds, dx, dy);
                for (k, x, y) in saved {
                    let g = self.st.geom_mut(k);
                    g.x = x;
                    g.y = y;
                }
                for (k, x, y) in windows {
                    let (nx, ny) = (x + sdx, y + sdy);
                    if self.st.geom(k).x != nx || self.st.geom(k).y != ny {
                        self.set_window_pos(k, nx, ny);
                    }
                }
            }
            Drag::Resize {
                start_cursor,
                start_size,
            } => {
                let s = self.ui_scale;
                let dx = (cur.0 - start_cursor.0) / s;
                let dy = (cur.1 - start_cursor.1) / s;
                let before = self.layout_snapshot();
                let w_px = 275 + start_size.0 * PL_SEG_W + dx;
                let h_px = 116 + start_size.1 * PL_SEG_H + dy;
                let nw = ((w_px - 275 + PL_SEG_W / 2) / PL_SEG_W).max(0);
                let nh = if self.st.pl.shade {
                    start_size.1
                } else {
                    ((h_px - 116 + PL_SEG_H / 2) / PL_SEG_H).max(0)
                };
                if (nw, nh) != self.st.pl_size {
                    self.st.pl_size = (nw, nh);
                    self.sync_sizes();
                    self.relayout_from(before);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Input

    fn window_mouse(&mut self, k: WinKind, ev: MouseEvent) {
        let wm = match k {
            WinKind::Main => self.main.mouse(&mut self.st, ev),
            WinKind::Eq => self.eq.mouse(&mut self.st, ev),
            WinKind::Playlist => self.pl.mouse(&mut self.st, ev),
        };
        for r in wm {
            match r {
                WmRequest::BeginMove => {
                    let start = self.cursor_global(k);
                    let group = if k == WinKind::Main {
                        self.docked_group(k)
                    } else {
                        vec![k]
                    };
                    let windows = group
                        .into_iter()
                        .map(|w| (w, self.st.geom(w).x, self.st.geom(w).y))
                        .collect();
                    self.drag = Some(Drag::Move {
                        start_cursor: start,
                        windows,
                    });
                    self.capture = Some(k);
                }
                WmRequest::BeginResize => {
                    let start = self.cursor_global(k);
                    self.drag = Some(Drag::Resize {
                        start_cursor: start,
                        start_size: self.st.pl_size,
                    });
                    self.capture = Some(k);
                }
            }
        }
        if self.st.dirty
            && let Some(w) = self.wins.get(&k)
        {
            w.window.request_redraw();
        }
    }

    fn to_skin(&self, k: WinKind, p: (f64, f64)) -> (i32, i32) {
        let s = self.scale(k) as f64;
        ((p.0 / s).floor() as i32, (p.1 / s).floor() as i32)
    }

    fn update_cursor(&mut self, k: WinKind) {
        let Some(w) = self.wins.get(&k) else { return };
        let (x, y) = self.to_skin(k, w.cursor);
        let kind = match k {
            WinKind::Main => self.main.cursor(&self.st, x, y),
            WinKind::Eq => self.eq.cursor(&self.st, x, y),
            WinKind::Playlist => self.pl.cursor(&self.st, x, y),
        };
        let w = self.wins.get_mut(&k).unwrap();
        if w.cursor_kind != Some(kind) {
            w.cursor_kind = Some(kind);
            match self
                .cursors
                .get(&kind)
                .or_else(|| self.cursors.get(&CursorKind::Normal))
            {
                Some(c) => w.window.set_cursor(Cursor::Custom(c.clone())),
                None => w.window.set_cursor(Cursor::Icon(CursorIcon::Default)),
            }
        }
    }

    fn handle_key(
        &mut self,
        el: &ActiveEventLoop,
        key: &winit::keyboard::Key,
        code: PhysicalKey,
        focused: WinKind,
    ) {
        use winit::keyboard::Key as K;
        let m = self.mods;
        if !self.menus.is_empty() {
            self.menu_key(el, key);
            return;
        }
        let ch = match key {
            K::Character(s) => s.chars().next().map(|c| c.to_ascii_lowercase()),
            _ => None,
        };
        // Number pad (by physical key, independent of NumLock).
        if let PhysicalKey::Code(c) = code {
            let cmd = match c {
                KeyCode::Numpad0 => Some(Cmd::OpenFiles),
                KeyCode::Numpad1 => Some(Cmd::NextN(-10)),
                KeyCode::Numpad2 => Some(Cmd::VolumeBy(-1)),
                KeyCode::Numpad3 => Some(Cmd::NextN(10)),
                KeyCode::Numpad4 => Some(Cmd::Previous),
                KeyCode::Numpad5 => Some(Cmd::Play),
                KeyCode::Numpad6 => Some(Cmd::Next),
                KeyCode::Numpad7 => Some(Cmd::SeekBy(-5.0)),
                KeyCode::Numpad8 => Some(Cmd::VolumeBy(1)),
                KeyCode::Numpad9 => Some(Cmd::SeekBy(5.0)),
                _ => None,
            };
            if let Some(c) = cmd {
                self.st.push(c);
                return;
            }
        }
        // Playlist editor keys.
        if focused == WinKind::Playlist {
            let n = self.st.playlist.len();
            let sel = self.st.playlist.entries.iter().rposition(|e| e.selected);
            let moved = |app: &mut App, i: usize| {
                if m.shift {
                    app.st.playlist.shift_click(i);
                } else {
                    app.st.playlist.click(i);
                }
                pl_win::ensure_visible(&mut app.st, i);
                app.st.dirty = true;
            };
            match key {
                K::Named(NamedKey::ArrowUp) if n > 0 => {
                    if m.alt {
                        self.st.playlist.move_selected(-1);
                        self.st.dirty = true;
                    } else {
                        moved(self, sel.map(|s| s.saturating_sub(1)).unwrap_or(0));
                    }
                    return;
                }
                K::Named(NamedKey::ArrowDown) if n > 0 => {
                    if m.alt {
                        self.st.playlist.move_selected(1);
                        self.st.dirty = true;
                    } else {
                        moved(self, sel.map(|s| (s + 1).min(n - 1)).unwrap_or(0));
                    }
                    return;
                }
                K::Named(NamedKey::PageUp) if n > 0 => {
                    let rows = pl_win::Geometry::of(&self.st).visible_rows().max(1);
                    moved(self, sel.unwrap_or(0).saturating_sub(rows));
                    return;
                }
                K::Named(NamedKey::PageDown) if n > 0 => {
                    let rows = pl_win::Geometry::of(&self.st).visible_rows().max(1);
                    moved(self, (sel.unwrap_or(0) + rows).min(n - 1));
                    return;
                }
                K::Named(NamedKey::Home) if n > 0 => {
                    moved(self, 0);
                    return;
                }
                K::Named(NamedKey::End) if n > 0 => {
                    moved(self, n - 1);
                    return;
                }
                K::Named(NamedKey::Enter) => {
                    self.st.push(Cmd::PlPlaySelected);
                    return;
                }
                K::Named(NamedKey::Delete) => {
                    self.st.push(if m.ctrl && m.shift {
                        Cmd::PlClear
                    } else if m.ctrl {
                        Cmd::PlCrop
                    } else {
                        Cmd::PlRemoveSelected
                    });
                    return;
                }
                _ => {}
            }
            if m.ctrl {
                let cmd = match ch {
                    Some('a') => Some(Cmd::PlSelectAll),
                    Some('i') => Some(Cmd::PlInvertSelection),
                    Some('n') => Some(Cmd::PlNew),
                    Some('s') => Some(Cmd::PlSave),
                    Some('o') => Some(Cmd::PlLoad),
                    Some('e') => Some(Cmd::PlEditEntry),
                    Some('r') if m.shift => Some(Cmd::PlRandomize),
                    _ => None,
                };
                if let Some(c) = cmd {
                    self.st.push(c);
                    return;
                }
            }
            if m.alt && ch == Some('3') {
                self.st.push(Cmd::FileInfo);
                return;
            }
        }
        let cmd = match (key, ch) {
            (K::Named(NamedKey::ArrowLeft), _) => Some(Cmd::SeekBy(-5.0)),
            (K::Named(NamedKey::ArrowRight), _) => Some(Cmd::SeekBy(5.0)),
            (K::Named(NamedKey::ArrowUp), _) => Some(Cmd::VolumeBy(1)),
            (K::Named(NamedKey::ArrowDown), _) => Some(Cmd::VolumeBy(-1)),
            (K::Named(NamedKey::Insert), _) if focused == WinKind::Playlist => {
                Some(Cmd::PlAddFiles)
            }
            (_, Some('i')) if m.ctrl && m.alt => Some(Cmd::EditBookmarks),
            (_, Some('k')) if m.ctrl && m.shift => Some(Cmd::ToggleVisPlugin),
            (_, Some(c)) if m.alt => match c {
                'i' => Some(Cmd::AddBookmark),
                'o' => Some(Cmd::VisOptionsDialog),
                'k' => Some(Cmd::VisPluginConfig),
                'w' => Some(Cmd::ToggleWindow(WinKind::Main)),
                'e' => Some(Cmd::ToggleWindow(WinKind::Playlist)),
                'g' => Some(Cmd::ToggleWindow(WinKind::Eq)),
                's' => Some(Cmd::SkinBrowser),
                '3' => Some(Cmd::FileInfo),
                _ => None,
            },
            (_, Some(c)) if m.ctrl => match c {
                'k' => Some(Cmd::PluginsDialog),
                'd' => Some(Cmd::ToggleDouble),
                'a' => Some(Cmd::ToggleAlwaysOnTop),
                'e' => Some(Cmd::ToggleEasyMove),
                't' => Some(Cmd::ToggleTimeMode),
                'l' => Some(Cmd::OpenUrl),
                'j' => Some(Cmd::JumpToTime),
                'p' => Some(Cmd::Preferences),
                'v' => Some(Cmd::StopAfterCurrent),
                'z' => Some(Cmd::StartOfList),
                'r' => Some(Cmd::PlReverse),
                'w' => Some(Cmd::ToggleShade(focused)),
                _ => None,
            },
            (_, Some(c)) => match c {
                'z' => Some(Cmd::Previous),
                'x' => Some(Cmd::Play),
                'c' => Some(Cmd::Pause),
                'v' if m.shift => Some(Cmd::StopFadeout),
                'v' => Some(Cmd::Stop),
                'b' => Some(Cmd::Next),
                'l' if m.shift => Some(Cmd::OpenFolder),
                'l' => Some(Cmd::OpenFiles),
                'j' => Some(Cmd::JumpToFile),
                'r' => Some(Cmd::ToggleRepeat),
                's' => Some(Cmd::ToggleShuffle),
                _ => None,
            },
            _ => None,
        };
        // Easter egg: typing "nullsoft"-style magic word toggles the alternate title bar.
        if let Some(c) = ch {
            self.llama_keys(c);
        }
        if let Some(c) = cmd {
            self.st.push(c);
        }
    }

    fn llama_keys(&mut self, c: char) {
        thread_local! {
            static BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
        }
        BUF.with(|b| {
            let mut b = b.borrow_mut();
            b.push(c);
            if b.len() > 7 {
                let cut = b.len() - 7;
                b.drain(..cut);
            }
            if b.as_str() == "ferramp" {
                b.clear();
                self.st.llama = !self.st.llama;
                self.st.dirty = true;
            }
        });
    }

    // ------------------------------------------------------------------
    // Menus

    fn open_menu(&mut self, el: &ActiveEventLoop, id: MenuId, x: i32, y: i32, owner: WinKind) {
        self.close_menus();
        let skins = list_skins();
        let bookmarks: Vec<String> = self.bookmarks.iter().map(|b| b.0.clone()).collect();
        let entries = match (&self.plugins, id) {
            (Some(sys), MenuId::Main) if self.plugins_ready => sys.menu_entries(),
            _ => Vec::new(),
        };
        let plugin_items: Vec<(String, bool, bool)> = entries
            .iter()
            .map(|(_, e)| (e.label.clone(), e.checked, e.enabled))
            .collect();
        self.plugin_menu = entries.iter().map(|(a, e)| (*a, e.id)).collect();
        let items = menu::build(
            id,
            &MenuContext {
                state: &self.st,
                skins: &skins,
                bookmarks: &bookmarks,
                plugin_items: &plugin_items,
            },
        );
        let g = *self.st.geom(owner);
        let s = self.scale(owner);
        self.menu_owner = Some(owner);
        self.menu_opened_at = Instant::now();
        // A press that opened the menu must be released before a press
        // outside it can close the menu again.
        self.menu_released = !mouse_button_down();
        self.push_menu(el, items, g.x + x * s, g.y + y * s);
    }

    fn push_menu(&mut self, el: &ActiveEventLoop, items: Vec<Item>, mut x: i32, mut y: i32) {
        let level = MenuLevel::new(items, &self.st.fonts);
        let (w, h) = (level.width * self.ui_scale, level.height * self.ui_scale);
        // Keep the menu on its monitor.
        for (mx, my, mw, mh) in self.monitor_rects() {
            if x >= mx && x < mx + mw && y >= my && y < my + mh {
                if x + w > mx + mw {
                    x = (mx + mw - w).max(mx);
                }
                if y + h > my + mh {
                    y = (my + mh - h).max(my);
                }
            }
        }
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(w as u32, h as u32))
            .with_position(PhysicalPosition::new(x, y))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false);
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            attrs = attrs
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::PopupMenu]);
        }
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                debug!("menu window: {e}");
                return;
            }
        };
        let Some(os) = OsWin::new(window) else {
            debug!("menu surface failed");
            return;
        };
        debug!("menu at {x},{y} size {w}x{h}");
        crate::platform::make_popup(&os.window);
        os.window.request_redraw();
        self.menus.push(MenuPopup { level, os, x, y });
    }

    fn close_menus(&mut self) {
        self.menus.clear();
        self.menu_owner = None;
    }

    fn close_menus_from(&mut self, depth: usize) {
        self.menus.truncate(depth);
        if self.menus.is_empty() {
            self.menu_owner = None;
        }
    }

    fn menu_index(&self, id: WindowId) -> Option<usize> {
        self.menus.iter().position(|m| m.os.window.id() == id)
    }

    fn menu_hover(&mut self, el: &ActiveEventLoop, depth: usize, item: Option<usize>) {
        let m = &mut self.menus[depth];
        if m.level.hover == item {
            return;
        }
        m.level.hover = item;
        m.os.window.request_redraw();
        self.close_menus_from(depth + 1);
        if let Some(i) = item
            && let Item::Sub {
                items,
                enabled: true,
                ..
            } = &self.menus[depth].level.items[i]
        {
            let items = items.clone();
            let m = &self.menus[depth];
            let r = m.level.item_rect(i);
            let x = m.x + (m.level.width - 3) * self.ui_scale;
            let y = m.y + (r.y - 3) * self.ui_scale;
            self.push_menu(el, items, x, y);
        }
    }

    fn menu_activate(&mut self, el: &ActiveEventLoop, depth: usize, i: usize) {
        match self.menus[depth].level.items.get(i).cloned() {
            Some(Item::Action {
                cmd, enabled: true, ..
            }) => {
                self.close_menus();
                self.st.push(cmd);
            }
            Some(Item::Sub { .. }) => {
                self.menus[depth].level.hover = None;
                self.menu_hover(el, depth, Some(i));
            }
            _ => {}
        }
    }

    fn menu_key(&mut self, el: &ActiveEventLoop, key: &winit::keyboard::Key) {
        use winit::keyboard::Key as K;
        let depth = self.menus.len() - 1;
        let n = self.menus[depth].level.items.len();
        let selectable: Vec<usize> = (0..n)
            .filter(|&i| !matches!(self.menus[depth].level.items[i], Item::Sep))
            .collect();
        if selectable.is_empty() {
            return;
        }
        let cur = self.menus[depth]
            .level
            .hover
            .and_then(|h| selectable.iter().position(|&s| s == h));
        match key {
            K::Named(NamedKey::Escape) => {
                if depth == 0 {
                    self.close_menus();
                } else {
                    self.close_menus_from(depth);
                }
            }
            K::Named(NamedKey::ArrowDown) => {
                let next = cur.map(|c| (c + 1) % selectable.len()).unwrap_or(0);
                self.menus[depth].level.hover = Some(selectable[next]);
                self.menus[depth].os.window.request_redraw();
            }
            K::Named(NamedKey::ArrowUp) => {
                let next = cur
                    .map(|c| (c + selectable.len() - 1) % selectable.len())
                    .unwrap_or(selectable.len() - 1);
                self.menus[depth].level.hover = Some(selectable[next]);
                self.menus[depth].os.window.request_redraw();
            }
            K::Named(NamedKey::ArrowRight) | K::Named(NamedKey::Enter) => {
                if let Some(c) = cur {
                    self.menu_activate(el, depth, selectable[c]);
                    if let Some(sub) = self.menus.get_mut(depth + 1) {
                        sub.level.hover =
                            sub.level.items.iter().position(|i| !matches!(i, Item::Sep));
                    }
                }
            }
            K::Named(NamedKey::ArrowLeft) if depth > 0 => self.close_menus_from(depth),
            _ => {}
        }
    }

    /// Close menus when the mouse is pressed outside all of them.
    fn check_menu_outside_click(&mut self) {
        if self.menus.is_empty() || self.menu_opened_at.elapsed() < Duration::from_millis(150) {
            return;
        }
        if !mouse_button_down() {
            self.menu_released = true;
            return;
        }
        if !self.menu_released {
            return;
        }
        let Some((cx, cy)) = crate::platform::global_cursor() else {
            return;
        };
        let inside = self.menus.iter().any(|m| {
            let s = m.os.window.inner_size();
            cx >= m.x && cy >= m.y && cx < m.x + s.width as i32 && cy < m.y + s.height as i32
        });
        if !inside {
            debug!("menu: outside click at {cx},{cy}");
            self.close_menus();
        }
    }

    /// Keep an open Jump-to-file dialog in step with the playlist, so its
    /// rows never point at entries that have moved.
    fn sync_jump_dialog(&mut self) {
        let entries = &self.st.playlist.entries;
        for d in &mut self.dialogs {
            if d.dialog.kind == DialogKind::JumpToFile
                && !d
                    .dialog
                    .entries
                    .iter()
                    .map(String::as_str)
                    .eq(entries.iter().map(|e| e.title.as_str()))
            {
                d.dialog
                    .set_entries(entries.iter().map(|e| e.title.clone()).collect());
                d.os.window.request_redraw();
            }
        }
    }

    /// Taskbar title: "N. Title - Ferramp", like the original.
    fn update_title(&mut self) {
        let title = match self.st.media_text_short() {
            Some(t) => format!("{t} - Ferramp"),
            None => "Ferramp".to_string(),
        };
        if title != self.window_title
            && let Some(w) = self.wins.get(&WinKind::Main)
        {
            w.window.set_title(&title);
            self.window_title = title;
        }
    }

    // ------------------------------------------------------------------
    // Tooltips

    fn hide_tooltip(&mut self) {
        self.tip = None;
    }

    fn update_tooltip(&mut self, el: &ActiveEventLoop) {
        let Some((k, text, since)) = self.hover_tip.clone() else {
            self.hide_tooltip();
            return;
        };
        let busy = !self.menus.is_empty()
            || self.drag.is_some()
            || self.capture.is_some()
            || mouse_button_down();
        if busy || since.elapsed() > Duration::from_secs(7) {
            self.hide_tooltip();
            if busy {
                self.hover_tip = None;
            }
            return;
        }
        if self.tip.is_some() || since.elapsed() < Duration::from_millis(600) {
            return;
        }
        let Some((cx, cy)) = crate::platform::global_cursor() else {
            return;
        };
        // The pointer may have left without a leave event (window moved away).
        let (wx, wy, ww, wh) = self.rect_of(k);
        if cx < wx || cy < wy || cx >= wx + ww || cy >= wy + wh {
            self.hover_tip = None;
            return;
        }
        let fonts = &self.st.fonts;
        let tw = fonts.measure(&text, menu::FONT_PX, crate::text::Weight::Regular);
        let (w, h) = (tw + 6, 16);
        let s = self.ui_scale;
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_decorations(false)
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new((w * s) as u32, (h * s) as u32))
            .with_position(PhysicalPosition::new(cx, cy + 20 * s))
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false);
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            attrs = attrs
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::Tooltip]);
        }
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_skip_taskbar(true);
        }
        let Ok(window) = el.create_window(attrs) else {
            return;
        };
        let Some(os) = OsWin::new(window) else { return };
        crate::platform::make_popup(&os.window);
        os.window.request_redraw();
        self.tip = Some(Tooltip { os, w, h, text });
    }

    // ------------------------------------------------------------------
    // Dialogs

    fn open_dialog(&mut self, el: &ActiveEventLoop, dialog: Dialog) {
        if let Some(i) = self
            .dialogs
            .iter()
            .position(|d| d.dialog.kind == dialog.kind)
        {
            self.dialogs[i].os.window.focus_window();
            return;
        }
        let s = self.ui_scale;
        let main = self.st.main;
        let (w, h) = (dialog.w * s, dialog.h * s);
        let (mut x, mut y) = (main.x + 40, main.y + 40);
        // Keep the dialog on the monitor that holds the main window.
        if let Some(m) = el.available_monitors().find(|m| {
            let (p, sz) = (m.position(), m.size());
            main.x >= p.x
                && main.y >= p.y
                && main.x < p.x + sz.width as i32
                && main.y < p.y + sz.height as i32
        }) {
            let (p, sz) = (m.position(), m.size());
            x = x.min(p.x + sz.width as i32 - w - 8).max(p.x);
            y = y.min(p.y + sz.height as i32 - h - 48).max(p.y);
        }
        let attrs = Window::default_attributes()
            .with_title(dialog.title.clone())
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(w as u32, h as u32))
            .with_position(PhysicalPosition::new(x, y))
            .with_window_icon(Some(app_icon()));
        let Ok(window) = el.create_window(attrs) else {
            return;
        };
        let Some(os) = OsWin::new(window) else { return };
        os.window.request_redraw();
        if self.st.always_on_top {
            os.window.set_window_level(WindowLevel::AlwaysOnTop);
        }
        self.dialogs.push(DialogWin { dialog, os });
    }

    /// Shows the classic skin offer and fetches the original's preview.
    fn open_classic_skin(&mut self, el: &ActiveEventLoop) {
        if let Some(d) = self
            .dialogs
            .iter()
            .find(|d| d.dialog.kind == DialogKind::ClassicSkin)
        {
            d.os.window.focus_window();
            return;
        }
        let builtin = crate::render::docked(Skin::builtin());
        self.open_dialog(el, Dialog::classic_skin(builtin));
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let r = crate::classic_skin::preview();
            let _ = proxy.send_event(UserEvent::Classic(crate::classic_skin::Fetched::Preview(r)));
        });
    }

    fn download_classic_skin(&mut self) {
        if self.classic_busy {
            return;
        }
        self.classic_busy = true;
        self.classic_status("Downloading...");
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let r = crate::classic_skin::download();
            let _ = proxy.send_event(UserEvent::Classic(crate::classic_skin::Fetched::Skin(r)));
        });
    }

    fn classic_status(&mut self, msg: &str) {
        for d in &mut self.dialogs {
            if d.dialog.kind == DialogKind::ClassicSkin {
                d.dialog.set_status(msg);
                d.os.window.request_redraw();
            }
        }
    }

    fn classic_fetched(&mut self, el: &ActiveEventLoop, f: crate::classic_skin::Fetched) {
        use crate::classic_skin::Fetched;
        match f {
            Fetched::Preview(r) => {
                for d in &mut self.dialogs {
                    if d.dialog.kind == DialogKind::ClassicSkin {
                        d.dialog.set_image("original", r);
                        d.os.window.request_redraw();
                        break;
                    }
                }
            }
            Fetched::Skin(Ok(path)) => {
                self.classic_busy = false;
                self.dialogs
                    .retain(|d| d.dialog.kind != DialogKind::ClassicSkin);
                self.load_skin(el, Some(path));
            }
            Fetched::Skin(Err(e)) => {
                self.classic_busy = false;
                self.classic_status(&format!("Download failed: {e}"));
            }
        }
    }

    fn museum_dialog(&mut self) -> Option<&mut DialogWin> {
        self.dialogs
            .iter_mut()
            .find(|d| d.dialog.kind == DialogKind::SkinMuseum)
    }

    fn museum_status(&mut self, msg: &str) {
        if let Some(d) = self.museum_dialog() {
            d.dialog.set_status(msg);
            d.os.window.request_redraw();
        }
    }

    fn open_museum(&mut self, el: &ActiveEventLoop) {
        if let Some(d) = self.museum_dialog() {
            d.os.window.focus_window();
            return;
        }
        self.museum = Museum::default();
        self.open_dialog(el, Dialog::skin_museum());
        self.museum_load(String::new(), false);
    }

    /// Fetches the first page of `query`, or the next page when `more`.
    fn museum_load(&mut self, query: String, more: bool) {
        if self.museum.loading || (more && self.museum.exhausted) {
            return;
        }
        let offset = if more { self.museum.results.len() } else { 0 };
        self.museum.query = query.clone();
        self.museum.loading = true;
        self.museum_status("Loading...");
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = crate::museum::page(&query, offset);
            let f = crate::museum::Fetched::Page {
                query,
                offset,
                result,
            };
            let _ = proxy.send_event(UserEvent::Museum(f));
        });
    }

    fn museum_select(&mut self, i: usize) {
        let Some(e) = self.museum.results.get(i).cloned() else {
            return;
        };
        self.museum.selected = Some(e.md5.clone());
        if let Some(b) = self.museum.shots.get(&e.md5).cloned() {
            if let Some(d) = self.museum_dialog() {
                d.dialog.set_image("shot", Ok(b));
                d.os.window.request_redraw();
            }
            return;
        }
        if let Some(d) = self.museum_dialog() {
            d.dialog.set_image_note("shot", "Loading preview...");
            d.os.window.request_redraw();
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = crate::museum::screenshot(&e.md5);
            let f = crate::museum::Fetched::Screenshot { md5: e.md5, result };
            let _ = proxy.send_event(UserEvent::Museum(f));
        });
    }

    fn museum_download(&mut self, i: usize) {
        let Some(e) = self.museum.results.get(i).cloned() else {
            return;
        };
        if self.museum.downloading {
            return;
        }
        self.museum.downloading = true;
        let name = e.name();
        self.museum_status(&format!("Downloading {name}..."));
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = crate::museum::download(&e);
            let f = crate::museum::Fetched::Skin { name, result };
            let _ = proxy.send_event(UserEvent::Museum(f));
        });
    }

    fn museum_fetched(&mut self, el: &ActiveEventLoop, f: crate::museum::Fetched) {
        use crate::museum::{Fetched, PAGE};
        match f {
            Fetched::Page {
                query,
                offset,
                result,
            } => {
                if query != self.museum.query {
                    return;
                }
                self.museum.loading = false;
                match result {
                    Ok(page) => {
                        self.museum.exhausted = page.len() < PAGE / 2;
                        if offset == 0 {
                            self.museum.results = page;
                        } else {
                            self.museum.results.extend(page);
                        }
                        let items: Vec<String> =
                            self.museum.results.iter().map(|e| e.name()).collect();
                        let n = items.len();
                        let msg = if n == 0 {
                            "No skins found.".to_string()
                        } else {
                            format!("{n} skins. Each skin belongs to its author.")
                        };
                        if let Some(d) = self.museum_dialog() {
                            d.dialog.set_list_items(items, offset > 0);
                            if offset == 0 {
                                d.dialog.set_image_note("shot", "Pick a skin to see it");
                            }
                        }
                        self.museum_status(&msg);
                    }
                    Err(e) => self.museum_status(&format!("Could not reach the museum: {e}")),
                }
            }
            Fetched::Screenshot { md5, result } => {
                if let Ok(b) = &result {
                    self.museum.shots.insert(md5.clone(), b.clone());
                }
                if self.museum.selected.as_deref() == Some(md5.as_str())
                    && let Some(d) = self.museum_dialog()
                {
                    d.dialog.set_image("shot", result);
                    d.os.window.request_redraw();
                }
            }
            Fetched::Skin { name, result } => {
                self.museum.downloading = false;
                match result {
                    Ok(path) => {
                        self.load_skin(el, Some(path));
                        self.museum_status(&format!("Now using {name}."));
                    }
                    Err(e) => self.museum_status(&format!("Download failed: {e}")),
                }
            }
        }
    }

    fn dialog_index(&self, id: WindowId) -> Option<usize> {
        self.dialogs.iter().position(|d| d.os.window.id() == id)
    }

    fn dialog_outcome(&mut self, idx: usize, out: Outcome) {
        if self.dialogs[idx].dialog.kind == DialogKind::Preferences {
            self.prefs = self.dialogs[idx].dialog.prefs;
            self.st.dirty = true;
        }
        match out {
            Outcome::None => {}
            Outcome::Close => {
                self.dialogs.remove(idx);
            }
            Outcome::Run(cmds) => {
                for c in cmds {
                    self.st.push(c);
                }
            }
            Outcome::Done(cmds) => {
                for c in cmds {
                    self.st.push(c);
                }
                self.dialogs.remove(idx);
            }
        }
        for d in &self.dialogs {
            d.os.window.request_redraw();
        }
    }

    fn file_info_dialog(&self) -> Option<Dialog> {
        let idx = self
            .st
            .playlist
            .entries
            .iter()
            .position(|e| e.selected)
            .or(self.st.playlist.current)?;
        let e = self.st.playlist.entries.get(idx)?;
        let mut fields = vec![("Location".to_string(), e.location())];
        if let Some(p) = e.path()
            && let Some(info) = metadata::read(p)
        {
            let opt = |o: &Option<String>| o.clone().unwrap_or_default();
            fields.push(("Title".into(), opt(&info.title)));
            fields.push(("Artist".into(), opt(&info.artist)));
            fields.push(("Album".into(), opt(&info.album)));
            fields.push((
                "Year".into(),
                info.year.map(|y| y.to_string()).unwrap_or_default(),
            ));
            fields.push((
                "Track".into(),
                info.track_no.map(|y| y.to_string()).unwrap_or_default(),
            ));
            fields.push(("Genre".into(), opt(&info.genre)));
            fields.push(("Comment".into(), opt(&info.comment)));
            fields.push((
                "Length".into(),
                info.duration.map(format_time).unwrap_or_default(),
            ));
            fields.push((
                "Bitrate".into(),
                info.bitrate
                    .map(|b| format!("{b} kbps"))
                    .unwrap_or_default(),
            ));
            fields.push((
                "Sample rate".into(),
                info.sample_rate
                    .map(|b| format!("{b} Hz"))
                    .unwrap_or_default(),
            ));
            fields.push((
                "Channels".into(),
                info.channels
                    .map(|c| {
                        if c == 1 {
                            "Mono".into()
                        } else {
                            format!("{c} (stereo)")
                        }
                    })
                    .unwrap_or_default(),
            ));
            if let Ok(m) = std::fs::metadata(p) {
                fields.push(("File size".into(), format!("{} bytes", m.len())));
            }
        } else {
            fields.push(("Title".into(), e.title.clone()));
        }
        Some(Dialog::file_info(fields))
    }

    // ------------------------------------------------------------------
    // Files

    fn pick(&self, pick: Pick) {
        let proxy = self.proxy.clone();
        let start = self.settings.get_str("last_dir").map(PathBuf::from);
        let run = move || {
            let mut d = rfd::FileDialog::new();
            if let Some(s) = &start {
                d = d.set_directory(s);
            }
            let plugin_ext: Vec<String> = crate::plugins::global()
                .map(|s| s.extensions())
                .unwrap_or_default();
            let mut audio: Vec<&str> = metadata::AUDIO_EXTENSIONS.to_vec();
            audio.extend(plugin_ext.iter().map(String::as_str));
            let result: Vec<PathBuf> = match pick {
                Pick::OpenFiles | Pick::AddFiles => d
                    .set_title(if pick == Pick::OpenFiles {
                        "Open file(s)"
                    } else {
                        "Add file(s) to playlist"
                    })
                    .add_filter(
                        "All supported types",
                        &[audio.as_slice(), &["m3u", "m3u8", "pls"]].concat(),
                    )
                    .add_filter("Playlists", &["m3u", "m3u8", "pls"])
                    .add_filter("All files", &["*"])
                    .pick_files()
                    .unwrap_or_default(),
                Pick::OpenFolder | Pick::AddFolder => d
                    .set_title("Open folder")
                    .pick_folder()
                    .into_iter()
                    .collect(),
                Pick::LoadPlaylist => d
                    .set_title("Load playlist")
                    .add_filter("Playlists", &["m3u", "m3u8", "pls"])
                    .pick_file()
                    .into_iter()
                    .collect(),
                Pick::SavePlaylist => d
                    .set_title("Save playlist")
                    .add_filter("M3U playlist", &["m3u"])
                    .add_filter("M3U8 playlist (UTF-8)", &["m3u8"])
                    .add_filter("PLS playlist", &["pls"])
                    .set_file_name("playlist.m3u")
                    .save_file()
                    .into_iter()
                    .collect(),
                Pick::LoadSkin => d
                    .set_title("Load skin")
                    .add_filter("Skins", &["wsz", "zip"])
                    .pick_file()
                    .into_iter()
                    .collect(),
                Pick::LoadEqf => d
                    .set_title("Load equalizer preset")
                    .add_filter("Equalizer presets", &["eqf", "q1"])
                    .pick_file()
                    .into_iter()
                    .collect(),
                Pick::SaveEqf => d
                    .set_title("Save equalizer preset")
                    .add_filter("Equalizer presets", &["eqf"])
                    .set_file_name("preset.eqf")
                    .save_file()
                    .into_iter()
                    .collect(),
                Pick::SaveHtml => d
                    .set_title("Save HTML playlist")
                    .add_filter("HTML", &["html"])
                    .set_file_name("playlist.html")
                    .save_file()
                    .into_iter()
                    .collect(),
            };
            let _ = proxy.send_event(UserEvent::Picked(pick, result));
        };
        if cfg!(target_os = "macos") {
            run();
        } else {
            std::thread::spawn(run);
        }
    }

    fn request_info(&self, ids: &[u64]) {
        for id in ids {
            if let Some(i) = self.st.playlist.index_of(*id)
                && let Some(p) = self.st.playlist.entries[i].path()
            {
                self.info.request(*id, p.to_path_buf());
            }
        }
    }

    /// Expand folders and playlist files into playlist items.
    fn expand(paths: Vec<PathBuf>) -> Vec<(Source, Option<String>, Option<f64>)> {
        let mut out = Vec::new();
        for p in paths {
            let s = p.to_string_lossy().to_string();
            if s.contains("://") {
                out.push((Source::parse(&s), None, None));
            } else if p.is_dir() {
                let mut files = Vec::new();
                walk(&p, &mut files, 0);
                files.sort_by_key(|f| f.to_string_lossy().to_lowercase());
                out.extend(files.into_iter().map(|f| (Source::File(f), None, None)));
            } else if metadata::is_playlist_file(&p) {
                out.extend(playlist::load_playlist_file(&p));
            } else {
                out.push((Source::File(p), None, None));
            }
        }
        out
    }

    /// Replace the playlist and start playing (`play`), or append.
    fn open_paths(&mut self, paths: Vec<PathBuf>, play: bool) {
        if let Some(parent) = paths.first().and_then(|p| p.parent()) {
            self.settings.set("last_dir", parent.to_string_lossy());
        }
        let items = Self::expand(paths);
        if items.is_empty() {
            return;
        }
        if play {
            self.st.engine.stop();
            self.st.playlist.clear();
            let ids = self.st.playlist.append(items);
            self.request_info(&ids);
            self.play_index(0);
        } else {
            let ids = self.st.playlist.append(items);
            self.request_info(&ids);
        }
        self.st.dirty = true;
    }

    fn insert_paths(&mut self, paths: Vec<PathBuf>, at: usize) {
        let items = Self::expand(paths);
        let ids = self.st.playlist.insert(at, items);
        self.request_info(&ids);
        self.st.dirty = true;
    }

    // ------------------------------------------------------------------
    // Playback

    fn play_index(&mut self, i: usize) {
        let Some(e) = self.st.playlist.entries.get(i) else {
            return;
        };
        let src = e.source.clone();
        let title = e.title.clone();
        let duration = e.duration;
        self.st.playlist.set_current(i);
        self.st.now = Default::default();
        self.st.now.title = Some(title);
        if self.st.fade.take().is_some() {
            self.st.engine.set_volume(self.st.volume as f32 / 100.0);
        }
        if let Some(d) = duration {
            self.st.now.duration = Some(d);
        }
        self.st.error = None;
        self.st.working = true;
        self.st.vis.reset();
        self.st.marquee_step = 0;
        self.st.marquee_drag = 0;
        self.st.engine.play(src);
        if self.st.eq_auto
            && let Some(k) = self.current_file_key()
            && let Some((_, p, b)) = self.eq_auto_presets.iter().find(|x| x.0 == k).cloned()
        {
            self.st.eq.preamp = p;
            self.st.eq.bands = b;
            self.st.engine.set_eq(self.st.eq);
        }
        pl_win::ensure_visible(&mut self.st, i);
        self.st.dirty = true;
    }

    fn track_finished(&mut self) {
        if self.st.stop_after_current {
            self.st.stop_after_current = false;
            self.st.engine.mark_stopped();
            self.st.dirty = true;
            return;
        }
        let (shuffle, repeat) = (self.st.shuffle, self.st.repeat);
        match self.st.playlist.next_index(shuffle, repeat) {
            Some(n) => self.play_index(n),
            None => {
                self.st.engine.mark_stopped();
                self.st.dirty = true;
            }
        }
    }

    fn poll_audio(&mut self) {
        while let Some(ev) = self.st.engine.next_event() {
            match ev {
                AudioEvent::Opened {
                    duration,
                    kbps,
                    khz,
                    channels,
                    stream_name,
                } => {
                    self.st.working = false;
                    let st = &mut self.st;
                    if duration.is_some() {
                        st.now.duration = duration;
                    }
                    st.now.kbps = kbps;
                    st.now.khz = khz;
                    st.now.channels = channels;
                    st.now.stream_name = stream_name.clone();
                    if let Some(c) = st.playlist.current {
                        let e = &mut st.playlist.entries[c];
                        if e.duration.is_none() {
                            e.duration = duration;
                        }
                        if let Some(n) = stream_name
                            && !e.has_info
                        {
                            e.title = n;
                            e.has_info = true;
                        }
                    }
                    st.dirty = true;
                }
                AudioEvent::Error(e) => {
                    self.st.working = false;
                    self.errors_in_row += 1;
                    self.st.engine.mark_stopped();
                    self.st.show_message(e.clone(), 3.0);
                    self.st.error = Some(e);
                    if self.errors_in_row < self.st.playlist.len().max(1) {
                        let (shuffle, repeat) = (self.st.shuffle, self.st.repeat);
                        if let Some(n) = self.st.playlist.next_index(shuffle, repeat) {
                            self.play_index(n);
                        }
                    }
                }
                AudioEvent::StreamTitle(t) => {
                    self.st.now.stream_title = Some(playlist::clean_title(&t));
                    self.st.dirty = true;
                }
            }
        }
        // Only audio that reaches the output proves a track works; an
        // opened file that fails at the device still counts as an error.
        if self.errors_in_row > 0 && self.st.engine.has_played() {
            self.errors_in_row = 0;
        }
        if self.st.engine.finished() {
            self.track_finished();
        }
        while let Ok((id, info)) = self.info.rx.try_recv() {
            match info {
                Some(i) => self.st.playlist.set_info(id, i.display_title(), i.duration),
                None => self.st.playlist.set_info(id, None, None),
            }
            self.st.dirty = true;
        }
    }

    // ------------------------------------------------------------------
    // Commands

    fn run(&mut self, el: &ActiveEventLoop, cmd: Cmd) {
        // With a visualization plugin chosen, Ctrl+Shift+K starts and stops it.
        if cmd == Cmd::ToggleVisPlugin && self.selected_vis_plugin().is_some() {
            self.toggle_vis_plugin();
            return;
        }
        debug!("cmd: {cmd:?}");
        let st = &mut self.st;
        st.dirty = true;
        match cmd {
            Cmd::Play => match st.status() {
                Status::Paused => st.engine.resume(),
                _ => {
                    if st.playlist.is_empty() {
                        self.pick(Pick::OpenFiles);
                    } else {
                        let i = st.playlist.current.unwrap_or(0);
                        self.play_index(i);
                    }
                }
            },
            Cmd::Pause => st.engine.pause(),
            Cmd::Stop => {
                st.engine.stop();
                st.fade = None;
                st.engine.set_volume(st.volume as f32 / 100.0);
            }
            Cmd::StopFadeout => {
                if st.status() == Status::Playing {
                    st.fade = Some((Instant::now(), st.volume));
                } else {
                    st.engine.stop();
                }
            }
            Cmd::StopAfterCurrent => st.stop_after_current = !st.stop_after_current,
            Cmd::Next | Cmd::Previous | Cmd::NextN(_) | Cmd::StartOfList => {
                let (shuffle, repeat) = (st.shuffle, st.repeat);
                if st.status() == Status::Stopped {
                    st.playlist.orphaned = false;
                }
                let n = st.playlist.len();
                if n == 0 {
                    return;
                }
                let target = match cmd {
                    Cmd::Next => st.playlist.next_index(shuffle, true),
                    Cmd::Previous => st.playlist.prev_index(shuffle),
                    Cmd::NextN(k) => {
                        let c = st.playlist.current.unwrap_or(0) as i64;
                        Some((c + k as i64).clamp(0, n as i64 - 1) as usize)
                    }
                    _ => Some(0),
                };
                let _ = repeat;
                if let Some(t) = target {
                    if st.status() == Status::Stopped {
                        st.playlist.set_current(t);
                        st.now = Default::default();
                        pl_win::ensure_visible(st, t);
                    } else {
                        let paused = st.status() == Status::Paused;
                        self.play_index(t);
                        if paused {
                            self.st.engine.pause();
                        }
                    }
                }
            }
            Cmd::Eject | Cmd::OpenFiles => self.pick(Pick::OpenFiles),
            Cmd::OpenFolder => self.pick(Pick::OpenFolder),
            Cmd::OpenUrl => self.open_dialog(el, Dialog::open_url(false)),
            Cmd::OpenLocation(loc, play) => {
                let p = PathBuf::from(&loc);
                if play {
                    self.open_paths(vec![p], true);
                } else {
                    self.open_paths(vec![p], false);
                }
            }
            Cmd::Enqueue(i) => {
                // Play the chosen entry after the current one.
                if let Some(c) = st.playlist.current
                    && i != c
                    && i < st.playlist.len()
                {
                    st.playlist.select_none();
                    st.playlist.entries[i].selected = true;
                    // Moving an entry from above the current one shifts the
                    // current entry up by one.
                    let target = if i < c {
                        c
                    } else {
                        (c + 1).min(st.playlist.len() - 1)
                    } as isize;
                    let delta = target - i as isize;
                    st.playlist.move_selected(delta);
                    st.show_message("Queued to play next", 1.5);
                }
            }
            Cmd::PlayIndex(i) => self.play_index(i),
            Cmd::SeekTo(t) => {
                if st.status() != Status::Stopped {
                    st.engine.seek(t);
                }
            }
            Cmd::SeekBy(d) => {
                if st.status() != Status::Stopped {
                    let p = (st.position() + d).max(0.0);
                    match st.duration() {
                        Some(len) if p >= len => {}
                        _ => st.engine.seek(p),
                    }
                }
            }
            Cmd::SetVolume(v) => {
                st.volume = v.min(100);
                st.engine.set_volume(st.volume as f32 / 100.0);
            }
            Cmd::VolumeBy(d) => {
                st.volume = (st.volume as i32 + d).clamp(0, 100) as u8;
                st.engine.set_volume(st.volume as f32 / 100.0);
                let text = crate::ui::volume_text(st.volume);
                st.show_message(text, 1.0);
            }
            Cmd::SetBalance(b) => {
                st.balance = b.clamp(-100, 100);
                st.engine.set_balance(st.balance as f32 / 100.0);
            }
            Cmd::ToggleShuffle => st.shuffle = !st.shuffle,
            Cmd::ToggleRepeat => st.repeat = !st.repeat,
            Cmd::ToggleTimeMode => st.time_remaining = !st.time_remaining,
            Cmd::ToggleDouble => {
                let before = self.layout_snapshot();
                self.st.doubled = !self.st.doubled;
                self.sync_sizes();
                self.relayout_from(before);
            }
            Cmd::ToggleAlwaysOnTop => {
                st.always_on_top = !st.always_on_top;
                self.apply_window_level();
            }
            Cmd::ToggleEasyMove => st.easy_move = !st.easy_move,
            Cmd::ToggleAutoscroll => {
                st.autoscroll = !st.autoscroll;
                st.marquee_step = 0;
            }
            Cmd::CycleVis => {
                st.vis_mode = st.vis_mode.next();
                st.vis.reset();
            }
            Cmd::SetVisMode(m) => {
                st.vis_mode = m;
                st.vis.reset();
            }
            Cmd::SetVisOptions(o) => st.vis_opts = o,
            Cmd::ToggleWindow(k) => {
                let v = !self.st.geom(k).visible;
                self.set_visible(k, v);
            }
            Cmd::CloseWindow(k) => self.set_visible(k, false),
            Cmd::ToggleShade(k) => {
                let before = self.layout_snapshot();
                let g = self.st.geom_mut(k);
                g.shade = !g.shade;
                self.sync_sizes();
                self.relayout_from(before);
            }
            Cmd::Minimize => {
                for w in self.wins.values() {
                    w.window.set_minimized(true);
                }
            }
            Cmd::Exit => {
                self.exiting = true;
                self.save_settings();
                self.st.engine.stop();
                self.stop_plugins();
                el.exit();
            }
            Cmd::About => self.open_dialog(el, Dialog::about()),
            Cmd::JumpToFile => {
                let titles = self
                    .st
                    .playlist
                    .entries
                    .iter()
                    .map(|e| e.title.clone())
                    .collect();
                self.open_dialog(el, Dialog::jump_to_file(titles));
            }
            Cmd::JumpToTime => {
                if self.st.status() != Status::Stopped {
                    let d = Dialog::jump_to_time(self.st.position(), self.st.duration());
                    self.open_dialog(el, d);
                }
            }
            Cmd::FileInfo => {
                // Files an input plugin plays get that plugin's own info box.
                if let Some(sys) = self.plugins.clone().filter(|_| self.plugins_ready)
                    && let Some(p) = self.selected_or_current_path()
                    && sys.prefers(&p)
                    && sys.info_box(&p)
                {
                    return;
                }
                if let Some(d) = self.file_info_dialog() {
                    self.open_dialog(el, d);
                }
            }
            Cmd::Preferences => {
                let d = Dialog::preferences(self.prefs);
                self.open_dialog(el, d);
            }
            Cmd::SkinBrowser => {
                let skins = list_skins();
                let d = Dialog::skin_browser(skins, self.st.skin.path.as_ref());
                self.open_dialog(el, d);
            }
            Cmd::LoadSkinDialog => {
                let _ = std::fs::create_dir_all(config::skins_dir());
                self.pick(Pick::LoadSkin);
            }
            Cmd::LoadSkin(p) => self.load_skin(el, p),
            Cmd::ShowMenu(id, x, y, owner) => self.open_menu(el, id, x, y, owner),
            Cmd::EqToggleOn => {
                st.eq.enabled = !st.eq.enabled;
                st.engine.set_eq(st.eq);
            }
            Cmd::EqToggleAuto => st.eq_auto = !st.eq_auto,
            Cmd::EqSetBand(i, v) => {
                st.eq.bands[i] = v.min(63);
                st.engine.set_eq(st.eq);
            }
            Cmd::EqSetPreamp(v) => {
                st.eq.preamp = v.min(63);
                st.engine.set_eq(st.eq);
            }
            Cmd::EqAll(v) => {
                st.eq.bands = [v; 10];
                st.engine.set_eq(st.eq);
            }
            Cmd::EqLoadEqf => self.pick(Pick::LoadEqf),
            Cmd::EqSaveEqf => self.pick(Pick::SaveEqf),
            Cmd::PlAddFiles => self.pick(Pick::AddFiles),
            Cmd::PlAddFolder => self.pick(Pick::AddFolder),
            Cmd::PlAddUrl => self.open_dialog(el, Dialog::open_url(true)),
            Cmd::PlRemoveSelected => st.playlist.remove_selected(),
            Cmd::PlCrop => st.playlist.crop(),
            Cmd::PlClear | Cmd::PlNew => st.playlist.clear(),
            Cmd::PlRemoveDead => st.playlist.remove_dead_files(),
            Cmd::PlSelectAll => st.playlist.select_all(),
            Cmd::PlSelectNone => st.playlist.select_none(),
            Cmd::PlInvertSelection => st.playlist.invert_selection(),
            Cmd::PlSortTitle => st.playlist.sort_by_title(),
            Cmd::PlSortFilename => st.playlist.sort_by_filename(),
            Cmd::PlSortPath => st.playlist.sort_by_path(),
            Cmd::PlReverse => st.playlist.reverse(),
            Cmd::PlRandomize => st.playlist.randomize(),
            Cmd::PlSave => self.pick(Pick::SavePlaylist),
            Cmd::PlLoad => self.pick(Pick::LoadPlaylist),
            Cmd::PlScrollBy(d) => {
                let rows = pl_win::Geometry::of(st).visible_rows();
                let max = st.playlist.len().saturating_sub(rows) as i32;
                st.playlist.scroll = (st.playlist.scroll as i32 + d).clamp(0, max.max(0)) as usize;
            }
            Cmd::PlPlaySelected => {
                if let Some(i) = st.playlist.entries.iter().position(|e| e.selected) {
                    self.play_index(i);
                }
            }
            Cmd::PlReadInfo => {
                let ids: Vec<u64> = st
                    .playlist
                    .entries
                    .iter()
                    .filter(|e| e.selected)
                    .map(|e| e.id)
                    .collect();
                self.request_info(&ids);
            }
            Cmd::PlGenerateHtml => self.pick(Pick::SaveHtml),
            Cmd::VisOptionsDialog => {
                let d = Dialog::vis_options(self.st.vis_mode, self.st.vis_opts);
                self.open_dialog(el, d);
            }
            Cmd::PluginsDialog => self.open_plugins_dialog(el),
            Cmd::PluginMenu(i) => {
                if let (Some(sys), Some(&(arch, id))) = (&self.plugins, self.plugin_menu.get(i)) {
                    sys.menu_command(arch, id);
                }
            }
            Cmd::VisPluginConfig => {
                if let Some((p, m)) = self.selected_vis_plugin() {
                    if let Some(sys) = &self.plugins {
                        sys.configure(&p, m);
                    }
                } else {
                    self.st.push(Cmd::VisOptionsDialog);
                }
            }
            Cmd::PluginSelect(row) => self.plugin_selected(row),
            Cmd::PluginAction(a, row) => self.plugin_action(a, row),
            Cmd::ToggleVisPlugin => {
                if st.vis_mode == VisMode::Off {
                    st.vis_mode = self.last_vis_mode;
                } else {
                    self.last_vis_mode = st.vis_mode;
                    st.vis_mode = VisMode::Off;
                }
            }
            Cmd::GetMoreSkins => self.open_museum(el),
            Cmd::MuseumWeb => match &self.museum.selected {
                Some(md5) => open_url(&format!("https://{}/skin/{md5}", crate::museum::SITE)),
                None => open_url(&format!("https://{}/", crate::museum::SITE)),
            },
            Cmd::MuseumSearch(q) => self.museum_load(q, false),
            Cmd::MuseumMore => {
                let q = self.museum.query.clone();
                self.museum_load(q, true);
            }
            Cmd::MuseumSelect(i) => self.museum_select(i),
            Cmd::MuseumDownload(i) => self.museum_download(i),
            Cmd::ClassicSkin => self.open_classic_skin(el),
            Cmd::ClassicSkinDownload => self.download_classic_skin(),
            Cmd::AddBookmark => {
                if let Some(e) = st.playlist.current_entry() {
                    let b = (e.title.clone(), e.location());
                    self.bookmarks.push(b);
                    self.save_libraries();
                    self.st.show_message("Bookmark added", 1.0);
                }
            }
            Cmd::BookmarkSelected => {
                let sel: Vec<(String, String)> = st
                    .playlist
                    .entries
                    .iter()
                    .filter(|e| e.selected)
                    .map(|e| (e.title.clone(), e.location()))
                    .collect();
                self.bookmarks.extend(sel);
                self.save_libraries();
            }
            Cmd::EditBookmarks => {
                let names = self.bookmarks.iter().map(|b| b.0.clone()).collect();
                self.open_dialog(el, Dialog::bookmarks(names));
            }
            Cmd::PlayBookmark(i) => {
                if let Some((_, loc)) = self.bookmarks.get(i).cloned() {
                    self.open_paths(vec![PathBuf::from(loc)], true);
                }
            }
            Cmd::RemoveBookmark(i) => {
                if i < self.bookmarks.len() {
                    self.bookmarks.remove(i);
                    self.save_libraries();
                }
            }
            Cmd::PlEditEntry => {
                if let Some(e) = st.playlist.entries.iter().find(|e| e.selected) {
                    let d = Dialog::playlist_entry(&e.location());
                    self.open_dialog(el, d);
                }
            }
            Cmd::PlSetLocation(loc) => {
                if let Some(i) = st.playlist.entries.iter().position(|e| e.selected) {
                    let src = Source::parse(loc.trim());
                    let e = &mut st.playlist.entries[i];
                    e.title = playlist::default_title(&src);
                    e.source = src;
                    e.has_info = false;
                    e.duration = None;
                    let id = e.id;
                    self.request_info(&[id]);
                }
            }
            Cmd::EqLibrary(op) => self.eq_lib(op, el),
            Cmd::EqLibCommit(op, name) => self.eq_lib_commit(op, name),
        }
    }

    fn set_visible(&mut self, k: WinKind, v: bool) {
        if k == WinKind::Main && !v {
            // The main window can't be hidden while it is the only one shown.
            if !self.st.eqw.visible && !self.st.pl.visible {
                return;
            }
        }
        self.st.geom_mut(k).visible = v;
        if let Some(w) = self.wins.get(&k) {
            w.window.set_visible(v);
            if v {
                let g = *self.st.geom(k);
                w.window.set_outer_position(PhysicalPosition::new(g.x, g.y));
                w.window.request_redraw();
            }
        }
        self.st.dirty = true;
    }

    fn load_skin(&mut self, el: &ActiveEventLoop, p: Option<PathBuf>) {
        let base = self.builtin.get_or_insert_with(Skin::builtin);
        match p {
            None => self.st.skin = Skin::builtin(),
            Some(path) => match Skin::load(&path, base) {
                Ok(s) => self.st.skin = s,
                Err(e) => {
                    self.st.show_message(format!("Skin error: {e}"), 3.0);
                    return;
                }
            },
        }
        self.rebuild_cursors(el);
        for k in WinKind::ALL {
            self.apply_shape(k);
        }
        self.redraw_all();
    }

    fn picked(&mut self, el: &ActiveEventLoop, pick: Pick, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if let Some(dir) = paths[0].parent()
            && !matches!(pick, Pick::LoadSkin)
        {
            self.settings.set("last_dir", dir.to_string_lossy());
        }
        match pick {
            Pick::OpenFiles | Pick::OpenFolder => self.open_paths(paths, true),
            Pick::AddFiles | Pick::AddFolder => self.open_paths(paths, false),
            Pick::LoadPlaylist => {
                self.st.playlist.clear();
                self.open_paths(paths, false);
            }
            Pick::SavePlaylist => {
                let p = &paths[0];
                let r = match p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                {
                    Some(e) if e == "pls" => self.st.playlist.save_pls(p),
                    _ => self.st.playlist.save_m3u(p),
                };
                if let Err(e) = r {
                    self.st
                        .show_message(format!("Could not save playlist: {e}"), 3.0);
                }
            }
            Pick::LoadSkin => {
                // Copy into the skins folder so it shows up in the skin list.
                let src = &paths[0];
                let dir = config::skins_dir();
                let _ = std::fs::create_dir_all(&dir);
                let dst = dir.join(src.file_name().unwrap_or_default());
                let use_path =
                    if src.parent() != Some(dir.as_path()) && std::fs::copy(src, &dst).is_ok() {
                        dst
                    } else {
                        src.clone()
                    };
                self.load_skin(el, Some(use_path));
            }
            Pick::LoadEqf => {
                if let Some(presets) = std::fs::read(&paths[0])
                    .ok()
                    .and_then(|b| config::parse_eqf(&b))
                    && let Some((name, p, b)) = presets.first().cloned()
                {
                    self.st.eq.preamp = p;
                    self.st.eq.bands = b;
                    self.st.engine.set_eq(self.st.eq);
                    // Libraries with several presets extend the Load menu.
                    for pr in presets {
                        if !self.eqf_presets.iter().any(|e| e.0 == pr.0) {
                            self.eqf_presets.push(pr);
                        }
                    }
                    self.st.show_message(format!("Preset: {name}"), 1.5);
                }
            }
            Pick::SaveEqf => {
                let name = paths[0]
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let data = config::write_eqf(&name, self.st.eq.preamp, &self.st.eq.bands);
                let _ = std::fs::write(&paths[0], data);
            }
            Pick::SaveHtml => {
                let _ = std::fs::write(&paths[0], html_playlist(&self.st));
            }
        }
        self.st.dirty = true;
    }

    // ------------------------------------------------------------------
    // Painting

    fn paint(&mut self, k: WinKind) {
        let (w, h) = self.st.base_size(k);
        let scale = self.scale(k);
        let Some(os) = self.wins.get_mut(&k) else {
            return;
        };
        if os.frame.w != w || os.frame.h != h {
            os.frame = Bitmap::new(w, h, 0xFF00_0000);
        }
        let mut frame = std::mem::replace(&mut os.frame, Bitmap::new(0, 0, 0));
        match k {
            WinKind::Main => self.main.render(&mut self.st, &mut frame),
            WinKind::Eq => self.eq.render(&mut self.st, &mut frame),
            WinKind::Playlist => self.pl.render(&mut self.st, &mut frame),
        }
        let os = self.wins.get_mut(&k).unwrap();
        os.frame = frame;
        os.present(scale);
    }

    fn tick(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        // Commands queued by windows, menus and dialogs.
        let mut guard = 0;
        while !self.st.queue.is_empty() && guard < 64 {
            guard += 1;
            let cmds = std::mem::take(&mut self.st.queue);
            for c in cmds {
                self.run(el, c);
                if self.exiting {
                    return;
                }
            }
        }
        if !self.dropped.is_empty() {
            let dropped = std::mem::take(&mut self.dropped);
            let k = dropped[0].0;
            let y = dropped[0].2;
            let paths: Vec<PathBuf> = dropped.into_iter().map(|d| d.1).collect();
            if k == WinKind::Playlist {
                let at = self.pl.row_at(&self.st, y);
                self.insert_paths(paths, at);
            } else {
                self.open_paths(paths, true);
            }
        }
        self.poll_audio();
        self.poll_plugins();
        // Everything below runs at the frame rate; other wake-ups (input,
        // redraw requests) must not spin the loop.
        if now < self.next_frame {
            if self.st.dirty {
                self.st.dirty = false;
                self.redraw_all();
                for m in &self.menus {
                    m.os.window.request_redraw();
                }
            }
            el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
            return;
        }
        // Fade-out stop.
        if let Some((t0, vol)) = self.st.fade {
            let f = t0.elapsed().as_secs_f32() / 1.5;
            if f >= 1.0 {
                self.st.fade = None;
                self.st.engine.stop();
                self.st.engine.set_volume(self.st.volume as f32 / 100.0);
                self.st.dirty = true;
            } else {
                self.st.engine.set_volume(vol as f32 / 100.0 * (1.0 - f));
            }
        }
        if self.st.tick(now) {
            self.st.dirty = true;
        }
        self.check_menu_outside_click();
        self.sync_jump_dialog();
        if std::mem::take(&mut self.focus_check)
            && !self.menus.is_empty()
            && self.menu_opened_at.elapsed() > Duration::from_millis(300)
        {
            let ours = self.wins.values().any(|w| w.window.has_focus())
                || self.menus.iter().any(|m| m.os.window.has_focus())
                || self.dialogs.iter().any(|d| d.os.window.has_focus());
            if !ours {
                self.close_menus();
            }
        }
        self.update_tooltip(el);
        if self.drag.is_some() {
            if mouse_button_down() || crate::platform::global_cursor().is_none() {
                self.update_drag();
            } else {
                self.drag = None;
                self.capture = None;
            }
        }
        // Visualisation and time display while playing.
        let playing = self.st.status() == Status::Playing;
        if playing {
            let mut samples = std::mem::take(&mut self.st.vis_samples);
            self.st.engine.vis_samples(&mut samples);
            self.st.vis_samples = samples;
            if self.st.vis_mode == VisMode::Analyzer {
                let small = self.st.main.shade;
                let opts = self.st.vis_opts;
                let samples = std::mem::take(&mut self.st.vis_samples);
                self.st.vis.analyze(&samples, small, &opts);
                if std::env::var_os("FERRAMP_DEBUG_VIS").is_some() {
                    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
                    eprintln!(
                        "vis: {} samples, peak {peak:.3}, bars {:?}",
                        samples.len(),
                        self.st.vis.heights()
                    );
                }
                self.st.vis_samples = samples;
            }
            if let Some(w) = self.wins.get(&WinKind::Main)
                && self.st.main.visible
            {
                w.window.request_redraw();
            }
            if self.st.pl.visible
                && let Some(w) = self.wins.get(&WinKind::Playlist)
            {
                w.window.request_redraw();
            }
        } else if self.st.status() == Status::Paused
            && let Some(w) = self.wins.get(&WinKind::Main)
        {
            // Blinking time.
            w.window.request_redraw();
        }
        if self.st.dirty {
            self.st.dirty = false;
            self.redraw_all();
            for m in &self.menus {
                m.os.window.request_redraw();
            }
        }
        self.update_title();
        if self.last_save.elapsed() > Duration::from_secs(60) {
            self.save_settings();
        }
        let animating = self.st.status() != Status::Stopped
            || self.drag.is_some()
            || !self.menus.is_empty()
            || self.st.message.is_some()
            || self.hover_tip.is_some()
            || self.main.wants_animation(&self.st);
        self.next_frame = now
            + if animating {
                FRAME
            } else {
                Duration::from_millis(250)
            };
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn new_events(&mut self, el: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::Init = cause
            && !self.created
        {
            self.create_windows(el);
        }
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if !self.created {
            self.create_windows(el);
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Wake => {}
            UserEvent::Picked(p, paths) => self.picked(el, p, paths),
            UserEvent::Classic(f) => self.classic_fetched(el, f),
            UserEvent::Museum(f) => self.museum_fetched(el, f),
            UserEvent::PluginsReady(sys) => self.plugins_loaded(sys),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let WindowEvent::ModifiersChanged(m) = &event {
            let s: ModifiersState = m.state();
            self.mods = Mods {
                shift: s.shift_key(),
                ctrl: s.control_key() || s.super_key(),
                alt: s.alt_key(),
            };
            return;
        }
        if let Some(t) = &mut self.tip
            && t.os.window.id() == id
        {
            if let WindowEvent::RedrawRequested = event {
                let mut chrome = Bitmap::new(t.w, t.h, crate::gfx::rgb(255, 255, 225));
                chrome.rect_outline(0, 0, t.w, t.h, crate::gfx::rgb(0, 0, 0));
                let clip = crate::gfx::Rect::new(1, 1, t.w - 2, t.h - 2);
                let mut tq = crate::text::TextQueue::default();
                tq.draw(
                    &self.st.fonts,
                    &t.text,
                    3,
                    1,
                    menu::FONT_PX,
                    crate::text::Weight::Regular,
                    crate::gfx::rgb(0, 0, 0),
                    clip,
                );
                let s = self.ui_scale;
                let mut f = Bitmap::new(t.w * s, t.h * s, 0);
                tq.compose(&self.st.fonts, &chrome, &mut f, s);
                t.os.frame = f;
                t.os.present(1);
            }
            return;
        }
        // Pop-up menus.
        if let Some(depth) = self.menu_index(id) {
            match event {
                WindowEvent::RedrawRequested => {
                    let m = &mut self.menus[depth];
                    let s = self.ui_scale;
                    let mut f = Bitmap::new(m.level.width * s, m.level.height * s, 0);
                    m.level.render(&self.st.fonts, &mut f, s);
                    m.os.frame = f;
                    m.os.present(1);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let s = self.ui_scale as f64;
                    let item = self.menus[depth]
                        .level
                        .hit((position.x / s) as i32, (position.y / s) as i32);
                    if item.is_some() || depth + 1 == self.menus.len() {
                        self.menu_hover(el, depth, item);
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    ..
                } => {
                    if self.menu_opened_at.elapsed() < Duration::from_millis(250) {
                        return;
                    }
                    if let Some(i) = self.menus[depth].level.hover {
                        self.menu_activate(el, depth, i);
                    }
                }
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                } => {
                    self.menu_opened_at = Instant::now() - Duration::from_secs(1);
                }
                _ => {}
            }
            return;
        }
        // Dialogs.
        if let Some(idx) = self.dialog_index(id) {
            match event {
                WindowEvent::CloseRequested => {
                    self.dialog_outcome(idx, Outcome::Close);
                }
                WindowEvent::RedrawRequested => {
                    let d = &mut self.dialogs[idx];
                    let s = self.ui_scale;
                    let mut f = Bitmap::new(d.dialog.w * s, d.dialog.h * s, 0);
                    d.dialog.render(&self.st.fonts, &mut f, s);
                    d.os.frame = f;
                    d.os.present(1);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    self.dialogs[idx].os.cursor = (position.x, position.y);
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    let s = self.ui_scale as f64;
                    let c = self.dialogs[idx].os.cursor;
                    let (x, y) = ((c.0 / s) as i32, (c.1 / s) as i32);
                    let out = if state == ElementState::Pressed {
                        let double = self.is_double(id, c);
                        self.dialogs[idx].dialog.mouse_down(x, y, double)
                    } else {
                        self.dialogs[idx].dialog.mouse_up(x, y)
                    };
                    self.dialog_outcome(idx, out);
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    self.dialogs[idx].dialog.wheel(wheel_steps(delta));
                    self.dialogs[idx].os.window.request_redraw();
                }
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed =>
                {
                    use winit::keyboard::Key as K;
                    let k = match &event.logical_key {
                        K::Named(NamedKey::Enter) => DKey::Enter,
                        K::Named(NamedKey::Escape) => DKey::Escape,
                        K::Named(NamedKey::Tab) => DKey::Tab,
                        K::Named(NamedKey::Backspace) => DKey::Backspace,
                        K::Named(NamedKey::Delete) => DKey::Delete,
                        K::Named(NamedKey::ArrowLeft) => DKey::Left,
                        K::Named(NamedKey::ArrowRight) => DKey::Right,
                        K::Named(NamedKey::ArrowUp) => DKey::Up,
                        K::Named(NamedKey::ArrowDown) => DKey::Down,
                        K::Named(NamedKey::Home) => DKey::Home,
                        K::Named(NamedKey::End) => DKey::End,
                        K::Named(NamedKey::PageUp) => DKey::PageUp,
                        K::Named(NamedKey::PageDown) => DKey::PageDown,
                        K::Named(NamedKey::Space) => DKey::Char(' '),
                        K::Character(s) => {
                            let t = event
                                .text
                                .as_ref()
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| s.to_string());
                            match t.chars().next() {
                                Some(c) if !c.is_control() => DKey::Char(if self.mods.ctrl {
                                    s.chars().next().unwrap_or(c)
                                } else {
                                    c
                                }),
                                _ => DKey::Other,
                            }
                        }
                        _ => DKey::Other,
                    };
                    let clip = if self.mods.ctrl && matches!(k, DKey::Char('v') | DKey::Char('V')) {
                        arboard::Clipboard::new()
                            .ok()
                            .and_then(|mut c| c.get_text().ok())
                    } else {
                        None
                    };
                    let out = self.dialogs[idx].dialog.key(&k, self.mods, clip);
                    self.dialogs[idx].os.window.request_redraw();
                    self.dialog_outcome(idx, out);
                }
                _ => {}
            }
            return;
        }
        let Some(&k) = self.ids.get(&id) else { return };
        match event {
            WindowEvent::CloseRequested => self.st.push(Cmd::Exit),
            WindowEvent::RedrawRequested => self.paint(k),
            WindowEvent::Focused(f) => {
                if f {
                    self.st.focused = Some(k);
                } else if self.st.focused == Some(k) {
                    self.st.focused = None;
                    // Whether another application took focus is decided on the
                    // next frame (a click on a menu may move focus to it first).
                    self.focus_check = true;
                }
                self.st.dirty = true;
            }
            WindowEvent::Moved(p) => {
                debug!("moved event {k:?} {},{}", p.x, p.y);
                if self.drag.is_none() {
                    let g = self.st.geom_mut(k);
                    g.x = p.x;
                    g.y = p.y;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(w) = self.wins.get_mut(&k) {
                    w.cursor = (position.x, position.y);
                }
                if self.drag.is_some() {
                    self.update_drag();
                    return;
                }
                let target = self.capture.unwrap_or(k);
                let (x, y) = if target == k {
                    self.to_skin(k, (position.x, position.y))
                } else {
                    // Mouse captured by another window: translate coordinates.
                    let (gx, gy) = self.cursor_global(k);
                    let g = self.st.geom(target);
                    let s = self.scale(target);
                    ((gx - g.x) / s, (gy - g.y) / s)
                };
                self.hover_y.insert(k, y);
                if k == WinKind::Main && self.capture.is_none() {
                    let text = self.main.tooltip(&self.st, x, y);
                    if self.hover_tip.as_ref().map(|h| &h.1) != Some(&text) {
                        self.hover_tip = Some((k, text, Instant::now()));
                        self.hide_tooltip();
                    }
                }
                self.window_mouse(target, MouseEvent::Move { x, y });
                self.update_cursor(k);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hover_tip.as_ref().is_some_and(|h| h.0 == k) {
                    self.hover_tip = None;
                    self.hide_tooltip();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.next_frame = Instant::now();
                self.hover_tip = None;
                self.hide_tooltip();
                let pos = self.wins.get(&k).map(|w| w.cursor).unwrap_or_default();
                let (x, y) = self.to_skin(k, pos);
                let b = match button {
                    MouseButton::Left => Button::Left,
                    MouseButton::Right => Button::Right,
                    _ => Button::Middle,
                };
                match state {
                    ElementState::Pressed => {
                        if !self.menus.is_empty() {
                            // A click outside an open menu only closes it.
                            self.close_menus();
                            self.pl.close_menus(&mut self.st);
                            return;
                        }
                        if let Some(w) = self.wins.get(&k) {
                            w.window.focus_window();
                        }
                        self.st.focused = Some(k);
                        let double = b == Button::Left && self.is_double(id, pos);
                        self.capture = Some(k);
                        self.window_mouse(
                            k,
                            MouseEvent::Down {
                                x,
                                y,
                                button: b,
                                double,
                                mods: self.mods,
                            },
                        );
                    }
                    ElementState::Released => {
                        if self.drag.take().is_some() {
                            self.capture = None;
                            return;
                        }
                        let target = self.capture.take().unwrap_or(k);
                        let (x, y) = if target == k {
                            (x, y)
                        } else {
                            let (gx, gy) = self.cursor_global(k);
                            let g = self.st.geom(target);
                            let s = self.scale(target);
                            ((gx - g.x) / s, (gy - g.y) / s)
                        };
                        self.window_mouse(target, MouseEvent::Up { x, y, button: b });
                    }
                }
                self.update_cursor(k);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let d = wheel_steps(delta);
                if d != 0 {
                    self.window_mouse(k, MouseEvent::Wheel { delta: d });
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.handle_key(el, &event.logical_key, event.physical_key, k);
                }
            }
            WindowEvent::HoveredFile(_) => {}
            WindowEvent::DroppedFile(path) => {
                let y = self.hover_y.get(&k).copied().unwrap_or(0);
                self.dropped.push((k, path, y));
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.created && !self.exiting {
            self.tick(el);
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        if !self.exiting {
            self.save_settings();
        }
    }
}

impl App {
    fn is_double(&mut self, id: WindowId, pos: (f64, f64)) -> bool {
        let now = Instant::now();
        let dbl = matches!(self.last_down, Some((wid, t, p)) if wid == id
            && now.duration_since(t) < Duration::from_millis(450)
            && (p.0 - pos.0).abs() < 5.0 && (p.1 - pos.1).abs() < 5.0);
        self.last_down = if dbl { None } else { Some((id, now, pos)) };
        dbl
    }
}

fn parse_eq_values(s: Option<&str>) -> (u8, [u8; 10]) {
    let mut bands = [31u8; 10];
    let mut pre = 31u8;
    if let Some(s) = s {
        let v: Vec<u8> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 11 {
            pre = v[0].min(63);
            for i in 0..10 {
                bands[i] = v[i + 1].min(63);
            }
        }
    }
    (pre, bands)
}

fn format_eq_values(pre: u8, bands: &[u8; 10]) -> String {
    std::iter::once(pre)
        .chain(bands.iter().copied())
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Open a web page in the user's browser.
pub fn open_url(url: &str) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        eprintln!("ferramp: cannot open {url}: {e}");
    }
}

fn wheel_steps(delta: MouseScrollDelta) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
        MouseScrollDelta::PixelDelta(p) => (p.y / 40.0).round() as i32,
    }
}

/// New positions after a size change. `before` holds the old rectangles,
/// `sizes` the new sizes. A window whose top edge lay on another's bottom
/// edge (overlapping horizontally) stays below it; likewise for left/right.
fn relayout(
    before: &[(WinKind, (i32, i32, i32, i32))],
    sizes: &[(WinKind, (i32, i32))],
) -> Vec<(WinKind, (i32, i32))> {
    let n = before.len();
    let size = |k: WinKind| {
        sizes
            .iter()
            .find(|s| s.0 == k)
            .map(|s| s.1)
            .unwrap_or((0, 0))
    };
    // Anchor of each window: (index of anchor, attached below?).
    let mut anchor: Vec<Option<(usize, bool)>> = vec![None; n];
    for (i, (_, r)) in before.iter().enumerate() {
        for (j, (_, p)) in before.iter().enumerate() {
            if i == j {
                continue;
            }
            let overlap_x = r.0 < p.0 + p.2 && p.0 < r.0 + r.2;
            let overlap_y = r.1 < p.1 + p.3 && p.1 < r.1 + r.3;
            if overlap_x && r.1 == p.1 + p.3 {
                anchor[i] = Some((j, true));
                break;
            }
            if overlap_y && r.0 == p.0 + p.2 && anchor[i].is_none() {
                anchor[i] = Some((j, false));
            }
        }
    }
    fn place(
        i: usize,
        before: &[(WinKind, (i32, i32, i32, i32))],
        anchor: &[Option<(usize, bool)>],
        size: &dyn Fn(WinKind) -> (i32, i32),
        memo: &mut Vec<Option<(i32, i32)>>,
        depth: usize,
    ) -> (i32, i32) {
        if let Some(p) = memo[i] {
            return p;
        }
        let r = before[i].1;
        let pos = match anchor[i] {
            Some((j, below)) if depth < 8 => {
                let (px, py) = place(j, before, anchor, size, memo, depth + 1);
                let pr = before[j].1;
                let (pw, ph) = size(before[j].0);
                if below {
                    (px + (r.0 - pr.0), py + ph)
                } else {
                    (px + pw, py + (r.1 - pr.1))
                }
            }
            _ => (r.0, r.1),
        };
        memo[i] = Some(pos);
        pos
    }
    let mut memo = vec![None; n];
    (0..n)
        .map(|i| (before[i].0, place(i, before, &anchor, &size, &mut memo, 0)))
        .collect()
}

fn touching(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    let (ax, ay, aw, ah) = a;
    let (bx, by, bw, bh) = b;
    let overlap_x = ax < bx + bw && bx < ax + aw;
    let overlap_y = ay < by + bh && by < ay + ah;
    (overlap_y && (ax + aw == bx || bx + bw == ax))
        || (overlap_x && (ay + ah == by || by + bh == ay))
}

/// A path for display, with the home folder written as `~` on Unix.
fn short_path(p: &Path) -> String {
    if cfg!(unix)
        && let Some(h) = dirs::home_dir()
        && let Ok(rest) = p.strip_prefix(&h)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}
fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth > 16 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else if metadata::is_audio_file(&p) {
            out.push(p);
        }
    }
}

/// Skins available in the skins folder (and next to the executable).
pub fn list_skins() -> Vec<PathBuf> {
    let mut dirs = vec![config::skins_dir()];
    // Skins of an existing classic player installation (Windows or Wine).
    for pf in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(p) = std::env::var_os(pf) {
            dirs.push(PathBuf::from(p).join("Winamp").join("Skins"));
        }
    }
    if let Some(h) = dirs::home_dir() {
        for pf in ["Program Files", "Program Files (x86)"] {
            dirs.push(h.join(".wine/drive_c").join(pf).join("Winamp/Skins"));
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(d) = exe.parent()
    {
        dirs.push(d.join("Skins"));
    }
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("wsz") | Some("zip"))
                || (p.is_dir() && p.join("main.bmp").exists())
            {
                out.push(p);
            }
        }
    }
    out.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    out.dedup();
    out
}

fn mouse_button_down() -> bool {
    imp_buttons::down()
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp_buttons {
    pub fn down() -> bool {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{ConnectionExt, KeyButMask};
        thread_local! {
            static C: Option<(x11rb::rust_connection::RustConnection, u32)> =
                x11rb::connect(None).ok().map(|(c, s)| { let r = c.setup().roots[s].root; (c, r) });
        }
        C.with(|c| {
            let Some((conn, root)) = c else { return false };
            conn.query_pointer(*root)
                .ok()
                .and_then(|r| r.reply().ok())
                .map(|r| {
                    let m = r.mask;
                    m.contains(KeyButMask::BUTTON1)
                        || m.contains(KeyButMask::BUTTON2)
                        || m.contains(KeyButMask::BUTTON3)
                })
                .unwrap_or(false)
        })
    }
}

#[cfg(windows)]
mod imp_buttons {
    pub fn down() -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON,
        };
        // SAFETY: simple state query.
        unsafe {
            (GetAsyncKeyState(VK_LBUTTON as i32) as u16 & 0x8000) != 0
                || (GetAsyncKeyState(VK_RBUTTON as i32) as u16 & 0x8000) != 0
        }
    }
}

#[cfg(target_os = "macos")]
mod imp_buttons {
    pub fn down() -> bool {
        objc2_app_kit::NSEvent::pressedMouseButtons() != 0
    }
}

fn html_playlist(st: &State) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let (total, _) = st.playlist.total_duration();
    let mut s = String::from(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>Ferramp playlist</title>\n<style>body{background:#000040;color:#fff;font-family:Arial,sans-serif}h1{color:#ffbf00}li{color:#ffbf00}span{color:#fff}</style></head><body>\n",
    );
    s.push_str(&format!(
        "<h1>Ferramp playlist</h1><p>{} tracks in playlist, total length {}</p>\n<ol>\n",
        st.playlist.len(),
        format_time(total)
    ));
    for e in &st.playlist.entries {
        s.push_str(&format!(
            "<li><span>{}</span> ({})</li>\n",
            esc(&e.title),
            e.duration.map(format_time).unwrap_or_else(|| "?".into())
        ));
    }
    s.push_str("</ol></body></html>\n");
    s
}

/// Window icon: the built-in skin's emblem, drawn at 32x32.
fn app_icon() -> winit::window::Icon {
    let mut b = Bitmap::new(32, 32, 0);
    let skin = Skin::builtin();
    let mut tmp = Bitmap::new(15, 17, 0);
    skin.draw(
        &mut tmp,
        crate::skin::sprites::Sprite {
            sheet: crate::skin::sprites::Sheet::Main,
            x: 252,
            y: 90,
            w: 15,
            h: 17,
        },
        0,
        0,
    );
    for y in 0..32 {
        for x in 0..32 {
            let c = tmp.get(x * 15 / 32, y * 17 / 32);
            b.set(x, y, c | 0xFF00_0000);
        }
    }
    winit::window::Icon::from_rgba(b.to_rgba8(), 32, 32).expect("icon")
}

// ----------------------------------------------------------------------
// Classic plugins

impl App {
    fn plugin_dirs(&self) -> Vec<PathBuf> {
        let mut v = vec![config::config_dir().join("Plugins")];
        if let Some(extra) = self.settings.get_str("plugin_dirs") {
            v.extend(
                extra
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from),
            );
        }
        v
    }

    /// Plugins left out at start: disabled in the dialog, or found hanging.
    fn plugin_skip(&self) -> HashMap<String, String> {
        self.settings
            .get_str("plugins_skip")
            .unwrap_or("")
            .split(';')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn save_plugin_skip(&mut self, skip: &HashMap<String, String>) {
        let mut v: Vec<String> = skip
            .iter()
            .map(|(k, r)| format!("{k}={}", r.replace([';', '='], ",")))
            .collect();
        v.sort();
        self.settings.set("plugins_skip", v.join(";"));
    }

    fn save_plugin_noinfo(&mut self) {
        if let Some(sys) = &self.plugins {
            let v = sys.no_info().join(";");
            if self.settings.get_str("plugins_noinfo").unwrap_or("") != v {
                self.settings.set("plugins_noinfo", v);
            }
        }
    }

    /// Starts the plugin hosts in the background if there are plugins.
    fn start_plugins(&mut self) {
        let dirs = self.plugin_dirs();
        let main_dir = dirs[0].clone();
        if std::fs::create_dir_all(&main_dir).is_ok() {
            let readme = main_dir.join("README.txt");
            if !readme.exists() {
                let _ = std::fs::write(
                    &readme,
                    "Put classic player plug-ins here: in_*.dll (input), out_*.dll (output),\n\
                     dsp_*.dll (effects), vis_*.dll (visualization) and gen_*.dll (general).\n\
                     Libraries they need go in the folder above this one.\n\
                     On Linux and macOS they run under Wine. See Options > Plug-ins.\n",
                );
            }
        }
        let proxy = self.proxy.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::Wake);
        });
        let dir = config::config_dir();
        let sys = Arc::new(crate::plugins::Plugins::new(
            dirs,
            dir.join("PluginSettings"),
            dir.join("logs"),
            wake,
        ));
        sys.set_skip(self.plugin_skip());
        sys.set_no_info(
            self.settings
                .get_str("plugins_noinfo")
                .unwrap_or("")
                .split(';')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        );
        self.plugins = Some(sys.clone());
        self.plugins_ready = false;
        if sys.plugin_files().is_empty() {
            return;
        }
        self.st.engine.set_plugins_loading(true);
        let proxy = self.proxy.clone();
        let _ = std::thread::Builder::new()
            .name("plugins".into())
            .spawn(move || {
                sys.start();
                let _ = proxy.send_event(UserEvent::PluginsReady(sys));
            });
    }

    fn stop_plugins(&mut self) {
        self.save_plugin_noinfo();
        if self.output_plugin_active() {
            self.switch_output(None);
        }
        if let Some(sys) = self.plugins.take() {
            self.st.engine.set_plugins(None);
            crate::plugins::set_global(None);
            sys.shutdown();
        }
        self.plugins_ready = false;
    }

    fn plugins_loaded(&mut self, sys: Arc<crate::plugins::Plugins>) {
        // A reload may have replaced the system meanwhile.
        if !self.plugins.as_ref().is_some_and(|p| Arc::ptr_eq(p, &sys)) {
            sys.shutdown();
            return;
        }
        self.plugins_ready = true;
        crate::plugins::set_global(Some(sys.clone()));
        self.st.engine.set_plugins(Some(sys.clone()));
        // Remember plugins that hung, so the next start leaves them out at once.
        self.save_plugin_skip(&sys.skipped());
        if let Some((p, m)) = self.plugin_setting("plugin_dsp") {
            let _ = sys.dsp_start(&p, m);
        }
        if let Some((p, _)) = self.plugin_setting("plugin_out") {
            self.switch_output(Some(p));
        }
        if self.settings.get_bool("plugin_vis_on", false)
            && let Some((p, m)) = self.selected_vis_plugin()
        {
            sys.vis_start(&p, m);
            self.plug.vis_was_running = true;
        }
        // Playlist entries in formats only a plugin reads get their tags now.
        let ids: Vec<u64> = self
            .st
            .playlist
            .entries
            .iter()
            .filter(|e| e.path().is_some_and(|p| sys.prefers(p)))
            .map(|e| e.id)
            .collect();
        self.request_info(&ids);
        self.plug = PlugSync {
            vis_was_running: self.plug.vis_was_running,
            ..Default::default()
        };
        self.refresh_plugins_dialog();
        self.st.dirty = true;
    }

    /// A saved "file<TAB>module" choice, if that plugin is loaded.
    fn plugin_setting(&self, key: &str) -> Option<(crate::plugins::Plugin, u32)> {
        let sys = self.plugins.as_ref()?;
        let v = self.settings.get_str(key)?;
        let (file, m) = v.split_once('\t').unwrap_or((v, "0"));
        let p = sys.by_file(file).filter(|p| p.info.error.is_none())?;
        Some((p, m.parse().unwrap_or(0)))
    }

    fn selected_vis_plugin(&self) -> Option<(crate::plugins::Plugin, u32)> {
        if !self.plugins_ready {
            return None;
        }
        self.plugin_setting("plugin_vis")
    }

    fn output_plugin_active(&self) -> bool {
        self.settings
            .get_str("plugin_out")
            .is_some_and(|s| !s.is_empty())
            && self.plugins_ready
    }

    /// Plays through an output plugin, or the sound card again with `None`;
    /// a playing track continues where it was.
    fn switch_output(&mut self, p: Option<crate::plugins::Plugin>) {
        let resume = (self.st.status() != Status::Stopped)
            .then(|| (self.st.playlist.current, self.st.position()));
        let out = match (&p, &self.plugins) {
            (Some(p), Some(sys)) => Some((sys.clone(), p.clone())),
            _ => None,
        };
        match self.st.engine.set_output(out) {
            Ok(()) => {
                let v = p.map(|p| format!("{}\t0", p.info.file)).unwrap_or_default();
                self.settings.set("plugin_out", v);
            }
            Err(e) => {
                self.st.show_message(format!("Output plug-in: {e}"), 3.0);
                self.settings.set("plugin_out", "");
                let _ = self.st.engine.set_output(None);
            }
        }
        if let Some((Some(i), pos)) = resume {
            self.st.push(Cmd::PlayIndex(i));
            if pos > 0.5 {
                self.st.push(Cmd::SeekTo(pos));
            }
        }
        self.st.dirty = true;
    }

    fn toggle_vis_plugin(&mut self) {
        let Some(sys) = self.plugins.clone() else {
            return;
        };
        if sys.vis_running() {
            sys.vis_stop();
            self.settings.set_bool("plugin_vis_on", false);
        } else if let Some((p, m)) = self.selected_vis_plugin() {
            sys.vis_start(&p, m);
            self.plug.vis_was_running = true;
            self.settings.set_bool("plugin_vis_on", true);
        }
        self.refresh_plugins_dialog();
    }

    fn selected_or_current_path(&self) -> Option<PathBuf> {
        let pl = &self.st.playlist;
        let i = pl.entries.iter().position(|e| e.selected).or(pl.current)?;
        pl.entries.get(i)?.path().map(Path::to_path_buf)
    }

    fn player_state(&self) -> ferramp_plugproto::PlayerState {
        let st = &self.st;
        let mut eq: Vec<u8> = st.eq.bands.to_vec();
        eq.push(st.eq.preamp);
        eq.push(st.eq.enabled as u8);
        ferramp_plugproto::PlayerState {
            status: match st.status() {
                Status::Playing => 1,
                Status::Paused => 3,
                Status::Stopped => 0,
            },
            position_ms: (st.position().max(0.0) * 1000.0) as u32,
            length_ms: st.duration().map(|d| (d * 1000.0) as i32).unwrap_or(-1),
            volume: st.volume as u32 * 255 / 100,
            pan: st.balance as i32 * 127 / 100,
            playlist_pos: st.playlist.current.map(|c| c as i32).unwrap_or(0),
            shuffle: st.shuffle,
            repeat: st.repeat,
            rate: st.now.khz * 1000,
            kbps: st.now.kbps,
            channels: st.now.channels,
            eq,
        }
    }

    fn playlist_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for e in &self.st.playlist.entries {
            e.id.hash(&mut h);
            e.title.hash(&mut h);
            e.duration.map(|d| d as i64).hash(&mut h);
        }
        h.finish()
    }

    /// Runs every frame: plugin requests in, player state and audio out.
    fn poll_plugins(&mut self) {
        let Some(sys) = self.plugins.clone() else {
            return;
        };
        for c in sys.take_commands() {
            self.plugin_command(c);
        }
        if !self.plugins_ready {
            return;
        }
        let now = Instant::now();
        if sys.has_general() && self.plug.next_state.is_none_or(|t| now >= t) {
            self.plug.next_state = Some(now + Duration::from_millis(200));
            let sig = self.playlist_signature();
            if sig != self.plug.playlist_sig {
                self.plug.playlist_sig = sig;
                let entries: Vec<ferramp_plugproto::PlEntry> = self
                    .st
                    .playlist
                    .entries
                    .iter()
                    .map(|e| ferramp_plugproto::PlEntry {
                        file: e.location(),
                        title: e.title.clone(),
                        length_s: e.duration.map(|d| d.round() as i32).unwrap_or(-1),
                    })
                    .collect();
                sys.send_playlist(&entries);
            }
            let state = self.player_state();
            if self.plug.last_state.as_ref() != Some(&state) {
                sys.send_state(&state);
                self.plug.last_state = Some(state);
            }
        }
        if sys.no_info().len()
            != self
                .settings
                .get_str("plugins_noinfo")
                .map(|s| s.split(';').filter(|x| !x.is_empty()).count())
                .unwrap_or(0)
        {
            self.save_plugin_noinfo();
        }
        if sys.wants_pcm() && self.plug.next_vis.is_none_or(|t| now >= t) {
            self.plug.next_vis = Some(now + Duration::from_millis(20));
            let mut buf = Vec::new();
            self.st.engine.vis_samples(&mut buf);
            let samples: Vec<i16> = buf
                .iter()
                .rev()
                .take(1152)
                .rev()
                .map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            sys.vis_pcm(self.st.engine.out_rate(), 1, samples);
        }
        if !sys.vis_running() && self.plug.vis_was_running {
            // The plugin closed its window itself.
            self.plug.vis_was_running = false;
            self.settings.set_bool("plugin_vis_on", false);
            self.st.dirty = true;
        }
    }

    /// Something a general purpose plugin asked the player to do.
    fn plugin_command(&mut self, c: ferramp_plugproto::Command) {
        use ferramp_plugproto::Command as P;
        let cmd = match c {
            P::Play | P::StartPlay => Cmd::Play,
            P::Pause => Cmd::Pause,
            P::Stop => Cmd::Stop,
            P::FadeStop => Cmd::StopFadeout,
            P::Next => Cmd::Next,
            P::Prev => Cmd::Previous,
            P::SeekMs(ms) => Cmd::SeekTo(ms as f64 / 1000.0),
            P::Forward5 => Cmd::SeekBy(5.0),
            P::Rewind5 => Cmd::SeekBy(-5.0),
            P::Volume(v) => Cmd::SetVolume((v.min(255) * 100 / 255) as u8),
            P::VolumeUp => Cmd::VolumeBy(1),
            P::VolumeDown => Cmd::VolumeBy(-1),
            P::Pan(p) => Cmd::SetBalance((p.clamp(-127, 127) * 100 / 127) as i8),
            P::PlaylistPos(i) => {
                if i >= 0 && (i as usize) < self.st.playlist.len() {
                    self.st.playlist.set_current(i as usize);
                    self.st.dirty = true;
                }
                return;
            }
            P::ClearPlaylist => Cmd::PlClear,
            P::Enqueue(path) => Cmd::OpenLocation(path, false),
            P::PlayFile(path) => Cmd::OpenLocation(path, true),
            P::Shuffle(on) if on != self.st.shuffle => Cmd::ToggleShuffle,
            P::Repeat(on) if on != self.st.repeat => Cmd::ToggleRepeat,
            P::Shuffle(_) | P::Repeat(_) => return,
            P::Eq(v) => {
                for (i, b) in v.iter().take(10).enumerate() {
                    self.st.push(Cmd::EqSetBand(i, (*b).min(63)));
                }
                if let Some(p) = v.get(10) {
                    self.st.push(Cmd::EqSetPreamp((*p).min(63)));
                }
                if let Some(on) = v.get(11)
                    && (*on != 0) != self.st.eq.enabled
                {
                    self.st.push(Cmd::EqToggleOn);
                }
                return;
            }
            P::OpenFiles => Cmd::OpenFiles,
            P::Preferences => Cmd::Preferences,
            P::Quit => Cmd::Exit,
        };
        self.st.push(cmd);
    }

    fn plugin_dialog_rows(&mut self) -> Vec<String> {
        self.plugin_rows.clear();
        let Some(sys) = self.plugins.clone() else {
            return vec![];
        };
        let skip = sys.skipped();
        let active_dsp = self.settings.get_str("plugin_dsp").map(str::to_string);
        let active_out = self.settings.get_str("plugin_out").map(str::to_string);
        let active_vis = self.settings.get_str("plugin_vis").map(str::to_string);
        let mut rows = Vec::new();
        let mut push = |rows: &mut Vec<String>, p: &crate::plugins::Plugin, m: u32, label: &str| {
            let key = format!("{}\t{m}", p.info.file);
            let kind = match p.info.kind {
                Some(ferramp_plugproto::Kind::Input) => "Input",
                Some(ferramp_plugproto::Kind::Output) => "Output",
                Some(ferramp_plugproto::Kind::Dsp) => "DSP",
                Some(ferramp_plugproto::Kind::Vis) => "Vis",
                Some(ferramp_plugproto::Kind::General) => "General",
                None => "?",
            };
            let mark = if active_dsp.as_deref() == Some(&key) && sys.dsp_active()
                || active_out.as_deref() == Some(&key)
            {
                "> "
            } else if active_vis.as_deref() == Some(&key) {
                if sys.vis_running() { "> " } else { "* " }
            } else if skip.contains_key(&p.info.file.to_lowercase()) || p.info.error.is_some() {
                "x "
            } else {
                "  "
            };
            rows.push(format!("{mark}[{kind}] {label}"));
            self.plugin_rows.push((p.info.file.clone(), m));
        };
        for p in sys.list() {
            if p.info.modules.len() > 1 {
                for (i, m) in p.info.modules.iter().enumerate() {
                    push(
                        &mut rows,
                        &p,
                        i as u32,
                        &format!("{} - {m}", p.info.description),
                    );
                }
            } else {
                push(&mut rows, &p, 0, &p.info.description);
            }
        }
        rows
    }

    fn open_plugins_dialog(&mut self, el: &ActiveEventLoop) {
        let rows = self.plugin_dialog_rows();
        let mut runtime = crate::plugins::Plugins::runtime_status();
        if let Some(e) = self
            .plugins
            .as_ref()
            .and_then(|s| s.errors().into_iter().next())
        {
            runtime = format!("{runtime} Problem: {e}");
        }
        if let Some(sys) = &self.plugins {
            if !self.plugins_ready && !sys.plugin_files().is_empty() {
                runtime = format!("Loading plug-ins... {runtime}");
            } else if rows.is_empty() {
                runtime = format!(
                    "No plug-ins found in {}. {runtime}",
                    self.plugin_dirs()[0].display()
                );
            }
        }
        self.open_dialog(el, Dialog::plugins(rows, &runtime));
    }

    fn refresh_plugins_dialog(&mut self) {
        if !self
            .dialogs
            .iter()
            .any(|d| d.dialog.kind == DialogKind::Plugins)
        {
            return;
        }
        let rows = self.plugin_dialog_rows();
        let runtime = crate::plugins::Plugins::runtime_status();
        for d in &mut self.dialogs {
            if d.dialog.kind == DialogKind::Plugins {
                let sel = d.dialog.selected_row();
                d.dialog = Dialog::plugins(rows.clone(), &runtime);
                if let Some(i) = sel {
                    d.dialog.select_row(i);
                }
                d.os.window.request_redraw();
            }
        }
        if let Some(i) = self
            .dialogs
            .iter()
            .find(|d| d.dialog.kind == DialogKind::Plugins)
            .and_then(|d| d.dialog.selected_row())
        {
            self.plugin_selected(i);
        }
    }

    fn row_plugin(&self, row: usize) -> Option<(crate::plugins::Plugin, u32)> {
        let (file, m) = self.plugin_rows.get(row)?.clone();
        Some((self.plugins.as_ref()?.by_file(&file)?, m))
    }

    fn plugin_selected(&mut self, row: usize) {
        let Some((p, _)) = self.row_plugin(row) else {
            return;
        };
        let skip = self
            .plugins
            .as_ref()
            .map(|s| s.skipped())
            .unwrap_or_default();
        let title = format!("{} ({})", p.info.description, short_path(&p.path));
        let detail = if let Some(why) = skip.get(&p.info.file.to_lowercase()) {
            format!("Not loaded: {why}. Enable it and press Reload to try again.")
        } else if let Some(e) = &p.info.error {
            format!("Could not be used: {e}.")
        } else {
            match p.info.kind {
                Some(ferramp_plugproto::Kind::Input) => {
                    let ext: Vec<String> = p.extensions().take(12).collect();
                    let more = p.extensions().count().saturating_sub(12);
                    format!(
                        "Plays {}{}.",
                        ext.join(", "),
                        if more > 0 {
                            format!(" and {more} more")
                        } else {
                            String::new()
                        }
                    )
                }
                Some(ferramp_plugproto::Kind::Dsp) => {
                    "Effect. Start / Stop applies it to everything played.".into()
                }
                Some(ferramp_plugproto::Kind::Vis) => {
                    "Visualization. Start / Stop, or Ctrl+Shift+K.".into()
                }
                Some(ferramp_plugproto::Kind::Output) => {
                    "Output. Start / Stop plays through it instead of the sound card.".into()
                }
                Some(ferramp_plugproto::Kind::General) => {
                    "General purpose plug-in, running.".into()
                }
                None => String::new(),
            }
        };
        for d in &mut self.dialogs {
            if d.dialog.kind == DialogKind::Plugins {
                d.dialog.set_details(&title, &detail);
                d.os.window.request_redraw();
            }
        }
    }

    fn plugin_action(&mut self, action: crate::ui::PluginAction, row: usize) {
        use crate::ui::PluginAction as A;
        let Some(sys) = self.plugins.clone() else {
            return;
        };
        match action {
            A::Folder => {
                let dir = self.plugin_dirs()[0].clone();
                let _ = std::fs::create_dir_all(&dir);
                open_url(&dir.to_string_lossy());
                return;
            }
            A::Rescan => {
                self.stop_plugins();
                self.start_plugins();
                self.refresh_plugins_dialog();
                for d in &mut self.dialogs {
                    if d.dialog.kind == DialogKind::Plugins {
                        d.dialog.set_details("Loading plug-ins...", "");
                    }
                }
                return;
            }
            _ => {}
        }
        let Some((p, m)) = self.row_plugin(row) else {
            return;
        };
        let key = format!("{}\t{m}", p.info.file);
        match action {
            A::Configure => sys.configure(&p, m),
            A::About => sys.about(&p, m),
            A::Use => match p.info.kind {
                Some(ferramp_plugproto::Kind::Dsp) => {
                    if sys.dsp_active() && self.settings.get_str("plugin_dsp") == Some(key.as_str())
                    {
                        sys.dsp_stop();
                        self.settings.set("plugin_dsp", "");
                    } else if sys.dsp_start(&p, m).is_ok() {
                        self.settings.set("plugin_dsp", key);
                    }
                }
                Some(ferramp_plugproto::Kind::Output) => {
                    if self.settings.get_str("plugin_out") == Some(key.as_str()) {
                        self.switch_output(None);
                    } else {
                        self.switch_output(Some(p.clone()));
                    }
                }
                Some(ferramp_plugproto::Kind::Vis) => {
                    let same = self.settings.get_str("plugin_vis") == Some(key.as_str());
                    if sys.vis_running() && same {
                        sys.vis_stop();
                        self.settings.set_bool("plugin_vis_on", false);
                    } else {
                        self.settings.set("plugin_vis", key);
                        sys.vis_start(&p, m);
                        self.plug.vis_was_running = true;
                        self.settings.set_bool("plugin_vis_on", true);
                    }
                }
                _ => {}
            },
            A::Enable => {
                let mut skip = self.plugin_skip();
                let f = p.info.file.to_lowercase();
                if skip.remove(&f).is_none() {
                    skip.insert(f, "disabled in the plug-in list".into());
                }
                self.save_plugin_skip(&skip);
                sys.set_skip(skip);
            }
            A::Folder | A::Rescan => {}
        }
        self.refresh_plugins_dialog();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relayout_keeps_stack_docked() {
        let before = vec![
            (WinKind::Main, (40, 40, 275, 116)),
            (WinKind::Eq, (40, 156, 275, 116)),
            (WinKind::Playlist, (40, 272, 275, 232)),
        ];
        // Double size: main and eq twice as big.
        let sizes = vec![
            (WinKind::Main, (550, 232)),
            (WinKind::Eq, (550, 232)),
            (WinKind::Playlist, (275, 232)),
        ];
        let out = relayout(&before, &sizes);
        assert_eq!(out[1].1, (40, 272));
        assert_eq!(out[2].1, (40, 504));
        // Shade the main window.
        let sizes = vec![
            (WinKind::Main, (275, 14)),
            (WinKind::Eq, (275, 116)),
            (WinKind::Playlist, (275, 232)),
        ];
        let out = relayout(&before, &sizes);
        assert_eq!(out[1].1, (40, 54));
        assert_eq!(out[2].1, (40, 170));
        // Side by side: a window right of the playlist follows its width.
        let before = vec![
            (WinKind::Playlist, (0, 0, 275, 116)),
            (WinKind::Eq, (275, 0, 275, 116)),
        ];
        let sizes = vec![(WinKind::Playlist, (325, 116)), (WinKind::Eq, (275, 116))];
        assert_eq!(relayout(&before, &sizes)[1].1, (325, 0));
    }

    #[test]
    fn docking_detection() {
        let a = (0, 0, 275, 116);
        assert!(touching(a, (0, 116, 275, 116)));
        assert!(touching(a, (275, 50, 275, 116)));
        assert!(!touching(a, (0, 120, 275, 116)));
        assert!(!touching(a, (300, 116, 275, 116)));
    }
}
