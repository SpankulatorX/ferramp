//! End-to-end tests with the test plugins (`crates/testplugins`) in a real
//! host process, under Wine on Linux and macOS. Build the host and the
//! plugins for Windows first, then:
//!
//! `cargo test -- --ignored plugins::tests`
//!
//! `FERRAMP_TEST_PLUGIN_DLL` points at the test plugin DLL if it is not in
//! the usual target folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferramp_plugproto::{Command, Kind, PlEntry, PlayerState};

use super::Plugins;
use super::input::PluginTrack;
use crate::audio::Track;

fn test_dll() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FERRAMP_TEST_PLUGIN_DLL") {
        return Some(p.into());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    ["i686-pc-windows-gnu", "i686-pc-windows-msvc"]
        .iter()
        .flat_map(|t| {
            ["release", "debug"].map(|p| root.join(t).join(p).join("ferramp_testplugins.dll"))
        })
        .find(|p| p.is_file())
}

/// A Windows path for the plugins' log files (they run inside Wine).
fn win_path(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if cfg!(windows) {
        s
    } else {
        format!("Z:{}", s.replace('/', "\\"))
    }
}

fn read_log(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Reads a whole track; returns the frames.
fn read_all(t: &mut PluginTrack) -> Vec<[f32; 2]> {
    let mut frames = Vec::new();
    let start = Instant::now();
    while t.read(&mut frames).unwrap() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "track never ended"
        );
    }
    frames
}

