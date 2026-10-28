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
use std::time::Duration;

use ferramp_plugproto::{
    Command, FileInfo, FromHost, Kind, PlEntry, PlayerState, PluginInfo, ToHost,
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
    pub fn kind(&self) -> Option<Kind> {
        self.info.kind
    }

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

/// Events for one open input stream.
pub type StreamTx = Sender<FromHost>;

#[derive(Default)]
struct Routes {
    streams: HashMap<u32, StreamTx>,
}

pub struct Plugins {
    dirs: Vec<PathBuf>,
    ini_dir: PathBuf,
    log_dir: PathBuf,
    hosts: Mutex<HashMap<Arch, Arc<Host>>>,
    list: RwLock<Vec<Plugin>>,
    routes: Arc<Mutex<Routes>>,
    commands: Arc<Mutex<VecDeque<Command>>>,
    vis_open: Arc<AtomicBool>,
    /// Host of the active DSP and visualization modules.
    dsp: Mutex<Option<Arch>>,
    vis: Mutex<Option<Arch>>,
    next_stream: AtomicU32,
    errors: Mutex<Vec<String>>,
    wake: Arc<dyn Fn() + Send + Sync>,
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
            log_dir,
            hosts: Mutex::new(HashMap::new()),
            list: RwLock::new(Vec::new()),
            routes: Arc::new(Mutex::new(Routes::default())),
            commands: Arc::new(Mutex::new(VecDeque::new())),
            vis_open: Arc::new(AtomicBool::new(false)),
            dsp: Mutex::new(None),
            vis: Mutex::new(None),
            next_stream: AtomicU32::new(1),
            errors: Mutex::new(Vec::new()),
            wake,
        }
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
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
    /// background thread.
    pub fn start(&self) {
        let files = self.plugin_files();
        let mut all = Vec::new();
        let mut errors = Vec::new();
        for (arch, paths) in files {
            match self.start_host(arch) {
                Ok(h) => match h.request(&ToHost::Scan, Duration::from_secs(120)) {
                    Ok(FromHost::Plugins(list)) => {
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
                    }
                    Ok(other) => errors.push(format!("unexpected reply {other:?}")),
                    Err(e) => errors.push(e.to_string()),
                },
                Err(e) => errors.push(e.to_string()),
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

    fn start_host(&self, arch: Arch) -> Result<Arc<Host>, HostError> {
        if let Some(h) = self.hosts.lock().unwrap().get(&arch)
            && h.alive()
        {
            return Ok(h.clone());
        }
        let (tx, rx) = mpsc::channel::<FromHost>();
        let _ = std::fs::create_dir_all(&self.log_dir);
        let log = self
            .log_dir
            .join(format!("plughost-{arch:?}.log").to_lowercase());
        let host = Arc::new(Host::spawn(arch, Some(&log), tx)?);
        let (routes, commands, vis_open, wake) = (
            self.routes.clone(),
            self.commands.clone(),
            self.vis_open.clone(),
            self.wake.clone(),
        );
        let log_dir = self.log_dir.clone();
        std::thread::Builder::new()
            .name("plughost-events".into())
            .spawn(move || {
                for ev in rx {
                    dispatch(ev, &routes, &commands, &vis_open, &*wake, &log_dir);
                }
                // The host is gone: end every stream that was waiting on it.
                let streams: Vec<StreamTx> =
                    routes.lock().unwrap().streams.values().cloned().collect();
                for s in streams {
                    let _ = s.send(FromHost::StreamFailed {
                        stream: 0,
                        msg: "the plugin host stopped".into(),
                    });
                }
                vis_open.store(false, Ordering::SeqCst);
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
            },
            // A fresh Wine prefix can take a while to set up.
            Duration::from_secs(120),
        )?;
        self.hosts.lock().unwrap().insert(arch, host.clone());
        Ok(host)
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
                h.request(&ToHost::Scan, Duration::from_secs(120)).ok()?;
                Some(h)
            }
            None => None,
        }
    }

    /// The input plugin for `path`: the one claiming its extension.
    pub fn input_for(&self, path: &Path) -> Option<Plugin> {
        let ext = path.extension()?.to_str()?.to_lowercase();
        self.of_kind(Kind::Input)
            .into_iter()
            .find(|p| p.extensions().any(|e| e == ext))
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
        let h = self.host(p.arch)?;
        match h.request(
            &ToHost::FileInfo {
                plugin: p.info.id,
                path: path.to_string_lossy().into_owned(),
            },
            Duration::from_secs(10),
        ) {
            Ok(FromHost::Info(i)) => Some(i),
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

    pub fn about(&self, p: &Plugin, module: u32) {
        if let Some(h) = self.host(p.arch) {
            let _ = h.send(&ToHost::About {
                plugin: p.info.id,
                module,
            });
        }
    }

    fn register_stream(&self, tx: StreamTx) -> u32 {
        let id = self.next_stream.fetch_add(1, Ordering::SeqCst);
        self.routes.lock().unwrap().streams.insert(id, tx);
        id
    }

    fn unregister_stream(&self, id: u32) {
        self.routes.lock().unwrap().streams.remove(&id);
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
    // Visualization

    pub fn vis_start(&self, p: &Plugin, module: u32) {
        self.vis_stop();
        if let Some(h) = self.host(p.arch) {
            self.vis_open.store(true, Ordering::SeqCst);
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
        self.vis_open.store(false, Ordering::SeqCst);
    }

    /// Whether a visualization plugin is running (it may close itself).
    pub fn vis_running(&self) -> bool {
        self.vis_open.load(Ordering::SeqCst)
    }

    pub fn vis_pcm(&self, rate: u32, channels: u32, samples: Vec<i16>) {
        if !self.vis_running() {
            return;
        }
        let a = *self.vis.lock().unwrap();
        if let Some(a) = a
            && let Some(h) = self.hosts.lock().unwrap().get(&a).cloned()
        {
            let _ = h.send(&ToHost::VisPcm {
                rate,
                channels,
                samples,
            });
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
        self.commands.lock().unwrap().drain(..).collect()
    }

    pub fn shutdown(&self) {
        self.vis_stop();
        self.dsp_stop();
        let hosts: Vec<Arc<Host>> = self.hosts.lock().unwrap().drain().map(|(_, h)| h).collect();
        for h in hosts {
            h.shutdown();
        }
    }
}

fn dispatch(
    ev: FromHost,
    routes: &Mutex<Routes>,
    commands: &Mutex<VecDeque<Command>>,
    vis_open: &AtomicBool,
    wake: &(dyn Fn() + Send + Sync),
    log_dir: &Path,
) {
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
        let tx = routes.lock().unwrap().streams.get(&id).cloned();
        if let Some(tx) = tx {
            let _ = tx.send(ev);
        }
        return;
    }
    match ev {
        FromHost::Command(c) => {
            commands.lock().unwrap().push_back(c);
            wake();
        }
        FromHost::VisClosed => {
            vis_open.store(false, Ordering::SeqCst);
            wake();
        }
        FromHost::Log(s) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("plugins.log"))
            {
                let _ = writeln!(f, "{s}");
            }
        }
        _ => {}
    }
}
