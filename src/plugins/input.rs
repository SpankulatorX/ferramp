//! A track decoded by an input plugin, read by the engine's decoder thread
//! like any other track.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use ferramp_plugproto::{FromHost, StreamFormat, ToHost};

use super::Plugins;
use super::host::Host;
use crate::audio::{Track, TrackInfo};

/// Bytes the host may send ahead of what the decoder has taken.
const WINDOW: u32 = 256 * 1024;

pub struct PluginTrack {
    sys: Arc<Plugins>,
    host: Arc<Host>,
    stream: u32,
    rx: Receiver<FromHost>,
    format: StreamFormat,
    info: TrackInfo,
    /// After a seek, until the host confirms the flush.
    discarding: bool,
    base_ms: u32,
    frames_since_base: u64,
    last_played: Instant,
}

impl PluginTrack {
    pub fn open(sys: &Arc<Plugins>, path: &Path) -> Result<PluginTrack, String> {
        let plugin = sys.input_for(path).ok_or("no input plugin for this file")?;
        let host = sys
            .host(plugin.arch)
            .ok_or("the plugin host is not running")?;
        let (tx, rx) = mpsc::channel();
        let stream = sys.register_stream(tx);
        let fail = |e: String| {
            sys.unregister_stream(stream);
            e
        };
        host.send(&ToHost::Open {
            stream,
            plugin: plugin.info.id,
            path: path.to_string_lossy().into_owned(),
            credit: WINDOW,
        })
        .map_err(|e| fail(e.to_string()))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let format = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(FromHost::Opened { format, .. }) => break format,
                Ok(FromHost::StreamFailed { msg, .. }) => return Err(fail(msg)),
                Ok(_) => {}
                Err(_) => {
                    let _ = host.send(&ToHost::Close { stream });
                    return Err(fail(format!(
                        "{} did not start playing",
                        plugin.info.description
                    )));
                }
            }
        };
        let duration = (format.length_ms > 0).then(|| format.length_ms as f64 / 1000.0);
        let info = TrackInfo {
            rate: format.rate,
            channels: format.channels,
            duration,
            kbps: if format.kbps > 0 {
                format.kbps
            } else {
                format.rate * format.channels * format.bits / 1000
            },
            stream_name: None,
        };
        Ok(PluginTrack {
            sys: sys.clone(),
            host,
            stream,
            rx,
            format,
            info,
            discarding: false,
            base_ms: 0,
            frames_since_base: 0,
            last_played: Instant::now(),
        })
    }

    fn report_played(&mut self) {
        if self.last_played.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.last_played = Instant::now();
        let ms =
            self.base_ms as u64 + self.frames_since_base * 1000 / self.format.rate.max(1) as u64;
        let _ = self.host.send(&ToHost::Played {
            stream: self.stream,
            ms: ms as u32,
        });
    }
}

/// Appends interleaved integer PCM as stereo f32 frames.
pub fn convert(data: &[u8], bits: u32, channels: u32, out: &mut Vec<[f32; 2]>) {
    let bytes = (bits / 8) as usize;
    let ch = channels.max(1) as usize;
    let frame = bytes * ch;
    if bytes == 0 || frame == 0 {
        return;
    }
    let sample = |s: &[u8]| -> f32 {
        match bytes {
            1 => (s[0] as f32 - 128.0) / 128.0,
            2 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
            3 => {
                (((s[2] as i32) << 24 | (s[1] as i32) << 16 | (s[0] as i32) << 8) >> 8) as f32
                    / 8_388_608.0
            }
            _ => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
        }
    };
    for f in data.chunks_exact(frame) {
        let l = sample(&f[..bytes]);
        let r = if ch > 1 {
            sample(&f[bytes..2 * bytes])
        } else {
            l
        };
        out.push([l, r]);
    }
}

impl Track for PluginTrack {
    fn info(&self) -> &TrackInfo {
        &self.info
    }

    fn read(&mut self, frames: &mut Vec<[f32; 2]>) -> Result<bool, String> {
        loop {
            match self.rx.recv_timeout(Duration::from_millis(30)) {
                Ok(FromHost::Pcm { data, .. }) => {
                    if self.discarding {
                        continue;
                    }
                    let before = frames.len();
                    convert(&data, self.format.bits, self.format.channels, frames);
                    self.frames_since_base += (frames.len() - before) as u64;
                    let _ = self.host.send(&ToHost::Credit {
                        stream: self.stream,
                        bytes: data.len() as u32,
                    });
                    self.report_played();
                    return Ok(true);
                }
                Ok(FromHost::Flushed { ms, .. }) => {
                    self.discarding = false;
                    self.base_ms = ms;
                    self.frames_since_base = 0;
                }
                Ok(FromHost::Eof { .. }) if !self.discarding => return Ok(false),
                Ok(FromHost::StreamFailed { msg, .. }) => return Err(msg),
                Ok(FromHost::StreamInfo { kbps, .. }) if kbps > 0 => self.info.kbps = kbps,
                Ok(_) => {}
                // Nothing yet: let the decoder loop look at its commands.
                Err(RecvTimeoutError::Timeout) => return Ok(true),
                Err(RecvTimeoutError::Disconnected) => return Err("the plugin host stopped".into()),
            }
        }
    }

    fn seek(&mut self, secs: f64) -> Option<f64> {
        if !self.format.seekable {
            return None;
        }
        let ms = (secs.max(0.0) * 1000.0) as u32;
        self.host
            .send(&ToHost::Seek {
                stream: self.stream,
                ms,
            })
            .ok()?;
        self.discarding = true;
        Some(ms as f64 / 1000.0)
    }

    fn kbps(&self) -> Option<u32> {
        Some(self.info.kbps)
    }
}

impl Drop for PluginTrack {
    fn drop(&mut self) {
        let _ = self.host.send(&ToHost::Close {
            stream: self.stream,
        });
        self.sys.unregister_stream(self.stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_pcm_formats() {
        let mut out = Vec::new();
        convert(&[0x00, 0x80, 0xFF, 0x7F], 16, 2, &mut out);
        assert_eq!(out, vec![[-1.0, 32767.0 / 32768.0]]);
        out.clear();
        convert(&[128, 255], 8, 1, &mut out);
        assert_eq!(out, vec![[0.0, 0.0], [127.0 / 128.0, 127.0 / 128.0]]);
        out.clear();
        // 24-bit: -1 and +1/2 of full scale.
        convert(&[0x00, 0x00, 0x80, 0x00, 0x00, 0x40], 24, 2, &mut out);
        assert_eq!(out, vec![[-1.0, 0.5]]);
        out.clear();
        // Five channels: the first two are kept, partial frames dropped.
        convert(&[1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 9], 16, 5, &mut out);
        assert_eq!(out.len(), 1);
    }
}
