//! The three classic windows (main, equalizer, playlist editor), the
//! visualiser and the classic pop-up menus.

pub mod dialogs;
pub mod eq_win;
pub mod main_win;
pub mod menu;
pub mod pl_win;
pub mod vis;

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WinKind {
    Main,
    Eq,
    Playlist,
}

impl WinKind {
    pub const ALL: [WinKind; 3] = [WinKind::Main, WinKind::Eq, WinKind::Playlist];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Mouse input in window (skin pixel) coordinates.
#[derive(Clone, Copy, Debug)]
pub enum MouseEvent {
    Down {
        x: i32,
        y: i32,
        button: Button,
        double: bool,
        mods: Mods,
    },
    Move {
        x: i32,
        y: i32,
    },
    Up {
        x: i32,
        y: i32,
        button: Button,
    },
    Wheel {
        delta: i32,
    },
}

/// Which pop-up menu to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuId {
    Main,
    Options,
    Playback,
    Vis,
    EqPresets,
    PlaylistRemove,
    PlaylistContext,
    PlaylistSort,
    PlaylistMiscOptions,
    Skins,
    SongTitle,
    Time,
}

/// Buttons of the plugin dialog, applied to the selected row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginAction {
    Configure,
    About,
    /// Start or stop a DSP, visualization or output plugin.
    Use,
    /// Load the plugin at start, or not.
    Enable,
    Folder,
    Rescan,
}

/// Commands: everything the user can do, from any window, menu or key.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Play,
    Pause,
    Stop,
    StopFadeout,
    StopAfterCurrent,
    Next,
    Previous,
    NextN(i32),
    StartOfList,
    Eject,
    OpenFiles,
    OpenUrl,
    OpenFolder,
    PlayIndex(usize),
    /// Play a URL or path now (true) or add it to the playlist (false).
    OpenLocation(String, bool),
    /// Put a playlist entry in the play queue.
    Enqueue(usize),
    SeekTo(f64),
    SeekBy(f64),
    SetVolume(u8),
    VolumeBy(i32),
    SetBalance(i8),
    ToggleShuffle,
    ToggleRepeat,
    ToggleTimeMode,
    ToggleDouble,
    ToggleAlwaysOnTop,
    ToggleEasyMove,
    ToggleAutoscroll,
    CycleVis,
    SetVisMode(vis::VisMode),
    SetVisOptions(vis::VisOptions),
    VisOptionsDialog,
    ToggleVisPlugin,
    GetMoreSkins,
    /// Offer to download the original base skin.
    ClassicSkin,
    ClassicSkinDownload,
    /// Winamp Skin Museum (opened by GetMoreSkins): search (empty =
    /// browse), next page, preview, download, show on the website.
    MuseumSearch(String),
    MuseumMore,
    MuseumSelect(usize),
    MuseumDownload(usize),
    MuseumWeb,
    /// Classic plugins: the plugin dialog, the selected visualization
    /// plugin's configuration, a row picked in the dialog, a button there.
    PluginsDialog,
    VisPluginConfig,
    PluginSelect(usize),
    PluginAction(PluginAction, usize),
    AddBookmark,
    BookmarkSelected,
    EditBookmarks,
    PlayBookmark(usize),
    PlEditEntry,
    EqLibrary(EqLibOp),
    /// Apply a library operation to the named preset (from the preset dialog).
    EqLibCommit(EqLibOp, String),
    RemoveBookmark(usize),
    PlSetLocation(String),
    PlReadInfo,
    PlGenerateHtml,
    ToggleWindow(WinKind),
    CloseWindow(WinKind),
    ToggleShade(WinKind),
    Minimize,
    Exit,
    About,
    JumpToFile,
    JumpToTime,
    FileInfo,
    Preferences,
    SkinBrowser,
    LoadSkinDialog,
    LoadSkin(Option<PathBuf>),
    ShowMenu(MenuId, i32, i32, WinKind),
    // Equalizer
    EqToggleOn,
    EqToggleAuto,
    EqSetBand(usize, u8),
    EqSetPreamp(u8),
    EqAll(u8),
    EqLoadEqf,
    EqSaveEqf,
    // Playlist editor
    PlAddFiles,
    PlAddFolder,
    PlAddUrl,
    PlRemoveSelected,
    PlCrop,
    PlClear,
    PlRemoveDead,
    PlSelectAll,
    PlSelectNone,
    PlInvertSelection,
    PlSortTitle,
    PlSortFilename,
    PlSortPath,
    PlReverse,
    PlRandomize,
    PlNew,
    PlSave,
    PlLoad,
    PlScrollBy(i32),
    PlPlaySelected,
}

/// Operations on the equalizer preset library (named presets) and the
/// per-file auto-load presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqLibOp {
    LoadPreset,
    LoadAuto,
    LoadDefault,
    SavePreset,
    SaveAuto,
    SaveDefault,
    DeletePreset,
    DeleteAuto,
}

/// Requests from a window to the window manager.
#[derive(Clone, Debug, PartialEq)]
pub enum WmRequest {
    /// Start moving this window (and windows docked to it) with the mouse.
    BeginMove,
    /// Start resizing the playlist with the mouse.
    BeginResize,
}

/// Text shown for a pressed or dragged control, overriding the song title.
pub fn balance_text(b: i8) -> String {
    if b == 0 {
        "Balance: Center".into()
    } else {
        format!(
            "Balance: {}% {}",
            b.unsigned_abs(),
            if b < 0 { "Left" } else { "Right" }
        )
    }
}

pub fn volume_text(v: u8) -> String {
    format!("Volume: {v}%")
}

pub fn eq_text(band: Option<usize>, value: u8) -> String {
    let db = crate::audio::dsp::slider_to_db(value);
    let label = match band {
        None => "PREAMP".to_string(),
        Some(i) => {
            let hz = crate::audio::dsp::EQ_FREQS[i];
            if hz < 1000.0 {
                format!("{}HZ", hz as u32)
            } else {
                format!("{}KHZ", hz as u32 / 1000)
            }
        }
    };
    let rounded = (db * 10.0).round() / 10.0;
    let sign = if rounded >= 0.0 { "+" } else { "" };
    format!("EQ: {label}: {sign}{rounded:.1} DB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marquee_texts() {
        assert_eq!(balance_text(0), "Balance: Center");
        assert_eq!(balance_text(-40), "Balance: 40% Left");
        assert_eq!(volume_text(54), "Volume: 54%");
        assert_eq!(eq_text(Some(0), 0), "EQ: 60HZ: +12.0 DB");
        assert_eq!(eq_text(Some(5), 63), "EQ: 3KHZ: -12.0 DB");
        assert_eq!(eq_text(None, 31), "EQ: PREAMP: +0.0 DB");
    }
}
