//! Classic plugin support: input, output, DSP, visualization and general
//! purpose plugins (Windows DLLs), loaded by `ferramp-plughost`, which runs
//! natively on Windows and under Wine on Linux and macOS.
//!
//! `Plugins` starts one host per plugin CPU type found in the plugin
//! folders, keeps the list of what they loaded, routes host events to the
//! streams and features that wait for them, and restarts a host that died.

pub mod host;
pub mod input;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ferramp_plugproto::{
    Command, FileInfo, FromHost, Kind, MenuEntry, PlEntry, PlayerState, PluginInfo, ToHost,
};

use host::{Arch, Host, HostError};

/// Formats the built-in decoder handles; other extensions go to plugins
/// first.
pub const NATIVE_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "mp1", "ogg", "oga", "flac", "wav", "wave", "aif", "aiff", "aifc", "m4a", "m4b",
    "mp4", "aac", "alac", "caf", "mka", "mkv", "webm",
];

#[derive(Clone, Debug)]
pub struct Plugin {
    pub arch: Arch,
    pub path: PathBuf,
    pub info: PluginInfo,
}

impl Plugin {
    /// Lower-case extensions the plugin claims.
    pub fn extensions(&self) -> impl Iterator<Item = String> + '_ {
        self.info
            .extensions
            .iter()
            .flat_map(|(pat, _)| pat.split(';'))
            .map(|e| e.trim().trim_start_matches("*.").to_lowercase())
            .filter(|e| !e.is_empty())
    }
}

/// The running plugin system, for code without access to the app (the tag
/// reader thread, folder scans).
static GLOBAL: RwLock<Option<Arc<Plugins>>> = RwLock::new(None);

pub fn global() -> Option<Arc<Plugins>> {
    GLOBAL.read().unwrap().clone()
}

pub fn set_global(p: Option<Arc<Plugins>>) {
    *GLOBAL.write().unwrap() = p;
}

/// Events for one open input stream.
pub type StreamTx = Sender<FromHost>;

#[derive(Default)]
struct Routes {
    /// Each stream, with the host (by architecture) that decodes it.
    streams: HashMap<u32, (Arch, StreamTx)>,
}

/// What the host event threads share with the rest.
struct Hub {
    routes: Mutex<Routes>,
    commands: Mutex<VecDeque<Command>>,
    vis_open: AtomicBool,
    wake: Arc<dyn Fn() + Send + Sync>,
    log_dir: PathBuf,
    /// When the host last reported loading progress, and the plugin it was
    /// loading then (`None` once it finished).
    progress: Mutex<(Instant, Option<String>)>,
    /// Hosts where a general plugin asked for analyzer data.
    pcm_for: Mutex<Vec<Arch>>,
}

pub struct Plugins {
    dirs: Vec<PathBuf>,
    ini_dir: PathBuf,
    hosts: Mutex<HashMap<Arch, Arc<Host>>>,
    /// Hosts with only the input plugins, for file information: a plugin
    /// that crashes on a question must not stop what is playing.
    info_hosts: Mutex<HashMap<Arch, Arc<Host>>>,
    list: RwLock<Vec<Plugin>>,
    hub: Arc<Hub>,
    /// Plugin files not to load, with the reason.
    skip: Mutex<HashMap<String, String>>,
    /// Host of the active DSP and visualization modules.
    dsp: Mutex<Option<Arch>>,
    vis: Mutex<Option<Arch>>,
    out: Mutex<Option<Arch>>,
    next_stream: AtomicU32,
    errors: Mutex<Vec<String>>,
    /// Plugins whose file information query crashed the host; not asked again.
    no_info: Mutex<Vec<String>>,
}

impl std::fmt::Debug for Plugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Plugins({} loaded)", self.list.read().unwrap().len())
    }
}

