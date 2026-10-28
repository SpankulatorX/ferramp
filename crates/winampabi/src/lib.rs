//! The classic player plugin interface, written from its public description
//! for interoperability: the module structures plugins export, the message
//! numbers they send to the player window, and the menu command ids they
//! post. All function pointers use the C calling convention (cdecl on x86),
//! which is what the original interface declares.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_int, c_short, c_uchar, c_void};

pub type HWND = *mut c_void;
pub type HINSTANCE = *mut c_void;

// ----------------------------------------------------------------------
// Output plugins (`out_*.dll`, export `winampGetOutModule`)

pub const OUT_VER: c_int = 0x10;

#[repr(C)]
pub struct Out_Module {
    pub version: c_int,
    pub description: *const c_char,
    pub id: isize,
    pub hMainWindow: HWND,
    pub hDllInstance: HINSTANCE,
    pub Config: Option<unsafe extern "C" fn(HWND)>,
    pub About: Option<unsafe extern "C" fn(HWND)>,
    pub Init: Option<unsafe extern "C" fn()>,
    pub Quit: Option<unsafe extern "C" fn()>,
    /// Returns the latency in ms, or a negative value on failure.
    pub Open: Option<
        unsafe extern "C" fn(
            samplerate: c_int,
            numchannels: c_int,
            bitspersamp: c_int,
            bufferlenms: c_int,
            prebufferms: c_int,
        ) -> c_int,
    >,
    pub Close: Option<unsafe extern "C" fn()>,
    /// 0 on success.
    pub Write: Option<unsafe extern "C" fn(buf: *mut c_char, len: c_int) -> c_int>,
    /// Bytes that can be written without blocking.
    pub CanWrite: Option<unsafe extern "C" fn() -> c_int>,
    pub IsPlaying: Option<unsafe extern "C" fn() -> c_int>,
    /// Returns the previous pause state.
    pub Pause: Option<unsafe extern "C" fn(pause: c_int) -> c_int>,
    /// 0..=255; -666 asks for the current value.
    pub SetVolume: Option<unsafe extern "C" fn(volume: c_int)>,
    /// -128..=128.
    pub SetPan: Option<unsafe extern "C" fn(pan: c_int)>,
    /// Discards buffered audio; playback resumes at `t` ms.
    pub Flush: Option<unsafe extern "C" fn(t: c_int)>,
    pub GetOutputTime: Option<unsafe extern "C" fn() -> c_int>,
    pub GetWrittenTime: Option<unsafe extern "C" fn() -> c_int>,
}

pub type GetOutModule = unsafe extern "C" fn() -> *mut Out_Module;

// ----------------------------------------------------------------------
// Input plugins (`in_*.dll`, export `winampGetInModule2`)

pub const IN_VER: c_int = 0x100;
/// Set in `version` when the plugin's file name arguments are UTF-16.
pub const IN_UNICODE: c_int = 0x0F00_0000;
/// Bit 0 of `UsesOutputPlug`: the plugin sends PCM to `outMod`.
pub const IN_USES_OUTPUT_PLUGIN: c_int = 1;
/// Size of the title buffer passed to `GetFileInfo`, in characters.
pub const GETFILEINFO_TITLE_LENGTH: usize = 2048;

