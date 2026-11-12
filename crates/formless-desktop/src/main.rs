//! `formless-desktop`: opens the Phase 0 shell over a synthetic fixture.
//!
//! It never starts, contacts or configures a Daemon Core.
//!
//! Options:
//! - `--sessions N`, `--blocks N`: size the fixture.
//! - `--huge-block-mb N`: add one N MiB output block at the tail.
//! - `--text-scale P`: start at P percent text size (100 to 200).
//! - `--window WxH`: open at this size instead of 1440x900.
//! - `--keep-window`: do not ask the window to grow to fit the text size, so
//!   a small window shows the reflowed shell. With `--smoke`, focus is then
//!   moved to each part of the page in turn and where the page scrolled to
//!   is printed.
//! - `--background`: do not bring the application to the front when it
//!   opens, so whatever the user is typing into keeps the keyboard.
//! - `--lifetime S`: quit by itself after S seconds. With `--background`,
//!   this is for inspecting the window from outside without leaving it open.
//! - `--smoke`: print startup timings up to the first rendered frame, quit.
//! - `--stream-seconds S` (with `--stream-rate R`, default 100 deltas per
//!   second; 0 streams nothing and measures the idle window): stream into
//!   the tail for S seconds while recording when frames are delivered,
//!   print the result, quit.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use formless_desktop::model::{tokens, Fit, TextScale};
use formless_desktop::shell::{PageSection, Shell};
use formless_desktop::{fixture, shell};
use gpui_kit::prelude::*;
use gpui_kit::{px, size, App, Bounds, Entity, Window, WindowBounds, WindowOptions};

const USAGE: &str = "usage: formless-desktop [--sessions N] [--blocks N] [--huge-block-mb N] \
                     [--text-scale P] [--window WxH] [--keep-window] \
                     [--background] [--lifetime S] \
                     [--smoke] [--stream-seconds S [--stream-rate R]]";

struct Options {
    sessions: usize,
    blocks: usize,
    huge_block_mb: usize,
    text_scale: usize,
    window: (f32, f32),
    keep_window: bool,
    background: bool,
    lifetime_seconds: usize,
    smoke: bool,
    stream_seconds: usize,
    stream_rate: usize,
}

