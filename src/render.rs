//! Headless rendering of the windows to PNG files, for skin previews and
//! pixel comparisons against reference screenshots.

use std::path::{Path, PathBuf};

use crate::audio::{Engine, Source, Status};
use crate::gfx::Bitmap;
use crate::skin::Skin;
use crate::state::State;
use crate::ui::WinKind;
use crate::ui::eq_win::EqWin;
use crate::ui::main_win::MainWin;
use crate::ui::pl_win::PlWin;

fn save(b: &Bitmap, path: &Path) {
    if let Some(img) = image::RgbaImage::from_raw(b.w as u32, b.h as u32, b.to_rgba8())
        && let Err(e) = img.save(path)
    {
        eprintln!("{}: {e}", path.display());
    }
}

pub fn dump_builtin_skin(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let skin = Skin::builtin();
    for s in crate::skin::sprites::Sheet::ALL {
        if let Some(b) = skin.sheet(s) {
            save(b, &dir.join(format!("{}.png", s.file_stem())));
        }
    }
}

fn render(
    st: &mut State,
    k: WinKind,
    main: &mut MainWin,
    eq: &mut EqWin,
    pl: &mut PlWin,
) -> Bitmap {
    let (w, h) = st.base_size(k);
    let mut b = Bitmap::new(w, h, 0xFF00_0000);
    match k {
        WinKind::Main => main.render(st, &mut b),
        WinKind::Eq => eq.render(st, &mut b),
        WinKind::Playlist => pl.render(st, &mut b),
    }
    b
}

/// Main window, equalizer and playlist docked in one image, at 1x.
pub fn docked(skin: Skin) -> Bitmap {
    let mut st = State::new(Engine::headless(), skin);
    st.focused = Some(WinKind::Main);
    let (mut main, mut eq, mut pl) = (MainWin::new(), EqWin::new(), PlWin::new());
    let parts: Vec<Bitmap> = [WinKind::Main, WinKind::Eq, WinKind::Playlist]
        .into_iter()
        .map(|k| render(&mut st, k, &mut main, &mut eq, &mut pl))
        .collect();
    let w = parts.iter().map(|b| b.w).max().unwrap_or(0);
    let h = parts.iter().map(|b| b.h).sum();
    let mut out = Bitmap::new(w, h, 0xFF00_0000);
    let mut y = 0;
    for b in &parts {
        out.blit(b, 0, 0, b.w, b.h, 0, y);
        y += b.h;
    }
    out
}

/// Fixed scenarios matching the reference screenshots (tools/webamp shots).
fn render_scenarios(dir: &Path, skin: &Skin) {
    let skin_path = skin.path.clone();
    let fresh = || {
        let base = Skin::builtin();
        let skin = match &skin_path {
            Some(p) => Skin::load(p, &base).unwrap_or(base),
            None => base,
        };
        let mut st = State::new(Engine::headless(), skin);
        st.pl_size = (0, 4);
        st.focused = Some(WinKind::Main);
        st
    };
    let (mut main, mut eq, mut pl) = (MainWin::new(), EqWin::new(), PlWin::new());
    // Toggles, volume and balance.
    let mut st = fresh();
    st.shuffle = true;
    st.repeat = true;
    st.volume = 30;
    st.balance = -40;
    save(
        &render(&mut st, WinKind::Main, &mut main, &mut eq, &mut pl),
        &dir.join("sc-toggles-main.png"),
    );
    // Equalizer values (0 = +12 dB, 63 = -12 dB, 31 = centre).
    let mut st = fresh();
    st.focused = Some(WinKind::Eq);
    st.eq.enabled = true;
    st.eq_auto = true;
    st.eq.preamp = 0;
    st.eq.bands = [63, 0, 31, 63, 0, 31, 63, 0, 31, 63];
    save(
        &render(&mut st, WinKind::Eq, &mut main, &mut eq, &mut pl),
        &dir.join("sc-eqvals-eq.png"),
    );
    // Window shade modes.
    let mut st = fresh();
    st.main.shade = true;
    st.eqw.shade = true;
    st.pl.shade = true;
    save(
        &render(&mut st, WinKind::Main, &mut main, &mut eq, &mut pl),
        &dir.join("sc-shade-main.png"),
    );
    save(
        &render(&mut st, WinKind::Eq, &mut main, &mut eq, &mut pl),
        &dir.join("sc-shade-eq.png"),
    );
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("sc-shade-pl.png"),
    );
    // Resized playlist.
    let mut st = fresh();
    st.pl_size = (3, 2);
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("sc-plwide-pl.png"),
    );
    st.pl_size = (4, 2);
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("sc-plwide4-pl.png"),
    );
}