/// `in_char` is `char` or `wchar_t` (u16) depending on `IN_UNICODE`; the
/// pointers are declared as `c_void` and cast at the call site.
#[repr(C)]
pub struct In_Module {
    pub version: c_int,
    pub description: *const c_char,
    pub hMainWindow: HWND,
    pub hDllInstance: HINSTANCE,
    /// Pairs of `"ext1;ext2\0Description\0"`, ending with an empty string.
    pub FileExtensions: *const c_char,
    pub is_seekable: c_int,
    pub UsesOutputPlug: c_int,
    pub Config: Option<unsafe extern "C" fn(HWND)>,
    pub About: Option<unsafe extern "C" fn(HWND)>,
    pub Init: Option<unsafe extern "C" fn()>,
    pub Quit: Option<unsafe extern "C" fn()>,
    pub GetFileInfo: Option<
        unsafe extern "C" fn(file: *const c_void, title: *mut c_void, length_ms: *mut c_int),
    >,
    pub InfoBox: Option<unsafe extern "C" fn(file: *const c_void, parent: HWND) -> c_int>,
    pub IsOurFile: Option<unsafe extern "C" fn(file: *const c_void) -> c_int>,
    /// 0 on success, -1 if the file was not found, other values on error.
    pub Play: Option<unsafe extern "C" fn(file: *const c_void) -> c_int>,
    pub Pause: Option<unsafe extern "C" fn()>,
    pub UnPause: Option<unsafe extern "C" fn()>,
    pub IsPaused: Option<unsafe extern "C" fn() -> c_int>,
    pub Stop: Option<unsafe extern "C" fn()>,
    pub GetLength: Option<unsafe extern "C" fn() -> c_int>,
    pub GetOutputTime: Option<unsafe extern "C" fn() -> c_int>,
    pub SetOutputTime: Option<unsafe extern "C" fn(ms: c_int)>,
    pub SetVolume: Option<unsafe extern "C" fn(volume: c_int)>,
    pub SetPan: Option<unsafe extern "C" fn(pan: c_int)>,
    // Filled in by the player before Init:
    pub SAVSAInit: Option<unsafe extern "C" fn(maxlatency_ms: c_int, srate: c_int)>,
    pub SAVSADeInit: Option<unsafe extern "C" fn()>,
    pub SAAddPCMData:
        Option<unsafe extern "C" fn(pcm: *mut c_void, nch: c_int, bps: c_int, timestamp: c_int)>,
    pub SAGetMode: Option<unsafe extern "C" fn() -> c_int>,
    pub SAAdd:
        Option<unsafe extern "C" fn(data: *mut c_void, timestamp: c_int, csa: c_int) -> c_int>,
    pub VSAAddPCMData:
        Option<unsafe extern "C" fn(pcm: *mut c_void, nch: c_int, bps: c_int, timestamp: c_int)>,
    pub VSAGetMode:
        Option<unsafe extern "C" fn(spec_nch: *mut c_int, wave_nch: *mut c_int) -> c_int>,
    pub VSAAdd: Option<unsafe extern "C" fn(data: *mut c_void, timestamp: c_int) -> c_int>,
    pub VSASetInfo: Option<unsafe extern "C" fn(srate: c_int, nch: c_int)>,
    pub dsp_isactive: Option<unsafe extern "C" fn() -> c_int>,
    pub dsp_dosamples: Option<
        unsafe extern "C" fn(
            samples: *mut c_short,
            numsamples: c_int,
            bps: c_int,
            nch: c_int,
            srate: c_int,
        ) -> c_int,
    >,
    // Implemented by the plugin:
    pub EQSet: Option<unsafe extern "C" fn(on: c_int, data: *mut c_char, preamp: c_int)>,
    // Filled in by the player:
    pub SetInfo:
        Option<unsafe extern "C" fn(bitrate: c_int, srate: c_int, stereo: c_int, synched: c_int)>,
    pub outMod: *mut Out_Module,
}

pub type GetInModule = unsafe extern "C" fn() -> *mut In_Module;

/// Optional input plugin exports.
pub type GetExtendedFileInfo = unsafe extern "C" fn(
    file: *const c_char,
    meta: *const c_char,
    ret: *mut c_char,
    retlen: c_int,
) -> c_int;
pub type GetExtendedFileInfoW = unsafe extern "C" fn(
    file: *const u16,
    meta: *const c_char,
    ret: *mut u16,
    retlen: c_int,
) -> c_int;
pub type ExtendedReadOpen = unsafe extern "C" fn(
    file: *const c_char,
    size: *mut c_int,
    bps: *mut c_int,
    nch: *mut c_int,
    srate: *mut c_int,
) -> isize;
pub type ExtendedReadOpenW = unsafe extern "C" fn(
    file: *const u16,
    size: *mut c_int,
    bps: *mut c_int,
    nch: *mut c_int,
    srate: *mut c_int,
) -> isize;
pub type ExtendedReadGetData = unsafe extern "C" fn(
    handle: isize,
    dest: *mut c_char,
    len: c_int,
    killswitch: *mut c_int,
) -> isize;
pub type ExtendedReadSetTime = unsafe extern "C" fn(handle: isize, ms: c_int) -> c_int;
pub type ExtendedReadClose = unsafe extern "C" fn(handle: isize);

