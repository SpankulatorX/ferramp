//! Pop-up menus in the classic Windows style the original used, and the
//! menu contents (main menu, options, playback, visualisation, presets,
//! playlist menus).

use std::path::PathBuf;

use crate::gfx::{Bitmap, Rect, rgb};
use crate::state::State;
use crate::text::{Fonts, TextQueue, Weight};
use crate::ui::vis::VisMode;
use crate::ui::{Cmd, EqLibOp, MenuId, WinKind};

pub const FACE: u32 = rgb(212, 208, 200);
pub const HILITE: u32 = rgb(255, 255, 255);
pub const LIGHT: u32 = rgb(212, 208, 200);
pub const SHADOW: u32 = rgb(128, 128, 128);
pub const DARK: u32 = rgb(64, 64, 64);
pub const SELECT_BG: u32 = rgb(10, 36, 106);
pub const TEXT: u32 = rgb(0, 0, 0);
pub const GRAY_TEXT: u32 = rgb(128, 128, 128);
pub const FONT_PX: f32 = 11.0;
const ITEM_H: i32 = 17;
const SEP_H: i32 = 9;

#[derive(Clone, Debug)]
pub enum Item {
    Action {
        label: String,
        hotkey: String,
        cmd: Cmd,
        checked: bool,
        enabled: bool,
        default: bool,
    },
    Sub {
        label: String,
        items: Vec<Item>,
        enabled: bool,
    },
    Sep,
}

fn act(label: &str, hotkey: &str, cmd: Cmd) -> Item {
    Item::Action {
        label: label.into(),
        hotkey: hotkey.into(),
        cmd,
        checked: false,
        enabled: true,
        default: false,
    }
}

fn check(label: &str, hotkey: &str, cmd: Cmd, on: bool) -> Item {
    Item::Action {
        label: label.into(),
        hotkey: hotkey.into(),
        cmd,
        checked: on,
        enabled: true,
        default: false,
    }
}

fn sub(label: &str, items: Vec<Item>) -> Item {
    Item::Sub {
        label: label.into(),
        items,
        enabled: true,
    }
}

pub struct MenuContext<'a> {
    pub state: &'a State,
    pub skins: &'a [PathBuf],
    pub bookmarks: &'a [String],
}

fn disabled(mut it: Item) -> Item {
    if let Item::Action { enabled, .. } = &mut it {
        *enabled = false;
    }
    it
}