fn parse_options() -> Result<Options, String> {
    let mut options = Options {
        sessions: 10_000,
        blocks: 10_000,
        huge_block_mb: 0,
        text_scale: 100,
        window: (1440., 900.),
        keep_window: false,
        background: false,
        lifetime_seconds: 0,
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
            "--text-scale" => options.text_scale = count("--text-scale")?,
            "--window" => {
                let value = args.next().ok_or("--window needs WIDTHxHEIGHT")?;
                options.window = value
                    .split_once('x')
                    .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                    .ok_or(format!("--window: {value:?} is not WIDTHxHEIGHT"))?;
            }
            "--keep-window" => options.keep_window = true,
            "--background" => options.background = true,
            "--lifetime" => options.lifetime_seconds = count("--lifetime")?,
            "--smoke" => options.smoke = true,
            "--stream-seconds" => options.stream_seconds = count("--stream-seconds")?,
            "--stream-rate" => options.stream_rate = count("--stream-rate")?,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(options)
}

/// Print what the window and the layout settled at. Returns whether the
/// shell is reflowed.
fn report_window(requested: (f32, f32), shell: &Entity<Shell>, window: &Window, cx: &App) -> bool {
    let viewport = window.viewport_size();
    let shell = shell.read(cx);
    let reflowed = shell.fit(window) == Fit::Reflowed;
    // The text size shown is the one asked for. Both are printed because a
    // window too small for it used to be given a smaller size.
    println!(
        "window: requested={}x{} viewport={}x{} text_scale_requested={} text_scale_shown={} fit={}",
        requested.0,
        requested.1,
        f32::from(viewport.width),
        f32::from(viewport.height),
        shell.text_scale().percent(),
        shell.text_scale().percent(),
        if reflowed { "reflowed" } else { "whole" },
    );
    if reflowed {
        let page = shell.page_metrics();
        let parts: Vec<String> = page
            .sections
            .iter()
            .map(|(section, part)| {
                format!(
                    "{section:?}:{:.0}..{:.0}x{:.0}..{:.0}",
                    f32::from(part.left()),
                    f32::from(part.right()),
                    f32::from(part.top()),
                    f32::from(part.bottom())
                )
            })
            .collect();
        println!(
            "page: viewport={:.0}x{:.0} scroll_max={:.0} parts_left..rightxtop..bottom={}",
            f32::from(page.viewport.size.width),
            f32::from(page.viewport.size.height),
            f32::from(page.scroll_max),
            parts.join(",")
        );
    }
    reflowed
}

/// Print where a reflowed page is after focus moved to one of its parts,
/// and whether that part is now inside the window.
fn report_section(section: PageSection, shell: &Entity<Shell>, cx: &App) {
    let page = shell.read(cx).page_metrics();
    let Some(part) = page.shown(section) else {
        println!("page-focus: section={section:?} MISSING");
        return;
    };
    println!(
        "page-focus: section={section:?} scrolled={:.0} of {:.0} shown_at={:.0}..{:.0} \
         window={:.0}..{:.0} inside={}",
        // Adding zero turns the negative zero of an unscrolled page into zero.
        f32::from(page.scrolled) + 0.0,
        f32::from(page.scroll_max),
        f32::from(part.top()),
        f32::from(part.bottom()),
        f32::from(page.viewport.top()),
        f32::from(page.viewport.bottom()),
        page.is_inside(section),
    );
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
                            shell.push_stream_delta(fixture::stream_delta(index), cx);
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
            // Where the streamed block ended up: how large it is, and how
            // much of it its row leaves out on each side. Nothing after the
            // window means the newest text is what is on show.
            let (tail_bytes, tail_before, tail_after) =
                shell.read(cx).last_block_window().unwrap_or_default();
            let work = shell.read(cx).stream_work();
            println!(
                "stream: seconds={seconds} rate={rate} deltas_sent={sent} deltas_received={deltas} \
                 view_updates={flushes} frames={} frame_interval_ms p50={:.1} p95={:.1} p99={:.1} \
                 max={:.1} over_16.7ms={} over_33.4ms={} over_100ms={} tail_bytes={tail_bytes} \
                 tail_not_shown_before={tail_before} tail_not_shown_after={tail_after} \
                 row_rebuilds={} row_bytes_copied={}",
                frames.len(),
                percentile(&intervals, 0.50),
                percentile(&intervals, 0.95),
                percentile(&intervals, 0.99),
                intervals.last().copied().unwrap_or(0.0),
                intervals.iter().filter(|ms| **ms > 16.7).count(),
                intervals.iter().filter(|ms| **ms > 33.4).count(),
                intervals.iter().filter(|ms| **ms > 100.0).count(),
                work.rebuilds,
                work.bytes_copied,
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

        let bounds = Bounds::centered(None, size(px(options.window.0), px(options.window.1)), cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(
                px(tokens::MIN_WINDOW_WIDTH),
                px(tokens::MIN_WINDOW_HEIGHT),
            )),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(window_options, cx, |window, cx| {
            window.set_window_title("Formless (Phase 0 spike)");
            let shell = cx.new(|cx| shell::Shell::new(&sessions, &running, blocks, window, cx));
            let scale = TextScale::from_percent(options.text_scale.min(1_000) as u16);
            shell.update(cx, |shell, cx| {
                if options.keep_window {
                    shell.set_text_scale(scale, window, cx)
                } else {
                    shell.set_text_scale_growing_window(scale, window, cx)
                }
            });
            shell
        })
        .expect("failed to open the shell window");
        if !options.background {
            cx.activate(true);
        }
        if options.lifetime_seconds > 0 {
            let lifetime = Duration::from_secs(options.lifetime_seconds as u64);
            cx.spawn(async move |cx| {
                cx.background_executor().timer(lifetime).await;
                cx.update(|cx| cx.quit());
            })
            .detach();
        }

        if options.smoke || options.stream_seconds > 0 {
            window
                .update(cx, |_, window, _| {
                    window.on_next_frame(move |window, cx| {
                        // Since process start: the platform application is
                        // up, then the synthetic data exists, then one frame
                        // of the shell has been drawn.
                        println!(
                            "smoke: sessions={} blocks={} huge_block_mb={} text_scale={} \
                             app_ready_ms={} fixture_ms={} first_frame_ms={}",
                            options.sessions,
                            options.blocks,
                            options.huge_block_mb,
                            options.text_scale,
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
                            let handle = window.window_handle();
                            let requested_window = options.window;
                            cx.spawn(async move |cx| {
                                cx.background_executor().timer(Duration::from_secs(1)).await;
                                // A second after the first frame: what the
                                // window and the layout settled at.
                                let reflowed = cx.update(|cx| {
                                    cx.update_window(handle, |_, window, cx| {
                                        report_window(requested_window, &shell, window, cx)
                                    })
                                    .unwrap_or(false)
                                });
                                if reflowed {
                                    // Every part of the page has to be
                                    // reachable: move focus to each in turn
                                    // and see where the page scrolled to.
                                    for section in [
                                        PageSection::Header,
                                        PageSection::Conversation,
                                        PageSection::Composer,
                                        PageSection::Status,
                                        PageSection::Header,
                                    ] {
                                        cx.update(|cx| {
                                            let _ = cx.update_window(handle, |_, window, cx| {
                                                shell.update(cx, |shell, cx| {
                                                    shell.focus_section(section, window, cx)
                                                });
                                            });
                                        });
                                        cx.background_executor()
                                            .timer(Duration::from_millis(300))
                                            .await;
                                        cx.update(|cx| report_section(section, &shell, cx));
                                    }
                                }
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