// ----------------------------------------------------------------------
// DSP plugins (`dsp_*.dll`, export `winampDSPGetHeader2`)

pub const DSP_HDRVER: c_int = 0x20;

#[repr(C)]
pub struct winampDSPModule {
    pub description: *const c_char,
    pub hwndParent: HWND,
    pub hDllInstance: HINSTANCE,
    pub Config: Option<unsafe extern "C" fn(this: *mut winampDSPModule)>,
    /// 0 on success.
    pub Init: Option<unsafe extern "C" fn(this: *mut winampDSPModule) -> c_int>,
    /// Returns the new sample count; the buffer has room for twice
    /// `numsamples`.
    pub ModifySamples: Option<
        unsafe extern "C" fn(
            this: *mut winampDSPModule,
            samples: *mut c_short,
            numsamples: c_int,
            bps: c_int,
            nch: c_int,
            srate: c_int,
        ) -> c_int,
    >,
    pub Quit: Option<unsafe extern "C" fn(this: *mut winampDSPModule)>,
    pub userData: *mut c_void,
}

#[repr(C)]
pub struct winampDSPHeader {
    pub version: c_int,
    pub description: *const c_char,
    pub getModule: Option<unsafe extern "C" fn(index: c_int) -> *mut winampDSPModule>,
}

/// Newer players pass the main window; older plugins ignore the argument,
/// which is harmless with the caller-cleans-up convention.
pub type GetDspHeader = unsafe extern "C" fn(hwnd: HWND) -> *mut winampDSPHeader;

// ----------------------------------------------------------------------
// Visualization plugins (`vis_*.dll`, export `winampVisGetHeader`)

pub const VIS_HDRVER: c_int = 0x101;

#[repr(C)]
pub struct winampVisModule {
    pub description: *const c_char,
    pub hwndParent: HWND,
    pub hDllInstance: HINSTANCE,
    pub sRate: c_int,
    pub nCh: c_int,
    pub latencyMs: c_int,
    pub delayMs: c_int,
    pub spectrumNch: c_int,
    pub waveformNch: c_int,
    pub spectrumData: [[c_uchar; 576]; 2],
    pub waveformData: [[c_uchar; 576]; 2],
    pub Config: Option<unsafe extern "C" fn(this: *mut winampVisModule)>,
    /// 0 on success.
    pub Init: Option<unsafe extern "C" fn(this: *mut winampVisModule) -> c_int>,
    /// 0 to keep going, non-zero to stop.
    pub Render: Option<unsafe extern "C" fn(this: *mut winampVisModule) -> c_int>,
    pub Quit: Option<unsafe extern "C" fn(this: *mut winampVisModule)>,
    pub userData: *mut c_void,
}

#[repr(C)]
pub struct winampVisHeader {
    pub version: c_int,
    pub description: *const c_char,
    pub getModule: Option<unsafe extern "C" fn(index: c_int) -> *mut winampVisModule>,
}

pub type GetVisHeader = unsafe extern "C" fn(hwnd: HWND) -> *mut winampVisHeader;

// ----------------------------------------------------------------------
// General purpose plugins (`gen_*.dll`, export `winampGetGeneralPurposePlugin`)

pub const GPPHDR_VER: c_int = 0x10;