/// Menu contents. `&` marks the keyboard mnemonic (drawn underlined).
pub fn build(id: MenuId, ctx: &MenuContext) -> Vec<Item> {
    let st = ctx.state;
    match id {
        MenuId::Main => vec![
            act("&Ferramp...", "", Cmd::About),
            Item::Sep,
            sub(
                "&Play",
                vec![
                    act("&File...", "L", Cmd::OpenFiles),
                    act("&Location...", "Ctrl+L", Cmd::OpenUrl),
                    act("&Directory...", "Shift+L", Cmd::OpenFolder),
                    Item::Sep,
                    sub("&Bookmark", bookmark_items(ctx)),
                ],
            ),
            act("View &file info", "Alt+3", Cmd::FileInfo),
            sub("&Bookmarks", {
                let mut v = vec![
                    act("&Edit bookmarks...", "(Ctrl+Alt+I)", Cmd::EditBookmarks),
                    act("&Add current as bookmark", "(Alt+I)", Cmd::AddBookmark),
                ];
                if !ctx.bookmarks.is_empty() {
                    v.push(Item::Sep);
                    v.extend(bookmark_items(ctx));
                }
                v
            }),
            Item::Sep,
            check(
                "&Main Window",
                "Alt+W",
                Cmd::ToggleWindow(WinKind::Main),
                st.main.visible,
            ),
            check(
                "Playlist &Editor",
                "Alt+E",
                Cmd::ToggleWindow(WinKind::Playlist),
                st.pl.visible,
            ),
            check(
                "E&qualizer",
                "Alt+G",
                Cmd::ToggleWindow(WinKind::Eq),
                st.eqw.visible,
            ),
            Item::Sep,
            sub("&Options", build(MenuId::Options, ctx)),
            sub("Play&back", build(MenuId::Playback, ctx)),
            sub("&Visualization", build(MenuId::Vis, ctx)),
            sub("&Skins", build(MenuId::Skins, ctx)),
            Item::Sep,
            act("E&xit", "", Cmd::Exit),
        ],
        MenuId::SongTitle => vec![
            act("&File info", "Alt+3 or Dblclick", Cmd::FileInfo),
            act("&Jump to file...", "J", Cmd::JumpToFile),
            act("Jump to &time...", "Ctrl+J", Cmd::JumpToTime),
            check(
                "&Autoscroll songname",
                "",
                Cmd::ToggleAutoscroll,
                st.autoscroll,
            ),
        ],
        MenuId::Time => vec![
            check(
                "Time &elapsed",
                "(Ctrl+T toggles)",
                Cmd::ToggleTimeMode,
                !st.time_remaining,
            ),
            check(
                "Time re&maining",
                "(Ctrl+T toggles)",
                Cmd::ToggleTimeMode,
                st.time_remaining,
            ),
        ],
        MenuId::Skins => {
            let mut v = vec![
                act("S&kin Browser...", "Alt+S", Cmd::SkinBrowser),
                act("&Classic Look...", "", Cmd::ClassicSkin),
                Item::Sep,
            ];
            v.push(check(
                "<Base Skin>",
                "",
                Cmd::LoadSkin(None),
                st.skin.path.is_none(),
            ));
            for p in ctx.skins {
                let name = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let on = st.skin.path.as_deref() == Some(p.as_path());
                v.push(check(
                    &name.replace('&', "&&"),
                    "",
                    Cmd::LoadSkin(Some(p.clone())),
                    on,
                ));
            }
            v.push(act("<< Get more skins! >>", "", Cmd::GetMoreSkins));
            v
        }
        MenuId::Options => vec![
            act("&Preferences...", "Ctrl+P", Cmd::Preferences),
            sub("&Skins", build(MenuId::Skins, ctx)),
            Item::Sep,
            check(
                "Time &elapsed",
                "(Ctrl+T toggles)",
                Cmd::ToggleTimeMode,
                !st.time_remaining,
            ),
            check(
                "Time re&maining",
                "(Ctrl+T toggles)",
                Cmd::ToggleTimeMode,
                st.time_remaining,
            ),
            Item::Sep,
            check(
                "&Always On Top",
                "Ctrl+A",
                Cmd::ToggleAlwaysOnTop,
                st.always_on_top,
            ),
            check("&Double Size", "Ctrl+D", Cmd::ToggleDouble, st.doubled),
            check("&EasyMove", "Ctrl+E", Cmd::ToggleEasyMove, st.easy_move),
            Item::Sep,
            check("&Repeat", "R", Cmd::ToggleRepeat, st.repeat),
            check("S&huffle", "S", Cmd::ToggleShuffle, st.shuffle),
        ],
        MenuId::Playback => vec![
            act("Pre&vious", "Z", Cmd::Previous),
            act("Pla&y", "X", Cmd::Play),
            act("P&ause", "C", Cmd::Pause),
            act("&Stop", "V", Cmd::Stop),
            act("&Next", "B", Cmd::Next),
            Item::Sep,
            act("Stop w/ f&adeout", "Shift+V", Cmd::StopFadeout),
            check(
                "Stop after c&urrent",
                "Ctrl+V",
                Cmd::StopAfterCurrent,
                st.stop_after_current,
            ),
            act("&Back 5 seconds", "Left", Cmd::SeekBy(-5.0)),
            act("&Fwd 5 seconds", "Right", Cmd::SeekBy(5.0)),
            act("Start of l&ist", "Ctrl+Z", Cmd::StartOfList),
            act("10 &tracks back", "Num. 1", Cmd::NextN(-10)),
            act("10 t&racks fwd", "Num. 3", Cmd::NextN(10)),
            Item::Sep,
            act("&Jump to time", "Ctrl+J", Cmd::JumpToTime),
        ],
        MenuId::Vis => vec![
            act("&Visualization options...", "Alt+O", Cmd::VisOptionsDialog),
            Item::Sep,
            check(
                "&Start/Stop plug-in",
                "Ctrl+Shift+K",
                Cmd::ToggleVisPlugin,
                st.vis_mode != VisMode::Off,
            ),
            act("&Configure plug-in...", "Alt+K", Cmd::VisOptionsDialog),
            act("S&elect plug-in...", "Ctrl+K", Cmd::VisOptionsDialog),
        ],
        MenuId::EqPresets => vec![
            sub(
                "&Load",
                vec![
                    act("&Preset...", "", Cmd::EqLibrary(EqLibOp::LoadPreset)),
                    act(
                        "&Auto-load preset...",
                        "",
                        Cmd::EqLibrary(EqLibOp::LoadAuto),
                    ),
                    act("&Default", "", Cmd::EqLibrary(EqLibOp::LoadDefault)),
                    Item::Sep,
                    act("From &EQF...", "", Cmd::EqLoadEqf),
                ],
            ),
            sub(
                "&Save",
                vec![
                    act("&Preset...", "", Cmd::EqLibrary(EqLibOp::SavePreset)),
                    act(
                        "&Auto-load preset...",
                        "",
                        Cmd::EqLibrary(EqLibOp::SaveAuto),
                    ),
                    act("&Default", "", Cmd::EqLibrary(EqLibOp::SaveDefault)),
                    Item::Sep,
                    act("To &EQF...", "", Cmd::EqSaveEqf),
                ],
            ),
            sub(
                "&Delete",
                vec![
                    act("&Preset...", "", Cmd::EqLibrary(EqLibOp::DeletePreset)),
                    act(
                        "&Auto-load preset...",
                        "",
                        Cmd::EqLibrary(EqLibOp::DeleteAuto),
                    ),
                ],
            ),
        ],
        MenuId::PlaylistRemove => vec![act("Remove all &dead files", "", Cmd::PlRemoveDead)],
        MenuId::PlaylistSort => vec![
            act("Sort list by &title", "", Cmd::PlSortTitle),
            act("Sort list by &filename", "", Cmd::PlSortFilename),
            act("Sort list by &path and filename", "", Cmd::PlSortPath),
            Item::Sep,
            act("&Reverse list", "", Cmd::PlReverse),
            act("R&andomize list", "", Cmd::PlRandomize),
        ],
        MenuId::PlaylistMiscOptions => vec![
            act("&Generate HTML playlist", "", Cmd::PlGenerateHtml),
            Item::Sep,
            act("&Read extended info on selection", "", Cmd::PlReadInfo),
        ],
        MenuId::PlaylistContext => {
            let any = st.playlist.selected_count() > 0;
            let on = |it: Item| if any { it } else { disabled(it) };
            vec![
                on(act("&Play item(s)", "Enter", Cmd::PlPlaySelected)),
                Item::Sep,
                on(act("&Remove item(s)", "Delete", Cmd::PlRemoveSelected)),
                on(act("&Crop files", "Ctrl+Delete", Cmd::PlCrop)),
                Item::Sep,
                on(act("&File info", "Alt+3", Cmd::FileInfo)),
                on(act("Playlist &entry", "Ctrl+E", Cmd::PlEditEntry)),
                on(act("&Bookmark item(s)", "Alt+I", Cmd::BookmarkSelected)),
            ]
        }
    }
}

