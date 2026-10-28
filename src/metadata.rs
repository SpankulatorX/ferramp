//! Tag and duration reading for playlist entries, done on a worker thread.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use lofty::prelude::*;

#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    pub track_no: Option<u32>,
    pub duration: Option<f64>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

impl TrackInfo {
    /// "Artist - Title", falling back to whichever part exists.
    pub fn display_title(&self) -> Option<String> {
        match (&self.artist, &self.title) {
            (Some(a), Some(t)) if !a.is_empty() && !t.is_empty() => Some(format!("{a} - {t}")),
            (_, Some(t)) if !t.is_empty() => Some(t.clone()),
            _ => None,
        }
    }
}

/// Tags and duration, from the file itself or from the input plugin that
/// plays it.
pub fn read(path: &Path) -> Option<TrackInfo> {
    if let Some(sys) = crate::plugins::global()
        && sys.prefers(path)
    {
        return read_via_plugin(&sys, path).or_else(|| read_tags(path));
    }
    read_tags(path).or_else(|| crate::plugins::global().and_then(|sys| read_via_plugin(&sys, path)))
}

fn read_via_plugin(sys: &crate::plugins::Plugins, path: &Path) -> Option<TrackInfo> {
    let fi = sys.file_info(path)?;
    let tag = |k: &str| {
        fi.tags
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let mut info = TrackInfo {
        artist: tag("artist"),
        title: tag("title"),
        album: tag("album"),
        genre: tag("genre"),
        comment: tag("comment"),
        year: tag("year").and_then(|y| y.get(..4).and_then(|y| y.parse().ok())),
        track_no: tag("track")
            .and_then(|t| t.split('/').next().and_then(|n| n.trim().parse().ok())),
        duration: (fi.length_ms > 0).then(|| fi.length_ms as f64 / 1000.0),
        bitrate: tag("bitrate")
            .and_then(|b| b.parse::<u32>().ok())
            .map(|b| if b > 10_000 { b / 1000 } else { b }),
        ..Default::default()
    };
    // Plugins without extended info give a ready-made title.
    if info.title.is_none() && !fi.title.trim().is_empty() {
        info.title = Some(fi.title.trim().to_string());
    }
    Some(info)
}

fn read_tags(path: &Path) -> Option<TrackInfo> {
    let tagged = lofty::read_from_path(path).ok()?;
    let props = tagged.properties();
    let mut info = TrackInfo {
        duration: Some(props.duration().as_secs_f64()).filter(|d| *d > 0.0),
        bitrate: props.audio_bitrate().or(props.overall_bitrate()),
        sample_rate: props.sample_rate(),
        channels: props.channels(),
        ..Default::default()
    };
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        let clean = |s: Option<std::borrow::Cow<'_, str>>| {
            s.map(|v| v.trim().trim_matches('\0').to_string())
                .filter(|v| !v.is_empty())
        };
        info.artist = clean(tag.artist());
        info.title = clean(tag.title());
        info.album = clean(tag.album());
        info.genre = clean(tag.genre());
        info.comment = clean(tag.comment());
        info.track_no = tag.track();
        info.year = tag.date().map(|d| d.year as u32);
    }
    Some(info)
}

/// Background reader: send (id, path), receive (id, info).
pub struct InfoWorker {
    tx: Sender<(u64, PathBuf)>,
    pub rx: Receiver<(u64, Option<TrackInfo>)>,
}

impl InfoWorker {
    pub fn new(wake: impl Fn() + Send + 'static) -> InfoWorker {
        let (tx, jobs) = mpsc::channel::<(u64, PathBuf)>();
        let (done_tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tag-reader".into())
            .spawn(move || {
                while let Ok((id, path)) = jobs.recv() {
                    let info = read(&path);
                    if done_tx.send((id, info)).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .expect("spawn tag reader");
        InfoWorker { tx, rx }
    }

    pub fn request(&self, id: u64, path: PathBuf) {
        let _ = self.tx.send((id, path));
    }
}

/// File extensions the player lists when adding folders or filtering dialogs.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "mp1", "ogg", "oga", "flac", "wav", "wave", "aif", "aiff", "aifc", "m4a", "m4b",
    "mp4", "aac", "alac", "caf", "mka", "webm",
];

/// Whether a folder scan should add `p`: a built-in format, or one an
/// input plugin plays.
pub fn is_audio_file(p: &Path) -> bool {
    let Some(ext) = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    else {
        return false;
    };
    AUDIO_EXTENSIONS.contains(&ext.as_str())
        || crate::plugins::global().is_some_and(|sys| sys.extensions().contains(&ext))
}

pub fn is_playlist_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "m3u" | "m3u8" | "pls"))
        .unwrap_or(false)
}
