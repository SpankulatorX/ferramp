//! Small dialogs drawn in the classic Windows style: About, Open Location,
//! Jump to File, Jump to Time, File Info, Skin Browser, Preferences and the
//! classic skin offer.

use std::path::PathBuf;

use crate::gfx::{Bitmap, Rect, rgb};
use crate::text::{Fonts, Weight};
use crate::ui::menu::{DARK, FACE, HILITE, SELECT_BG, SHADOW, TEXT};
use crate::ui::{Cmd, Mods};

const FONT: f32 = 11.0;
const WHITE: u32 = rgb(255, 255, 255);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    About,
    OpenUrl,
    AddUrl,
    JumpToFile,
    JumpToTime,
    FileInfo,
    SkinBrowser,
    Preferences,
    EqPresets,
    Bookmarks,
    PlaylistEntry,
    VisOptions,
    ClassicSkin,
}

#[derive(Clone, Debug)]
pub enum Control {
    Label {
        text: String,
        rect: Rect,
        bold: bool,
    },
    Input {
        text: String,
        cursor: usize,
        rect: Rect,
    },
    List {
        items: Vec<String>,
        values: Vec<usize>,
        selected: Option<usize>,
        scroll: usize,
        rect: Rect,
    },
    Button {
        label: String,
        rect: Rect,
        id: &'static str,
        default: bool,
    },
    Check {
        label: String,
        checked: bool,
        rect: Rect,
        id: &'static str,
    },
    Group {
        label: String,
        rect: Rect,
    },
    /// A picture, or `note` centred in a sunken box until it arrives.
    Image {
        image: Option<Bitmap>,
        note: String,
        rect: Rect,
        id: &'static str,
    },
}

/// Something the dialog wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    None,
    Close,
    Run(Vec<Cmd>),
    /// Run the commands and close.
    Done(Vec<Cmd>),
}

pub struct Dialog {
    pub kind: DialogKind,
    pub title: String,
    pub w: i32,
    pub h: i32,
    pub controls: Vec<Control>,
    pub focus: Option<usize>,
    pressed: Option<usize>,
    /// Data used by the dialog's logic (playlist titles, skin paths...).
    pub entries: Vec<String>,
    pub paths: Vec<PathBuf>,
    pub prefs: Prefs,
    pub eq_op: Option<crate::ui::EqLibOp>,
    pub vis: Option<(crate::ui::vis::VisMode, crate::ui::vis::VisOptions)>,
    /// Index of the label that shows progress messages, if any.
    status: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Prefs {
    pub snap: bool,
    pub smooth_text: bool,
    pub show_numbers: bool,
}

fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x, y, w, h)
}

impl Dialog {
    fn new(kind: DialogKind, title: &str, w: i32, h: i32, controls: Vec<Control>) -> Dialog {
        let focus = controls
            .iter()
            .position(|c| matches!(c, Control::Input { .. } | Control::List { .. }));
        Dialog {
            kind,
            title: title.into(),
            w,
            h,
            controls,
            focus,
            pressed: None,
            entries: vec![],
            paths: vec![],
            prefs: Prefs::default(),
            eq_op: None,
            vis: None,
            status: None,
        }
    }

    /// Choose (or name) an equalizer preset for a library operation.
    pub fn eq_presets(op: crate::ui::EqLibOp, names: Vec<String>) -> Dialog {
        use crate::ui::EqLibOp::*;
        let (title, button) = match op {
            LoadPreset => ("Load EQ preset", "Load"),
            LoadAuto => ("Load auto-load preset", "Load"),
            SavePreset => ("Save EQ preset", "Save"),
            SaveAuto => ("Save auto-load preset", "Save"),
            DeletePreset => ("Delete EQ preset", "Delete"),
            DeleteAuto => ("Delete auto-load preset", "Delete"),
            LoadDefault | SaveDefault => ("EQ preset", "OK"),
        };
        let saving = matches!(op, SavePreset | SaveAuto);
        let values = (0..names.len()).collect();
        let mut c = vec![];
        let list_y = if saving { 36 } else { 10 };
        if saving {
            c.push(Control::Label {
                text: "Name:".into(),
                rect: r(10, 12, 40, 14),
                bold: false,
            });
            c.push(Control::Input {
                text: String::new(),
                cursor: 0,
                rect: r(50, 8, 220, 21),
            });
        }
        c.push(Control::List {
            items: names.clone(),
            values,
            selected: if names.is_empty() { None } else { Some(0) },
            scroll: 0,
            rect: r(10, list_y, 260, 180),
        });
        c.push(Control::Button {
            label: button.into(),
            rect: r(114, list_y + 188, 75, 23),
            id: "ok",
            default: true,
        });
        c.push(Control::Button {
            label: "Cancel".into(),
            rect: r(195, list_y + 188, 75, 23),
            id: "cancel",
            default: false,
        });
        let mut d = Dialog::new(DialogKind::EqPresets, title, 280, list_y + 222, c);
        d.entries = names;
        d.eq_op = Some(op);
        if saving {
            d.focus = Some(1);
        }
        d
    }