#[repr(C)]
pub struct winampGeneralPurposePlugin {
    pub version: c_int,
    pub description: *const c_char,
    /// 0 on success.
    pub init: Option<unsafe extern "C" fn() -> c_int>,
    pub config: Option<unsafe extern "C" fn()>,
    pub quit: Option<unsafe extern "C" fn()>,
    pub hwndParent: HWND,
    pub hDllInstance: HINSTANCE,
}

pub type GetGeneralPurposePlugin = unsafe extern "C" fn() -> *mut winampGeneralPurposePlugin;

// ----------------------------------------------------------------------
// Player window messages

pub const WM_USER: u32 = 0x0400;
/// `SendMessage(player, WM_WA_IPC, wparam, IPC_*)`.
pub const WM_WA_IPC: u32 = WM_USER;
/// Posted by input plugins when the track has finished playing.
pub const WM_WA_MPEG_EOF: u32 = WM_USER + 2;
/// The player window's class name, which plugins look up.
pub const PLAYER_CLASS: &str = "Winamp v1.x";

pub mod ipc {
    pub const GETVERSION: isize = 0;
    pub const PLAYFILE: isize = 100;
    pub const DELETE: isize = 101;
    pub const STARTPLAY: isize = 102;
    pub const CHDIR: isize = 103;
    pub const ISPLAYING: isize = 104;
    pub const GETOUTPUTTIME: isize = 105;
    pub const JUMPTOTIME: isize = 106;
    pub const WRITEPLAYLIST: isize = 120;
    pub const SETPLAYLISTPOS: isize = 121;
    pub const SETVOLUME: isize = 122;
    pub const SETPANNING: isize = 123;
    pub const GETLISTLENGTH: isize = 124;
    pub const GETLISTPOS: isize = 125;
    pub const GETINFO: isize = 126;
    pub const GETEQDATA: isize = 127;
    pub const SETEQDATA: isize = 128;
    pub const ADDBOOKMARK: isize = 129;
    pub const RESTARTWINAMP: isize = 135;
    pub const GETSKIN: isize = 201;
    pub const GETPLAYLISTFILE: isize = 211;
    pub const GETPLAYLISTTITLE: isize = 212;
    pub const GETPLAYLISTTITLEW: isize = 213;
    pub const GETPLAYLISTFILEW: isize = 214;
    pub const GETHTTPGETTER: isize = 240;
    pub const INETAVAILABLE: isize = 242;
    pub const UPDTITLE: isize = 243;
    pub const REFRESHPLCACHE: isize = 247;
    pub const GET_SHUFFLE: isize = 250;
    pub const GET_REPEAT: isize = 251;
    pub const SET_SHUFFLE: isize = 252;
    pub const SET_REPEAT: isize = 253;
    pub const ENABLEDISABLE_ALL_WINDOWS: isize = 259;
    pub const GETWND: isize = 260;
    pub const ISWNDVISIBLE: isize = 261;
    pub const GET_HMENU: isize = 281;
    pub const GET_EXTENDED_FILE_INFO: isize = 290;
    pub const GET_BASIC_FILE_INFO: isize = 291;
    pub const GET_EXTLIST: isize = 292;
    pub const INFOBOX: isize = 293;
    pub const FORMAT_TITLE: isize = 297;
    pub const GETUNCOMPRESSINTERFACE: isize = 331;
    pub const GETINIFILE: isize = 334;
    pub const GETINIDIRECTORY: isize = 335;
    pub const GETPLUGINDIRECTORY: isize = 336;
    pub const GETM3UDIRECTORY: isize = 337;
    pub const OPENURLBOX: isize = 360;
    pub const OPENFILEBOX: isize = 362;
    pub const OPENDIRBOX: isize = 363;
    pub const SETDIALOGBOXPARENT: isize = 364;
    pub const GETDIALOGBOXPARENT: isize = 365;
    pub const ISFULLSTOP: isize = 400;
    pub const GET_IVIDEOOUTPUT: isize = 500;
    pub const IS_PLAYING_VIDEO: isize = 501;
    pub const GET_GENSKINBITMAP: isize = 503;
    pub const GET_EMBEDIF: isize = 505;
    pub const CB_MISC: isize = 603;
    pub const SETRATING: isize = 639;
    pub const GETRATING: isize = 640;
    pub const GETREGISTEREDVERSION: isize = 770;
    pub const ISMAINWNDVISIBLE: isize = 900;
    pub const PLAYFILEW: isize = 1100;
    pub const GET_BASIC_FILE_INFOW: isize = 1291;
    pub const GETINIFILEW: isize = 1334;
    pub const GETINIDIRECTORYW: isize = 1335;
    pub const GETPLUGINDIRECTORYW: isize = 1336;
    pub const GETM3UDIRECTORYW: isize = 1338;
    pub const PLAYING_FILE: isize = 3003;
    pub const GET_API_SERVICE: isize = 3025;
    pub const GET_EXTENDED_FILE_INFOW: isize = 3026;
    pub const GET_PLAYING_FILENAME: isize = 3031;
    pub const GET_PLAYING_TITLE: isize = 3034;
    pub const PLAYING_FILEW: isize = 13003;
    /// Returns a message number unique to the string in wparam.
    pub const REGISTER_WINAMP_IPCMESSAGE: isize = 65536;

