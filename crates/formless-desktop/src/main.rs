//! `formless-desktop`: opens the Phase 0 shell over a synthetic fixture.
//!
//! It never starts, contacts or configures a Daemon Core.
//!
//! Options:
//! - `--sessions N`, `--blocks N`: size the fixture.
//! - `--huge-block-mb N`: add one N MiB output block at the tail.
//! - `--smoke`: print startup timings up to the first rendered frame, quit.
//! - `--stream-seconds S` (with `--stream-rate R`, default 100 deltas per
//!   second; 0 streams nothing and measures the idle window): stream into
//!   the tail for S seconds while recording when frames are delivered,
//!   print the result, quit.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use formless_desktop::shell::Shell;
use formless_desktop::{fixture, shell};
use gpui_kit::prelude::*;
use gpui_kit::{px, size, App, Bounds, Entity, Window, WindowBounds, WindowOptions};

const USAGE: &str = "usage: formless-desktop [--sessions N] [--blocks N] [--huge-block-mb N] \
                     [--smoke] [--stream-seconds S [--stream-rate R]]";

struct Options {
    sessions: usize,
    blocks: usize,
    huge_block_mb: usize,
    smoke: bool,
    stream_seconds: usize,
    stream_rate: usize,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        sessions: 10_000,
        blocks: 10_000,
        huge_block_mb: 0,
        smoke: false,
        stream_seconds: 0,
        stream_rate: 100,
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
            "--huge-block-mb" => options.huge_block_mb = count("--huge-block-mb")?,
            "--smoke" => options.smoke = true,
            "--stream-seconds" => options.stream_seconds = count("--stream-seconds")?,
            "--stream-rate" => options.stream_rate = count("--stream-rate")?,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(options)
}

/// Record when each display frame is delivered, for as long as `running`
/// holds. A frame callback does not dirty the window, so this observes the
/// frame cadence without adding redraws of its own.
fn sample_frames(window: &Window, frames: Rc<RefCell<Vec<Instant>>>, running: Rc<RefCell<bool>>) {
    window.on_next_frame(move |window, _| {
        frames.borrow_mut().push(Instant::now());
        if *running.borrow() {
            sample_frames(window, frames, running);
        }
    });
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

/// Stream into the tail for `seconds` at `rate` deltas per second, then
/// print how frames were delivered meanwhile and quit.
fn run_stream(window: &Window, shell: Entity<Shell>, seconds: usize, rate: usize, cx: &mut App) {
    const TICK: Duration = Duration::from_millis(10);
    const WORDS: [&str; 8] = [
        "The ",
        "stream ",
        "keeps ",
        "going ",
        "ต่อไป ",
        "続く ",
        "and ",
        "on. ",
    ];
    let frames = Rc::new(RefCell::new(Vec::new()));
    let running = Rc::new(RefCell::new(true));
    sample_frames(window, frames.clone(), running.clone());
    if rate > 0 {
        shell.update(cx, |shell, cx| shell.begin_stream(cx));
    }

    cx.spawn(async move |cx| {
        let started = Instant::now();
        let length = Duration::from_secs(seconds as u64);
        let mut sent = 0usize;
        while started.elapsed() < length {
            cx.background_executor().timer(TICK).await;
            // Send whatever the rate says is due by now, so a late tick does
            // not lower the rate.
            let due = (started.elapsed().as_secs_f64() * rate as f64) as usize;
            if due > sent {
                cx.update(|cx| {
                    shell.update(cx, |shell, cx| {
                        for index in sent..due {
                            let word = if index % 60 == 59 {
                                "\n\n"
                            } else {
                                WORDS[index % WORDS.len()]
                            };
                            shell.push_stream_delta(word, cx);
                        }
                    })
                });
                sent = due;
            }
        }
        cx.update(|cx| {
            if rate > 0 {
                shell.update(cx, |shell, cx| shell.finish_stream(cx));
            }
            *running.borrow_mut() = false;
            let frames = frames.borrow();
            let mut intervals: Vec<f64> = frames
                .windows(2)
                .map(|pair| (pair[1] - pair[0]).as_secs_f64() * 1000.0)
                .collect();
            intervals.sort_by(|a, b| a.total_cmp(b));
            let (deltas, flushes) = shell.read(cx).stream_stats();
            println!(
                "stream: seconds={seconds} rate={rate} deltas_sent={sent} deltas_received={deltas} \
                 view_updates={flushes} frames={} frame_interval_ms p50={:.1} p95={:.1} p99={:.1} \
                 max={:.1} over_16.7ms={} over_33.4ms={} over_100ms={}",
                frames.len(),
                percentile(&intervals, 0.50),
                percentile(&intervals, 0.95),
                percentile(&intervals, 0.99),
                intervals.last().copied().unwrap_or(0.0),
                intervals.iter().filter(|ms| **ms > 16.7).count(),
                intervals.iter().filter(|ms| **ms > 33.4).count(),
                intervals.iter().filter(|ms| **ms > 100.0).count(),
            );
        });
        cx.background_executor()
            .timer(Duration::from_millis(300))
            .await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

fn main() {
    let started = Instant::now();
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("formless-desktop: {message}");
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    gpui_kit::application().run(move |cx: &mut App| {
        gpui_kit::init(cx);
        shell::bind_keys(cx);

        let app_ready = started.elapsed();
        let sessions = fixture::sessions(options.sessions);
        let running = fixture::running(&sessions);
        let mut blocks = fixture::timeline(options.blocks);
        if options.huge_block_mb > 0 {
            blocks.push(fixture::huge_block(
                blocks.len() as u64 + 1,
                options.huge_block_mb * 1024 * 1024,
            ));
        }
        let fixture_built = started.elapsed() - app_ready;

        let bounds = Bounds::centered(None, size(px(1440.), px(900.)), cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.), px(360.))),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(window_options, cx, |window, cx| {
            window.set_window_title("Formless (Phase 0 spike)");
            cx.new(|cx| shell::Shell::new(&sessions, &running, blocks, window, cx))
        })
        .expect("failed to open the shell window");
        cx.activate(true);

        if options.smoke || options.stream_seconds > 0 {
            window
                .update(cx, |_, window, _| {
                    window.on_next_frame(move |window, cx| {
                        // Since process start: the platform application is
                        // up, then the synthetic data exists, then one frame
                        // of the shell has been drawn.
                        println!(
                            "smoke: sessions={} blocks={} huge_block_mb={} app_ready_ms={} \
                             fixture_ms={} first_frame_ms={}",
                            options.sessions,
                            options.blocks,
                            options.huge_block_mb,
                            app_ready.as_millis(),
                            fixture_built.as_millis(),
                            started.elapsed().as_millis()
                        );
                        if options.stream_seconds > 0 {
                            run_stream(
                                window,
                                shell,
                                options.stream_seconds,
                                options.stream_rate,
                                cx,
                            );
                        } else {
                            cx.spawn(async move |cx| {
                                cx.background_executor().timer(Duration::from_secs(1)).await;
                                cx.update(|cx| cx.quit());
                            })
                            .detach();
                        }
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