    pub fn bookmarks(names: Vec<String>) -> Dialog {
        let values = (0..names.len()).collect();
        let c = vec![
            Control::List {
                items: names.clone(),
                values,
                selected: if names.is_empty() { None } else { Some(0) },
                scroll: 0,
                rect: r(10, 10, 380, 200),
            },
            Control::Button {
                label: "Play".into(),
                rect: r(10, 218, 75, 23),
                id: "ok",
                default: true,
            },
            Control::Button {
                label: "Remove".into(),
                rect: r(91, 218, 75, 23),
                id: "remove",
                default: false,
            },
            Control::Button {
                label: "Close".into(),
                rect: r(315, 218, 75, 23),
                id: "cancel",
                default: false,
            },
        ];
        let mut d = Dialog::new(DialogKind::Bookmarks, "Edit Bookmarks", 400, 252, c);
        d.entries = names;
        d
    }

    pub fn playlist_entry(location: &str) -> Dialog {
        let c = vec![
            Control::Label {
                text: "Location of the playlist entry:".into(),
                rect: r(10, 12, 340, 14),
                bold: false,
            },
            Control::Input {
                text: location.to_string(),
                cursor: location.chars().count(),
                rect: r(10, 30, 340, 21),
            },
            Control::Button {
                label: "OK".into(),
                rect: r(194, 62, 75, 23),
                id: "ok",
                default: true,
            },
            Control::Button {
                label: "Cancel".into(),
                rect: r(275, 62, 75, 23),
                id: "cancel",
                default: false,
            },
        ];
        Dialog::new(DialogKind::PlaylistEntry, "Playlist entry", 360, 96, c)
    }