fn bookmark_items(ctx: &MenuContext) -> Vec<Item> {
    if ctx.bookmarks.is_empty() {
        return vec![disabled(act("(no bookmarks)", "", Cmd::EditBookmarks))];
    }
    ctx.bookmarks
        .iter()
        .enumerate()
        .map(|(i, b)| act(&b.replace('&', "&&"), "", Cmd::PlayBookmark(i)))
        .collect()
}

/// Split a label into display text and the index (in chars) of its
/// mnemonic letter: "&File" -> ("File", Some(0)); "&&" is a literal '&'.
pub fn mnemonic(label: &str) -> (String, Option<usize>) {
    let mut out = String::new();
    let mut idx = None;
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.peek() {
                Some('&') => {
                    chars.next();
                    out.push('&');
                }
                Some(_) if idx.is_none() => idx = Some(out.chars().count()),
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    (out, idx)
}

/// One open level of a menu (the root or a submenu).
pub struct MenuLevel {
    pub items: Vec<Item>,
    pub hover: Option<usize>,
    pub width: i32,
    pub height: i32,
    label_w: i32,
}

impl MenuLevel {
    pub fn new(items: Vec<Item>, fonts: &Fonts) -> MenuLevel {
        let mut label_w = 0;
        let mut key_w = 0;
        let mut has_sub = false;
        let mut height = 6;
        for it in &items {
            match it {
                Item::Action {
                    label,
                    hotkey,
                    default,
                    ..
                } => {
                    let w = if *default {
                        Weight::Bold
                    } else {
                        Weight::Regular
                    };
                    label_w = label_w.max(fonts.measure(&mnemonic(label).0, FONT_PX, w));
                    key_w = key_w.max(fonts.measure(hotkey, FONT_PX, Weight::Regular));
                    height += ITEM_H;
                }
                Item::Sub { label, .. } => {
                    label_w =
                        label_w.max(fonts.measure(&mnemonic(label).0, FONT_PX, Weight::Regular));
                    has_sub = true;
                    height += ITEM_H;
                }
                Item::Sep => height += SEP_H,
            }
        }
        let mut width = 3 + 17 + label_w + 17;
        if key_w > 0 {
            width += key_w + 10;
        }
        if has_sub || key_w == 0 {
            width += 4;
        }
        MenuLevel {
            items,
            hover: None,
            width: width.max(80),
            height,
            label_w,
        }
    }

    pub fn item_rect(&self, i: usize) -> Rect {
        let mut y = 3;
        for (j, it) in self.items.iter().enumerate() {
            let h = if matches!(it, Item::Sep) {
                SEP_H
            } else {
                ITEM_H
            };
            if j == i {
                return Rect::new(3, y, self.width - 6, h);
            }
            y += h;
        }
        Rect::default()
    }

    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.items.len())
            .find(|&i| self.item_rect(i).contains(x, y) && !matches!(self.items[i], Item::Sep))
    }

    /// Draws the menu at `scale`x into `out`, with sharp text on high-DPI screens.
    pub fn render(&self, fonts: &Fonts, out: &mut Bitmap, scale: i32) {
        let mut chrome = Bitmap::new(self.width, self.height, FACE);
        let mut tq = TextQueue::default();
        self.render_layers(fonts, &mut chrome, &mut tq);
        tq.compose(fonts, &chrome, out, scale);
    }

    fn render_layers(&self, fonts: &Fonts, out: &mut Bitmap, tq: &mut TextQueue) {
        let (w, h) = (self.width, self.height);
        out.fill(FACE);
        // Classic raised frame.
        out.hline(0, 0, w - 1, LIGHT);
        out.vline(0, 0, h - 1, LIGHT);
        out.hline(1, 1, w - 3, HILITE);
        out.vline(1, 1, h - 3, HILITE);
        out.hline(1, h - 2, w - 2, SHADOW);
        out.vline(w - 2, 1, h - 2, SHADOW);
        out.hline(0, h - 1, w, DARK);
        out.vline(w - 1, 0, h, DARK);
        for (i, it) in self.items.iter().enumerate() {
            let r = self.item_rect(i);
            match it {
                Item::Sep => {
                    out.hline(r.x + 1, r.y + 3, r.w - 2, SHADOW);
                    out.hline(r.x + 1, r.y + 4, r.w - 2, HILITE);
                }
                Item::Action {
                    label,
                    hotkey,
                    checked,
                    enabled,
                    default,
                    ..
                } => {
                    let hot = self.hover == Some(i);
                    self.draw_row(
                        fonts, out, tq, r, label, hotkey, *checked, *enabled, *default, hot, false,
                    );
                }
                Item::Sub { label, enabled, .. } => {
                    let hot = self.hover == Some(i);
                    self.draw_row(
                        fonts, out, tq, r, label, "", false, *enabled, false, hot, true,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
        fonts: &Fonts,
        out: &mut Bitmap,
        tq: &mut TextQueue,
        r: Rect,
        label: &str,
        hotkey: &str,
        checked: bool,
        enabled: bool,
        default: bool,
        hot: bool,
        arrow: bool,
    ) {
        if hot && enabled {
            out.fill_rect(r.x, r.y, r.w, r.h, SELECT_BG);
        }
        let fg = if !enabled {
            GRAY_TEXT
        } else if hot {
            HILITE
        } else {
            TEXT
        };
        let ty = r.y + 2;
        let weight = if default {
            Weight::Bold
        } else {
            Weight::Regular
        };
        let clip = r;
        let mut draw = |text: &str, x: i32, c: u32, wgt: Weight| {
            tq.draw(fonts, text, x, ty, FONT_PX, wgt, c, clip);
        };
        let (text, mn) = mnemonic(label);
        let underline = |out: &mut Bitmap, x: i32, c: u32| {
            if let Some(i) = mn {
                let pre: String = text.chars().take(i).collect();
                let ch: String = text.chars().skip(i).take(1).collect();
                let ux = x + fonts.measure(&pre, FONT_PX, weight);
                let uw = fonts.measure(&ch, FONT_PX, weight);
                out.hline(ux, ty + fonts.ascent(FONT_PX, weight) + 1, uw.max(1), c);
            }
        };
        if !enabled && !hot {
            draw(&text, r.x + 18, HILITE, weight);
            underline(out, r.x + 18, HILITE);
        }
        draw(&text, r.x + 17, fg, weight);
        underline(out, r.x + 17, fg);
        if !hotkey.is_empty() {
            let kw = fonts.measure(hotkey, FONT_PX, Weight::Regular);
            let kx = r.x + 17 + self.label_w + 10;
            let kx = kx.max(r.x + r.w - 14 - kw);
            draw(hotkey, kx, fg, Weight::Regular);
        }
        if checked {
            let cx = r.x + 5;
            let cy = r.y + 5;
            for (dx, dy) in [(0, 2), (1, 3), (2, 4), (3, 3), (4, 2), (5, 1), (6, 0)] {
                out.vline(cx + dx, cy + dy, 3, fg);
            }
        }
        if arrow {
            let ax = r.x + r.w - 10;
            let ay = r.y + 4;
            for i in 0..4 {
                out.vline(ax + i, ay + i, 7 - 2 * i, fg);
            }
        }
    }
}

/// Built-in equalizer presets: name, preamp and ten bands on the 0..63 scale
/// (0 = +12 dB). Values follow the presets shipped with the classic player
/// as published by the Webamp project (MIT).
pub const BUILTIN_PRESETS: &[(&str, u8, [u8; 10])] = &[
    ("Classical", 31, [31, 31, 31, 31, 31, 31, 44, 44, 44, 48]),
    ("Club", 31, [31, 31, 26, 22, 22, 22, 26, 31, 31, 31]),
    ("Dance", 31, [16, 20, 28, 32, 32, 42, 44, 44, 32, 32]),
    (
        "Laptop speakers/headphones",
        31,
        [24, 14, 23, 38, 36, 29, 24, 16, 11, 8],
    ),
    ("Large hall", 31, [15, 15, 22, 22, 31, 40, 40, 40, 31, 31]),
    ("Party", 31, [20, 20, 31, 31, 31, 31, 31, 31, 20, 20]),
    ("Pop", 31, [35, 24, 20, 19, 23, 34, 36, 36, 35, 35]),
    ("Reggae", 31, [31, 31, 33, 42, 31, 21, 21, 31, 31, 31]),
    ("Rock", 31, [19, 24, 41, 45, 38, 25, 17, 14, 14, 14]),
    ("Soft", 31, [24, 29, 34, 36, 34, 25, 18, 16, 14, 12]),
    ("Ska", 31, [36, 40, 39, 33, 25, 22, 17, 16, 14, 16]),
    ("Full Bass", 31, [16, 16, 16, 22, 29, 39, 46, 49, 50, 50]),
    ("Soft Rock", 31, [25, 25, 28, 33, 39, 41, 38, 33, 27, 17]),
    ("Full Treble", 31, [48, 48, 48, 39, 27, 14, 6, 6, 6, 4]),
    (
        "Full Bass & Treble",
        31,
        [20, 22, 31, 44, 40, 29, 18, 14, 12, 12],
    ),
    ("Live", 31, [40, 31, 25, 23, 22, 22, 25, 27, 27, 28]),
    ("Techno", 31, [19, 22, 31, 41, 40, 31, 19, 16, 16, 17]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mnemonics() {
        assert_eq!(mnemonic("&File..."), ("File...".to_string(), Some(0)));
        assert_eq!(mnemonic("Play&back"), ("Playback".to_string(), Some(4)));
        assert_eq!(mnemonic("A && B"), ("A & B".to_string(), None));
    }

    #[test]
    fn layout_and_hit() {
        let fonts = Fonts::new();
        let items = vec![
            act("Play", "X", Cmd::Play),
            Item::Sep,
            act("Stop", "V", Cmd::Stop),
        ];
        let m = MenuLevel::new(items, &fonts);
        assert_eq!(m.height, 6 + ITEM_H * 2 + SEP_H);
        assert_eq!(m.hit(10, 5), Some(0));
        assert_eq!(m.hit(10, 3 + ITEM_H + 2), None);
        assert_eq!(m.hit(10, 3 + ITEM_H + SEP_H + 2), Some(2));
        let mut b = Bitmap::new(m.width * 2, m.height * 2, 0);
        m.render(&fonts, &mut b, 2);
    }
}
