//! `formless-desktop`: opens the Phase 0 shell over a synthetic fixture.
//!
//! It never starts, contacts or configures a Daemon Core.
//!
//! Options: `--sessions N` and `--blocks N` size the fixture;
//! `--smoke` prints startup timings up to the first rendered frame and quits.

use std::time::{Duration, Instant};

use formless_desktop::{fixture, shell};
use gpui_kit::prelude::*;
use gpui_kit::{px, size, App, Bounds, WindowBounds, WindowOptions};

struct Options {
    sessions: usize,
    blocks: usize,
    smoke: bool,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        sessions: 10_000,
        blocks: 10_000,
        smoke: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut count = |name: &str| -> Result<usize, String> {
            let value = args.next().ok_or(format!("{name} needs a number"))?;
            value
                .parse()
                .map_err(|_| format!("{name}: {value:?} is not a number"))
        };
        match arg.as_str() {
            "--sessions" => options.sessions = count("--sessions")?,
            "--blocks" => options.blocks = count("--blocks")?,
            "--smoke" => options.smoke = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(options)
}

fn main() {
    let started = Instant::now();
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("formless-desktop: {message}");
            eprintln!("usage: formless-desktop [--sessions N] [--blocks N] [--smoke]");
            std::process::exit(2);
        }
    };

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);

        let app_ready = started.elapsed();
        let sessions = fixture::sessions(options.sessions);
        let running = fixture::running(&sessions);
        let blocks = fixture::timeline(options.blocks);
        let fixture_built = started.elapsed() - app_ready;

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(360.))),
            ..Default::default()
        };
        let (window, _) = gpui_kit::open_window(window_options, cx, |window, cx| {
            window.set_window_title("Formless (Phase 0 spike)");
            cx.new(|cx| shell::Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("failed to open the shell window");
        cx.activate(true);

        if options.smoke {
            window
                .update(cx, |_, window, _| {
                    window.on_next_frame(move |_, cx| {
                        // Since process start: the platform application is
                        // up, then the synthetic data exists, then one frame
                        // of the shell has been drawn.
                        println!(
                            "smoke: sessions={} blocks={} app_ready_ms={} fixture_ms={} first_frame_ms={}",
                            options.sessions,
                            options.blocks,
                            app_ready.as_millis(),
                            fixture_built.as_millis(),
                            started.elapsed().as_millis()
                        );
                        cx.spawn(async move |cx| {
                            cx.background_executor().timer(Duration::from_secs(1)).await;
                            cx.update(|cx| cx.quit());
                        })
                        .detach();
                    });
                })
                .expect("shell window closed before its first frame");
        } else {
            // One main window per Profile: closing it ends this process. It
            // does not stop any Run, because no Daemon Core is involved.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
        }
    });
}