#[test]
#[ignore]
fn all_plugin_kinds_end_to_end() {
    let Some(dll) = test_dll() else {
        eprintln!("test plugin DLL not built; skipping");
        return;
    };
    let base = std::env::temp_dir().join(format!("ferramp-plugtest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let plugdir = base.join("Plugins");
    let logs = base.join("logs");
    std::fs::create_dir_all(&plugdir).unwrap();
    std::fs::create_dir_all(&logs).unwrap();
    for name in [
        "in_ferramptest.dll",
        "out_ferramptest.dll",
        "dsp_ferramptest.dll",
        "vis_ferramptest.dll",
        "gen_ferramptest.dll",
    ] {
        std::fs::copy(&dll, plugdir.join(name)).unwrap();
    }
    let tone = base.join("a.fttone");
    std::fs::write(&tone, "freq=1000 seconds=1.5 rate=48000 channels=2").unwrap();
    let tone_classic = base.join("b-noxr.fttone");
    std::fs::write(&tone_classic, "freq=500 seconds=1 rate=44100 channels=1").unwrap();
    // SAFETY: set before the host process (which inherits it) starts.
    unsafe {
        std::env::set_var("FERRAMP_TEST_DIR", win_path(&logs));
        std::env::set_var("FERRAMP_TEST_GEN_CMD", "next");
    }
    let sys = Arc::new(Plugins::new(
        vec![plugdir.clone()],
        base.join("ini"),
        base.join("hostlogs"),
        Arc::new(|| {}),
    ));
    sys.start();
    let list = sys.list();
    assert!(sys.errors().is_empty(), "host errors: {:?}", sys.errors());
    let kinds: Vec<Kind> = list.iter().filter_map(|p| p.info.kind).collect();
    for k in [
        Kind::Input,
        Kind::Output,
        Kind::Dsp,
        Kind::Vis,
        Kind::General,
    ] {
        assert!(kinds.contains(&k), "{k:?} plugin missing: {list:?}");
    }
    for p in &list {
        assert!(
            p.info.error.is_none(),
            "{} failed: {:?}",
            p.info.file,
            p.info.error
        );
    }
    let input = list
        .iter()
        .find(|p| p.info.kind == Some(Kind::Input))
        .unwrap();
    assert_eq!(input.info.description, "Ferramp Test Input v1");
    assert!(input.info.extended_read);
    assert!(read_log(&logs, "in.log").contains("init callbacks=true"));
    let dsp = list
        .iter()
        .find(|p| p.info.kind == Some(Kind::Dsp))
        .unwrap()
        .clone();
    assert_eq!(
        dsp.info.modules,
        vec!["Invert samples", "Double every frame"]
    );

    // Input through the pull decoder, with metadata and a seek.
    assert!(sys.prefers(&tone));
    let info = sys.file_info(&tone).expect("file info");
    assert_eq!(info.length_ms, 1500);
    assert!(
        info.tags
            .iter()
            .any(|(k, v)| k == "artist" && v == "Ferramp")
    );
    let mut t = PluginTrack::open(&sys, &tone).expect("open pull");
    assert_eq!((t.info().rate, t.info().channels), (48000, 2));
    assert_eq!(t.info().duration, Some(1.5));
    let frames = read_all(&mut t);
    assert_eq!(frames.len(), 72000);
    let peak = frames.iter().map(|f| f[0].abs()).fold(0.0f32, f32::max);
    assert!((0.3..0.4).contains(&peak), "peak {peak}");
    assert_eq!(t.seek(1.0), Some(1.0));
    let after = read_all(&mut t);
    assert_eq!(after.len(), 24000, "frames after seeking to 1.0 s");
    drop(t);

    // Input through the classic path (the plugin's own thread and our
    // output module), mono 44.1 kHz.
    let mut t = PluginTrack::open(&sys, &tone_classic).expect("open classic");
    assert_eq!((t.info().rate, t.info().channels), (44100, 1));
    let frames = read_all(&mut t);
    assert_eq!(frames.len(), 44100);
    assert!(
        frames.iter().all(|f| f[0] == f[1]),
        "mono is duplicated to both sides"
    );
    drop(t);
    wait_for("classic play log", Duration::from_secs(5), || {
        read_log(&logs, "in.log").contains("play ")
    });

    // DSP: invert, then double.
    sys.dsp_start(&dsp, 0).unwrap();
    assert!(sys.dsp_active());
    let out = sys
        .dsp_process(44100, 2, vec![100, -200, 300, -400])
        .unwrap();
    assert_eq!(out, vec![-100, 200, -300, 400]);
    sys.dsp_start(&dsp, 1).unwrap();
    let out = sys.dsp_process(44100, 2, vec![1, 2, 3, 4]).unwrap();
    assert_eq!(out, vec![1, 2, 1, 2, 3, 4, 3, 4]);
    sys.dsp_stop();
    assert!(!sys.dsp_active());
    assert!(read_log(&logs, "dsp.log").contains("parent=true"));

    // Visualization: the module renders with the PCM we feed it.
    let vis = list
        .iter()
        .find(|p| p.info.kind == Some(Kind::Vis))
        .unwrap()
        .clone();
    sys.vis_start(&vis, 0);
    let sine: Vec<i16> = (0..2048)
        .flat_map(|i| {
            let v = ((i as f32 / 44100.0 * 1000.0 * std::f32::consts::TAU).sin() * 20000.0) as i16;
            [v, v]
        })
        .collect();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3)
        && !read_log(&logs, "vis.log").contains("render 40 ")
    {
        sys.vis_pcm(44100, 2, sine.clone());
        std::thread::sleep(Duration::from_millis(30));
    }
    let vlog = read_log(&logs, "vis.log");
    assert!(vlog.contains("init parent=true"), "{vlog}");
    let line = vlog
        .lines()
        .rfind(|l| l.starts_with("render"))
        .expect("renders");
    let spec: u64 = line
        .split("spectrum=")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let wave: u64 = line.split("wave=").nth(1).unwrap().trim().parse().unwrap();
    // A sine at 61 % of full scale peaks at 0.61 * 127 = 77 in the 8-bit waveform.
    assert!(spec > 1000 && (70..=85).contains(&wave), "{line}");
    sys.vis_stop();
    wait_for("vis quit", Duration::from_secs(5), || {
        read_log(&logs, "vis.log").contains("quit renders=")
    });

    // General purpose: it sees the state and playlist we send, and its
    // "Next" press comes back as a command.
    sys.send_playlist(&[
        PlEntry {
            file: tone.to_string_lossy().into_owned(),
            title: "Tone A".into(),
            length_s: 1,
        },
        PlEntry {
            file: "/music/b.mp3".into(),
            title: "Song B".into(),
            length_s: 200,
        },
    ]);
    sys.send_state(&PlayerState {
        status: 1,
        position_ms: 1234,
        length_ms: 200_000,
        volume: 200,
        playlist_pos: 1,
        shuffle: true,
        rate: 44100,
        kbps: 320,
        channels: 2,
        eq: vec![31; 12],
        ..Default::default()
    });
    wait_for(
        "general plugin to see the state",
        Duration::from_secs(5),
        || {
            read_log(&logs, "gen.log")
                .contains("status=1 pos_ms=1234 len_s=200 list=1/2 vol=200 shuffle=1")
        },
    );
    let glog = read_log(&logs, "gen.log");
    assert!(
        glog.contains("init parent=true ini=Z:") || glog.contains("init parent=true ini="),
        "{glog}"
    );
    assert!(glog.contains("title=Song B"), "{glog}");
    assert!(
        glog.contains("file=Z:\\music\\b.mp3") || cfg!(windows),
        "{glog}"
    );
    assert!(
        glog.contains("playing_file"),
        "playing notification: {glog}"
    );
    wait_for("Next command", Duration::from_secs(5), || {
        sys.take_commands().contains(&Command::Next)
    });

    // Output: the engine plays a track through the test output plugin.
    // SAFETY: no other thread reads the environment here.
    unsafe { std::env::set_var("FERRAMP_NULL_AUDIO", "1") };
    let mut engine = crate::audio::Engine::new();
    engine.set_plugins(Some(sys.clone()));
    let out = list
        .iter()
        .find(|p| p.info.kind == Some(Kind::Output))
        .unwrap()
        .clone();
    engine
        .set_output(Some((sys.clone(), out)))
        .expect("open output plugin");
    engine.play(crate::audio::Source::File(tone.clone()));
    wait_for(
        "the track to play through the output plugin",
        Duration::from_secs(20),
        || engine.finished(),
    );
    let olog = read_log(&logs, "out.log");
    assert!(olog.contains("open 44100 2 16"), "{olog}");
    // 1.5 s at 44.1 kHz, 16-bit stereo, plus the resampler's tail.
    let raw = std::fs::metadata(logs.join("out.raw"))
        .map(|m| m.len())
        .unwrap_or(0);
    assert!((250_000..290_000).contains(&raw), "wrote {raw} bytes");
    engine.set_output(None).unwrap();
    drop(engine);

    sys.shutdown();
    let glog = read_log(&logs, "gen.log");
    assert!(glog.contains("quit"), "{glog}");
    let _ = std::fs::remove_dir_all(&base);
}

/// Loads real plugins from `$FERRAMP_REAL_PLUGIN_DIR` and prints what each
/// reported; with `$FERRAMP_REAL_PLUGIN_FILES` (paths separated by `|`),
/// also plays a few seconds of each file through its input plugin.
#[test]
#[ignore]
fn real_plugins_report() {
    let Some(dir) = std::env::var_os("FERRAMP_REAL_PLUGIN_DIR").map(PathBuf::from) else {
        return;
    };
    let base = std::env::temp_dir().join(format!("ferramp-realplug-{}", std::process::id()));
    let sys = Arc::new(Plugins::new(
        vec![dir],
        base.join("ini"),
        base.join("logs"),
        Arc::new(|| {}),
    ));
    let t0 = Instant::now();
    sys.start();
    println!(
        "started in {:.1} s; host errors: {:?}",
        t0.elapsed().as_secs_f32(),
        sys.errors()
    );
    for p in sys.list() {
        println!(
            "{:<8} {:<22} {:?} | {} | modules {:?} | ext {} | xr={} uni={} | error {:?}",
            format!("{:?}", p.arch),
            p.info.file,
            p.info.kind.map(|k| k.label()),
            p.info.description,
            p.info.modules,
            p.extensions().collect::<Vec<_>>().join(","),
            p.info.extended_read,
            p.info.unicode,
            p.info.error
        );
    }
    let files = std::env::var("FERRAMP_REAL_PLUGIN_FILES").unwrap_or_default();
    for f in files.split('|').filter(|f| !f.is_empty()) {
        let path = Path::new(f);
        let who = sys.input_for(path).map(|p| p.info.file).unwrap_or_default();
        let info = sys.file_info(path);
        match PluginTrack::open(&sys, path) {
            Ok(mut t) => {
                let mut frames = Vec::new();
                let start = Instant::now();
                while frames.len() < t.info().rate as usize * 3
                    && start.elapsed() < Duration::from_secs(15)
                {
                    if !t.read(&mut frames).unwrap_or(false) {
                        break;
                    }
                }
                let peak = frames
                    .iter()
                    .map(|f| f[0].abs().max(f[1].abs()))
                    .fold(0.0f32, f32::max);
                println!(
                    "PLAY {} via {who}: {} Hz {} ch, {:?} s, {} frames in {:.1} s, peak {peak:.2}, info {:?}",
                    path.display(),
                    t.info().rate,
                    t.info().channels,
                    t.info().duration,
                    frames.len(),
                    start.elapsed().as_secs_f32(),
                    info.map(|i| (i.title, i.length_ms, i.tags))
                );
            }
            Err(e) => println!("PLAY {} via {who}: FAILED {e}", path.display()),
        }
    }
    sys.shutdown();
}