pub fn render_all(dir: &Path, skin_path: Option<&str>, files: &[String]) {
    let _ = std::fs::create_dir_all(dir);
    let base = Skin::builtin();
    let skin = match skin_path {
        Some(p) => Skin::load(Path::new(p), &base).unwrap_or_else(|e| {
            eprintln!("{e}");
            Skin::builtin()
        }),
        None => base,
    };
    render_scenarios(dir, &skin);
    let mut st = State::new(Engine::headless(), skin);
    st.volume = 78;
    let items: Vec<(Source, Option<String>, Option<f64>)> = files
        .iter()
        .map(|f| {
            let p = PathBuf::from(f);
            let info = crate::metadata::read(&p);
            (
                Source::File(p),
                info.as_ref().and_then(|i| i.display_title()),
                info.as_ref().and_then(|i| i.duration),
            )
        })
        .collect();
    st.playlist.append(items);
    let (mut main, mut eq, mut pl) = (MainWin::new(), EqWin::new(), PlWin::new());
    st.focused = Some(WinKind::Main);

    save(
        &render(&mut st, WinKind::Main, &mut main, &mut eq, &mut pl),
        &dir.join("main-stopped.png"),
    );
    st.pl_size = (0, 0);
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("playlist-small.png"),
    );
    st.pl_size = (0, 4);
    save(
        &render(&mut st, WinKind::Eq, &mut main, &mut eq, &mut pl),
        &dir.join("eq-default.png"),
    );
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("playlist-default.png"),
    );

    // Playing: a synthetic signal feeds the visualiser.
    let duration = st
        .playlist
        .entries
        .first()
        .and_then(|e| e.duration)
        .unwrap_or(225.0);
    let pos = std::env::var("FERRAMP_RENDER_POS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(83.0);
    st.engine.simulate(Status::Playing, pos);
    st.now.duration = Some(duration);
    st.now.kbps = 128;
    st.now.khz = 44;
    st.now.channels = 2;
    if st.playlist.current.is_none() && !st.playlist.is_empty() {
        st.playlist.current = Some(0);
    }
    st.vis_samples = (0..2048)
        .map(|i| {
            let t = i as f32 / 44100.0;
            0.35 * (t * 2.0 * std::f32::consts::PI * 110.0).sin()
                + 0.2 * (t * 2.0 * std::f32::consts::PI * 880.0).sin()
                + 0.1 * (t * 2.0 * std::f32::consts::PI * 5000.0).sin()
        })
        .collect();
    let samples = st.vis_samples.clone();
    let opts = st.vis_opts;
    for _ in 0..4 {
        st.vis.analyze(&samples, false, &opts);
    }
    save(
        &render(&mut st, WinKind::Main, &mut main, &mut eq, &mut pl),
        &dir.join("main-playing.png"),
    );
    st.vis_mode = crate::ui::vis::VisMode::Oscilloscope;
    save(
        &render(&mut st, WinKind::Main, &mut main, &mut eq, &mut pl),
        &dir.join("main-scope.png"),
    );
    st.vis_mode = crate::ui::vis::VisMode::Analyzer;
    st.main.shade = true;
    for _ in 0..4 {
        st.vis.analyze(&samples, true, &opts);
    }
    save(
        &render(&mut st, WinKind::Main, &mut main, &mut eq, &mut pl),
        &dir.join("main-shade.png"),
    );
    st.main.shade = false;

    st.focused = Some(WinKind::Eq);
    st.eq.enabled = true;
    let rock = crate::ui::menu::BUILTIN_PRESETS[8];
    st.eq.preamp = rock.1;
    st.eq.bands = rock.2;
    save(
        &render(&mut st, WinKind::Eq, &mut main, &mut eq, &mut pl),
        &dir.join("eq.png"),
    );
    st.eqw.shade = true;
    save(
        &render(&mut st, WinKind::Eq, &mut main, &mut eq, &mut pl),
        &dir.join("eq-shade.png"),
    );
    st.eqw.shade = false;

    st.focused = Some(WinKind::Playlist);
    st.pl_size = (0, 4);
    if st.playlist.len() > 1 {
        st.playlist.entries[1].selected = true;
    }
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("playlist.png"),
    );
    st.pl_size = (3, 4);
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("playlist-wide.png"),
    );
    st.pl.shade = true;
    save(
        &render(&mut st, WinKind::Playlist, &mut main, &mut eq, &mut pl),
        &dir.join("playlist-shade.png"),
    );
    println!("rendered to {}", dir.display());
}