    pub fn vis_options(mode: crate::ui::vis::VisMode, o: crate::ui::vis::VisOptions) -> Dialog {
        use crate::ui::vis::{AnalyzerStyle, OscStyle, VisMode};
        let ck = |label: &str, on: bool, x: i32, y: i32, id: &'static str| Control::Check {
            label: label.into(),
            checked: on,
            rect: r(x, y, 140, 16),
            id,
        };
        let speeds = ["Slower", "Slow", "Moderate", "Fast", "Faster"];
        let speed_ids_f = ["f0", "f1", "f2", "f3", "f4"];
        let speed_ids_p = ["p0", "p1", "p2", "p3", "p4"];
        let mut c = vec![
            Control::Group {
                label: "Display".into(),
                rect: r(10, 8, 150, 82),
            },
            ck(
                "Spectrum analyzer",
                mode == VisMode::Analyzer,
                20,
                26,
                "m-an",
            ),
            ck(
                "Oscilloscope",
                mode == VisMode::Oscilloscope,
                20,
                46,
                "m-osc",
            ),
            ck("Disabled", mode == VisMode::Off, 20, 66, "m-off"),
            Control::Group {
                label: "Analyzer".into(),
                rect: r(170, 8, 160, 162),
            },
            ck(
                "Normal style",
                o.analyzer == AnalyzerStyle::Normal,
                180,
                26,
                "a-n",
            ),
            ck(
                "Fire style",
                o.analyzer == AnalyzerStyle::Fire,
                180,
                46,
                "a-f",
            ),
            ck(
                "Line style",
                o.analyzer == AnalyzerStyle::Line,
                180,
                66,
                "a-l",
            ),
            ck("Show peaks", o.peaks, 180, 96, "peaks"),
            ck("Thin bands", !o.wide_bands, 180, 126, "thin"),
            ck("Wide bands", o.wide_bands, 180, 146, "wide"),
            Control::Group {
                label: "Oscilloscope".into(),
                rect: r(10, 98, 150, 72),
            },
            ck("Dots", o.osc == OscStyle::Dots, 20, 116, "o-d"),
            ck("Lines", o.osc == OscStyle::Lines, 20, 134, "o-l"),
            ck("Solid", o.osc == OscStyle::Solid, 20, 152, "o-s"),
            Control::Group {
                label: "Analyzer falloff".into(),
                rect: r(10, 178, 150, 120),
            },
            Control::Group {
                label: "Peak falloff".into(),
                rect: r(170, 178, 160, 120),
            },
        ];
        for (i, sp) in speeds.iter().enumerate() {
            c.push(ck(
                sp,
                o.falloff == i as u8,
                20,
                196 + i as i32 * 20,
                speed_ids_f[i],
            ));
            c.push(ck(
                sp,
                o.peak_falloff == i as u8,
                180,
                196 + i as i32 * 20,
                speed_ids_p[i],
            ));
        }
        c.push(Control::Button {
            label: "Close".into(),
            rect: r(255, 306, 75, 23),
            id: "ok",
            default: true,
        });
        let mut d = Dialog::new(DialogKind::VisOptions, "Visualization options", 340, 338, c);
        d.vis = Some((mode, o));
        d
    }

    fn sync_vis_checks(&mut self) {
        use crate::ui::vis::{AnalyzerStyle, OscStyle, VisMode};
        let Some((mode, o)) = self.vis else { return };
        for c in &mut self.controls {
            if let Control::Check { checked, id, .. } = c {
                *checked = match *id {
                    "m-an" => mode == VisMode::Analyzer,
                    "m-osc" => mode == VisMode::Oscilloscope,
                    "m-off" => mode == VisMode::Off,
                    "a-n" => o.analyzer == AnalyzerStyle::Normal,
                    "a-f" => o.analyzer == AnalyzerStyle::Fire,
                    "a-l" => o.analyzer == AnalyzerStyle::Line,
                    "peaks" => o.peaks,
                    "thin" => !o.wide_bands,
                    "wide" => o.wide_bands,
                    "o-d" => o.osc == OscStyle::Dots,
                    "o-l" => o.osc == OscStyle::Lines,
                    "o-s" => o.osc == OscStyle::Solid,
                    f if f.starts_with('f') => o.falloff == f[1..].parse::<u8>().unwrap_or(9),
                    p if p.starts_with('p') && p != "peaks" => {
                        o.peak_falloff == p[1..].parse::<u8>().unwrap_or(9)
                    }
                    other => {
                        let _ = other;
                        *checked
                    }
                };
            }
        }
    }

    pub fn about() -> Dialog {
        let lines = [
            (concat!("Ferramp ", env!("CARGO_PKG_VERSION")), true),
            ("A from-scratch skinnable audio player in Rust,", false),
            ("compatible with classic skins (.wsz).", false),
            ("", false),
            ("Free software: MIT or Apache-2.0 licence.", false),
            ("Built-in skin and all code are original work.", false),
            ("Interface layout data after Webamp (MIT).", false),
            ("Fonts: Liberation Sans (SIL OFL 1.1).", false),
        ];
        let mut c: Vec<Control> = lines
            .iter()
            .enumerate()
            .map(|(i, (t, b))| Control::Label {
                text: t.to_string(),
                rect: r(16, 16 + i as i32 * 16, 300, 14),
                bold: *b,
            })
            .collect();
        c.push(Control::Button {
            label: "OK".into(),
            rect: r(132, 156, 75, 23),
            id: "ok",
            default: true,
        });
        Dialog::new(DialogKind::About, "About Ferramp", 340, 192, c)
    }

    /// First-start offer to fetch the original base skin, with the built-in
    /// skin on the left and the original's preview on the right.
    pub fn classic_skin(builtin: Bitmap) -> Dialog {
        let source = format!("Source: {} (101 KB)", crate::classic_skin::ITEM_PAGE);
        let lines = [
            "Ferramp comes with its own skin, drawn from scratch. The classic look most",
            "people remember is the original Winamp base skin, which belongs to its",
            "owners and cannot be included with Ferramp. Ferramp can download it for you",
            "from the Internet Archive, where the preview on the right also comes from.",
            &source,
        ];
        let mut c = vec![Control::Label {
            text: "Use the classic look?".into(),
            rect: r(16, 12, 570, 14),
            bold: true,
        }];
        for (i, t) in lines.iter().enumerate() {
            c.push(Control::Label {
                text: t.to_string(),
                rect: r(16, 34 + i as i32 * 16, 570, 14),
                bold: false,
            });
        }
        let (iw, ih) = (builtin.w, builtin.h);
        let top = 136;
        for (x, caption) in [(16, "Built-in skin"), (311, "Original base skin")] {
            c.push(Control::Label {
                text: caption.into(),
                rect: r(x, top - 18, iw, 14),
                bold: true,
            });
        }
        c.push(Control::Image {
            image: Some(builtin),
            note: String::new(),
            rect: r(16, top, iw, ih),
            id: "builtin",
        });
        c.push(Control::Image {
            image: None,
            note: "Loading preview...".into(),
            rect: r(311, top, iw, ih),
            id: "original",
        });
        let by = top + ih + 14;
        c.push(Control::Label {
            text: "You can switch skins at any time with Alt+S.".into(),
            rect: r(16, by + 4, 330, 14),
            bold: false,
        });
        let status = c.len() - 1;
        c.push(Control::Button {
            label: "Download and use".into(),
            rect: r(350, by, 120, 23),
            id: "download",
            default: true,
        });
        c.push(Control::Button {
            label: "Keep built-in".into(),
            rect: r(476, by, 110, 23),
            id: "cancel",
            default: false,
        });
        let mut d = Dialog::new(
            DialogKind::ClassicSkin,
            "Classic Look",
            602,
            by + 23 + 14,
            c,
        );
        d.status = Some(status);
        d
    }

    /// Fills an image control, or shows why it could not be loaded.
    pub fn set_image(&mut self, which: &str, result: Result<Bitmap, String>) {
        for c in &mut self.controls {
            if let Control::Image {
                image, note, id, ..
            } = c
                && *id == which
            {
                match result {
                    Ok(b) => *image = Some(b),
                    Err(e) => *note = format!("Preview unavailable: {e}"),
                }
                return;
            }
        }
    }

    pub fn set_status(&mut self, msg: &str) {
        if let Some(i) = self.status
            && let Some(Control::Label { text, .. }) = self.controls.get_mut(i)
        {
            *text = msg.to_string();
        }
    }

    pub fn open_url(add: bool) -> Dialog {
        let c = vec![
            Control::Label {
                text: "Enter location (URL or file path) to open:".into(),
                rect: r(10, 12, 340, 14),
                bold: false,
            },
            Control::Input {
                text: String::new(),
                cursor: 0,
                rect: r(10, 30, 340, 21),
            },
            Control::Button {
                label: "Open".into(),
                rect: r(194, 62, 75, 23),
                id: "ok",
                default: true,
            },
            Control::Button {
                label: "Cancel".into(),
                rect: r(275, 62, 75, 23),
                id: "cancel",
                default: false,
            },
        ];
        let (kind, title) = if add {
            (DialogKind::AddUrl, "Add Location")
        } else {
            (DialogKind::OpenUrl, "Open Location")
        };
        Dialog::new(kind, title, 360, 96, c)
    }

    pub fn jump_to_time(current: f64, duration: Option<f64>) -> Dialog {
        let fmt = |s: f64| crate::playlist::format_time(s);
        let info = match duration {
            Some(d) => format!("Track length: {}   Current: {}", fmt(d), fmt(current)),
            None => format!("Current: {}", fmt(current)),
        };
        let c = vec![
            Control::Label {
                text: info,
                rect: r(10, 10, 260, 14),
                bold: false,
            },
            Control::Label {
                text: "Jump to (mm:ss):".into(),
                rect: r(10, 34, 100, 14),
                bold: false,
            },
            Control::Input {
                text: fmt(current),
                cursor: fmt(current).len(),
                rect: r(110, 30, 160, 21),
            },
            Control::Button {
                label: "Jump".into(),
                rect: r(114, 62, 75, 23),
                id: "ok",
                default: true,
            },
            Control::Button {
                label: "Cancel".into(),
                rect: r(195, 62, 75, 23),
                id: "cancel",
                default: false,
            },
        ];
        Dialog::new(DialogKind::JumpToTime, "Jump to time", 280, 96, c)
    }

    pub fn jump_to_file(titles: Vec<String>) -> Dialog {
        let c = vec![
            Control::Label {
                text: "Search for:".into(),
                rect: r(10, 12, 80, 14),
                bold: false,
            },
            Control::Input {
                text: String::new(),
                cursor: 0,
                rect: r(80, 8, 330, 21),
            },
            Control::List {
                items: vec![],
                values: vec![],
                selected: Some(0),
                scroll: 0,
                rect: r(10, 36, 400, 220),
            },
            Control::Button {
                label: "Jump to file".into(),
                rect: r(174, 264, 75, 23),
                id: "ok",
                default: true,
            },
            Control::Button {
                label: "Enqueue".into(),
                rect: r(255, 264, 75, 23),
                id: "queue",
                default: false,
            },
            Control::Button {
                label: "Close".into(),
                rect: r(336, 264, 75, 23),
                id: "cancel",
                default: false,
            },
        ];
        let mut d = Dialog::new(DialogKind::JumpToFile, "Jump to file", 420, 298, c);
        d.entries = titles;
        d.focus = Some(1);
        d.refilter();
        d
    }

    pub fn file_info(fields: Vec<(String, String)>) -> Dialog {
        let mut c = Vec::new();
        let mut y = 12;
        for (k, v) in fields {
            c.push(Control::Label {
                text: format!("{k}:"),
                rect: r(10, y, 90, 14),
                bold: true,
            });
            c.push(Control::Label {
                text: v,
                rect: r(100, y, 330, 14),
                bold: false,
            });
            y += 18;
        }
        c.push(Control::Button {
            label: "Close".into(),
            rect: r(355, y + 8, 75, 23),
            id: "ok",
            default: true,
        });
        let h = y + 42;
        Dialog::new(DialogKind::FileInfo, "File info", 440, h, c)
    }

    pub fn skin_browser(skins: Vec<PathBuf>, current: Option<&PathBuf>) -> Dialog {
        let mut items = vec!["<Base Skin>".to_string()];
        items.extend(skins.iter().map(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        }));
        let sel = current
            .and_then(|c| skins.iter().position(|p| p == c))
            .map(|i| i + 1)
            .unwrap_or(0);
        let values = (0..items.len()).collect();
        let c = vec![
            Control::Label {
                text: "Select a skin (put .wsz files in the skins folder):".into(),
                rect: r(10, 10, 300, 14),
                bold: false,
            },
            Control::List {
                items,
                values,
                selected: Some(sel),
                scroll: 0,
                rect: r(10, 28, 300, 200),
            },
            Control::Button {
                label: "Open folder".into(),
                rect: r(10, 236, 75, 23),
                id: "folder",
                default: false,
            },
            Control::Button {
                label: "Close".into(),
                rect: r(235, 236, 75, 23),
                id: "ok",
                default: true,
            },
        ];
        let mut d = Dialog::new(DialogKind::SkinBrowser, "Skin Browser", 320, 270, c);
        d.paths = skins;
        d
    }

    pub fn preferences(p: Prefs) -> Dialog {
        let c = vec![
            Control::Group {
                label: "Windows".into(),
                rect: r(10, 10, 300, 52),
            },
            Control::Check {
                label: "Snap windows to each other and the screen edge".into(),
                checked: p.snap,
                rect: r(20, 28, 280, 16),
                id: "snap",
            },
            Control::Group {
                label: "Playlist editor".into(),
                rect: r(10, 70, 300, 70),
            },
            Control::Check {
                label: "Smooth (anti-aliased) playlist text".into(),
                checked: p.smooth_text,
                rect: r(20, 88, 280, 16),
                id: "smooth",
            },
            Control::Check {
                label: "Show track numbers".into(),
                checked: p.show_numbers,
                rect: r(20, 110, 280, 16),
                id: "numbers",
            },
            Control::Button {
                label: "Close".into(),
                rect: r(235, 150, 75, 23),
                id: "ok",
                default: true,
            },
        ];
        let mut d = Dialog::new(DialogKind::Preferences, "Preferences", 320, 184, c);
        d.prefs = p;
        d
    }

    fn input_text(&self) -> String {
        self.controls
            .iter()
            .find_map(|c| {
                if let Control::Input { text, .. } = c {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }

    fn list(
        &mut self,
    ) -> Option<(
        &mut Vec<String>,
        &mut Vec<usize>,
        &mut Option<usize>,
        &mut usize,
        Rect,
    )> {
        self.controls.iter_mut().find_map(|c| match c {
            Control::List {
                items,
                values,
                selected,
                scroll,
                rect,
            } => Some((items, values, selected, scroll, *rect)),
            _ => None,
        })
    }

    fn selected_value(&self) -> Option<usize> {
        self.controls.iter().find_map(|c| match c {
            Control::List {
                values, selected, ..
            } => selected.and_then(|s| values.get(s).copied()),
            _ => None,
        })
    }

    /// Jump-to-file: the playlist changed while the dialog is open. Keeps the
    /// filter and, when possible, the selected title and scroll position.
    pub fn set_entries(&mut self, titles: Vec<String>) {
        let keep = self
            .selected_value()
            .and_then(|i| self.entries.get(i).cloned());
        let old_scroll = self.list().map(|l| *l.3).unwrap_or(0);
        self.entries = titles;
        self.refilter();
        let values = self.list().map(|l| l.1.clone()).unwrap_or_default();
        let pos = keep.and_then(|t| values.iter().position(|&v| self.entries.get(v) == Some(&t)));
        if let Some((items, _, sel, scroll, _)) = self.list() {
            if pos.is_some() {
                *sel = pos;
            }
            *scroll = old_scroll.min(items.len().saturating_sub(1));
        }
    }

    /// Jump-to-file: filter entries by the search words.
    fn refilter(&mut self) {
        let query = self.input_text().to_lowercase();
        let words: Vec<&str> = query.split_whitespace().collect();
        let mut items = Vec::new();
        let mut values = Vec::new();
        for (i, t) in self.entries.iter().enumerate() {
            let lower = t.to_lowercase();
            if words.iter().all(|w| lower.contains(w)) {
                items.push(format!("{}. {}", i + 1, t));
                values.push(i);
            }
        }
        if let Some((li, lv, sel, scroll, _)) = self.list() {
            *li = items;
            *lv = values;
            *sel = if li.is_empty() { None } else { Some(0) };
            *scroll = 0;
        }
    }

    fn activate(&mut self, id: &str) -> Outcome {
        match (self.kind, id) {
            (_, "cancel") => Outcome::Close,
            (DialogKind::OpenUrl | DialogKind::AddUrl, "ok") => {
                let t = self.input_text().trim().to_string();
                if t.is_empty() {
                    return Outcome::Close;
                }
                let add = self.kind == DialogKind::AddUrl;
                Outcome::Done(vec![Cmd::OpenLocation(t, !add)])
            }
            (DialogKind::JumpToTime, "ok") => match parse_time(&self.input_text()) {
                Some(s) => Outcome::Done(vec![Cmd::SeekTo(s)]),
                None => Outcome::None,
            },
            (DialogKind::JumpToFile, "ok") => match self.selected_value() {
                Some(i) => Outcome::Done(vec![Cmd::PlayIndex(i)]),
                None => Outcome::None,
            },
            (DialogKind::JumpToFile, "queue") => match self.selected_value() {
                Some(i) => Outcome::Run(vec![Cmd::Enqueue(i)]),
                None => Outcome::None,
            },
            (DialogKind::SkinBrowser, "folder") => Outcome::Run(vec![Cmd::LoadSkinDialog]),
            (DialogKind::ClassicSkin, "download") => Outcome::Run(vec![Cmd::ClassicSkinDownload]),
            (DialogKind::EqPresets, "ok") => {
                let op = self.eq_op.unwrap_or(crate::ui::EqLibOp::LoadPreset);
                let typed = self.input_text().trim().to_string();
                let name = if !typed.is_empty() {
                    typed
                } else {
                    match self.selected_value().and_then(|i| self.entries.get(i)) {
                        Some(n) => n.clone(),
                        None => return Outcome::None,
                    }
                };
                Outcome::Done(vec![Cmd::EqLibCommit(op, name)])
            }
            (DialogKind::Bookmarks, "ok") => match self.selected_value() {
                Some(i) => Outcome::Run(vec![Cmd::PlayBookmark(i)]),
                None => Outcome::None,
            },
            (DialogKind::Bookmarks, "remove") => match self.selected_value() {
                Some(i) => {
                    if let Some((items, values, sel, _, _)) = self.list() {
                        if i < items.len() {
                            items.remove(i);
                            values.pop();
                        }
                        *sel = if items.is_empty() {
                            None
                        } else {
                            Some(i.min(items.len() - 1))
                        };
                    }
                    Outcome::Run(vec![Cmd::RemoveBookmark(i)])
                }
                None => Outcome::None,
            },
            (DialogKind::PlaylistEntry, "ok") => {
                Outcome::Done(vec![Cmd::PlSetLocation(self.input_text())])
            }
            (DialogKind::SkinBrowser, "ok") => Outcome::Close,
            (DialogKind::Preferences, "ok") => Outcome::Close,
            (_, "ok") => Outcome::Close,
            _ => Outcome::None,
        }
    }

    fn list_changed(&mut self) -> Outcome {
        if self.kind == DialogKind::SkinBrowser {
            let skin = match self.selected_value() {
                Some(0) | None => None,
                Some(i) => self.paths.get(i - 1).cloned(),
            };
            return Outcome::Run(vec![Cmd::LoadSkin(skin)]);
        }
        Outcome::None
    }

    pub fn default_button(&self) -> Option<&'static str> {
        self.controls.iter().find_map(|c| match c {
            Control::Button {
                id, default: true, ..
            } => Some(*id),
            _ => None,
        })
    }

    // ------------------------------------------------------------------

    pub fn mouse_down(&mut self, x: i32, y: i32, double: bool) -> Outcome {
        let mut out = Outcome::None;
        for i in 0..self.controls.len() {
            let hit = match &self.controls[i] {
                Control::Input { rect, .. }
                | Control::List { rect, .. }
                | Control::Button { rect, .. }
                | Control::Check { rect, .. } => rect.contains(x, y),
                _ => false,
            };
            if !hit {
                continue;
            }
            match &mut self.controls[i] {
                Control::Input { .. } => self.focus = Some(i),
                Control::List {
                    items,
                    selected,
                    scroll,
                    rect,
                    ..
                } => {
                    self.focus = Some(i);
                    let row = ((y - rect.y - 2) / 16) as usize + *scroll;
                    if row < items.len() {
                        *selected = Some(row);
                        out = if double {
                            self.activate("ok")
                        } else {
                            self.list_changed()
                        };
                    }
                }
                Control::Button { .. } => self.pressed = Some(i),
                Control::Check { checked, id, .. } => {
                    *checked = !*checked;
                    let v = *checked;
                    let id = *id;
                    match id {
                        "snap" => self.prefs.snap = v,
                        "smooth" => self.prefs.smooth_text = v,
                        "numbers" => self.prefs.show_numbers = v,
                        _ => {}
                    }
                    if let Some((mode, mut o)) = self.vis {
                        use crate::ui::vis::{AnalyzerStyle, OscStyle, VisMode};
                        let mut mode = mode;
                        match id {
                            "m-an" => mode = VisMode::Analyzer,
                            "m-osc" => mode = VisMode::Oscilloscope,
                            "m-off" => mode = VisMode::Off,
                            "a-n" => o.analyzer = AnalyzerStyle::Normal,
                            "a-f" => o.analyzer = AnalyzerStyle::Fire,
                            "a-l" => o.analyzer = AnalyzerStyle::Line,
                            "peaks" => o.peaks = v,
                            "thin" => o.wide_bands = false,
                            "wide" => o.wide_bands = true,
                            "o-d" => o.osc = OscStyle::Dots,
                            "o-l" => o.osc = OscStyle::Lines,
                            "o-s" => o.osc = OscStyle::Solid,
                            f if f.starts_with('f') => o.falloff = f[1..].parse().unwrap_or(2),
                            p if p.starts_with('p') => o.peak_falloff = p[1..].parse().unwrap_or(1),
                            _ => {}
                        }
                        self.vis = Some((mode, o));
                        self.sync_vis_checks();
                        out = Outcome::Run(vec![Cmd::SetVisMode(mode), Cmd::SetVisOptions(o)]);
                    }
                }
                _ => {}
            }
            break;
        }
        out
    }

    pub fn mouse_up(&mut self, x: i32, y: i32) -> Outcome {
        if let Some(i) = self.pressed.take()
            && let Control::Button { rect, id, .. } = &self.controls[i]
            && rect.contains(x, y)
        {
            let id = *id;
            return self.activate(id);
        }
        Outcome::None
    }

    pub fn wheel(&mut self, delta: i32) {
        if let Some((items, _, _, scroll, rect)) = self.list() {
            let rows = ((rect.h - 4) / 16) as usize;
            let max = items.len().saturating_sub(rows);
            *scroll = (*scroll as i32 - delta * 3).clamp(0, max as i32) as usize;
        }
    }

    pub fn key(&mut self, key: &Key, mods: Mods, clipboard: Option<String>) -> Outcome {
        match key {
            Key::Escape => return Outcome::Close,
            Key::Enter => {
                if let Some(id) = self.default_button() {
                    return self.activate(id);
                }
                return Outcome::Close;
            }
            Key::Tab => {
                let focusable: Vec<usize> = (0..self.controls.len())
                    .filter(|&i| {
                        matches!(
                            self.controls[i],
                            Control::Input { .. } | Control::List { .. }
                        )
                    })
                    .collect();
                if !focusable.is_empty() {
                    let cur = self
                        .focus
                        .and_then(|f| focusable.iter().position(|&x| x == f))
                        .unwrap_or(0);
                    self.focus = Some(focusable[(cur + 1) % focusable.len()]);
                }
                return Outcome::None;
            }
            _ => {}
        }
        // Up/Down move the list selection even while typing in the search box.
        if matches!(key, Key::Up | Key::Down | Key::PageUp | Key::PageDown)
            && let Some((items, _, selected, scroll, rect)) = self.list()
        {
            if items.is_empty() {
                return Outcome::None;
            }
            let rows = ((rect.h - 4) / 16).max(1) as usize;
            let cur = selected.unwrap_or(0) as i64;
            let step = match key {
                Key::Up => -1,
                Key::Down => 1,
                Key::PageUp => -(rows as i64),
                _ => rows as i64,
            };
            let n = (cur + step).clamp(0, items.len() as i64 - 1) as usize;
            *selected = Some(n);
            if n < *scroll {
                *scroll = n;
            } else if n >= *scroll + rows {
                *scroll = n + 1 - rows;
            }
            return self.list_changed();
        }
        let Some(f) = self.focus else {
            return Outcome::None;
        };
        let mut changed = false;
        if let Control::Input { text, cursor, .. } = &mut self.controls[f] {
            let chars: Vec<char> = text.chars().collect();
            match key {
                Key::Char(c) if !mods.ctrl => {
                    let mut v = chars.clone();
                    v.insert(*cursor, *c);
                    *text = v.into_iter().collect();
                    *cursor += 1;
                    changed = true;
                }
                Key::Char('v') | Key::Char('V') if mods.ctrl => {
                    if let Some(p) = clipboard {
                        let p: String = p.chars().filter(|c| !c.is_control()).collect();
                        let mut v = chars.clone();
                        for (k, c) in p.chars().enumerate() {
                            v.insert(*cursor + k, c);
                        }
                        *cursor += p.chars().count();
                        *text = v.into_iter().collect();
                        changed = true;
                    }
                }
                Key::Char('a') | Key::Char('A') if mods.ctrl => *cursor = chars.len(),
                Key::Backspace if *cursor > 0 => {
                    let mut v = chars.clone();
                    v.remove(*cursor - 1);
                    *text = v.into_iter().collect();
                    *cursor -= 1;
                    changed = true;
                }
                Key::Delete if *cursor < chars.len() => {
                    let mut v = chars.clone();
                    v.remove(*cursor);
                    *text = v.into_iter().collect();
                    changed = true;
                }
                Key::Left => *cursor = cursor.saturating_sub(1),
                Key::Right => *cursor = (*cursor + 1).min(chars.len()),
                Key::Home => *cursor = 0,
                Key::End => *cursor = chars.len(),
                _ => {}
            }
        }
        if changed && self.kind == DialogKind::JumpToFile {
            self.refilter();
        }
        Outcome::None
    }

    pub fn render(&self, fonts: &Fonts, out: &mut Bitmap) {
        out.fill(FACE);
        for (i, c) in self.controls.iter().enumerate() {
            let focused = self.focus == Some(i);
            match c {
                Control::Image {
                    image, note, rect, ..
                } => match image {
                    Some(b) => out.blit(b, 0, 0, rect.w.min(b.w), rect.h.min(b.h), rect.x, rect.y),
                    None => {
                        sunken_field(out, *rect, rgb(0, 0, 0));
                        let inner = Rect::new(rect.x + 8, rect.y, rect.w - 16, rect.h);
                        let t = fonts.ellipsize(note, FONT, Weight::Regular, inner.w);
                        let tw = fonts.measure(&t, FONT, Weight::Regular);
                        let ty = rect.y + rect.h / 2 - 7;
                        fonts.draw(
                            out,
                            &t,
                            rect.x + (rect.w - tw) / 2,
                            ty,
                            FONT,
                            Weight::Regular,
                            WHITE,
                            inner,
                            true,
                        );
                    }
                },
                Control::Label { text, rect, bold } => {
                    let w = if *bold { Weight::Bold } else { Weight::Regular };
                    let t = fonts.ellipsize(text, FONT, w, rect.w);
                    fonts.draw(out, &t, rect.x, rect.y, FONT, w, TEXT, *rect, true);
                }
                Control::Group { label, rect } => {
                    etched_frame(out, rect.x, rect.y + 6, rect.w, rect.h - 6);
                    let tw = fonts.measure(label, FONT, Weight::Regular);
                    out.fill_rect(rect.x + 7, rect.y, tw + 4, 14, FACE);
                    fonts.draw(
                        out,
                        label,
                        rect.x + 9,
                        rect.y,
                        FONT,
                        Weight::Regular,
                        TEXT,
                        *rect,
                        true,
                    );
                }
                Control::Input { text, cursor, rect } => {
                    sunken_field(out, *rect, WHITE);
                    let inner = Rect::new(rect.x + 3, rect.y + 2, rect.w - 6, rect.h - 4);
                    // Scroll the text so the cursor stays visible.
                    let before: String = text.chars().take(*cursor).collect();
                    let cx = fonts.measure(&before, FONT, Weight::Regular);
                    let shift = (cx - inner.w + 4).max(0);
                    fonts.draw(
                        out,
                        text,
                        inner.x - shift,
                        rect.y + 4,
                        FONT,
                        Weight::Regular,
                        TEXT,
                        inner,
                        true,
                    );
                    if focused {
                        out.vline(inner.x - shift + cx, rect.y + 4, 13, TEXT);
                    }
                }
                Control::List {
                    items,
                    selected,
                    scroll,
                    rect,
                    ..
                } => {
                    sunken_field(out, *rect, WHITE);
                    let rows = ((rect.h - 4) / 16) as usize;
                    let inner = Rect::new(rect.x + 2, rect.y + 2, rect.w - 4, rect.h - 4);
                    for row in 0..rows {
                        let idx = scroll + row;
                        if idx >= items.len() {
                            break;
                        }
                        let y = rect.y + 2 + row as i32 * 16;
                        let sel = *selected == Some(idx);
                        if sel {
                            out.fill_rect(
                                inner.x,
                                y,
                                inner.w,
                                16,
                                if focused {
                                    SELECT_BG
                                } else {
                                    rgb(192, 192, 192)
                                },
                            );
                        }
                        let fg = if sel && focused { WHITE } else { TEXT };
                        fonts.draw(
                            out,
                            &items[idx],
                            inner.x + 2,
                            y + 1,
                            FONT,
                            Weight::Regular,
                            fg,
                            inner,
                            true,
                        );
                    }
                }
                Control::Button {
                    label,
                    rect,
                    default,
                    ..
                } => {
                    let down = self.pressed == Some(i);
                    button(out, *rect, down, *default);
                    let tw = fonts.measure(label, FONT, Weight::Regular);
                    let o = if down { 1 } else { 0 };
                    fonts.draw(
                        out,
                        label,
                        rect.x + (rect.w - tw) / 2 + o,
                        rect.y + 5 + o,
                        FONT,
                        Weight::Regular,
                        TEXT,
                        *rect,
                        true,
                    );
                }
                Control::Check {
                    label,
                    checked,
                    rect,
                    ..
                } => {
                    let b = Rect::new(rect.x, rect.y + 1, 13, 13);
                    sunken_field(out, b, WHITE);
                    if *checked {
                        for (dx, dy) in [(0, 2), (1, 3), (2, 4), (3, 3), (4, 2), (5, 1), (6, 0)] {
                            out.vline(b.x + 3 + dx, b.y + 3 + dy, 3, TEXT);
                        }
                    }
                    fonts.draw(
                        out,
                        label,
                        rect.x + 18,
                        rect.y + 1,
                        FONT,
                        Weight::Regular,
                        TEXT,
                        *rect,
                        true,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Other,
}

pub fn parse_time(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    let mut total = 0.0;
    for p in &parts {
        let v: f64 = p.trim().parse().ok()?;
        if v < 0.0 {
            return None;
        }
        total = total * 60.0 + v;
    }
    if parts.is_empty() || parts.len() > 3 {
        None
    } else {
        Some(total)
    }
}

fn sunken_field(out: &mut Bitmap, r: Rect, fill: u32) {
    out.fill_rect(r.x, r.y, r.w, r.h, fill);
    out.hline(r.x, r.y, r.w - 1, SHADOW);
    out.vline(r.x, r.y, r.h - 1, SHADOW);
    out.hline(r.x + 1, r.y + 1, r.w - 3, DARK);
    out.vline(r.x + 1, r.y + 1, r.h - 3, DARK);
    out.hline(r.x, r.y + r.h - 1, r.w, HILITE);
    out.vline(r.x + r.w - 1, r.y, r.h, HILITE);
    out.hline(r.x + 1, r.y + r.h - 2, r.w - 2, FACE);
    out.vline(r.x + r.w - 2, r.y + 1, r.h - 2, FACE);
}

fn etched_frame(out: &mut Bitmap, x: i32, y: i32, w: i32, h: i32) {
    out.rect_outline(x, y, w - 1, h - 1, SHADOW);
    out.rect_outline(x + 1, y + 1, w - 1, h - 1, HILITE);
}

fn button(out: &mut Bitmap, r: Rect, down: bool, default: bool) {
    out.fill_rect(r.x, r.y, r.w, r.h, FACE);
    let (x, y, w, h) = if default {
        out.rect_outline(r.x, r.y, r.w, r.h, TEXT);
        (r.x + 1, r.y + 1, r.w - 2, r.h - 2)
    } else {
        (r.x, r.y, r.w, r.h)
    };
    if down {
        out.rect_outline(x, y, w, h, SHADOW);
    } else {
        out.hline(x, y, w - 1, HILITE);
        out.vline(x, y, h - 1, HILITE);
        out.hline(x, y + h - 1, w, DARK);
        out.vline(x + w - 1, y, h, DARK);
        out.hline(x + 1, y + h - 2, w - 2, SHADOW);
        out.vline(x + w - 2, y + 1, h - 2, SHADOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_parsing() {
        assert_eq!(parse_time("1:30"), Some(90.0));
        assert_eq!(parse_time("45"), Some(45.0));
        assert_eq!(parse_time("1:00:00"), Some(3600.0));
        assert_eq!(parse_time("x"), None);
    }

    #[test]
    fn jump_to_file_filters_and_jumps() {
        let mut d = Dialog::jump_to_file(vec![
            "Alpha - One".into(),
            "Beta - Two".into(),
            "Alpha - Three".into(),
        ]);
        for c in "alpha th".chars() {
            d.key(&Key::Char(c), Mods::default(), None);
        }
        assert_eq!(d.selected_value(), Some(2));
        assert_eq!(
            d.key(&Key::Enter, Mods::default(), None),
            Outcome::Done(vec![Cmd::PlayIndex(2)])
        );
    }

    #[test]
    fn open_url_returns_drop() {
        let mut d = Dialog::open_url(false);
        d.key(&Key::Char('x'), Mods::default(), None);
        d.key(
            &Key::Char('v'),
            Mods {
                ctrl: true,
                ..Default::default()
            },
            Some("http://a/b".into()),
        );
        match d.key(&Key::Enter, Mods::default(), None) {
            Outcome::Done(cmds) => {
                assert_eq!(cmds, vec![Cmd::OpenLocation("xhttp://a/b".into(), true)])
            }
            o => panic!("{o:?}"),
        }
    }
}