    // IPC_CB_MISC wparam values, sent to the player window on changes.
    pub const CB_MISC_TITLE: usize = 0;
    pub const CB_MISC_VOLUME: usize = 1;
    pub const CB_MISC_STATUS: usize = 2;
    pub const CB_MISC_EQ: usize = 3;
    pub const CB_MISC_INFO: usize = 4;
}

/// `WM_COMMAND` ids plugins post to the player window.
pub mod cmd {
    pub const FILE_QUIT: u16 = 40001;
    pub const OPTIONS_PREFS: u16 = 40012;
    pub const FILE_REPEAT: u16 = 40022;
    pub const FILE_SHUFFLE: u16 = 40023;
    pub const FILE_PLAY: u16 = 40029;
    pub const BUTTON1: u16 = 40044; // previous
    pub const BUTTON2: u16 = 40045; // play
    pub const BUTTON3: u16 = 40046; // pause
    pub const BUTTON4: u16 = 40047; // stop
    pub const BUTTON5: u16 = 40048; // next
    pub const VOLUMEUP: u16 = 40058;
    pub const VOLUMEDOWN: u16 = 40059;
    pub const FFWD5S: u16 = 40060;
    pub const REW5S: u16 = 40061;
    pub const BUTTON4_SHIFT: u16 = 40147; // fade out and stop
}

/// What `IPC_GET_EMBEDIF` hands out: a function that creates a frame
/// window for a plugin's own window.
#[repr(C)]
pub struct embedWindowState {
    pub me: HWND,
    pub flags: c_int,
    pub r: [c_int; 4],
    pub user_ptr: *mut c_void,
    pub extra_data: [c_int; 64],
}

pub type EmbedWindow = unsafe extern "C" fn(state: *mut embedWindowState) -> HWND;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// Offsets as they are on 32-bit x86, where pointers are 4 bytes.
    #[test]
    #[cfg(target_pointer_width = "32")]
    fn layout_32() {
        assert_eq!(offset_of!(In_Module, outMod), 37 * 4);
        assert_eq!(size_of::<Out_Module>(), 20 * 4);
        assert_eq!(offset_of!(winampVisModule, spectrumData), 9 * 4);
        assert_eq!(offset_of!(winampVisModule, Config), 9 * 4 + 4 * 576);
        assert_eq!(size_of::<winampGeneralPurposePlugin>(), 7 * 4);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn layout_64() {
        // Three ints (one padded) and 35 pointer-sized fields.
        assert_eq!(size_of::<In_Module>(), 296);
        assert_eq!(offset_of!(In_Module, outMod), 288);
        assert_eq!(offset_of!(winampDSPModule, userData), 7 * 8);
    }
}