impl Plugins {
    /// `wake` is called when a plugin asks the player to do something.
    pub fn new(
        dirs: Vec<PathBuf>,
        ini_dir: PathBuf,
        log_dir: PathBuf,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Plugins {
        Plugins {
            dirs,
            ini_dir,
            hosts: Mutex::new(HashMap::new()),
            info_hosts: Mutex::new(HashMap::new()),
            list: RwLock::new(Vec::new()),
            hub: Arc::new(Hub {
                routes: Mutex::new(Routes::default()),
                commands: Mutex::new(VecDeque::new()),
                vis_open: AtomicBool::new(false),
                wake,
                log_dir,
                progress: Mutex::new((Instant::now(), None)),
                pcm_for: Mutex::new(Vec::new()),
            }),
            skip: Mutex::new(HashMap::new()),
            dsp: Mutex::new(None),
            vis: Mutex::new(None),
            out: Mutex::new(None),
            next_stream: AtomicU32::new(1),
            errors: Mutex::new(Vec::new()),
            no_info: Mutex::new(Vec::new()),
        }
    }

    /// Plugins not to load (user-disabled, or hung before), file name to reason.
    pub fn set_skip(&self, skip: HashMap<String, String>) {
        *self.skip.lock().unwrap() = skip
            .into_iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .collect();
    }

    /// Plugins not to ask for file information (they crashed on it).
    pub fn set_no_info(&self, files: Vec<String>) {
        *self.no_info.lock().unwrap() = files;
    }

    pub fn no_info(&self) -> Vec<String> {
        self.no_info.lock().unwrap().clone()
    }

    pub fn skipped(&self) -> HashMap<String, String> {
        self.skip.lock().unwrap().clone()
    }

    /// Plugin DLLs in the plugin folders, grouped by the CPU they need.
    pub fn plugin_files(&self) -> HashMap<Arch, Vec<PathBuf>> {
        let mut out: HashMap<Arch, Vec<PathBuf>> = HashMap::new();
        for d in &self.dirs {
            let Ok(rd) = std::fs::read_dir(d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if Kind::from_file_name(name).is_none() {
                    continue;
                }
                if let Some(a) = host::dll_arch(&p) {
                    out.entry(a).or_default().push(p);
                }
            }
        }
        out
    }

    /// Starts the hosts and loads every plugin. Blocking; call it from a
    /// background thread. A plugin that hangs while loading is left out
    /// (and remembered in the skip list) and the host is started again.
    pub fn start(&self) {
        let files = self.plugin_files();
        let mut all = Vec::new();
        let mut errors = Vec::new();
        for (arch, paths) in files {
            let mut attempts = 0;
            loop {
                attempts += 1;
                match self
                    .start_host(arch)
                    .map_err(ScanError::Host)
                    .and_then(|h| self.scan(&h))
                {
                    Ok(list) => {
                        for info in list {
                            let path = paths
                                .iter()
                                .find(|p| {
                                    p.file_name()
                                        .and_then(|n| n.to_str())
                                        .is_some_and(|n| n.eq_ignore_ascii_case(&info.file))
                                })
                                .cloned()
                                .unwrap_or_else(|| PathBuf::from(&info.file));
                            all.push(Plugin { arch, path, info });
                        }
                        break;
                    }
                    Err(e) if attempts < 8 && e.blamed().is_some() => {
                        if let Some(h) = self.hosts.lock().unwrap().remove(&arch) {
                            h.kill();
                        }
                        let (file, why) = e.blamed().unwrap();
                        self.skip
                            .lock()
                            .unwrap()
                            .insert(file.to_lowercase(), why.into());
                    }
                    Err(e) => {
                        errors.push(e.to_string());
                        break;
                    }
                }
            }
        }
        // Explain plugins the host skipped.
        let skip = self.skipped();
        for p in &mut all {
            if p.info.error.as_deref() == Some("disabled")
                && let Some(why) = skip.get(&p.info.file.to_lowercase())
            {
                p.info.error = Some(why.clone());
            }
        }
        all.sort_by_key(|p| {
            (
                p.info.kind.map(|k| k as u8).unwrap_or(9),
                p.info.description.to_lowercase(),
            )
        });
        *self.list.write().unwrap() = all;
        *self.errors.lock().unwrap() = errors;
    }

    /// Asks the host to load its plugins, watching its progress.
    fn scan(&self, h: &Host) -> Result<Vec<PluginInfo>, ScanError> {
        *self.hub.progress.lock().unwrap() = (Instant::now(), None);
        let rx = h.request_async(&ToHost::Scan).map_err(ScanError::Host)?;
        let started = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(FromHost::Plugins(list)) => return Ok(list),
                Ok(other) => {
                    return Err(ScanError::Host(HostError::Failed(format!(
                        "unexpected reply {other:?}"
                    ))));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // It crashed: blame the plugin it was loading.
                    let (_, loading) = self.hub.progress.lock().unwrap().clone();
                    return Err(match loading {
                        Some(f) => ScanError::Crashed(f),
                        None => ScanError::Host(HostError::Dead),
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            let (at, loading) = self.hub.progress.lock().unwrap().clone();
            if let Some(f) = loading
                && at.elapsed() > LOAD_TIMEOUT
            {
                return Err(ScanError::Hung(f));
            }
            if started.elapsed() > Duration::from_secs(600) {
                return Err(ScanError::Host(HostError::Timeout));
            }
        }
    }

    fn start_host(&self, arch: Arch) -> Result<Arc<Host>, HostError> {
        self.start_host_kind(arch, false)
    }

    fn start_host_kind(&self, arch: Arch, inputs_only: bool) -> Result<Arc<Host>, HostError> {
        let map = if inputs_only {
            &self.info_hosts
        } else {
            &self.hosts
        };
        if let Some(h) = map.lock().unwrap().get(&arch)
            && h.alive()
        {
            return Ok(h.clone());
        }
        let (tx, rx) = mpsc::channel::<FromHost>();
        let _ = std::fs::create_dir_all(&self.hub.log_dir);
        let log = self.hub.log_dir.join(
            format!(
                "plughost-{arch:?}{}.log",
                if inputs_only { "-info" } else { "" }
            )
            .to_lowercase(),
        );
        // The player's folder, as plugins see it: the one above Plugins.
        let run_dir = self
            .dirs
            .first()
            .and_then(|d| d.parent())
            .map(Path::to_path_buf);
        let host = Arc::new(Host::spawn(arch, Some(&log), run_dir.as_deref(), tx)?);
        let hub = self.hub.clone();
        std::thread::Builder::new()
            .name("plughost-events".into())
            .spawn(move || {
                for ev in rx {
                    if matches!(ev, FromHost::WantPcm) {
                        let mut p = hub.pcm_for.lock().unwrap();
                        if !p.contains(&arch) {
                            p.push(arch);
                        }
                        continue;
                    }
                    dispatch(ev, &hub);
                }
                // A host answering file information questions plays nothing:
                // its crash must not touch what is playing.
                if inputs_only {
                    return;
                }
                // The host is gone: end every stream that was waiting on it.
                let streams: Vec<StreamTx> = hub
                    .routes
                    .lock()
                    .unwrap()
                    .streams
                    .values()
                    .filter(|(a, _)| *a == arch)
                    .map(|(_, tx)| tx.clone())
                    .collect();
                for s in streams {
                    let _ = s.send(FromHost::StreamFailed {
                        stream: 0,
                        msg: "the plugin host stopped".into(),
                    });
                }
                hub.vis_open.store(false, Ordering::SeqCst);
            })
            .map_err(|e| HostError::Failed(e.to_string()))?;
        let dirs: Vec<String> = self
            .dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        let _ = std::fs::create_dir_all(&self.ini_dir);
        host.request(
            &ToHost::Hello {
                protocol: ferramp_plugproto::PROTOCOL,
                ini_dir: self.ini_dir.to_string_lossy().into_owned(),
                plugin_dirs: dirs,
                skip: self.skip.lock().unwrap().keys().cloned().collect(),
                inputs_only,
            },
            // A fresh Wine prefix can take a while to set up.
            Duration::from_secs(120),
        )?;
        map.lock().unwrap().insert(arch, host.clone());
        Ok(host)
    }

    /// The input-only host for file information, started on first use.
    fn info_host(&self, arch: Arch) -> Option<Arc<Host>> {
        if let Some(h) = self.info_hosts.lock().unwrap().get(&arch)
            && h.alive()
        {
            return Some(h.clone());
        }
        let h = self.start_host_kind(arch, true).ok()?;
        self.scan(&h).ok()?;
        Some(h)
    }

    /// The loaded plugin with this file name.
    pub fn by_file(&self, file: &str) -> Option<Plugin> {
        self.list
            .read()
            .unwrap()
            .iter()
            .find(|p| p.info.file.eq_ignore_ascii_case(file))
            .cloned()
    }

    /// Wine (or nothing, on Windows) and the host programs, for the
    /// plugin dialog.
    pub fn runtime_status() -> String {
        let host = host::find_host(Arch::X86).map(|p| p.display().to_string());
        if cfg!(windows) {
            return match host {
                Some(_) => "Plugins run in ferramp-plughost.".into(),
                None => "ferramp-plughost-x86.exe is missing next to ferramp.exe.".into(),
            };
        }
        match (host::find_wine(), host) {
            (Some(w), Some(_)) => format!("Plugins run under Wine ({}).", w.display()),
            (None, _) => "Install Wine to use Windows plugins (wine on the PATH).".into(),
            (_, None) => "The plugin host program is not installed.".into(),
        }
    }

    pub fn list(&self) -> Vec<Plugin> {
        self.list.read().unwrap().clone()
    }

    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    pub fn of_kind(&self, k: Kind) -> Vec<Plugin> {
        self.list()
            .into_iter()
            .filter(|p| p.info.kind == Some(k) && p.info.error.is_none())
            .collect()
    }

    pub fn host(&self, arch: Arch) -> Option<Arc<Host>> {
        let h = self.hosts.lock().unwrap().get(&arch).cloned();
        match h {
            Some(h) if h.alive() => Some(h),
            // Restart a host that died; its plugins are loaded again.
            Some(_) => {
                let h = self.start_host(arch).ok()?;
                self.scan(&h).ok()?;
                Some(h)
            }
            None => None,
        }
    }

    /// The input plugin for `path`: the first one claiming its extension.
    pub fn input_for(&self, path: &Path) -> Option<Plugin> {
        self.inputs_for(path).into_iter().next()
    }

    /// Every input plugin claiming the extension of `path`, in list order;
    /// when one cannot open a file the next one gets a try.
    pub fn inputs_for(&self, path: &Path) -> Vec<Plugin> {
        let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
        else {
            return Vec::new();
        };
        self.of_kind(Kind::Input)
            .into_iter()
            .filter(|p| p.extensions().any(|e| e == ext))
            .collect()
    }

    /// Whether a plugin, rather than the built-in decoder, should open
    /// `path` first.
    pub fn prefers(&self, path: &Path) -> bool {
        let native = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| NATIVE_EXTENSIONS.contains(&e.to_lowercase().as_str()));
        !native && self.input_for(path).is_some()
    }

    /// Extensions any input plugin claims, for file dialogs and folder scans.
    pub fn extensions(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .of_kind(Kind::Input)
            .iter()
            .flat_map(|p| p.extensions().collect::<Vec<_>>())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn file_info(&self, path: &Path) -> Option<FileInfo> {
        let p = self.input_for(path)?;
        if self.no_info.lock().unwrap().contains(&p.info.file) {
            return None;
        }
        let h = self.info_host(p.arch)?;
        match h.request(
            &ToHost::FileInfo {
                plugin: p.info.id,
                path: path.to_string_lossy().into_owned(),
            },
            Duration::from_secs(10),
        ) {
            Ok(FromHost::Info(i)) => Some(i),
            Err(HostError::Dead) | Err(HostError::Timeout) => {
                // Some plugins crash when asked about a file they are not
                // playing; play them, but do not ask again.
                self.no_info.lock().unwrap().push(p.info.file.clone());
                None
            }
            _ => None,
        }
    }

    pub fn info_box(&self, path: &Path) -> bool {
        let Some(p) = self.input_for(path) else {
            return false;
        };
        let Some(h) = self.host(p.arch) else {
            return false;
        };
        h.send(&ToHost::InfoBox {
            plugin: p.info.id,
            path: path.to_string_lossy().into_owned(),
        })
        .is_ok()
    }

    pub fn configure(&self, p: &Plugin, module: u32) {
        if let Some(h) = self.host(p.arch) {
            let _ = h.send(&ToHost::Configure {
                plugin: p.info.id,
                module,
            });
        }
    }

    /// The menu items general plugins added, with the host each lives in.
    pub fn menu_entries(&self) -> Vec<(Arch, MenuEntry)> {
        let with_general: Vec<Arch> = self
            .of_kind(Kind::General)
            .iter()
            .filter(|p| p.info.error.is_none())
            .map(|p| p.arch)
            .collect();
        let hosts: Vec<(Arch, Arc<Host>)> = self
            .hosts
            .lock()
            .unwrap()
            .iter()
            .filter(|(a, _)| with_general.contains(a))
            .map(|(a, h)| (*a, h.clone()))
            .collect();
        let mut out = Vec::new();
        for (arch, h) in hosts {
            if let Ok(FromHost::Menu(items)) =
                h.request(&ToHost::MenuQuery, Duration::from_millis(300))
            {
                out.extend(items.into_iter().map(|e| (arch, e)));
            }
        }
        out
    }

    /// Picks a plugin's menu item.
    pub fn menu_command(&self, arch: Arch, id: u32) {
        if let Some(h) = self.host(arch) {
            let _ = h.send(&ToHost::MenuCommand { id });
        }
    }

    pub fn about(&self, p: &Plugin, module: u32) {
        if let Some(h) = self.host(p.arch) {
            let _ = h.send(&ToHost::About {
                plugin: p.info.id,
                module,
            });
        }
    }

    fn register_stream(&self, arch: Arch, tx: StreamTx) -> u32 {
        let id = self.next_stream.fetch_add(1, Ordering::SeqCst);
        self.hub
            .routes
            .lock()
            .unwrap()
            .streams
            .insert(id, (arch, tx));
        id
    }

    fn unregister_stream(&self, id: u32) {
        self.hub.routes.lock().unwrap().streams.remove(&id);
    }

    // ------------------------------------------------------------------
    // DSP

    /// Starts a DSP module; only one is active at a time.
    pub fn dsp_start(&self, p: &Plugin, module: u32) -> Result<(), String> {
        self.dsp_stop();
        let h = self.host(p.arch).ok_or("the plugin host is not running")?;
        h.request(
            &ToHost::DspStart {
                plugin: p.info.id,
                module,
            },
            Duration::from_secs(10),
        )
        .map_err(|e| e.to_string())?;
        *self.dsp.lock().unwrap() = Some(p.arch);
        Ok(())
    }

    pub fn dsp_stop(&self) {
        if let Some(a) = self.dsp.lock().unwrap().take()
            && let Some(h) = self.hosts.lock().unwrap().get(&a).cloned()
        {
            let _ = h.request(&ToHost::DspStop, Duration::from_secs(5));
        }
    }

    pub fn dsp_active(&self) -> bool {
        self.dsp.lock().unwrap().is_some()
    }

    /// Runs interleaved 16-bit samples through the active DSP module.
    pub fn dsp_process(&self, rate: u32, channels: u32, samples: Vec<i16>) -> Option<Vec<i16>> {
        let a = (*self.dsp.lock().unwrap())?;
        let h = self.hosts.lock().unwrap().get(&a).cloned()?;
        match h.request(
            &ToHost::DspProcess {
                rate,
                channels,
                samples,
            },
            Duration::from_secs(2),
        ) {
            Ok(FromHost::DspOut(v)) => Some(v),
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Output

    /// Opens an output plugin; returns its latency in ms.
    pub fn out_open(&self, p: &Plugin, rate: u32, channels: u32, bits: u32) -> Result<i32, String> {
        let h = self.host(p.arch).ok_or("the plugin host is not running")?;
        match h.request(
            &ToHost::OutOpen {
                plugin: p.info.id,
                rate,
                channels,
                bits,
            },
            Duration::from_secs(10),
        ) {
            Ok(FromHost::OutOpened { latency_ms }) => {
                *self.out.lock().unwrap() = Some(p.arch);
                Ok(latency_ms)
            }
            Ok(other) => Err(format!("unexpected reply {other:?}")),
            Err(e) => Err(e.to_string()),
        }
    }

    fn out_host(&self) -> Option<Arc<Host>> {
        let a = (*self.out.lock().unwrap())?;
        self.hosts
            .lock()
            .unwrap()
            .get(&a)
            .cloned()
            .filter(|h| h.alive())
    }

    pub fn out_write(&self, data: Vec<u8>) -> bool {
        self.out_host()
            .is_some_and(|h| h.send(&ToHost::OutWrite(data)).is_ok())
    }

    /// (bytes it can take, ms played, ms written), or None if it is gone.
    pub fn out_status(&self) -> Option<(u32, u32, u32)> {
        match self
            .out_host()?
            .request(&ToHost::OutQuery, Duration::from_secs(2))
        {
            Ok(FromHost::OutStatus {
                can_write,
                output_ms,
                written_ms,
                ..
            }) => Some((can_write, output_ms, written_ms)),
            _ => None,
        }
    }

    pub fn out_pause(&self, pause: bool) {
        if let Some(h) = self.out_host() {
            let _ = h.send(&ToHost::OutPause(pause));
        }
    }

    pub fn out_flush(&self, ms: u32) {
        if let Some(h) = self.out_host() {
            let _ = h.send(&ToHost::OutFlush(ms));
        }
    }

    pub fn out_close(&self) {
        if let Some(h) = self.out_host() {
            let _ = h.send(&ToHost::OutClose);
        }
        *self.out.lock().unwrap() = None;
    }

    // ------------------------------------------------------------------
    // Visualization

    pub fn vis_start(&self, p: &Plugin, module: u32) {
        self.vis_stop();
        if let Some(h) = self.host(p.arch) {
            self.hub.vis_open.store(true, Ordering::SeqCst);
            *self.vis.lock().unwrap() = Some(p.arch);
            let _ = h.request(
                &ToHost::VisStart {
                    plugin: p.info.id,
                    module,
                },
                Duration::from_secs(10),
            );
        }
    }

    pub fn vis_stop(&self) {
        if let Some(a) = self.vis.lock().unwrap().take()
            && let Some(h) = self.hosts.lock().unwrap().get(&a).cloned()
        {
            let _ = h.request(&ToHost::VisStop, Duration::from_secs(5));
        }
        self.hub.vis_open.store(false, Ordering::SeqCst);
    }

    /// Whether a visualization plugin is running (it may close itself).
    pub fn vis_running(&self) -> bool {
        self.hub.vis_open.load(Ordering::SeqCst)
    }

    /// Whether a host wants the sound being played: for a running
    /// visualization plugin, or for a plugin reading analyzer data.
    pub fn wants_pcm(&self) -> bool {
        self.vis_running() || !self.hub.pcm_for.lock().unwrap().is_empty()
    }

    pub fn vis_pcm(&self, rate: u32, channels: u32, samples: Vec<i16>) {
        let mut targets = self.hub.pcm_for.lock().unwrap().clone();
        if self.vis_running()
            && let Some(a) = *self.vis.lock().unwrap()
            && !targets.contains(&a)
        {
            targets.push(a);
        }
        for a in targets {
            if let Some(h) = self.hosts.lock().unwrap().get(&a).cloned() {
                let _ = h.send(&ToHost::VisPcm {
                    rate,
                    channels,
                    samples: samples.clone(),
                });
            }
        }
    }

    // ------------------------------------------------------------------
    // General purpose plugins

    pub fn has_general(&self) -> bool {
        !self.of_kind(Kind::General).is_empty()
    }

    /// Sends the player state to every host with general purpose plugins.
    pub fn send_state(&self, s: &PlayerState) {
        for h in self.general_hosts() {
            let _ = h.send(&ToHost::State(s.clone()));
        }
    }

    pub fn send_playlist(&self, entries: &[PlEntry]) {
        for h in self.general_hosts() {
            let _ = h.send(&ToHost::Playlist(entries.to_vec()));
        }
    }

    fn general_hosts(&self) -> Vec<Arc<Host>> {
        let archs: Vec<Arch> = {
            let l = self.list.read().unwrap();
            let mut a: Vec<Arch> = l
                .iter()
                .filter(|p| p.info.kind == Some(Kind::General))
                .map(|p| p.arch)
                .collect();
            a.dedup();
            a
        };
        let hosts = self.hosts.lock().unwrap();
        archs
            .iter()
            .filter_map(|a| hosts.get(a).cloned())
            .filter(|h| h.alive())
            .collect()
    }

    /// Requests from plugins since the last call.
    pub fn take_commands(&self) -> Vec<Command> {
        self.hub.commands.lock().unwrap().drain(..).collect()
    }

    pub fn shutdown(&self) {
        self.vis_stop();
        self.dsp_stop();
        let mut hosts: Vec<Arc<Host>> =
            self.hosts.lock().unwrap().drain().map(|(_, h)| h).collect();
        hosts.extend(self.info_hosts.lock().unwrap().drain().map(|(_, h)| h));
        for h in hosts {
            h.shutdown();
        }
    }
}

fn dispatch(ev: FromHost, hub: &Hub) {
    let stream_of = |e: &FromHost| match e {
        FromHost::Opened { stream, .. }
        | FromHost::Pcm { stream, .. }
        | FromHost::Flushed { stream, .. }
        | FromHost::Eof { stream }
        | FromHost::StreamFailed { stream, .. }
        | FromHost::StreamInfo { stream, .. } => Some(*stream),
        _ => None,
    };
    if let Some(id) = stream_of(&ev) {
        let tx = hub
            .routes
            .lock()
            .unwrap()
            .streams
            .get(&id)
            .map(|(_, tx)| tx.clone());
        if let Some(tx) = tx {
            let _ = tx.send(ev);
        }
        return;
    }
    match ev {
        FromHost::Command(c) => {
            hub.commands.lock().unwrap().push_back(c);
            (hub.wake)();
        }
        FromHost::VisClosed => {
            hub.vis_open.store(false, Ordering::SeqCst);
            (hub.wake)();
        }
        FromHost::Log(s) => {
            if let Some(f) = s
                .strip_prefix("loading ")
                .or_else(|| s.strip_prefix("waiting "))
            {
                // "waiting": a plugin shows a dialog and the host still
                // answers messages; give the user time.
                *hub.progress.lock().unwrap() = (Instant::now(), Some(f.to_string()));
            } else if s.starts_with("loaded ") {
                *hub.progress.lock().unwrap() = (Instant::now(), None);
            }
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(hub.log_dir.join("plugins.log"))
            {
                let _ = writeln!(f, "{s}");
            }
        }
        _ => {}
    }
}

/// How long one plugin may take to load before it is left out.
const LOAD_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug)]
enum ScanError {
    /// The host stopped responding while loading this file.
    Hung(String),
    /// The host crashed while loading this file.
    Crashed(String),
    Host(HostError),
}

impl ScanError {
    /// The plugin that took the host down, and how.
    fn blamed(&self) -> Option<(&str, &'static str)> {
        match self {
            ScanError::Hung(f) => Some((f, "stopped responding while loading")),
            ScanError::Crashed(f) => Some((f, "crashed while loading")),
            ScanError::Host(_) => None,
        }
    }
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Host(e) => write!(f, "{e}"),
            _ => {
                let (p, why) = self.blamed().unwrap();
                write!(f, "{p} {why}")
            }
        }
    }
}
