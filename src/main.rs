//! Ferramp: a from-scratch, cross-platform reimplementation of the classic
#![cfg_attr(windows, windows_subsystem = "windows")]
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop
)]
//! skinnable desktop audio player.

/// Diagnostics on stderr when FERRAMP_DEBUG is set.
macro_rules! debug {
    ($($t:tt)*) => {
        if std::env::var_os("FERRAMP_DEBUG").is_some() {
            eprintln!($($t)*);
        }
    };
}

mod app;
mod audio;
mod classic_skin;
mod config;
mod gfx;
mod metadata;
mod museum;
mod net;
mod platform;
mod playlist;
mod plugins;
mod render;
mod skin;
mod state;
mod text;
mod ui;

use winit::event_loop::EventLoop;

fn usage() {
    println!(
        "Ferramp {}\n\nUsage: ferramp [options] [files, folders, playlists or URLs...]\n\n\
         Options:\n  \
         --skin <file.wsz>         use this skin\n  \
         --render <dir>            render every window to PNG files and exit\n  \
         --dump-skin <dir>         write the built-in skin's bitmaps as PNG files\n  \
         -h, --help                show this help\n  \
         -V, --version             show the version",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut skin_path: Option<String> = None;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                usage();
                return;
            }
            "-V" | "--version" => {
                println!("ferramp {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--skin" => {
                skin_path = args.get(i + 1).cloned();
                i += 1;
            }
            "--dump-skin" => {
                let dir = args
                    .get(i + 1)
                    .cloned()
                    .unwrap_or_else(|| "skin-dump".into());
                render::dump_builtin_skin(std::path::Path::new(&dir));
                return;
            }
            "--render" => {
                let dir = args.get(i + 1).cloned().unwrap_or_else(|| "render".into());
                let mut rest: Vec<String> = args.drain((i + 2).min(args.len())..).collect();
                // --skin may also follow --render DIR, as the README shows.
                if let Some(j) = rest.iter().position(|a| a == "--skin")
                    && j + 1 < rest.len()
                {
                    skin_path = Some(rest.remove(j + 1));
                    rest.remove(j);
                }
                render::render_all(std::path::Path::new(&dir), skin_path.as_deref(), &rest);
                return;
            }
            _ => files.push(args[i].clone()),
        }
        i += 1;
    }

    let mut builder = EventLoop::<app::UserEvent>::with_user_event();
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Docked, freely positioned borderless windows need X11 (XWayland on
        // Wayland desktops). FERRAMP_WAYLAND=1 opts into native Wayland.
        use winit::platform::x11::EventLoopBuilderExtX11;
        if std::env::var_os("FERRAMP_WAYLAND").is_none() && std::env::var_os("DISPLAY").is_some() {
            builder.with_x11();
        }
    }
    let event_loop = match builder.build() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("ferramp: cannot open a display: {e}");
            std::process::exit(1);
        }
    };
    let engine = audio::Engine::new();
    if let Some(e) = &engine.device_error {
        eprintln!("ferramp: audio output unavailable: {e}");
    }
    let mut skin = skin::Skin::builtin();
    if let Some(p) = &skin_path {
        match skin::Skin::load(std::path::Path::new(p), &skin) {
            Ok(s) => skin = s,
            Err(e) => eprintln!("ferramp: {e}"),
        }
    }
    let st = state::State::new(engine, skin);
    let mut app = app::App::new(st, event_loop.create_proxy(), files);
    if skin_path.is_some() {
        app.keep_cli_skin();
    }
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("ferramp: {e}");
    }
}
